//! 011-rate-limiting-qos T018 — `RuleRateLimiter` + `RateLimitScopeManager`.
//!
//! Each capped rule owns one `Arc<RuleRateLimiter>` (data-model § 2.2)
//! containing up to three optional [`TokenBucket`]s plus the static
//! `concurrent_max` ceiling and an [`AtomicU64`] live-count gauge. The
//! no-cap fast path is `Option<Arc<RuleRateLimiter>>`: the `None`
//! branch compiles to a single null check.
//!
//! [`RateLimitScopeManager`] owns the `RuleId → Arc<RuleRateLimiter>`
//! registry. It is the install/update/remove surface used by the rule
//! lifecycle; the hot path (TCP accept, UDP first-packet, copy loop)
//! keeps a long-lived `Arc<RuleRateLimiter>` snapshot from a single
//! [`get`](RateLimitScopeManager::get) at rule activation.
//!
//! Hot-reload (T033) replaces the registered `Arc` with one carrying
//! the new caps, with `tokens` / `last_refill_micros` carried forward
//! per [`TokenBucket::with_carryover`]. In-flight forwarders observe
//! the swap on their next acquire (the old cap stays in effect until
//! the snapshot is refreshed). The graceful-drain semantics of R-008
//! fall out for free: a lower `concurrent_max` rejects only *new*
//! accepts; live connections never get force-closed.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use forward_core::rate_limit::{effective_burst_u32, effective_burst_u64};
use forward_core::{RateLimit, RejectReason, RuleId};

use super::bucket::{Acquire, TokenBucket};

/// Direction tag for [`RuleRateLimiter::acquire_bandwidth`]. Maps to
/// the `direction` Prometheus label on
/// `rate_limit_throttle_seconds_total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandwidthDirection {
    In,
    Out,
}

/// Per-rule data-plane limiter. Constructed once from a
/// [`forward_core::RateLimit`] envelope; immutable thereafter (a
/// hot-reload allocates a fresh limiter through
/// [`RuleRateLimiter::with_carryover`] and swaps the registry's `Arc`).
#[derive(Debug)]
pub struct RuleRateLimiter {
    bandwidth_in: Option<TokenBucket>,
    bandwidth_out: Option<TokenBucket>,
    /// Token bucket shared between TCP accepts and UDP first-packets
    /// — a TCP rule and a UDP rule are distinct rules with distinct
    /// limiters, so one bucket per rule is sufficient.
    new_connections: Option<TokenBucket>,
    /// Static cap on `active_connections`. None = uncapped.
    concurrent_max: Option<u32>,
    /// Live count of accepted-but-not-closed connections. Mirrors the
    /// `RateLimitStats.active_connections` gauge.
    active_connections: AtomicU64,
}

/// Result of a connection-rate / concurrent-cap acquire. Distinct from
/// [`Acquire`] because the connection path has no "throttle and try
/// again later" mode — surplus accepts are RST'd immediately
/// (Q3 / FR-009).
#[derive(Debug)]
pub enum ConnectionAcquire {
    /// The accept is admitted. The guard increments
    /// `active_connections` on construction and decrements it on
    /// `Drop`.
    Granted(ActiveGuard),
    /// The accept must be rejected with the given reason. No state
    /// was mutated.
    Rejected(RejectReason),
}

/// RAII handle that decrements `active_connections` on drop. Owners
/// must hold this for the entire connection lifetime.
#[derive(Debug)]
pub struct ActiveGuard {
    limiter: Arc<RuleRateLimiter>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.limiter
            .active_connections
            .fetch_sub(1, Ordering::AcqRel);
    }
}

/// Result of [`RuleRateLimiter::acquire_bandwidth`]. Mirrors
/// [`Acquire`] with a `Granted` no-cap shortcut for ergonomic
/// caller-side branching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandwidthAcquire {
    /// `n` bytes admitted (no cap, or pool covered the request).
    Granted,
    /// Caller should `tokio::time::sleep(deficit)` then retry. The
    /// cumulative `deficit` per direction feeds
    /// `RuleStats.rate_limit.throttle_micros_in/out`.
    Throttled { deficit: Duration },
}

impl RuleRateLimiter {
    /// Build the runtime limiter from a validated control-plane
    /// envelope. `None` caps yield `None` buckets — the hot path then
    /// short-circuits without touching atomics. The validation
    /// invariant is enforced by `forward_core::rate_limit::validate`
    /// before this call (every cap > 0; bursts inside [rate/100,
    /// rate*60]).
    #[must_use]
    pub fn from_envelope(rl: &RateLimit) -> Self {
        let bandwidth_in = rl.bandwidth_in_bps.map(|rate| {
            let burst = effective_burst_u64(Some(rate), rl.bandwidth_in_burst).unwrap_or(rate);
            TokenBucket::new(rate, burst)
        });
        let bandwidth_out = rl.bandwidth_out_bps.map(|rate| {
            let burst = effective_burst_u64(Some(rate), rl.bandwidth_out_burst).unwrap_or(rate);
            TokenBucket::new(rate, burst)
        });
        let new_connections = rl.new_connections_per_sec.map(|rate| {
            let burst = effective_burst_u32(Some(rate), rl.new_connections_burst).unwrap_or(rate);
            TokenBucket::new(u64::from(rate), u64::from(burst))
        });
        Self {
            bandwidth_in,
            bandwidth_out,
            new_connections,
            concurrent_max: rl.concurrent_connections,
            active_connections: AtomicU64::new(0),
        }
    }

    /// Build a fresh limiter that inherits the live-count gauge and
    /// per-bucket token state from `prior` while taking caps from
    /// `next`. The caller swaps the registry's `Arc` after this
    /// returns (T033). Bucket carryover (`with_carryover`) ensures a
    /// raise doesn't suddenly mint a free burst and a lower doesn't
    /// strand the pool above the new ceiling. The live-count gauge
    /// transfers atomically so a lower `concurrent_max` rejects only
    /// new accepts (R-008 graceful drain).
    #[must_use]
    pub fn with_carryover(prior: &Self, next: &RateLimit) -> Self {
        let bandwidth_in = next.bandwidth_in_bps.map(|rate| {
            let burst = effective_burst_u64(Some(rate), next.bandwidth_in_burst).unwrap_or(rate);
            match &prior.bandwidth_in {
                Some(p) => TokenBucket::with_carryover(rate, burst, p),
                None => TokenBucket::new(rate, burst),
            }
        });
        let bandwidth_out = next.bandwidth_out_bps.map(|rate| {
            let burst = effective_burst_u64(Some(rate), next.bandwidth_out_burst).unwrap_or(rate);
            match &prior.bandwidth_out {
                Some(p) => TokenBucket::with_carryover(rate, burst, p),
                None => TokenBucket::new(rate, burst),
            }
        });
        let new_connections = next.new_connections_per_sec.map(|rate| {
            let burst = effective_burst_u32(Some(rate), next.new_connections_burst).unwrap_or(rate);
            match &prior.new_connections {
                Some(p) => TokenBucket::with_carryover(u64::from(rate), u64::from(burst), p),
                None => TokenBucket::new(u64::from(rate), u64::from(burst)),
            }
        });
        Self {
            bandwidth_in,
            bandwidth_out,
            new_connections,
            concurrent_max: next.concurrent_connections,
            // Carry the live count forward so a lower cap drains
            // gracefully instead of force-closing connections.
            active_connections: AtomicU64::new(prior.active_connections.load(Ordering::Acquire)),
        }
    }

    /// Snapshot the live-count gauge. Used by metrics drainage and by
    /// `concurrent_max` admission below; racy under concurrent
    /// accepts but acceptable per R-007 (soft cap ±1 over-shoot is
    /// closed before any byte flows).
    pub fn active_connections(&self) -> u64 {
        self.active_connections.load(Ordering::Acquire)
    }

    /// Try to admit a new connection.
    ///
    /// Order: connection-rate token first (FR-009), then concurrent
    /// ceiling. Both checks short-circuit on `None`. On admission the
    /// caller receives an [`ActiveGuard`] whose `Drop` decrements the
    /// gauge.
    ///
    /// `is_udp_first_packet` selects the reject-reason variant
    /// (`UdpFlowRate` vs `ConnRate`); the token-bucket math is shared.
    pub fn try_acquire_connection(
        self: &Arc<Self>,
        is_udp_first_packet: bool,
    ) -> ConnectionAcquire {
        // 1. Connection-rate / flow-rate token bucket.
        if let Some(bucket) = &self.new_connections
            && matches!(bucket.acquire(1), Acquire::Throttled { .. })
        {
            return ConnectionAcquire::Rejected(if is_udp_first_packet {
                RejectReason::UdpFlowRate
            } else {
                RejectReason::ConnRate
            });
        }

        // 2. Concurrent ceiling — fetch_add then compare (R-007).
        if let Some(max) = self.concurrent_max {
            let prev = self.active_connections.fetch_add(1, Ordering::AcqRel);
            if prev >= u64::from(max) {
                self.active_connections.fetch_sub(1, Ordering::AcqRel);
                return ConnectionAcquire::Rejected(RejectReason::ConnConcurrent);
            }
            return ConnectionAcquire::Granted(ActiveGuard {
                limiter: Arc::clone(self),
            });
        }

        // No concurrent cap — still bump the gauge for observability.
        self.active_connections.fetch_add(1, Ordering::AcqRel);
        ConnectionAcquire::Granted(ActiveGuard {
            limiter: Arc::clone(self),
        })
    }

    /// Try to admit `n` bytes through a bandwidth bucket. Returns
    /// `Granted` immediately when the requested direction is
    /// uncapped.
    pub fn acquire_bandwidth(&self, direction: BandwidthDirection, n: u64) -> BandwidthAcquire {
        let bucket = match direction {
            BandwidthDirection::In => self.bandwidth_in.as_ref(),
            BandwidthDirection::Out => self.bandwidth_out.as_ref(),
        };
        match bucket {
            Some(b) => match b.acquire(n) {
                Acquire::Granted => BandwidthAcquire::Granted,
                Acquire::Throttled { deficit } => BandwidthAcquire::Throttled { deficit },
            },
            None => BandwidthAcquire::Granted,
        }
    }
}

/// Registry of `RuleId → Arc<RuleRateLimiter>`. Owned by the
/// `forward-client` rate-limit subsystem; one per process. All public
/// methods are sync (lookup is not on the per-byte path; the per-rule
/// forwarder caches its `Arc<RuleRateLimiter>` once at activation).
#[derive(Debug, Default)]
pub struct RateLimitScopeManager {
    rules: RwLock<HashMap<RuleId, Arc<RuleRateLimiter>>>,
}

impl RateLimitScopeManager {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install (or replace) the limiter for `rule_id`. `rl = None`
    /// removes any existing limiter so the rule runs uncapped — the
    /// hot path observes `get` returning `None` after this.
    pub fn install(&self, rule_id: RuleId, rl: Option<&RateLimit>) {
        let mut guard = self.rules.write().expect("rate-limit registry poisoned");
        match rl {
            Some(envelope) => {
                let limiter = Arc::new(RuleRateLimiter::from_envelope(envelope));
                guard.insert(rule_id, limiter);
            }
            None => {
                guard.remove(&rule_id);
            }
        }
    }

    /// Hot-reload swap: build a successor limiter that inherits live
    /// state from the prior one (T033). When `rule_id` has no prior
    /// limiter, falls back to a fresh [`from_envelope`](RuleRateLimiter::from_envelope).
    pub fn update(&self, rule_id: RuleId, rl: Option<&RateLimit>) {
        let mut guard = self.rules.write().expect("rate-limit registry poisoned");
        match (guard.get(&rule_id).cloned(), rl) {
            (Some(prior), Some(envelope)) => {
                let next = Arc::new(RuleRateLimiter::with_carryover(&prior, envelope));
                guard.insert(rule_id, next);
            }
            (None, Some(envelope)) => {
                let next = Arc::new(RuleRateLimiter::from_envelope(envelope));
                guard.insert(rule_id, next);
            }
            (_, None) => {
                guard.remove(&rule_id);
            }
        }
    }

    /// Drop the limiter for `rule_id`. Idempotent.
    pub fn remove(&self, rule_id: RuleId) {
        let mut guard = self.rules.write().expect("rate-limit registry poisoned");
        guard.remove(&rule_id);
    }

    /// Snapshot the limiter for `rule_id`. Hot-path callers do this
    /// once at rule activation and hold the `Arc` for the rule's
    /// lifetime; periodic re-fetch is the hot-reload mechanism (T033).
    #[must_use]
    pub fn get(&self, rule_id: RuleId) -> Option<Arc<RuleRateLimiter>> {
        let guard = self.rules.read().expect("rate-limit registry poisoned");
        guard.get(&rule_id).cloned()
    }

    /// Number of currently-installed limiters. For tests + diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        let guard = self.rules.read().expect("rate-limit registry poisoned");
        guard.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rl_full() -> RateLimit {
        RateLimit {
            bandwidth_in_bps: Some(1_000_000),
            bandwidth_out_bps: Some(1_000_000),
            new_connections_per_sec: Some(10),
            concurrent_connections: Some(2),
            ..Default::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn from_envelope_none_yields_no_buckets() {
        let l = RuleRateLimiter::from_envelope(&RateLimit::default());
        assert!(l.bandwidth_in.is_none());
        assert!(l.bandwidth_out.is_none());
        assert!(l.new_connections.is_none());
        assert_eq!(l.concurrent_max, None);
        assert_eq!(l.active_connections(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn try_acquire_concurrent_rejects_after_cap() {
        let l = Arc::new(RuleRateLimiter::from_envelope(&RateLimit {
            concurrent_connections: Some(2),
            ..Default::default()
        }));
        let g1 = match l.try_acquire_connection(false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!("first accept must be admitted"),
        };
        let g2 = match l.try_acquire_connection(false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!("second accept must be admitted"),
        };
        // Third accept must reject.
        match l.try_acquire_connection(false) {
            ConnectionAcquire::Rejected(RejectReason::ConnConcurrent) => {}
            ConnectionAcquire::Rejected(other) => panic!("wrong reason: {other:?}"),
            ConnectionAcquire::Granted(_) => panic!("over-cap accept must reject"),
        }
        assert_eq!(l.active_connections(), 2);
        // After dropping one guard, a new accept is admitted.
        drop(g1);
        assert_eq!(l.active_connections(), 1);
        let _g3 = match l.try_acquire_connection(false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!("accept after drain must succeed"),
        };
        assert_eq!(l.active_connections(), 2);
        drop(g2);
    }

    #[tokio::test(start_paused = true)]
    async fn try_acquire_connection_rate_rejects_surplus() {
        // Rate 1/sec, burst defaults to 1 — the second back-to-back
        // accept must reject under ConnRate.
        let l = Arc::new(RuleRateLimiter::from_envelope(&RateLimit {
            new_connections_per_sec: Some(1),
            ..Default::default()
        }));
        let _g = match l.try_acquire_connection(false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!("first accept admitted"),
        };
        match l.try_acquire_connection(false) {
            ConnectionAcquire::Rejected(RejectReason::ConnRate) => {}
            other => panic!("expected ConnRate reject, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn try_acquire_udp_first_packet_uses_udp_flow_rate_reason() {
        let l = Arc::new(RuleRateLimiter::from_envelope(&RateLimit {
            new_connections_per_sec: Some(1),
            ..Default::default()
        }));
        let _g = match l.try_acquire_connection(true) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!("first packet admitted"),
        };
        match l.try_acquire_connection(true) {
            ConnectionAcquire::Rejected(RejectReason::UdpFlowRate) => {}
            other => panic!("expected UdpFlowRate reject, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_bandwidth_short_circuits_when_uncapped() {
        let l = RuleRateLimiter::from_envelope(&RateLimit::default());
        assert_eq!(
            l.acquire_bandwidth(BandwidthDirection::In, 1_000_000),
            BandwidthAcquire::Granted
        );
        assert_eq!(
            l.acquire_bandwidth(BandwidthDirection::Out, 1_000_000),
            BandwidthAcquire::Granted
        );
    }

    #[tokio::test(start_paused = true)]
    async fn acquire_bandwidth_throttles_when_drained() {
        let l = RuleRateLimiter::from_envelope(&RateLimit {
            bandwidth_in_bps: Some(1_000),
            ..Default::default()
        });
        assert_eq!(
            l.acquire_bandwidth(BandwidthDirection::In, 1_000),
            BandwidthAcquire::Granted
        );
        match l.acquire_bandwidth(BandwidthDirection::In, 500) {
            BandwidthAcquire::Throttled { deficit } => {
                assert!(deficit >= Duration::from_millis(450));
            }
            BandwidthAcquire::Granted => panic!("should be throttled"),
        }
        // The other direction is still uncapped and grants freely.
        assert_eq!(
            l.acquire_bandwidth(BandwidthDirection::Out, 10_000_000),
            BandwidthAcquire::Granted
        );
    }

    #[tokio::test(start_paused = true)]
    async fn carryover_preserves_live_count_gauge() {
        let prior = Arc::new(RuleRateLimiter::from_envelope(&RateLimit {
            concurrent_connections: Some(10),
            ..Default::default()
        }));
        let _g1 = match prior.try_acquire_connection(false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!(),
        };
        let _g2 = match prior.try_acquire_connection(false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!(),
        };
        assert_eq!(prior.active_connections(), 2);

        // Lower the cap to 1: live count carries forward, so the new
        // limiter should report 2 active. New accepts must reject
        // under the lower cap.
        let next = RuleRateLimiter::with_carryover(
            &prior,
            &RateLimit {
                concurrent_connections: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(next.active_connections(), 2);
        let next = Arc::new(next);
        match next.try_acquire_connection(false) {
            ConnectionAcquire::Rejected(RejectReason::ConnConcurrent) => {}
            other => panic!("expected ConnConcurrent reject under lowered cap, got {other:?}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn scope_manager_install_get_remove_round_trip() {
        let mgr = RateLimitScopeManager::new();
        let r1 = RuleId(1);
        let r2 = RuleId(2);
        assert!(mgr.is_empty());

        mgr.install(r1, Some(&rl_full()));
        assert!(mgr.get(r1).is_some());
        assert!(mgr.get(r2).is_none());
        assert_eq!(mgr.len(), 1);

        mgr.install(r2, None); // None is a no-op when nothing is installed
        assert_eq!(mgr.len(), 1);

        mgr.remove(r1);
        assert!(mgr.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn scope_manager_install_with_none_after_some_clears() {
        let mgr = RateLimitScopeManager::new();
        let r = RuleId(7);
        mgr.install(r, Some(&rl_full()));
        assert!(mgr.get(r).is_some());
        // Calling install with None drops the limiter — the rule now
        // runs uncapped.
        mgr.install(r, None);
        assert!(mgr.get(r).is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn scope_manager_update_carries_state() {
        let mgr = RateLimitScopeManager::new();
        let r = RuleId(42);
        mgr.install(r, Some(&rl_full()));
        let prior = mgr.get(r).unwrap();
        let _g = match prior.try_acquire_connection(false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!(),
        };
        assert_eq!(prior.active_connections(), 1);

        // Update with a new envelope. The new limiter must see the
        // live count of 1 thanks to carryover.
        mgr.update(
            r,
            Some(&RateLimit {
                concurrent_connections: Some(5),
                ..Default::default()
            }),
        );
        let next = mgr.get(r).unwrap();
        assert!(!Arc::ptr_eq(&prior, &next), "update must swap the Arc");
        assert_eq!(next.active_connections(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn scope_manager_update_on_empty_inserts_fresh_limiter() {
        let mgr = RateLimitScopeManager::new();
        let r = RuleId(3);
        // No prior install — update should still work and behave like
        // install.
        mgr.update(
            r,
            Some(&RateLimit {
                concurrent_connections: Some(1),
                ..Default::default()
            }),
        );
        assert!(mgr.get(r).is_some());
    }
}
