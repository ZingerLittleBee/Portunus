//! TTL-clamped DNS cache with single-flight coalescing and
//! stale-while-error grace.
//!
//! Spec: `003-domain-name-forward` `data-model.md` §
//! `ResolutionCacheEntry`. US1 shipped the happy-path variants
//! (`Pending` placeholder unused, `Resolved`); US2 (this module)
//! activates the full state machine: `Pending { notify }`,
//! `StaleAfterFailedRefresh { stale_addrs, fail_grace_until }`, and
//! `Failed { retry_after, last_reason }`.
//!
//! Lock discipline: the cache is `Arc<Mutex<HashMap<…>>>`. The mutex
//! is never held across an `await`. Single-flight is preserved by
//! inserting a `Pending { notify }` placeholder under the lock,
//! releasing the lock before awaiting the resolver, then re-acquiring
//! to write the result and `notify_waiters()`.
//!
//! Time abstraction: a `Clock` trait (see `super::clock`) is injected
//! so unit tests can advance time deterministically without
//! `tokio::time::sleep`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forward_core::Hostname;
use tokio::sync::{Mutex, Notify};

use super::clock::{Clock, SystemClock};
use super::{Resolve, ResolverError};

#[derive(Debug, Clone)]
pub(super) enum CacheEntry {
    /// A resolver call is in-flight. Concurrent waiters block on
    /// `notify`; the in-flight task overwrites this entry with
    /// `Resolved` / `Failed` / `StaleAfterFailedRefresh` and calls
    /// `notify_waiters()`.
    Pending {
        notify: Arc<Notify>,
        #[allow(dead_code)]
        started_at: Instant,
    },
    /// Successful resolver answer. `expiry` already incorporates the
    /// `[cache_floor, cache_ceiling]` clamp.
    Resolved { addrs: Vec<IpAddr>, expiry: Instant },
    /// Past TTL, fresh refresh just failed; we keep serving the last
    /// successful `stale_addrs` until `fail_grace_until` (FR-005).
    StaleAfterFailedRefresh {
        stale_addrs: Vec<IpAddr>,
        fail_grace_until: Instant,
    },
    /// Negative-cache window: most recent attempt failed and the
    /// stale grace (if any) is exhausted. New lookups before
    /// `retry_after` short-circuit with `last_reason`; lookups
    /// after fall through and trigger another resolver attempt.
    Failed {
        retry_after: Instant,
        last_reason: ResolverError,
    },
}

/// Discriminates *how* `get_or_resolve` produced the addrs. Lets the
/// caller log "fresh resolution" vs "cache hit" (T035) and bump the
/// dns_failures counter on stale-window hits (FR-005, US4 wires the
/// counter — US2 just exposes the signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerSource {
    /// The resolver was just called and returned a fresh answer.
    Fresh,
    /// A non-expired `Resolved` entry served the request.
    Cached,
    /// Fresh refresh failed; served stale addrs from the
    /// stale-while-error grace window.
    Stale,
}

#[derive(Debug, Clone)]
pub struct CacheResult {
    pub addrs: Vec<IpAddr>,
    pub source: AnswerSource,
}

#[derive(Debug, Clone)]
pub(super) struct Cache {
    inner: Arc<Mutex<HashMap<Hostname, CacheEntry>>>,
    clock: Arc<dyn Clock>,
}

impl Cache {
    pub(super) fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    pub(super) fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            clock,
        }
    }

    /// Look up `name` in the cache. State-machine transitions per
    /// `data-model.md`:
    ///   Resolved (not expired)         → Cached   (no resolver call)
    ///   Resolved (expired)             → refresh: Resolved / StaleAfterFailedRefresh
    ///   Pending                        → wait on notify, then re-read
    ///   StaleAfterFailedRefresh (in grace) → Stale (no resolver call within window)
    ///   Failed (within retry_after)    → Err (no resolver call)
    ///   Failed (after retry_after)     → refresh
    ///   miss                           → fresh resolve
    pub(super) async fn get_or_resolve<R: Resolve + ?Sized>(
        &self,
        name: &Hostname,
        resolver: &R,
        config: &super::ResolverConfig,
    ) -> Result<CacheResult, ResolverError> {
        // The state machine may need multiple iterations: a Pending
        // wait can resolve into a Failed entry whose retry_after has
        // already passed, etc. Cap to a few rounds to bail on
        // pathological races rather than spin forever.
        for _ in 0..4 {
            let action = {
                let mut guard = self.inner.lock().await;
                let now = self.clock.now();
                match guard.get(name) {
                    Some(CacheEntry::Resolved { addrs, expiry }) => {
                        if now < *expiry {
                            return Ok(CacheResult {
                                addrs: addrs.clone(),
                                source: AnswerSource::Cached,
                            });
                        }
                        // Expired — fall through to refresh, which
                        // installs a Pending so concurrent expiry
                        // refreshes coalesce.
                        let stale = addrs.clone();
                        let notify = Arc::new(Notify::new());
                        guard.insert(
                            name.clone(),
                            CacheEntry::Pending {
                                notify: Arc::clone(&notify),
                                started_at: now,
                            },
                        );
                        Action::Refresh {
                            stale_addrs: Some(stale),
                        }
                    }
                    Some(CacheEntry::Pending { notify, .. }) => Action::Wait {
                        notify: Arc::clone(notify),
                    },
                    Some(CacheEntry::StaleAfterFailedRefresh {
                        stale_addrs,
                        fail_grace_until,
                    }) => {
                        if now < *fail_grace_until {
                            return Ok(CacheResult {
                                addrs: stale_addrs.clone(),
                                source: AnswerSource::Stale,
                            });
                        }
                        // Grace expired — drop to a refresh attempt,
                        // installing a Pending placeholder.
                        let notify = Arc::new(Notify::new());
                        guard.insert(
                            name.clone(),
                            CacheEntry::Pending {
                                notify: Arc::clone(&notify),
                                started_at: now,
                            },
                        );
                        Action::Refresh { stale_addrs: None }
                    }
                    Some(CacheEntry::Failed {
                        retry_after,
                        last_reason,
                    }) => {
                        if now < *retry_after {
                            return Err(last_reason.clone());
                        }
                        // Retry window expired — install Pending and
                        // try again.
                        let notify = Arc::new(Notify::new());
                        guard.insert(
                            name.clone(),
                            CacheEntry::Pending {
                                notify: Arc::clone(&notify),
                                started_at: now,
                            },
                        );
                        Action::Refresh { stale_addrs: None }
                    }
                    None => {
                        let notify = Arc::new(Notify::new());
                        guard.insert(
                            name.clone(),
                            CacheEntry::Pending {
                                notify: Arc::clone(&notify),
                                started_at: now,
                            },
                        );
                        Action::Refresh { stale_addrs: None }
                    }
                }
            };

            match action {
                Action::Wait { notify } => {
                    notify.notified().await;
                    // Re-read the cache below.
                    continue;
                }
                Action::Refresh { stale_addrs } => {
                    let outcome = resolver.resolve(name).await;
                    let next = self.apply_refresh(name, outcome, stale_addrs, config).await;
                    return next;
                }
            }
        }
        // Pathological: bail with a generic error. In practice a
        // single Refresh path always returns within the loop.
        Err(ResolverError::Lookup("cache state oscillation".into()))
    }

    async fn apply_refresh(
        &self,
        name: &Hostname,
        outcome: Result<super::ResolveAnswer, ResolverError>,
        stale_addrs: Option<Vec<IpAddr>>,
        config: &super::ResolverConfig,
    ) -> Result<CacheResult, ResolverError> {
        let now = self.clock.now();
        match outcome {
            Ok(answer) if answer.addrs.is_empty() => {
                // Empty-answer set is a failure per data-model invariant.
                self.commit_failure(
                    name,
                    ResolverError::EmptyAnswer,
                    stale_addrs,
                    config,
                    now,
                )
                .await
            }
            Ok(answer) => {
                let clamped = clamp_ttl(answer.ttl, config);
                let entry = CacheEntry::Resolved {
                    addrs: answer.addrs.clone(),
                    expiry: now + clamped,
                };
                let mut guard = self.inner.lock().await;
                let prev = guard.insert(name.clone(), entry);
                if let Some(CacheEntry::Pending { notify, .. }) = prev {
                    notify.notify_waiters();
                }
                Ok(CacheResult {
                    addrs: answer.addrs,
                    source: AnswerSource::Fresh,
                })
            }
            Err(err) => {
                self.commit_failure(name, err, stale_addrs, config, now)
                    .await
            }
        }
    }

    async fn commit_failure(
        &self,
        name: &Hostname,
        err: ResolverError,
        stale_addrs: Option<Vec<IpAddr>>,
        config: &super::ResolverConfig,
        now: Instant,
    ) -> Result<CacheResult, ResolverError> {
        let entry = if let Some(stale) = stale_addrs.clone() {
            // We had a previous successful answer — keep it for the
            // grace window before transitioning to Failed.
            CacheEntry::StaleAfterFailedRefresh {
                stale_addrs: stale,
                fail_grace_until: now + config.stale_while_error_grace,
            }
        } else {
            CacheEntry::Failed {
                retry_after: now + config.negative_cache_retry,
                last_reason: err.clone(),
            }
        };

        let mut guard = self.inner.lock().await;
        let prev = guard.insert(name.clone(), entry);
        if let Some(CacheEntry::Pending { notify, .. }) = prev {
            notify.notify_waiters();
        }
        if let Some(stale) = stale_addrs {
            // Serve the just-installed stale addrs to *this* caller.
            // The cache state is now StaleAfterFailedRefresh and any
            // concurrent caller will read it on next wakeup.
            Ok(CacheResult {
                addrs: stale,
                source: AnswerSource::Stale,
            })
        } else {
            Err(err)
        }
    }

}

enum Action {
    Wait { notify: Arc<Notify> },
    Refresh { stale_addrs: Option<Vec<IpAddr>> },
}

fn clamp_ttl(ttl: Duration, config: &super::ResolverConfig) -> Duration {
    ttl.clamp(config.cache_floor, config.cache_ceiling)
}

#[cfg(test)]
mod tests {
    use super::super::ResolverConfig;
    use super::super::test_support::{MockClock, MockResolver};
    use super::*;
    use std::net::IpAddr;
    use std::sync::Arc;
    use std::time::Duration;

    fn host(name: &str) -> Hostname {
        Hostname::new(name).unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn fresh_cache() -> (Cache, Arc<MockClock>) {
        let clock = Arc::new(MockClock::new());
        (Cache::with_clock(clock.clone()), clock)
    }

    /// T013 (US1, regression): cold lookup → resolver fires once,
    /// hot lookup serves from cache.
    #[tokio::test]
    async fn cold_then_hot_calls_resolver_once() {
        let (cache, _clock) = fresh_cache();
        let host = host("api.example.com");
        let resolver = MockResolver::ok(vec![ip("10.0.0.5")], Duration::from_secs(60));
        let cfg = ResolverConfig::default();

        let first = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        assert_eq!(first.addrs, vec![ip("10.0.0.5")]);
        assert_eq!(first.source, AnswerSource::Fresh);
        assert_eq!(resolver.calls(), 1);

        let second = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        assert_eq!(second.addrs, vec![ip("10.0.0.5")]);
        assert_eq!(second.source, AnswerSource::Cached);
        assert_eq!(resolver.calls(), 1);
    }

    #[tokio::test]
    async fn empty_resolver_answer_is_an_error() {
        let (cache, _clock) = fresh_cache();
        let host = host("nowhere.example");
        let resolver = MockResolver::ok(vec![], Duration::from_secs(60));
        let cfg = ResolverConfig::default();

        let err = cache
            .get_or_resolve(&host, &resolver, &cfg)
            .await
            .unwrap_err();
        assert!(matches!(err, ResolverError::EmptyAnswer));
    }

    /// T024 (FR-003): TTL clamp boundaries.
    #[tokio::test]
    async fn ttl_below_floor_is_clamped_up() {
        let (cache, clock) = fresh_cache();
        let host = host("api.example.com");
        let resolver = MockResolver::ok(vec![ip("10.0.0.5")], Duration::from_secs(0));
        let cfg = ResolverConfig::default();

        let _ = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        // Just shy of 5 s should still be cached.
        clock.advance(Duration::from_secs(4));
        let r = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        assert_eq!(r.source, AnswerSource::Cached);
        assert_eq!(resolver.calls(), 1, "TTL=0 must clamp up to 5 s floor");
    }

    #[tokio::test]
    async fn ttl_above_ceiling_is_clamped_down() {
        let (cache, clock) = fresh_cache();
        let host = host("api.example.com");
        let resolver = MockResolver::ok(vec![ip("10.0.0.5")], Duration::from_secs(86_400));
        let cfg = ResolverConfig::default();

        let _ = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        // Just past 5 min ceiling — entry should be expired and
        // resolver re-fired.
        clock.advance(Duration::from_secs(301));
        let _ = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        assert_eq!(resolver.calls(), 2, "TTL=24h must clamp down to 5 min");
    }

    #[tokio::test]
    async fn ttl_inside_clamp_window_passes_through() {
        let (cache, clock) = fresh_cache();
        let host = host("api.example.com");
        let resolver = MockResolver::ok(vec![ip("10.0.0.5")], Duration::from_secs(30));
        let cfg = ResolverConfig::default();

        let _ = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        clock.advance(Duration::from_secs(29));
        let r = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        assert_eq!(r.source, AnswerSource::Cached);
        clock.advance(Duration::from_secs(2)); // past 30s
        let _ = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        assert_eq!(resolver.calls(), 2, "TTL=30s should expire at 30s exactly");
    }

    /// T023 (FR-012): single-flight — N concurrent waiters during
    /// the Pending window MUST share one resolver call.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn single_flight_coalesces_concurrent_misses() {
        let cache = Cache::new(); // SystemClock — we use real time delays here.
        let host = host("api.example.com");
        let resolver = Arc::new(MockResolver::delayed_ok(
            vec![ip("10.0.0.5")],
            Duration::from_secs(60),
            Duration::from_millis(150),
        ));
        let cfg = ResolverConfig::default();

        let mut handles = Vec::new();
        for _ in 0..8 {
            let cache = cache.clone();
            let host = host.clone();
            let resolver = Arc::clone(&resolver);
            let cfg = cfg;
            handles.push(tokio::spawn(async move {
                cache
                    .get_or_resolve(&host, resolver.as_ref(), &cfg)
                    .await
                    .unwrap()
            }));
        }
        for h in handles {
            let r = h.await.unwrap();
            assert_eq!(r.addrs, vec![ip("10.0.0.5")]);
        }
        assert_eq!(
            resolver.calls(),
            1,
            "FR-012: single-flight should coalesce all waiters into one resolver call"
        );
    }

    /// T025 (FR-005): stale-while-error grace.
    #[tokio::test]
    async fn refresh_failure_serves_stale_within_grace() {
        let (cache, clock) = fresh_cache();
        let host = host("api.example.com");
        let resolver = MockResolver::ok_then_fail(
            vec![ip("10.0.0.5")],
            Duration::from_secs(10),
            ResolverError::Lookup("nxdomain".into()),
        );
        let cfg = ResolverConfig::default();

        // Prime the cache.
        let r1 = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        assert_eq!(r1.source, AnswerSource::Fresh);
        assert_eq!(resolver.calls(), 1);

        // Past TTL — fresh refresh fires (and fails); stale served.
        clock.advance(Duration::from_secs(11));
        let r2 = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        assert_eq!(r2.source, AnswerSource::Stale);
        assert_eq!(r2.addrs, vec![ip("10.0.0.5")]);
        assert_eq!(resolver.calls(), 2);

        // Within grace — no resolver call, still stale.
        clock.advance(Duration::from_secs(10));
        let r3 = cache.get_or_resolve(&host, &resolver, &cfg).await.unwrap();
        assert_eq!(r3.source, AnswerSource::Stale);
        assert_eq!(resolver.calls(), 2, "in-grace lookups must not re-resolve");

        // Past grace (30s default) — refresh fires again, fails;
        // having no stale to fall back to (state was StaleAfterFailedRefresh
        // and grace expired), the cache transitions to Failed.
        clock.advance(Duration::from_secs(25));
        let err = cache
            .get_or_resolve(&host, &resolver, &cfg)
            .await
            .unwrap_err();
        assert!(matches!(err, ResolverError::Lookup(_)));
        assert_eq!(resolver.calls(), 3, "post-grace lookup must re-attempt");
    }

    /// T026: after grace expiry, the negative-cache `retry_after`
    /// window prevents back-to-back resolver calls on every request.
    #[tokio::test]
    async fn failed_state_negative_cache_blocks_retries() {
        let (cache, clock) = fresh_cache();
        let host = host("nowhere.example");
        let resolver = MockResolver::always_fail(ResolverError::Lookup("nxdomain".into()));
        let cfg = ResolverConfig::default();

        // First lookup fails — installs Failed { retry_after = now + 3s }.
        let _ = cache
            .get_or_resolve(&host, &resolver, &cfg)
            .await
            .unwrap_err();
        assert_eq!(resolver.calls(), 1);

        // Second lookup before retry_after — short-circuits, no call.
        clock.advance(Duration::from_secs(1));
        let _ = cache
            .get_or_resolve(&host, &resolver, &cfg)
            .await
            .unwrap_err();
        assert_eq!(
            resolver.calls(),
            1,
            "Failed-state lookups within retry_after MUST NOT re-resolve"
        );

        // Third lookup, also inside the window.
        clock.advance(Duration::from_secs(1));
        let _ = cache
            .get_or_resolve(&host, &resolver, &cfg)
            .await
            .unwrap_err();
        assert_eq!(resolver.calls(), 1);

        // Past retry_after — resolver fires again.
        clock.advance(Duration::from_secs(2)); // total 4s, past 3s window
        let _ = cache
            .get_or_resolve(&host, &resolver, &cfg)
            .await
            .unwrap_err();
        assert_eq!(resolver.calls(), 2, "post-retry_after lookup MUST re-attempt");
    }
}
