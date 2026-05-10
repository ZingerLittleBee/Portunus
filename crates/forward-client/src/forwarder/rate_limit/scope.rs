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

/// Cap scope tag — selects whether the limiter emits per-rule
/// reject reasons (`ConnConcurrent` / `ConnRate` / `UdpFlowRate`)
/// or per-owner reject reasons (`OwnerConcurrent` / `OwnerConnRate`
/// / `OwnerUdpFlowRate`). Per FR-014 owner rejects must carry
/// distinct reasons so operators can attribute a quota hit to the
/// right policy layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapScope {
    Rule,
    Owner,
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
    ///
    /// `Arc<AtomicU64>` (not bare `AtomicU64`) so [`with_carryover`]
    /// shares the gauge with the prior frame (R-008 / Q4): guards
    /// admitted under the prior limiter still decrement the same
    /// counter when they close, so graceful drain converges on the
    /// successor limiter without force-closing in-flight connections.
    active_connections: Arc<AtomicU64>,
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
            active_connections: Arc::new(AtomicU64::new(0)),
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
            // Share the live-count gauge with the prior frame so
            // existing `ActiveGuard`s (whose `Drop` reads the prior's
            // Arc) still decrement the counter the new limiter
            // observes. Without this, lowering the cap below the
            // live count would never let the gauge drain back down,
            // and new accepts would stay rejected forever.
            active_connections: Arc::clone(&prior.active_connections),
        }
    }

    /// Snapshot the live-count gauge. Used by metrics drainage and by
    /// `concurrent_max` admission below; racy under concurrent
    /// accepts but acceptable per R-007 (soft cap ±1 over-shoot is
    /// closed before any byte flows).
    pub fn active_connections(&self) -> u64 {
        self.active_connections.load(Ordering::Acquire)
    }

    /// Try to admit a new connection at the per-rule cap layer.
    /// Convenience wrapper around [`Self::try_acquire`]
    /// with `scope = CapScope::Rule` — preserved as the canonical
    /// per-rule entry point so existing call sites (T019 TCP accept,
    /// T021 UDP first-packet) keep their familiar shape.
    pub fn try_acquire_connection(
        self: &Arc<Self>,
        is_udp_first_packet: bool,
    ) -> ConnectionAcquire {
        self.try_acquire(CapScope::Rule, is_udp_first_packet)
    }

    /// Try to admit a new connection.
    ///
    /// Order: connection-rate / flow-rate token first (FR-009), then
    /// concurrent ceiling. Both checks short-circuit on `None`. On
    /// admission the caller receives an [`ActiveGuard`] whose `Drop`
    /// decrements the gauge.
    ///
    /// `scope` selects whether reject reasons are rule-scoped or
    /// owner-scoped (FR-014). `is_udp_first_packet` further chooses
    /// between `*ConnRate` (TCP) and `*UdpFlowRate` (UDP) within
    /// either scope.
    pub fn try_acquire(
        self: &Arc<Self>,
        scope: CapScope,
        is_udp_first_packet: bool,
    ) -> ConnectionAcquire {
        // 1. Connection-rate / flow-rate token bucket.
        if let Some(bucket) = &self.new_connections
            && matches!(bucket.acquire(1), Acquire::Throttled { .. })
        {
            return ConnectionAcquire::Rejected(rate_reject_reason(scope, is_udp_first_packet));
        }

        // 2. Concurrent ceiling — fetch_add then compare (R-007).
        if let Some(max) = self.concurrent_max {
            let prev = self.active_connections.fetch_add(1, Ordering::AcqRel);
            if prev >= u64::from(max) {
                self.active_connections.fetch_sub(1, Ordering::AcqRel);
                return ConnectionAcquire::Rejected(concurrent_reject_reason(scope));
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

    /// True when no limiter dimensions exist — bandwidth, connection
    /// rate, and concurrent ceiling are all `None`. Hot-path callers
    /// can use this to short-circuit out without performing a full
    /// `try_acquire_connection` round-trip on rules where the
    /// envelope was structurally non-null but every cap was unset.
    #[must_use]
    pub fn is_no_op(&self) -> bool {
        self.bandwidth_in.is_none()
            && self.bandwidth_out.is_none()
            && self.new_connections.is_none()
            && self.concurrent_max.is_none()
    }

    /// True when at least one bandwidth bucket is configured. The
    /// proxy hot path uses this to decide between
    /// `tokio::io::copy_bidirectional` (byte-stable, no extra atomics
    /// per chunk) and the throttling manual bidi loop (T020).
    #[must_use]
    pub fn has_bandwidth_cap(&self) -> bool {
        self.bandwidth_in.is_some() || self.bandwidth_out.is_some()
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

/// Outcome of [`try_acquire_layered`] — the FR-013 cascade where
/// a connection is admitted only when BOTH the per-owner ceiling
/// and the per-rule cap allow it. The per-owner gate runs first
/// (FR-013 ordering); a per-rule reject after a successful owner
/// admit transparently releases the owner slot via the dropped
/// guard.
#[derive(Debug)]
pub enum LayeredAcquire {
    /// Both gates admitted. Either guard is `None` when its layer
    /// is uncapped (no limiter installed); callers move both into
    /// the connection task so the gauges decrement on close.
    Granted {
        owner_guard: Option<ActiveGuard>,
        rule_guard: Option<ActiveGuard>,
    },
    /// The per-owner gate refused. Reason is one of the `OWNER_*`
    /// variants. The per-rule gate is NOT consulted, mirroring
    /// FR-013 ("per-owner ceiling binds before per-rule cap").
    OwnerRejected(RejectReason),
    /// The per-rule gate refused after the owner gate admitted
    /// (or the owner gate was absent). The owner slot, if any,
    /// has been released — the local `ActiveGuard` fell out of
    /// scope on this branch.
    RuleRejected(RejectReason),
}

/// FR-013 / FR-014 layered admission gate. Run on every TCP accept
/// and every UDP first-packet of a NEW flow. Order is fixed: owner
/// first, then rule. Both layers are independently optional so
/// uncapped rules / owners short-circuit through (and the v0.10
/// byte-stable hot path is preserved when both are `None`).
pub fn try_acquire_layered(
    owner: Option<&Arc<OwnerRateLimitHandle>>,
    rule: Option<&Arc<RuleRateLimiter>>,
    is_udp_first_packet: bool,
) -> LayeredAcquire {
    let owner_guard = if let Some(o) = owner {
        match o.try_acquire(is_udp_first_packet) {
            Some(ConnectionAcquire::Granted(g)) => Some(g),
            Some(ConnectionAcquire::Rejected(reason)) => {
                return LayeredAcquire::OwnerRejected(reason);
            }
            None => None,
        }
    } else {
        None
    };
    let rule_guard = if let Some(r) = rule {
        match r.try_acquire(CapScope::Rule, is_udp_first_packet) {
            ConnectionAcquire::Granted(g) => Some(g),
            ConnectionAcquire::Rejected(reason) => {
                // The local `owner_guard` (if any) drops here —
                // releasing the owner-scope slot we just claimed.
                // Without this branch, a rule-side reject would
                // strand the owner gauge above the live count.
                return LayeredAcquire::RuleRejected(reason);
            }
        }
    } else {
        None
    };
    LayeredAcquire::Granted {
        owner_guard,
        rule_guard,
    }
}

/// Centralised reject-reason mapping for the connection-rate /
/// flow-rate token bucket exhaustion path.
fn rate_reject_reason(scope: CapScope, is_udp_first_packet: bool) -> RejectReason {
    match (scope, is_udp_first_packet) {
        (CapScope::Rule, false) => RejectReason::ConnRate,
        (CapScope::Rule, true) => RejectReason::UdpFlowRate,
        (CapScope::Owner, false) => RejectReason::OwnerConnRate,
        (CapScope::Owner, true) => RejectReason::OwnerUdpFlowRate,
    }
}

/// Centralised reject-reason mapping for the concurrent-ceiling
/// exhaustion path.
fn concurrent_reject_reason(scope: CapScope) -> RejectReason {
    match scope {
        CapScope::Rule => RejectReason::ConnConcurrent,
        CapScope::Owner => RejectReason::OwnerConcurrent,
    }
}

/// Owner identifier — the v0.5 RBAC owner string keyed on the wire
/// as `OwnerRateLimitUpdate.owner_id`. Newtyped so the scope manager
/// can't be confused with a `RuleId`-keyed registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OwnerId(pub String);

impl OwnerId {
    #[must_use]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl std::fmt::Display for OwnerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Dynamic view over the current per-owner limiter for a rule's owner.
///
/// Rules outlive owner-cap mutations, so the data plane cannot hold a
/// one-time `Arc<OwnerRateLimiter>` snapshot and expect later
/// `OwnerRateLimitUpdate{SET|REMOVE}` pushes to take effect. Instead the
/// rule keeps this lightweight handle and snapshots the current limiter
/// from the process-lifetime registry at each admission / bandwidth
/// acquire.
#[derive(Debug)]
pub struct OwnerRateLimitHandle {
    owner_id: OwnerId,
    scope: Arc<OwnerRateLimitScopeManager>,
}

impl OwnerRateLimitHandle {
    #[must_use]
    pub fn new(owner_id: OwnerId, scope: Arc<OwnerRateLimitScopeManager>) -> Self {
        Self { owner_id, scope }
    }

    #[must_use]
    pub fn snapshot(&self) -> Option<Arc<OwnerRateLimiter>> {
        self.scope.get(&self.owner_id)
    }

    #[must_use]
    pub fn has_bandwidth_cap(&self) -> bool {
        self.snapshot()
            .is_some_and(|limiter| limiter.has_bandwidth_cap())
    }

    #[must_use]
    pub fn try_acquire(&self, is_udp_first_packet: bool) -> Option<ConnectionAcquire> {
        self.snapshot()
            .map(|limiter| limiter.try_acquire(CapScope::Owner, is_udp_first_packet))
    }

    #[must_use]
    pub fn acquire_bandwidth(
        &self,
        direction: BandwidthDirection,
        bytes: u64,
    ) -> Option<BandwidthAcquire> {
        self.snapshot()
            .map(|limiter| limiter.acquire_bandwidth(direction, bytes))
    }
}

/// Per-owner data-plane limiter — same shape as [`RuleRateLimiter`]
/// (data-model § 2.3). Sharing the underlying type keeps the
/// hot-path call sites symmetric: the per-owner gate (run BEFORE the
/// per-rule gate per FR-013) reuses [`RuleRateLimiter::try_acquire`]
/// with `scope = CapScope::Owner`.
pub type OwnerRateLimiter = RuleRateLimiter;

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
    #[allow(dead_code)] // tests + future operator-debug surface
    pub fn len(&self) -> usize {
        let guard = self.rules.read().expect("rate-limit registry poisoned");
        guard.len()
    }

    #[must_use]
    #[allow(dead_code)] // tests + future operator-debug surface
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Registry of `OwnerId → Arc<OwnerRateLimiter>` (data-model § 2.3).
/// Mirrors [`RateLimitScopeManager`] but keyed by RBAC owner string
/// — the tenant-isolation scope per FR-002. The forward-client
/// allocates one per process; the `OwnerRateLimitUpdate` server-push
/// (T031) drives `install`/`update`/`remove`; per-rule forwarders
/// look up their owner's limiter once at rule activation and cache
/// the `Arc` for the rule lifetime.
#[derive(Debug, Default)]
pub struct OwnerRateLimitScopeManager {
    owners: RwLock<HashMap<OwnerId, Arc<OwnerRateLimiter>>>,
}

impl OwnerRateLimitScopeManager {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install (or replace) the limiter for `owner_id`. `rl = None`
    /// removes any existing limiter so the owner runs uncapped — the
    /// hot path observes `get` returning `None` after this. Mirrors
    /// the `OwnerRateLimitUpdate { action = REMOVE }` server-push
    /// shape.
    pub fn install(&self, owner_id: &OwnerId, rl: Option<&RateLimit>) {
        let mut guard = self.owners.write().expect("owner registry poisoned");
        match rl {
            Some(envelope) => {
                let limiter = Arc::new(OwnerRateLimiter::from_envelope(envelope));
                guard.insert(owner_id.clone(), limiter);
            }
            None => {
                guard.remove(owner_id);
            }
        }
    }

    /// Hot-reload swap: build a successor limiter that inherits live
    /// state from the prior one. When `owner_id` has no prior
    /// limiter, falls back to a fresh `from_envelope`. Same semantics
    /// as [`RateLimitScopeManager::update`] — the carryover preserves
    /// `tokens` / `last_refill` and shares the `active_connections`
    /// gauge so a lowered concurrent cap drains gracefully (R-008).
    pub fn update(&self, owner_id: &OwnerId, rl: Option<&RateLimit>) {
        let mut guard = self.owners.write().expect("owner registry poisoned");
        match (guard.get(owner_id).cloned(), rl) {
            (Some(prior), Some(envelope)) => {
                let next = Arc::new(OwnerRateLimiter::with_carryover(&prior, envelope));
                guard.insert(owner_id.clone(), next);
            }
            (None, Some(envelope)) => {
                let next = Arc::new(OwnerRateLimiter::from_envelope(envelope));
                guard.insert(owner_id.clone(), next);
            }
            (_, None) => {
                guard.remove(owner_id);
            }
        }
    }

    /// Drop the limiter for `owner_id`. Idempotent.
    pub fn remove(&self, owner_id: &OwnerId) {
        let mut guard = self.owners.write().expect("owner registry poisoned");
        guard.remove(owner_id);
    }

    /// Snapshot the limiter for `owner_id`. Per-rule forwarders call
    /// this once at rule activation and cache the `Arc` for the
    /// rule's lifetime.
    #[must_use]
    pub fn get(&self, owner_id: &OwnerId) -> Option<Arc<OwnerRateLimiter>> {
        let guard = self.owners.read().expect("owner registry poisoned");
        guard.get(owner_id).cloned()
    }

    #[must_use]
    #[allow(dead_code)] // tests + future operator-debug surface
    pub fn len(&self) -> usize {
        let guard = self.owners.read().expect("owner registry poisoned");
        guard.len()
    }

    #[must_use]
    #[allow(dead_code)] // tests + future operator-debug surface
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// 011-rate-limiting-qos T032: registry of `OwnerId →
/// Arc<RateLimitStatsAccumulator>` for per-owner cumulative counters.
/// Multiple rules sharing the same owner share a single accumulator so
/// the `StatsReport.owner_rate_limit_stats` entry aggregates traffic
/// across the owner's rule set (FR-014). Allocation is lazy: the
/// per-rule control-plane lookup constructs the entry the first time
/// an owner-capped rule is installed and reuses it on subsequent
/// pushes for the same owner. Lifetime mirrors
/// [`OwnerRateLimitScopeManager`] — process-wide, surviving
/// reconnects.
#[derive(Debug, Default)]
pub struct OwnerRateLimitStatsRegistry {
    owners: RwLock<
        HashMap<OwnerId, Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>>,
    >,
}

impl OwnerRateLimitStatsRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Get-or-allocate the accumulator for `owner_id`. Per-rule wiring
    /// calls this once at rule activation; subsequent rules under the
    /// same owner reuse the same Arc so all of them increment one
    /// shared counter set.
    #[must_use]
    pub fn get_or_create(
        &self,
        owner_id: &OwnerId,
    ) -> Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator> {
        // Fast path: acquire read lock first to avoid contention when
        // the owner is already registered.
        if let Some(existing) = self
            .owners
            .read()
            .expect("owner-stats registry poisoned")
            .get(owner_id)
            .cloned()
        {
            return existing;
        }
        let mut guard = self.owners.write().expect("owner-stats registry poisoned");
        // Double-check under the write lock (race window between
        // releasing the read lock and acquiring the write lock).
        if let Some(existing) = guard.get(owner_id).cloned() {
            return existing;
        }
        let acc = Arc::new(crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator::new());
        guard.insert(owner_id.clone(), Arc::clone(&acc));
        acc
    }

    /// Snapshot every owner's current counters as a wire-shape
    /// `repeated OwnerRateLimitStats` payload. Owners whose
    /// accumulator drains to `None` (no event ever fired and gauge
    /// zero) are skipped — proto3 default-stripping keeps the wire
    /// shape byte-stable with v0.10 when no owner caps have fired.
    #[must_use]
    pub fn drain_to_proto(&self) -> Vec<forward_proto::v1::OwnerRateLimitStats> {
        let guard = self.owners.read().expect("owner-stats registry poisoned");
        let mut out = Vec::with_capacity(guard.len());
        for (owner_id, acc) in guard.iter() {
            if let Some(stats) = acc.drain_to_proto() {
                out.push(forward_proto::v1::OwnerRateLimitStats {
                    owner_id: owner_id.0.clone(),
                    stats: Some(stats),
                });
            }
        }
        out
    }

    /// Drop the accumulator for `owner_id`. Idempotent. Called when
    /// the last rule under `owner_id` is removed AND the owner has no
    /// installed cap — preserves the registry's "alive while
    /// observable" invariant. Currently unwired; the control loop
    /// keeps accumulators for the process lifetime so a removed-and-
    /// re-added owner keeps continuity.
    #[allow(dead_code)] // future GC sweep
    pub fn remove(&self, owner_id: &OwnerId) {
        let mut guard = self.owners.write().expect("owner-stats registry poisoned");
        guard.remove(owner_id);
    }

    #[must_use]
    #[allow(dead_code)] // tests + future operator-debug surface
    pub fn len(&self) -> usize {
        let guard = self.owners.read().expect("owner-stats registry poisoned");
        guard.len()
    }

    #[must_use]
    #[allow(dead_code)] // tests + future operator-debug surface
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

    /// 011-rate-limiting-qos T012: TCP rule with
    /// `new_connections_per_sec = R` enforces ±10% of R over a 60 s
    /// window. Drives `attempts` admits over a paused-time minute,
    /// asserts the admit count is within ±10% of `R × 60`.
    #[tokio::test(start_paused = true)]
    async fn t012_new_connections_per_sec_within_10pct_over_60s() {
        // Rate of 10/sec, no concurrent cap, default burst (= rate).
        // After 60 s the rate-limiter should have admitted ≈ 60 × 10
        // ≈ 600 accepts (subject to bucket initial-burst arithmetic).
        let r: u32 = 10;
        let l = Arc::new(RuleRateLimiter::from_envelope(&RateLimit {
            new_connections_per_sec: Some(r),
            ..Default::default()
        }));

        let target = u64::from(r) * 60;
        let mut admitted: u64 = 0;
        let start = tokio::time::Instant::now();
        // Drive accept-attempts at 1 ms cadence — enough to keep up
        // with a 10/s bucket (which refills every 100 ms).
        while start.elapsed() < Duration::from_secs(60) {
            if let ConnectionAcquire::Granted(_g) = l.try_acquire_connection(false) {
                admitted += 1;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        // ±10%: [target × 0.9, target × 1.1 + initial burst]. The
        // initial-burst accounts for the bucket starting full (10
        // accepts admit immediately on the first poll), so the
        // upper bound is target + burst.
        let lower = target * 9 / 10;
        let upper = target * 11 / 10 + u64::from(r);
        assert!(
            admitted >= lower && admitted <= upper,
            "admitted={admitted} outside [{lower}, {upper}] for rate={r}/s, 60s",
        );
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
    async fn t035_carryover_drains_concurrent_cap_below_live_count_without_force_close() {
        // R-008 / Q4 / FR-011: when the operator lowers the concurrent
        // cap below the current live count, the swap must NEVER force-
        // close in-flight connections. Existing `ActiveGuard`s stay
        // valid; new accepts reject; once enough guards drop the cap
        // becomes admissible again under the new ceiling.
        let prior = Arc::new(RuleRateLimiter::from_envelope(&RateLimit {
            concurrent_connections: Some(5),
            ..Default::default()
        }));
        let g1 = match prior.try_acquire_connection(false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!("g1 must admit"),
        };
        let g2 = match prior.try_acquire_connection(false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!("g2 must admit"),
        };
        let g3 = match prior.try_acquire_connection(false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!("g3 must admit"),
        };
        assert_eq!(prior.active_connections(), 3);

        // Operator lowers the cap to 2 — strictly below the live count
        // of 3. The successor limiter inherits the gauge; the existing
        // guards must remain bound to it.
        let next = Arc::new(RuleRateLimiter::with_carryover(
            &prior,
            &RateLimit {
                concurrent_connections: Some(2),
                ..Default::default()
            },
        ));
        assert_eq!(next.active_connections(), 3, "live count carries forward");
        // New accept under the new lower cap rejects.
        match next.try_acquire_connection(false) {
            ConnectionAcquire::Rejected(RejectReason::ConnConcurrent) => {}
            other => panic!("expected ConnConcurrent under lowered cap, got {other:?}"),
        }
        // Drop one guard from the prior frame — it decrements the
        // shared gauge. Live count goes from 3 to 2.
        drop(g1);
        assert_eq!(prior.active_connections(), 2);
        assert_eq!(next.active_connections(), 2);
        // Still at-cap — new accept rejects.
        match next.try_acquire_connection(false) {
            ConnectionAcquire::Rejected(RejectReason::ConnConcurrent) => {}
            other => panic!("at-cap accept must reject, got {other:?}"),
        }
        // Drop another guard — live count drops to 1, room for one
        // more under the new cap.
        drop(g2);
        assert_eq!(next.active_connections(), 1);
        let _g4 = match next.try_acquire_connection(false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!("admit allowed under new cap"),
        };
        assert_eq!(next.active_connections(), 2);
        drop(g3);
    }

    #[tokio::test(start_paused = true)]
    async fn t035_carryover_preserves_bandwidth_token_state_across_rate_raise() {
        // R-008: a cap raise must NOT mint a free burst. Drain the
        // bucket through `acquire_bandwidth`, then update to a higher
        // rate; the new bucket inherits the depleted token state, so
        // the next large acquire still throttles.
        let prior = RuleRateLimiter::from_envelope(&RateLimit {
            bandwidth_in_bps: Some(1_000),
            ..Default::default()
        });
        // Drain the full 1 KiB burst.
        assert_eq!(
            prior.acquire_bandwidth(BandwidthDirection::In, 1_000),
            BandwidthAcquire::Granted
        );
        // Pool empty — additional 500 must throttle on the prior.
        match prior.acquire_bandwidth(BandwidthDirection::In, 500) {
            BandwidthAcquire::Throttled { .. } => {}
            BandwidthAcquire::Granted => panic!("prior bucket must be drained"),
        }
        // Hot-reload: raise rate to 10x. The new burst defaults to
        // 1 × rate = 10_000, but the depleted token state carries.
        let next = RuleRateLimiter::with_carryover(
            &prior,
            &RateLimit {
                bandwidth_in_bps: Some(10_000),
                ..Default::default()
            },
        );
        // Immediately requesting 5_000 must NOT be granted — there
        // are no free tokens despite the larger burst (FR-011 / R-008
        // "no free burst on raise").
        match next.acquire_bandwidth(BandwidthDirection::In, 5_000) {
            BandwidthAcquire::Throttled { .. } => {}
            BandwidthAcquire::Granted => {
                panic!("rate raise must not mint a free burst (R-008)");
            }
        }
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

    /// T030: per-owner gate emits owner-prefixed reject reasons,
    /// distinct from the per-rule equivalents. Establishes the
    /// FR-014 "rejects carry distinct owner_* reasons" invariant.
    #[tokio::test(start_paused = true)]
    async fn t030_try_acquire_owner_emits_owner_reject_reasons() {
        let l = Arc::new(OwnerRateLimiter::from_envelope(&RateLimit {
            new_connections_per_sec: Some(1),
            concurrent_connections: Some(1),
            ..Default::default()
        }));
        // First TCP-shaped accept admits.
        let g = match l.try_acquire(CapScope::Owner, false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(r) => panic!("first owner-acquire admitted, got {r:?}"),
        };
        // Second back-to-back: rate token depleted → OwnerConnRate.
        match l.try_acquire(CapScope::Owner, false) {
            ConnectionAcquire::Rejected(RejectReason::OwnerConnRate) => {}
            other => panic!("expected OwnerConnRate, got {other:?}"),
        }
        // UDP-shaped first packet under the same rate exhaustion:
        // OwnerUdpFlowRate.
        match l.try_acquire(CapScope::Owner, true) {
            ConnectionAcquire::Rejected(RejectReason::OwnerUdpFlowRate) => {}
            other => panic!("expected OwnerUdpFlowRate, got {other:?}"),
        }
        // Drop the only guard so concurrent slot is free, but rate
        // bucket may still be empty — give the rate bucket a fresh
        // token.
        drop(g);
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        // Acquire one to fill the concurrent slot.
        let _g2 = match l.try_acquire(CapScope::Owner, false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(r) => panic!("post-refill admitted, got {r:?}"),
        };
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        // With concurrent at-cap (1/1) and rate refilled, the next
        // accept gets past the rate gate but hits the concurrent
        // ceiling → OwnerConcurrent.
        match l.try_acquire(CapScope::Owner, false) {
            ConnectionAcquire::Rejected(RejectReason::OwnerConcurrent) => {}
            other => panic!("expected OwnerConcurrent, got {other:?}"),
        }
    }

    /// T030: round-trip the owner registry — install / get / update
    /// / remove. Mirrors `scope_manager_install_get_remove_round_trip`
    /// but keyed by `OwnerId`.
    #[tokio::test(start_paused = true)]
    async fn t030_owner_scope_manager_install_get_remove_round_trip() {
        let mgr = OwnerRateLimitScopeManager::new();
        let alice = OwnerId::new("alice");
        let bob = OwnerId::new("bob");
        assert!(mgr.is_empty());

        mgr.install(&alice, Some(&rl_full()));
        assert!(mgr.get(&alice).is_some());
        assert!(mgr.get(&bob).is_none());
        assert_eq!(mgr.len(), 1);

        mgr.install(&bob, None);
        assert_eq!(mgr.len(), 1);

        mgr.remove(&alice);
        assert!(mgr.is_empty());
    }

    fn owner_handle_with(rl: RateLimit) -> Arc<OwnerRateLimitHandle> {
        let mgr = Arc::new(OwnerRateLimitScopeManager::new());
        let owner = OwnerId::new("alice");
        mgr.install(&owner, Some(&rl));
        Arc::new(OwnerRateLimitHandle::new(owner, mgr))
    }

    #[tokio::test(start_paused = true)]
    async fn t030_owner_handle_observes_updates_after_rule_activation() {
        let mgr = Arc::new(OwnerRateLimitScopeManager::new());
        let alice = OwnerId::new("alice");
        let handle = OwnerRateLimitHandle::new(alice.clone(), Arc::clone(&mgr));

        assert!(handle.snapshot().is_none(), "no cap installed yet");

        mgr.update(
            &alice,
            Some(&RateLimit {
                concurrent_connections: Some(1),
                ..Default::default()
            }),
        );

        let limiter = handle
            .snapshot()
            .expect("handle must see later owner-cap install");
        let guard = match handle.try_acquire(false) {
            Some(ConnectionAcquire::Granted(guard)) => guard,
            other => panic!("expected granted acquire, got {other:?}"),
        };
        assert_eq!(limiter.active_connections(), 1);
        drop(guard);
    }

    /// T030 / FR-013: owner gate runs before rule gate — when the
    /// owner is at-cap and the rule still has room, the cascade
    /// rejects with the OWNER_* reason and never touches the rule
    /// limiter's counters.
    #[tokio::test(start_paused = true)]
    async fn t030_layered_owner_binds_before_rule_when_owner_full() {
        let owner = owner_handle_with(RateLimit {
            concurrent_connections: Some(1),
            ..Default::default()
        });
        let rule = Arc::new(RuleRateLimiter::from_envelope(&RateLimit {
            concurrent_connections: Some(10),
            ..Default::default()
        }));
        // Saturate the owner cap.
        let _hold = owner.try_acquire(false).expect("owner limiter installed");
        match _hold {
            ConnectionAcquire::Granted(_) => {}
            ConnectionAcquire::Rejected(_) => panic!("first owner acquire admits"),
        }
        // Layered cascade: owner is at 1/1 → reject under
        // OwnerConcurrent. The rule's gauge must NOT change.
        let before_rule_active = rule.active_connections();
        match try_acquire_layered(Some(&owner), Some(&rule), false) {
            LayeredAcquire::OwnerRejected(RejectReason::OwnerConcurrent) => {}
            other => panic!("expected OwnerConcurrent reject, got {other:?}"),
        }
        assert_eq!(
            rule.active_connections(),
            before_rule_active,
            "rule limiter must be untouched when owner gate refuses"
        );
    }

    /// T030 / FR-013: when the rule rejects after the owner admits,
    /// the owner slot is RELEASED so the next admission attempt
    /// doesn't see a phantom +1 on the owner gauge.
    #[tokio::test(start_paused = true)]
    async fn t030_layered_owner_slot_released_when_rule_rejects() {
        let owner = owner_handle_with(RateLimit {
            concurrent_connections: Some(5),
            ..Default::default()
        });
        let rule = Arc::new(RuleRateLimiter::from_envelope(&RateLimit {
            concurrent_connections: Some(1),
            ..Default::default()
        }));
        // Saturate the rule cap via a direct (non-layered) acquire.
        let _rule_hold = match rule.try_acquire(CapScope::Rule, false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!("first rule acquire admits"),
        };
        let owner_before = owner
            .snapshot()
            .expect("owner limiter installed")
            .active_connections();
        match try_acquire_layered(Some(&owner), Some(&rule), false) {
            LayeredAcquire::RuleRejected(RejectReason::ConnConcurrent) => {}
            other => panic!("expected rule ConnConcurrent reject, got {other:?}"),
        }
        assert_eq!(
            owner
                .snapshot()
                .expect("owner limiter installed")
                .active_connections(),
            owner_before,
            "owner slot held during the failed rule probe must be released"
        );
    }

    /// T030: both gates pass — caller receives both guards. Their
    /// `Drop` ordering doesn't matter (independent gauges) but BOTH
    /// must be held by the connection task so per-owner and
    /// per-rule active counters decrement when the connection
    /// closes.
    #[tokio::test(start_paused = true)]
    async fn t030_layered_grants_both_guards_when_both_admit() {
        let owner = owner_handle_with(RateLimit {
            concurrent_connections: Some(2),
            ..Default::default()
        });
        let rule = Arc::new(RuleRateLimiter::from_envelope(&RateLimit {
            concurrent_connections: Some(2),
            ..Default::default()
        }));
        let (og, rg) = match try_acquire_layered(Some(&owner), Some(&rule), false) {
            LayeredAcquire::Granted {
                owner_guard,
                rule_guard,
            } => (owner_guard, rule_guard),
            other => panic!("expected Granted, got {other:?}"),
        };
        assert!(og.is_some());
        assert!(rg.is_some());
        assert_eq!(
            owner
                .snapshot()
                .expect("owner limiter installed")
                .active_connections(),
            1
        );
        assert_eq!(rule.active_connections(), 1);
        drop(og);
        drop(rg);
        assert_eq!(
            owner
                .snapshot()
                .expect("owner limiter installed")
                .active_connections(),
            0
        );
        assert_eq!(rule.active_connections(), 0);
    }

    /// T030: when both layers are uncapped (or absent), the cascade
    /// short-circuits to Granted with both guards None — no atomic
    /// ops, byte-stable v0.10 path.
    #[tokio::test(start_paused = true)]
    async fn t030_layered_short_circuits_when_no_limiters() {
        match try_acquire_layered(None, None, false) {
            LayeredAcquire::Granted {
                owner_guard,
                rule_guard,
            } => {
                assert!(owner_guard.is_none());
                assert!(rule_guard.is_none());
            }
            other => panic!("expected Granted with both None, got {other:?}"),
        }
    }

    /// T030: owner update with carryover preserves the live-count
    /// gauge across the swap (R-008 graceful drain on the owner
    /// scope, mirroring the per-rule invariant).
    #[tokio::test(start_paused = true)]
    async fn t030_owner_scope_manager_update_carries_state() {
        let mgr = OwnerRateLimitScopeManager::new();
        let owner = OwnerId::new("ops");
        mgr.install(&owner, Some(&rl_full()));
        let prior = mgr.get(&owner).unwrap();
        let _g = match prior.try_acquire(CapScope::Owner, false) {
            ConnectionAcquire::Granted(g) => g,
            ConnectionAcquire::Rejected(_) => panic!("first acquire admitted"),
        };
        assert_eq!(prior.active_connections(), 1);

        mgr.update(
            &owner,
            Some(&RateLimit {
                concurrent_connections: Some(5),
                ..Default::default()
            }),
        );
        let next = mgr.get(&owner).unwrap();
        assert!(!Arc::ptr_eq(&prior, &next), "update must swap the Arc");
        assert_eq!(next.active_connections(), 1);
    }

    /// T032: the stats registry returns the SAME Arc on repeated
    /// `get_or_create` calls for the same owner — cross-rule
    /// aggregation depends on a shared accumulator.
    #[test]
    fn t032_stats_registry_aggregates_across_rules_for_same_owner() {
        let reg = OwnerRateLimitStatsRegistry::new();
        let alice = OwnerId::new("alice");
        let bob = OwnerId::new("bob");
        let a1 = reg.get_or_create(&alice);
        let a2 = reg.get_or_create(&alice);
        let b1 = reg.get_or_create(&bob);
        assert!(Arc::ptr_eq(&a1, &a2), "same owner → same Arc");
        assert!(!Arc::ptr_eq(&a1, &b1), "different owners → distinct Arcs");
        assert_eq!(reg.len(), 2);
    }

    /// T032: drain emits one OwnerRateLimitStats entry per owner with
    /// non-empty counters; owners whose accumulators have no events
    /// are skipped (proto3 default-stripping preserves byte-stability
    /// with v0.10).
    #[test]
    fn t032_stats_registry_drain_skips_idle_owners() {
        use crate::forwarder::rate_limit::scope::RejectReason;
        let reg = OwnerRateLimitStatsRegistry::new();
        let alice = reg.get_or_create(&OwnerId::new("alice"));
        let _bob = reg.get_or_create(&OwnerId::new("bob"));
        // Alice has activity; Bob is idle.
        alice.record_reject(RejectReason::OwnerConcurrent);

        let drained = reg.drain_to_proto();
        assert_eq!(drained.len(), 1, "only owners with activity drain");
        assert_eq!(drained[0].owner_id, "alice");
        let stats = drained[0].stats.as_ref().expect("stats present");
        assert_eq!(stats.reject_total.len(), 1);
        assert_eq!(
            stats.reject_total[0].reason,
            forward_proto::v1::RateLimitRejectReason::OwnerConcurrent as i32,
        );
        assert_eq!(stats.reject_total[0].total, 1);
    }

    /// T032: drain on an empty registry is a no-op — keeps the
    /// `StatsReport.owner_rate_limit_stats` Vec empty so proto3
    /// default-stripping preserves v0.10 wire shape.
    #[test]
    fn t032_stats_registry_drain_empty_when_no_owners() {
        let reg = OwnerRateLimitStatsRegistry::new();
        assert!(reg.drain_to_proto().is_empty());
    }
}
