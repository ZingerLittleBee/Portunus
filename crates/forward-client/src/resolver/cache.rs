//! TTL-clamped DNS cache with single-flight coalescing.
//!
//! Spec: `003-domain-name-forward` `data-model.md` §
//! `ResolutionCacheEntry`. US1 ships only the happy-path variants
//! (`Pending`, `Resolved`); US2 (T029, T032) extends the state
//! machine with `StaleAfterFailedRefresh` and `Failed`.
//!
//! Lock discipline: the cache is `Arc<Mutex<HashMap<…>>>`. The mutex is
//! never held across an `await` — `get_or_resolve` either returns
//! cached data immediately, or releases the mutex before awaiting a
//! resolver call, then re-acquires to insert. Single-flight is
//! preserved by inserting a `Pending { notify }` placeholder before
//! releasing the mutex (US2 extension).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forward_core::Hostname;
use tokio::sync::Mutex;

use super::{Resolve, ResolverConfig, ResolverError};

#[derive(Debug, Clone)]
pub(super) enum CacheEntry {
    /// US1: a successful resolver answer with at least one address.
    /// `expiry` already incorporates the TTL clamp from
    /// `ResolverConfig::cache_floor`/`cache_ceiling`.
    Resolved { addrs: Vec<IpAddr>, expiry: Instant },
    // US2 extends with: Pending { notify, started_at },
    // StaleAfterFailedRefresh { stale_addrs, fail_grace_until },
    // Failed { retry_after, last_reason }.
}

#[derive(Debug, Default, Clone)]
pub(super) struct Cache {
    inner: Arc<Mutex<HashMap<Hostname, CacheEntry>>>,
}

impl Cache {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Look up `name` in the cache. On hit return the cached addrs;
    /// on miss invoke `resolver.resolve(name)` exactly once, clamp
    /// the resolver-reported TTL, store, and return the addrs.
    ///
    /// US1 scope: no single-flight; concurrent misses each invoke the
    /// resolver. US2 (T030) adds the `Pending` placeholder so
    /// concurrent waiters await an `Arc<Notify>` instead.
    pub(super) async fn get_or_resolve<R: Resolve + ?Sized>(
        &self,
        name: &Hostname,
        resolver: &R,
        config: &ResolverConfig,
    ) -> Result<Vec<IpAddr>, ResolverError> {
        // Cache-hit fast path: drop the lock before returning.
        {
            let guard = self.inner.lock().await;
            if let Some(CacheEntry::Resolved { addrs, expiry }) = guard.get(name) {
                if Instant::now() < *expiry {
                    return Ok(addrs.clone());
                }
            }
        }

        // Cache miss: resolve, clamp TTL, store. (Single-flight in US2.)
        let answer = resolver.resolve(name).await?;
        if answer.addrs.is_empty() {
            return Err(ResolverError::EmptyAnswer);
        }
        let clamped = clamp_ttl(answer.ttl, config);
        let entry = CacheEntry::Resolved {
            addrs: answer.addrs.clone(),
            expiry: Instant::now() + clamped,
        };
        self.inner.lock().await.insert(name.clone(), entry);
        Ok(answer.addrs)
    }

    #[cfg(test)]
    pub(super) async fn snapshot(&self) -> HashMap<Hostname, CacheEntry> {
        self.inner.lock().await.clone()
    }
}

fn clamp_ttl(ttl: Duration, config: &ResolverConfig) -> Duration {
    ttl.clamp(config.cache_floor, config.cache_ceiling)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::ResolveAnswer;

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts every `resolve()` invocation so tests can assert
    /// single-flight / cache-hit semantics.
    #[derive(Debug, Default)]
    struct CountingResolver {
        calls: AtomicUsize,
        ttl: Duration,
        addrs: Vec<IpAddr>,
    }

    impl CountingResolver {
        fn new(addrs: Vec<IpAddr>, ttl: Duration) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                ttl,
                addrs,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait::async_trait]
    impl Resolve for CountingResolver {
        async fn resolve(&self, _name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(ResolveAnswer {
                addrs: self.addrs.clone(),
                ttl: self.ttl,
            })
        }
    }

    #[tokio::test]
    async fn cold_then_hot_calls_resolver_once() {
        // T013: cold lookup → resolver invoked once → hot lookup
        // returns cached addrs without invoking the resolver.
        let host = Hostname::new("api.example.com").unwrap();
        let resolver = CountingResolver::new(
            vec!["10.0.0.5".parse().unwrap()],
            Duration::from_secs(60),
        );
        let cfg = ResolverConfig::default();
        let cache = Cache::new();

        let first = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        assert_eq!(first, vec!["10.0.0.5".parse::<IpAddr>().unwrap()]);
        assert_eq!(resolver.calls(), 1);

        // Hot path: same name, no second resolver call.
        let second = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        assert_eq!(second, first);
        assert_eq!(resolver.calls(), 1, "cache hit must not invoke resolver");
    }

    #[tokio::test]
    async fn empty_resolver_answer_is_an_error() {
        let host = Hostname::new("nowhere.example").unwrap();
        let resolver = CountingResolver::new(vec![], Duration::from_secs(60));
        let cfg = ResolverConfig::default();
        let cache = Cache::new();

        let err = cache
            .get_or_resolve(&host, &resolver, &cfg)
            .await
            .unwrap_err();
        assert!(matches!(err, ResolverError::EmptyAnswer));
    }

    #[tokio::test]
    async fn ttl_below_floor_is_clamped_up() {
        // FR-003: TTL=0 must clamp to cache_floor (5 s) so we don't
        // turn the client into a resolver-amplification source.
        let host = Hostname::new("api.example.com").unwrap();
        let resolver = CountingResolver::new(
            vec!["10.0.0.5".parse().unwrap()],
            Duration::from_secs(0),
        );
        let cfg = ResolverConfig::default();
        let cache = Cache::new();

        let _ = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        let snap = cache.snapshot().await;
        let CacheEntry::Resolved { expiry, .. } = snap.get(&host).cloned().unwrap();
        let remaining = expiry - Instant::now();
        // Floor is 5 s; allow a small lower bound to absorb
        // wall-clock drift between the get_or_resolve return and the
        // snapshot read.
        assert!(
            remaining >= Duration::from_secs(4),
            "TTL=0 should clamp to floor 5 s, got remaining {remaining:?}"
        );
    }

    #[tokio::test]
    async fn ttl_above_ceiling_is_clamped_down() {
        // FR-003: TTL=24h must clamp to cache_ceiling (5 min).
        let host = Hostname::new("api.example.com").unwrap();
        let resolver = CountingResolver::new(
            vec!["10.0.0.5".parse().unwrap()],
            Duration::from_secs(86_400),
        );
        let cfg = ResolverConfig::default();
        let cache = Cache::new();

        let _ = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        let snap = cache.snapshot().await;
        let CacheEntry::Resolved { expiry, .. } = snap.get(&host).cloned().unwrap();
        let remaining = expiry - Instant::now();
        assert!(
            remaining <= Duration::from_secs(300),
            "TTL=24h should clamp to ceiling 5 min, got remaining {remaining:?}"
        );
    }

}
