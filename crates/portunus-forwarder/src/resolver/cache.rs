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
//! Wakeup discipline (missed-wakeup safety): a waiter that finds a
//! `Pending` builds its `Notified` future and calls `.enable()` on it
//! **while still holding the cache lock**, then releases the lock and
//! awaits. `enable()` registers the waiter in the `Notify` wait-list
//! before the lock is dropped, so a `notify_waiters()` fired by the
//! completing task cannot slip through the gap between unlock and the
//! first poll of `notified().await` (which would otherwise strand the
//! waiter forever on a multi-threaded runtime).
//!
//! Liveness of `Pending`: the task that installs a `Pending` owns the
//! resolve. If that task's future is dropped/aborted mid-resolve the
//! placeholder would linger forever, so a `Pending` older than
//! `attempt_timeout + PENDING_ABANDON_MARGIN` is treated as abandoned:
//! both `get_or_resolve` and `evict_locked` may reap it, firing
//! `notify_waiters()` on the stale `Notify` first so any stuck waiters
//! re-loop and re-evaluate.
//!
//! Concurrency gate: the number of *in-flight* resolver calls is
//! bounded by a `Semaphore` sized from
//! `ResolverConfig::max_concurrent_resolves`. Only the task performing
//! the actual `resolver.resolve()` holds a permit; single-flight
//! waiters do not. When the gate is saturated a new lookup fails fast
//! with `ResolverError::Overloaded` rather than queueing.
//!
//! Time abstraction: a `Clock` trait (see `super::clock`) is injected
//! so unit tests can advance time deterministically without
//! `tokio::time::sleep`.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use portunus_core::Hostname;
use tokio::sync::{Mutex, Notify, OwnedSemaphorePermit, Semaphore};

use super::clock::{Clock, SystemClock};
use super::{Resolve, ResolverError};

/// Extra grace added to `ResolverConfig::attempt_timeout` before an
/// in-flight `Pending` entry is presumed abandoned (its owning resolve
/// task was dropped/aborted). Comfortably larger than any scheduling
/// jitter between the resolver returning and `apply_refresh` re-taking
/// the lock, so a healthy resolve is never mistaken for abandoned.
const PENDING_ABANDON_MARGIN: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub(super) enum CacheEntry {
    /// A resolver call is in-flight. Concurrent waiters block on
    /// `notify`; the in-flight task overwrites this entry with
    /// `Resolved` / `Failed` / `StaleAfterFailedRefresh` and calls
    /// `notify_waiters()`.
    Pending {
        notify: Arc<Notify>,
        /// When the owning resolve was installed. Used to reap a
        /// `Pending` whose owner was dropped/aborted mid-resolve (a
        /// `Pending` older than `attempt_timeout + PENDING_ABANDON_MARGIN`
        /// is presumed abandoned).
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
    /// Bounds concurrent in-flight `resolver.resolve()` calls. Sized
    /// from `ResolverConfig::max_concurrent_resolves`. Held only by the
    /// task performing an actual resolve — never by single-flight
    /// waiters.
    resolve_slots: Arc<Semaphore>,
}

impl Cache {
    pub(super) fn new(max_concurrent_resolves: usize) -> Self {
        Self::with_clock(Arc::new(SystemClock), max_concurrent_resolves)
    }

    pub(super) fn with_clock(clock: Arc<dyn Clock>, max_concurrent_resolves: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            clock,
            resolve_slots: Arc::new(Semaphore::new(max_concurrent_resolves)),
        }
    }

    /// Try to reserve a resolver slot without blocking. `Some(permit)`
    /// means this caller may perform a real `resolver.resolve()`;
    /// `None` means the `max_concurrent_resolves` gate is saturated and
    /// the caller must fail fast (never queue).
    fn try_reserve_slot(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.resolve_slots).try_acquire_owned().ok()
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
        let abandon_after = config.attempt_timeout + PENDING_ABANDON_MARGIN;
        // The state machine may need multiple iterations: a Pending
        // wait can resolve into a Failed entry whose retry_after has
        // already passed, etc. Cap to a few rounds to bail on
        // pathological races rather than spin forever.
        for _ in 0..4 {
            // Under the lock we either (a) return a cache hit / error
            // directly, (b) register a wait on an in-flight Pending and
            // `continue`, or (c) install our own Pending (holding a
            // resolver permit) and fall through to perform the resolve.
            // Case (c) yields `(stale_addrs, permit)`; the resolver call
            // and `apply_refresh` then run with the lock released.
            let (stale_addrs, permit): (Option<Vec<IpAddr>>, OwnedSemaphorePermit) = {
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
                        // Expired — refresh, installing a Pending so
                        // concurrent expiry refreshes coalesce.
                        let stale = addrs.clone();
                        let Some(permit) = self.try_reserve_slot() else {
                            return Err(ResolverError::Overloaded(config.max_concurrent_resolves));
                        };
                        install_pending(&mut guard, name, now);
                        (Some(stale), permit)
                    }
                    Some(CacheEntry::Pending { notify, started_at }) => {
                        if now.saturating_duration_since(*started_at) > abandon_after {
                            // The owner of this Pending never completed
                            // (its future was dropped/aborted). Take
                            // over: wake any stragglers stuck on the old
                            // notify so they re-loop, then install a
                            // fresh Pending of our own.
                            let old_notify = Arc::clone(notify);
                            let Some(permit) = self.try_reserve_slot() else {
                                return Err(ResolverError::Overloaded(
                                    config.max_concurrent_resolves,
                                ));
                            };
                            old_notify.notify_waiters();
                            install_pending(&mut guard, name, now);
                            (None, permit)
                        } else {
                            // Live in-flight resolve — wait on it. Build
                            // the Notified and `enable()` it while STILL
                            // holding `guard`, registering this waiter in
                            // the wait-list before the lock is released.
                            // The completer fires `notify_waiters()` under
                            // the same lock, so it cannot slip in between
                            // our unlock and registration (missed wakeup).
                            // `notified` borrows the owned `notify` Arc, not
                            // `guard`, so it outlives `drop(guard)`.
                            let notify = Arc::clone(notify);
                            let notified = notify.notified();
                            tokio::pin!(notified);
                            notified.as_mut().enable();
                            drop(guard);
                            notified.await;
                            // Re-read the cache on the next iteration.
                            continue;
                        }
                    }
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
                        // Grace expired — drop to a refresh attempt.
                        let Some(permit) = self.try_reserve_slot() else {
                            return Err(ResolverError::Overloaded(config.max_concurrent_resolves));
                        };
                        install_pending(&mut guard, name, now);
                        (None, permit)
                    }
                    Some(CacheEntry::Failed {
                        retry_after,
                        last_reason,
                    }) => {
                        if now < *retry_after {
                            return Err(last_reason.clone());
                        }
                        // Retry window expired — refresh.
                        let Some(permit) = self.try_reserve_slot() else {
                            return Err(ResolverError::Overloaded(config.max_concurrent_resolves));
                        };
                        install_pending(&mut guard, name, now);
                        (None, permit)
                    }
                    None => {
                        // Brand-new key grows the map — enforce the
                        // entry cap before inserting so the cache stays
                        // bounded under high-name-cardinality workloads.
                        let Some(permit) = self.try_reserve_slot() else {
                            return Err(ResolverError::Overloaded(config.max_concurrent_resolves));
                        };
                        evict_locked(&mut guard, now, config.max_cache_entries, abandon_after);
                        install_pending(&mut guard, name, now);
                        (None, permit)
                    }
                }
            };

            // We own an installed Pending and a resolver permit. Perform
            // the resolve with the lock released; the permit is dropped
            // once `apply_refresh` has committed the result and woken
            // the waiters.
            let outcome = resolver.resolve(name).await;
            let result = self.apply_refresh(name, outcome, stale_addrs, config).await;
            drop(permit);
            return result;
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
                self.commit_failure(name, ResolverError::EmptyAnswer, stale_addrs, config, now)
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

/// Insert a fresh `Pending { notify, started_at }` placeholder for
/// `name` under the caller-held lock, overwriting whatever entry was
/// there. The caller takes ownership of the single-flight refresh.
fn install_pending(map: &mut HashMap<Hostname, CacheEntry>, name: &Hostname, now: Instant) {
    map.insert(
        name.clone(),
        CacheEntry::Pending {
            notify: Arc::new(Notify::new()),
            started_at: now,
        },
    );
}

fn clamp_ttl(ttl: Duration, config: &super::ResolverConfig) -> Duration {
    ttl.clamp(config.cache_floor, config.cache_ceiling)
}

/// The instant past which an entry carries no useful value and may be
/// freely evicted. `Pending` has no deadline — it is mid-flight and
/// owns a `Notify` that single-flight waiters are blocked on, so a
/// *live* `Pending` is never a forced-eviction candidate (abandoned
/// ones are reaped separately in `evict_locked`'s first pass).
fn entry_deadline(entry: &CacheEntry) -> Option<Instant> {
    match entry {
        CacheEntry::Pending { .. } => None,
        CacheEntry::Resolved { expiry, .. } => Some(*expiry),
        CacheEntry::StaleAfterFailedRefresh {
            fail_grace_until, ..
        } => Some(*fail_grace_until),
        CacheEntry::Failed { retry_after, .. } => Some(*retry_after),
    }
}

/// Keep the cache map bounded (memory-leak guard). Called under the
/// lock immediately before a brand-new key is inserted. Two passes:
///
/// 1. Drop every entry whose useful lifetime has already elapsed — a
///    later lookup for the same name simply re-resolves. This pass also
///    reaps *abandoned* `Pending` entries (older than `abandon_after`,
///    i.e. whose owning resolve task was dropped), firing
///    `notify_waiters()` first so any stuck waiters re-loop.
/// 2. If still at/over `max_entries` (all remaining live), evict the
///    entries closest to their deadline (least remaining value) until
///    under the cap.
///
/// *Live* `Pending` entries are never evicted: removing one would
/// strand the single-flight waiters blocked on its `Notify`, leaking
/// those tasks.
fn evict_locked(
    map: &mut HashMap<Hostname, CacheEntry>,
    now: Instant,
    max_entries: usize,
    abandon_after: Duration,
) {
    if map.len() < max_entries {
        return;
    }
    map.retain(|_, entry| {
        if let CacheEntry::Pending { notify, started_at } = entry {
            // Reap an abandoned Pending (owner dropped mid-resolve);
            // keep a live one regardless of the cap.
            if now.saturating_duration_since(*started_at) > abandon_after {
                notify.notify_waiters();
                return false;
            }
            return true;
        }
        match entry_deadline(entry) {
            None => true,
            Some(deadline) => deadline > now,
        }
    });
    while map.len() >= max_entries {
        let victim = map
            .iter()
            .filter_map(|(k, e)| entry_deadline(e).map(|d| (k.clone(), d)))
            .min_by_key(|(_, d)| *d)
            .map(|(k, _)| k);
        match victim {
            Some(k) => {
                map.remove(&k);
            }
            // Only `Pending` entries remain — nothing safe to evict.
            None => break,
        }
    }
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
        let max = ResolverConfig::default().max_concurrent_resolves;
        (Cache::with_clock(clock.clone(), max), clock)
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
        // SystemClock — we use real time delays here.
        let cache = Cache::new(ResolverConfig::default().max_concurrent_resolves);
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

    /// Memory-leak guard: a workload that resolves an ever-growing set
    /// of distinct names MUST NOT grow the cache without bound. With a
    /// small `max_cache_entries`, the map size stays at/under the cap
    /// no matter how many unique names are looked up.
    #[tokio::test]
    async fn cache_is_bounded_by_max_entries() {
        let (cache, _clock) = fresh_cache();
        // Long TTL so nothing expires on its own — eviction is the only
        // thing that can keep the map bounded.
        let resolver = MockResolver::ok(vec![ip("10.0.0.5")], Duration::from_secs(300));
        let cfg = ResolverConfig {
            max_cache_entries: 4,
            ..ResolverConfig::default()
        };

        for i in 0..50u32 {
            let h = host(&format!("name{i}.example"));
            let r = cache.get_or_resolve(&h, &resolver, &cfg).await.unwrap();
            assert_eq!(r.addrs, vec![ip("10.0.0.5")]);
            let len = cache.inner.lock().await.len();
            assert!(
                len <= 4,
                "cache grew to {len} entries, exceeding the cap of 4"
            );
        }
    }

    /// Eviction MUST NOT remove a *live* `Pending` entry: doing so
    /// would strand the single-flight waiters blocked on its `Notify`.
    /// Here we pre-seed the map with `max_entries` live entries plus one
    /// in-flight `Pending`, then evict; the `Pending` must survive.
    #[tokio::test]
    async fn eviction_never_drops_pending_entries() {
        let (cache, clock) = fresh_cache();
        let now = clock.now();
        let abandon_after =
            ResolverConfig::default().attempt_timeout + super::PENDING_ABANDON_MARGIN;
        let mut guard = cache.inner.lock().await;
        // Two live (Resolved) entries.
        guard.insert(
            host("live-a.example"),
            CacheEntry::Resolved {
                addrs: vec![ip("10.0.0.1")],
                expiry: now + Duration::from_secs(300),
            },
        );
        guard.insert(
            host("live-b.example"),
            CacheEntry::Resolved {
                addrs: vec![ip("10.0.0.2")],
                expiry: now + Duration::from_secs(300),
            },
        );
        // One in-flight resolution.
        guard.insert(
            host("pending.example"),
            CacheEntry::Pending {
                notify: Arc::new(Notify::new()),
                started_at: now,
            },
        );
        // Force eviction down to a cap of 1.
        super::evict_locked(&mut guard, now, 1, abandon_after);
        assert!(
            guard.contains_key(&host("pending.example")),
            "live Pending entry must never be evicted"
        );
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
        assert_eq!(
            resolver.calls(),
            2,
            "post-retry_after lookup MUST re-attempt"
        );
    }

    /// Resolver fixture that blocks every `resolve()` call on a shared
    /// `Semaphore` gate (a controllable barrier) and counts calls. The
    /// test opens the gate (`add_permits`) to let a blocked call finish,
    /// which lets us hold a resolve in-flight deterministically without
    /// sleeping.
    struct GateResolver {
        calls: std::sync::atomic::AtomicUsize,
        gate: Arc<Semaphore>,
        addrs: Vec<IpAddr>,
        ttl: Duration,
    }

    impl GateResolver {
        fn new(gate: Arc<Semaphore>, addrs: Vec<IpAddr>, ttl: Duration) -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
                gate,
                addrs,
                ttl,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    #[async_trait::async_trait]
    impl Resolve for GateResolver {
        async fn resolve(
            &self,
            _name: &Hostname,
        ) -> Result<super::super::ResolveAnswer, ResolverError> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Block until the test opens the gate. `acquire` stores a
            // permit, so a release that races ahead of us is never lost.
            let _permit = self.gate.acquire().await.unwrap();
            Ok(super::super::ResolveAnswer {
                addrs: self.addrs.clone(),
                ttl: self.ttl,
            })
        }
    }

    /// Spin (cooperatively, no sleeps) until `name` is `Pending` in the
    /// cache, or panic after a generous bound so a regression fails fast
    /// instead of hanging CI.
    async fn wait_until_pending(cache: &Cache, name: &Hostname) {
        for _ in 0..100_000 {
            {
                let guard = cache.inner.lock().await;
                if matches!(guard.get(name), Some(CacheEntry::Pending { .. })) {
                    return;
                }
            }
            tokio::task::yield_now().await;
        }
        panic!("expected a Pending entry for {name} but none appeared");
    }

    /// Defect 2 (issue #45): a resolve future that is dropped/aborted
    /// mid-flight leaves a `Pending` placeholder behind. That orphan
    /// MUST NOT permanently poison the hostname — once it ages past
    /// `attempt_timeout + PENDING_ABANDON_MARGIN`, a fresh lookup reaps
    /// it, takes over the refresh, and succeeds. Deterministic: the
    /// abort is explicit and time is driven by `MockClock`.
    #[tokio::test]
    async fn abandoned_pending_is_reaped_and_does_not_poison_hostname() {
        let (cache, clock) = fresh_cache();
        let host = host("orphan.example");
        let gate = Arc::new(Semaphore::new(0));
        let resolver = Arc::new(GateResolver::new(
            Arc::clone(&gate),
            vec![ip("10.0.0.9")],
            Duration::from_secs(60),
        ));
        let cfg = ResolverConfig::default();

        // Task 1 installs a Pending (holding a resolver permit) and then
        // blocks in `resolve()` on the closed gate.
        let hung = {
            let cache = cache.clone();
            let host = host.clone();
            let resolver = Arc::clone(&resolver);
            tokio::spawn(async move { cache.get_or_resolve(&host, resolver.as_ref(), &cfg).await })
        };
        wait_until_pending(&cache, &host).await;

        // Abort task 1 mid-resolve: its future is dropped, orphaning the
        // Pending and releasing the resolver permit. Awaiting the handle
        // guarantees the drop has completed.
        hung.abort();
        assert!(hung.await.is_err(), "aborted task should not complete");

        // The orphan is still there but now stale.
        clock.advance(cfg.attempt_timeout + PENDING_ABANDON_MARGIN + Duration::from_secs(1));
        // Open the gate so the takeover resolve can complete.
        gate.add_permits(1);

        let result = cache
            .get_or_resolve(&host, resolver.as_ref(), &cfg)
            .await
            .expect("stale Pending must be reaped and re-resolved, not poisoned");
        assert_eq!(result.addrs, vec![ip("10.0.0.9")]);
        assert_eq!(result.source, AnswerSource::Fresh);
        assert_eq!(
            resolver.calls(),
            2,
            "first (aborted) call + the takeover call = 2 resolver invocations"
        );
    }

    /// Defect 3 (issue #45): `max_concurrent_resolves` bounds in-flight
    /// resolver calls. With a cap of 1 and one resolve held in-flight, a
    /// second lookup for a *different* hostname (which would need its own
    /// resolve) fails fast with `ResolverError::Overloaded` rather than
    /// queueing. Single-flight waiters are unaffected (they never take a
    /// permit) — that invariant stays covered by
    /// `single_flight_coalesces_concurrent_misses`.
    #[tokio::test]
    async fn max_concurrent_resolves_sheds_load_when_saturated() {
        let clock = Arc::new(MockClock::new());
        let cache = Cache::with_clock(clock, 1);
        let gate = Arc::new(Semaphore::new(0));
        let resolver = Arc::new(GateResolver::new(
            Arc::clone(&gate),
            vec![ip("10.0.0.1")],
            Duration::from_secs(60),
        ));
        let cfg = ResolverConfig {
            max_concurrent_resolves: 1,
            ..ResolverConfig::default()
        };

        // Occupy the single permit with an in-flight resolve for host A.
        let host_a = host("a.example");
        let busy = {
            let cache = cache.clone();
            let host_a = host_a.clone();
            let resolver = Arc::clone(&resolver);
            tokio::spawn(
                async move { cache.get_or_resolve(&host_a, resolver.as_ref(), &cfg).await },
            )
        };
        wait_until_pending(&cache, &host_a).await;

        // Host B needs its own resolve but the gate is saturated → shed.
        let host_b = host("b.example");
        let err = cache
            .get_or_resolve(&host_b, resolver.as_ref(), &cfg)
            .await
            .expect_err("saturated resolve gate must shed the new lookup");
        assert!(
            matches!(err, ResolverError::Overloaded(1)),
            "expected Overloaded(1), got {err:?}"
        );
        // B was shed before touching the resolver: only A called it.
        assert_eq!(
            resolver.calls(),
            1,
            "shed lookup must not call the resolver"
        );
        // No lingering entry for B (fail-fast leaves the cache clean).
        assert!(
            !cache.inner.lock().await.contains_key(&host_b),
            "a shed lookup must not install a Pending"
        );

        // Release A so the task can finish cleanly.
        gate.add_permits(1);
        let a = busy.await.unwrap().expect("host A resolve should succeed");
        assert_eq!(a.addrs, vec![ip("10.0.0.1")]);
    }

    /// Defect 2 (evict path): `evict_locked` reaps an *abandoned*
    /// Pending (older than the abandon threshold) while still refusing
    /// to drop a *live* one, and wakes any waiters on the reaped notify.
    #[tokio::test]
    async fn evict_reaps_abandoned_pending_but_keeps_live_one() {
        let (cache, clock) = fresh_cache();
        let now = clock.now();
        let abandon_after =
            ResolverConfig::default().attempt_timeout + super::PENDING_ABANDON_MARGIN;
        let mut guard = cache.inner.lock().await;

        // A live Pending (started just now) and an abandoned one (started
        // well before the abandon threshold).
        guard.insert(
            host("live.example"),
            CacheEntry::Pending {
                notify: Arc::new(Notify::new()),
                started_at: now,
            },
        );
        let stale_notify = Arc::new(Notify::new());
        guard.insert(
            host("stale.example"),
            CacheEntry::Pending {
                notify: Arc::clone(&stale_notify),
                started_at: now
                    .checked_sub(abandon_after + Duration::from_secs(1))
                    .unwrap(),
            },
        );
        // Register a waiter on the stale notify and enable it, so we can
        // assert the reap wakes it.
        let woken = stale_notify.notified();
        tokio::pin!(woken);
        woken.as_mut().enable();

        // Force eviction with a tiny cap. The abandoned Pending is reaped
        // (its waiter woken); the live one survives.
        super::evict_locked(&mut guard, now, 1, abandon_after);
        assert!(
            guard.contains_key(&host("live.example")),
            "live Pending must survive eviction"
        );
        assert!(
            !guard.contains_key(&host("stale.example")),
            "abandoned Pending must be reaped"
        );
        drop(guard);

        // The reap fired notify_waiters on the stale notify.
        woken.await;
    }
}
