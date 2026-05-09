// Phase 3 (T020+T021) lands the state machine + selection algorithm
// in this module. Phase 3 follow-up (T022) wires it into the
// forwarder activation entry. Until that wire-up commit lands, the
// items in this file are reachable only from the embedded tests, so
// the binary-crate dead-code analyser flags every export. The blanket
// `allow(dead_code)` lifts when the integration ships.
#![allow(dead_code)]

//! Per-target health state machine + selection algorithm
//! (007-multi-target-failover, US1 + US2).
//!
//! Allocated only for rules with `targets.len() >= 2` — single-target
//! rules never enter this module (Constitution Principle II — the
//! v0.6.0 hot path stays byte-identical).
//!
//! The state machine is purely in-memory and ephemeral: it lives for
//! the lifetime of the rule activation and resets on rule replace or
//! client restart. See `data-model.md` § 3 for the full contract.
//!
//! Defaults match `data-model.md` § "Defaults summary":
//!   * passive failure threshold: 3 consecutive failures within 30 s
//!   * passive recovery threshold: 2 consecutive successes
//!
//! These are NOT operator-tunable in v0.7 — they're the v1 baseline
//! the spec assumes throughout. The active-probe interval IS operator-
//! tunable (per-rule via `health_check_interval_secs`) and lives on
//! the rule, not on the state machine.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime};

use forward_core::RuleTarget;

/// Failure threshold: this many consecutive connect failures within
/// `FAILURE_WINDOW` flips a Healthy target to Failed (FR-008).
#[allow(dead_code)] // referenced by tests + Phase 3 forwarder integration (T022)
pub const PASSIVE_FAILURE_THRESHOLD: u32 = 3;

/// Sliding window for the failure threshold (FR-008). Failures spaced
/// further than this from the start of the current window roll the
/// window forward.
#[allow(dead_code)] // referenced inside `record_failure` via the literal value
pub const FAILURE_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

/// Recovery threshold: this many consecutive successes flip a Failed
/// target back to Healthy (FR-009).
#[allow(dead_code)] // referenced inside `record_success` via the literal value
pub const PASSIVE_RECOVERY_THRESHOLD: u32 = 2;

/// Per-target health discriminator. The on-the-wire encoding (see
/// `PerTargetStats.health` in proto-rule-extension.md §4) uses
/// 0 = Healthy, 1 = Failed; future states extend the value space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Healthy,
    Failed,
}

impl Health {
    /// On-the-wire encoding for `PerTargetStats.health`.
    #[must_use]
    pub fn as_wire(self) -> u32 {
        match self {
            Self::Healthy => 0,
            Self::Failed => 1,
        }
    }
}

/// In-memory per-target health record. One per `(rule_id, target_index)`
/// pair. See `data-model.md` § 3 for the full field contract.
#[derive(Debug)]
pub struct HealthState {
    state: Health,
    consecutive_failures: u32,
    consecutive_successes: u32,
    failure_window_start: Option<Instant>,
    last_failure_at: Option<SystemTime>,
    last_success_at: Option<SystemTime>,
    bytes_in: AtomicU64,
    bytes_out: AtomicU64,
    connections_accepted: AtomicU64,
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}

impl HealthState {
    /// New rule activation starts every target as Healthy
    /// (assumption: failover state is in-process and ephemeral).
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Health::Healthy,
            consecutive_failures: 0,
            consecutive_successes: 0,
            failure_window_start: None,
            last_failure_at: None,
            last_success_at: None,
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            connections_accepted: AtomicU64::new(0),
        }
    }

    /// Current health discriminator. Used by the selection algorithm.
    #[must_use]
    pub fn health(&self) -> Health {
        self.state
    }

    /// `consecutive_failures` since the last success. Surfaced on the
    /// `PerTargetStats` snapshot for operator visibility (FR-016).
    #[must_use]
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Wall-clock time of the most recent failure, if any.
    #[must_use]
    pub fn last_failure_at(&self) -> Option<SystemTime> {
        self.last_failure_at
    }

    /// Wall-clock time of the most recent success, if any.
    /// Phase 5 (T033) surfaces this on `PerTargetStats`.
    #[must_use]
    #[allow(dead_code)]
    pub fn last_success_at(&self) -> Option<SystemTime> {
        self.last_success_at
    }

    /// Snapshot the per-target byte / connection counters. Atomic
    /// `Relaxed` reads — the snapshot may be torn relative to a
    /// concurrent forwarder update, but the order of magnitude is
    /// correct. Same trade-off the v0.6.0 per-rule counters make.
    /// Wired into the stats reporter in Phase 5 (T033).
    #[must_use]
    #[allow(dead_code)]
    pub fn snapshot_bytes(&self) -> (u64, u64) {
        (
            self.bytes_in.load(Ordering::Relaxed),
            self.bytes_out.load(Ordering::Relaxed),
        )
    }

    #[must_use]
    #[allow(dead_code)]
    pub fn snapshot_connections(&self) -> u64 {
        self.connections_accepted.load(Ordering::Relaxed)
    }

    /// Credit `n` bytes inbound on this target. Called per buffer copy
    /// in the multi-target TCP / UDP data path (T034 lights this up).
    #[allow(dead_code)]
    pub fn add_bytes_in(&self, n: u64) {
        self.bytes_in.fetch_add(n, Ordering::Relaxed);
    }

    /// Credit `n` bytes outbound on this target.
    #[allow(dead_code)]
    pub fn add_bytes_out(&self, n: u64) {
        self.bytes_out.fetch_add(n, Ordering::Relaxed);
    }

    /// Increment the per-target accepted-connection counter (TCP
    /// connections OR new UDP flows). Phase 5 (T034) wires the call
    /// site into `accept_loop` for multi-target rules.
    #[allow(dead_code)]
    pub fn increment_connections_accepted(&self) {
        self.connections_accepted.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a connect failure for this target. Caller passes a
    /// monotonic `Instant` (used for the 30 s sliding window) and a
    /// wall-clock `SystemTime` (surfaced on the operator stats).
    /// Increments `target_failovers_total` exactly once on a
    /// Healthy→Failed transition (FR-010).
    pub fn record_failure(
        &mut self,
        now: Instant,
        wall: SystemTime,
        target_failovers_total: &AtomicU64,
    ) {
        self.last_failure_at = Some(wall);
        self.consecutive_successes = 0;

        // Manage the sliding window. If no window is open, OR the
        // existing window's first failure is older than FAILURE_WINDOW,
        // start a fresh window with a single counted failure.
        let window_open = self
            .failure_window_start
            .is_some_and(|start| now.duration_since(start) <= FAILURE_WINDOW);

        if window_open {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        } else {
            self.failure_window_start = Some(now);
            self.consecutive_failures = 1;
        }

        // Healthy → Failed transition.
        if self.state == Health::Healthy && self.consecutive_failures >= PASSIVE_FAILURE_THRESHOLD {
            self.state = Health::Failed;
            self.consecutive_successes = 0;
            target_failovers_total.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                event = "rule.target.health_changed",
                from = "Healthy",
                to = "Failed",
                consecutive_failures = self.consecutive_failures,
            );
        }
    }

    /// Record a connect success for this target. Resets the failure
    /// window. Increments `target_failovers_total` exactly once on a
    /// Failed→Healthy transition (FR-009 / FR-010).
    pub fn record_success(
        &mut self,
        _now: Instant,
        wall: SystemTime,
        target_failovers_total: &AtomicU64,
    ) {
        self.last_success_at = Some(wall);

        match self.state {
            Health::Healthy => {
                // Stay Healthy; reset failure tracking.
                self.consecutive_failures = 0;
                self.failure_window_start = None;
                self.consecutive_successes = 0;
            }
            Health::Failed => {
                self.consecutive_successes = self.consecutive_successes.saturating_add(1);
                if self.consecutive_successes >= PASSIVE_RECOVERY_THRESHOLD {
                    // Failed → Healthy transition.
                    self.state = Health::Healthy;
                    self.consecutive_failures = 0;
                    self.consecutive_successes = 0;
                    self.failure_window_start = None;
                    target_failovers_total.fetch_add(1, Ordering::Relaxed);
                    tracing::info!(
                        event = "rule.target.health_changed",
                        from = "Failed",
                        to = "Healthy",
                    );
                }
            }
        }
    }
}

/// Selection algorithm (`data-model.md` § 3).
///
/// Returns the index of the target that should receive the next new
/// connection / new UDP flow.
///
/// Policy:
///   1. Among targets with `Health::Healthy`, pick the lowest-index
///      one (caller pre-sorts by `(priority, row_index)` ascending).
///   2. If every target is Failed, fall back to index 0 — the highest-
///      priority target overall (FR-007 — "do something; surface the
///      failure" — never silently drop).
///
/// Panics if `targets` is empty — multi-target rules are guaranteed
/// non-empty by `forward_core::rule_target::validate` at push time.
#[must_use]
pub fn select(states: &[HealthState]) -> usize {
    debug_assert!(!states.is_empty(), "multi-target rule must have ≥1 target");
    // Caller guarantees `states` is in priority order. Walk it and
    // pick the first Healthy.
    for (i, s) in states.iter().enumerate() {
        if s.health() == Health::Healthy {
            return i;
        }
    }
    // FR-007: all Failed → still attempt the highest-priority target.
    0
}

/// Caller-side helper to sort a `(target, state)` zip by
/// `(priority, row_index)` ascending. The forwarder builds the per-
/// target `Vec<HealthState>` in the same order as `Rule.targets`, so
/// row index is preserved naturally.
///
/// Returns `None` when `targets` is empty (which the validator
/// rejects, but defensive programming costs nothing here).
#[must_use]
pub fn sort_priority(targets: &[RuleTarget]) -> Option<Vec<usize>> {
    if targets.is_empty() {
        return None;
    }
    let mut order: Vec<usize> = (0..targets.len()).collect();
    order.sort_by_key(|&i| (targets[i].priority, i));
    Some(order)
}

// =====================================================================
// Tests (T017 + T019)
// =====================================================================
//
// Test-first per Constitution Principle III: these tests must compile
// and pass independently of the forwarder integration (T022+) — they
// pin down the state machine and selection contracts in isolation.

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn t(host: &str, port: u16, priority: u32) -> RuleTarget {
        RuleTarget {
            host: host.to_string(),
            port,
            priority,
            proxy_protocol: None,
        }
    }

    fn fresh_counter() -> AtomicU64 {
        AtomicU64::new(0)
    }

    // ---- T017: state machine transitions ------------------------------

    #[test]
    fn fresh_state_is_healthy() {
        let s = HealthState::new();
        assert_eq!(s.health(), Health::Healthy);
        assert_eq!(s.consecutive_failures(), 0);
    }

    #[test]
    fn fewer_than_threshold_failures_stay_healthy() {
        let counter = fresh_counter();
        let now = Instant::now();
        let wall = SystemTime::now();
        let mut s = HealthState::new();
        s.record_failure(now, wall, &counter);
        s.record_failure(now + Duration::from_secs(1), wall, &counter);
        // Two failures within window — still Healthy.
        assert_eq!(s.health(), Health::Healthy);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn three_failures_within_30s_window_flip_to_failed() {
        let counter = fresh_counter();
        let now = Instant::now();
        let wall = SystemTime::now();
        let mut s = HealthState::new();
        s.record_failure(now, wall, &counter);
        s.record_failure(now + Duration::from_secs(10), wall, &counter);
        s.record_failure(now + Duration::from_secs(20), wall, &counter);
        assert_eq!(s.health(), Health::Failed);
        assert_eq!(s.consecutive_failures(), 3);
        // Healthy → Failed counts as one transition (FR-010).
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        assert!(s.last_failure_at().is_some());
    }

    #[test]
    fn three_failures_outside_window_stay_healthy() {
        // Two failures inside the window, third outside → window
        // resets, only "third" counts as the new window's first.
        let counter = fresh_counter();
        let now = Instant::now();
        let wall = SystemTime::now();
        let mut s = HealthState::new();
        s.record_failure(now, wall, &counter);
        s.record_failure(now + Duration::from_secs(20), wall, &counter);
        s.record_failure(now + Duration::from_secs(45), wall, &counter);
        // The third failure is > 30 s after the window's start, so it
        // opens a fresh window with consecutive_failures = 1.
        assert_eq!(s.health(), Health::Healthy);
        assert_eq!(s.consecutive_failures(), 1);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn success_resets_failure_count_when_healthy() {
        let counter = fresh_counter();
        let now = Instant::now();
        let wall = SystemTime::now();
        let mut s = HealthState::new();
        s.record_failure(now, wall, &counter);
        s.record_failure(now + Duration::from_secs(1), wall, &counter);
        s.record_success(now + Duration::from_secs(2), wall, &counter);
        assert_eq!(s.health(), Health::Healthy);
        assert_eq!(s.consecutive_failures(), 0);
        // No transition occurred.
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn one_success_in_failed_state_does_not_recover() {
        let counter = fresh_counter();
        let now = Instant::now();
        let wall = SystemTime::now();
        let mut s = HealthState::new();
        // Drive to Failed.
        for i in 0..3 {
            s.record_failure(now + Duration::from_secs(i), wall, &counter);
        }
        assert_eq!(s.health(), Health::Failed);
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        // One success — not enough to flip back.
        s.record_success(now + Duration::from_secs(10), wall, &counter);
        assert_eq!(s.health(), Health::Failed);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn two_consecutive_successes_recover_from_failed() {
        let counter = fresh_counter();
        let now = Instant::now();
        let wall = SystemTime::now();
        let mut s = HealthState::new();
        for i in 0..3 {
            s.record_failure(now + Duration::from_secs(i), wall, &counter);
        }
        s.record_success(now + Duration::from_secs(10), wall, &counter);
        s.record_success(now + Duration::from_secs(11), wall, &counter);
        assert_eq!(s.health(), Health::Healthy);
        // Healthy → Failed → Healthy = two transitions.
        assert_eq!(counter.load(Ordering::Relaxed), 2);
        assert_eq!(s.consecutive_failures(), 0);
    }

    #[test]
    fn failure_resets_consecutive_success_run_in_failed_state() {
        let counter = fresh_counter();
        let now = Instant::now();
        let wall = SystemTime::now();
        let mut s = HealthState::new();
        for i in 0..3 {
            s.record_failure(now + Duration::from_secs(i), wall, &counter);
        }
        // One success (1/2 toward recovery)…
        s.record_success(now + Duration::from_secs(10), wall, &counter);
        // …then a failure — recovery progress resets.
        s.record_failure(now + Duration::from_secs(11), wall, &counter);
        s.record_success(now + Duration::from_secs(12), wall, &counter);
        // Still Failed — the lone success after the resetting failure
        // is only 1/2.
        assert_eq!(s.health(), Health::Failed);
        // Only the original Healthy → Failed transition counted.
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    // ---- T019: selection algorithm ------------------------------------

    fn states_with_health(healths: &[Health]) -> Vec<HealthState> {
        healths
            .iter()
            .map(|&h| {
                let mut s = HealthState::new();
                if h == Health::Failed {
                    let now = Instant::now();
                    let wall = SystemTime::now();
                    let counter = fresh_counter();
                    for i in 0..PASSIVE_FAILURE_THRESHOLD {
                        s.record_failure(
                            now + Duration::from_millis(u64::from(i) * 10),
                            wall,
                            &counter,
                        );
                    }
                    debug_assert_eq!(s.health(), Health::Failed);
                }
                s
            })
            .collect()
    }

    #[test]
    fn select_picks_first_when_all_healthy() {
        let states = states_with_health(&[Health::Healthy, Health::Healthy]);
        assert_eq!(select(&states), 0);
    }

    #[test]
    fn select_skips_failed_to_first_healthy() {
        let states = states_with_health(&[Health::Failed, Health::Healthy]);
        assert_eq!(select(&states), 1);
    }

    #[test]
    fn select_falls_back_to_index_0_when_all_failed() {
        // FR-007 — "do something; surface the failure"; never silently
        // drop. The connection attempt then fails through the selected
        // target's connect timeout.
        let states = states_with_health(&[Health::Failed, Health::Failed]);
        assert_eq!(select(&states), 0);
    }

    #[test]
    fn select_with_three_targets_skips_all_failed_until_healthy() {
        let states = states_with_health(&[Health::Failed, Health::Failed, Health::Healthy]);
        assert_eq!(select(&states), 2);
    }

    #[test]
    fn sort_priority_preserves_row_order_on_ties() {
        // Two targets with priority=0 — ties broken by row index, so
        // the result is `[0, 1]` regardless of subsequent priorities.
        let targets = vec![t("a.test", 80, 0), t("b.test", 80, 0), t("c.test", 80, 5)];
        let order = sort_priority(&targets).expect("non-empty");
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn sort_priority_orders_lowest_first() {
        let targets = vec![t("a.test", 80, 5), t("b.test", 80, 1), t("c.test", 80, 3)];
        let order = sort_priority(&targets).expect("non-empty");
        assert_eq!(order, vec![1, 2, 0]);
    }
}
