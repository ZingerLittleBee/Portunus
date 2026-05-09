//! 011-rate-limiting-qos T022 — `RateLimitStatsAccumulator` and proto
//! drainage.
//!
//! Per-rule and per-owner accumulators count rate-limit rejects (by
//! reason) plus cumulative bandwidth-throttle wall-clock time and the
//! live active-connection gauge. Drained into `RuleStats.rate_limit`
//! (proto field 16) on every report tick. The drain path returns
//! `None` when the accumulator has never observed an event AND the
//! gauge is zero, so v0.10 byte-stability holds for uncapped rules
//! (proto3 default-stripping).
//!
//! All counters are `AtomicU64`. Cumulative — the server takes deltas.
//!
//! Spec: `specs/011-rate-limiting-qos/data-model.md` § 2.5.

use std::sync::atomic::{AtomicU64, Ordering};

use forward_core::RejectReason;
use forward_proto::v1 as proto;

use super::scope::BandwidthDirection;

/// One slot per [`RejectReason`] variant. Index via
/// [`reason_index`]. The order matches `RejectReason as i32` modulo
/// the proto's `UNSPECIFIED = 0` slot which is never accumulated.
const REJECT_REASON_COUNT: usize = 6;

/// Map [`RejectReason`] to its slot index in
/// [`RateLimitStatsAccumulator::reject_total_by_reason`].
fn reason_index(reason: RejectReason) -> usize {
    match reason {
        RejectReason::ConnConcurrent => 0,
        RejectReason::ConnRate => 1,
        RejectReason::UdpFlowRate => 2,
        RejectReason::OwnerConcurrent => 3,
        RejectReason::OwnerConnRate => 4,
        RejectReason::OwnerUdpFlowRate => 5,
    }
}

/// Inverse of [`reason_index`] — used by the drain path to label
/// each slot in the proto repeated field.
fn reason_from_index(idx: usize) -> RejectReason {
    match idx {
        0 => RejectReason::ConnConcurrent,
        1 => RejectReason::ConnRate,
        2 => RejectReason::UdpFlowRate,
        3 => RejectReason::OwnerConcurrent,
        4 => RejectReason::OwnerConnRate,
        5 => RejectReason::OwnerUdpFlowRate,
        _ => unreachable!("slot index {idx} out of range"),
    }
}

/// Map [`RejectReason`] to its proto enum tag. Centralised so the
/// drain path and any future call site share one source of truth.
fn reason_to_proto(reason: RejectReason) -> proto::RateLimitRejectReason {
    match reason {
        RejectReason::ConnConcurrent => proto::RateLimitRejectReason::ConnConcurrent,
        RejectReason::ConnRate => proto::RateLimitRejectReason::ConnRate,
        RejectReason::UdpFlowRate => proto::RateLimitRejectReason::UdpFlowRate,
        RejectReason::OwnerConcurrent => proto::RateLimitRejectReason::OwnerConcurrent,
        RejectReason::OwnerConnRate => proto::RateLimitRejectReason::OwnerConnRate,
        RejectReason::OwnerUdpFlowRate => proto::RateLimitRejectReason::OwnerUdpFlowRate,
    }
}

/// Per-rule (or per-owner) cumulative rate-limit counters.
///
/// Lock-free, all-atomic. Accumulators are constructed once at rule
/// install and shared via `Arc` between the limiter call sites and
/// the periodic `StatsReport` builder.
#[derive(Debug, Default)]
pub struct RateLimitStatsAccumulator {
    reject_total_by_reason: [AtomicU64; REJECT_REASON_COUNT],
    throttle_micros_in: AtomicU64,
    throttle_micros_out: AtomicU64,
    /// Mirrors the limiter's `active_connections` gauge. The
    /// limiter is the source of truth; this slot is updated by the
    /// stats drain so the wire snapshot is consistent with the
    /// reject totals.
    active_connections: AtomicU64,
}

impl RateLimitStatsAccumulator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bump the reject counter for `reason`. Called from the TCP
    /// accept path (T019), the UDP first-packet path (T021), and the
    /// per-owner counterparts (T024+).
    pub fn record_reject(&self, reason: RejectReason) {
        let idx = reason_index(reason);
        self.reject_total_by_reason[idx].fetch_add(1, Ordering::Relaxed);
    }

    /// Bump the cumulative throttle-time counter for `direction` by
    /// `micros`. Called from the bandwidth copy loop (T020) on each
    /// `BandwidthAcquire::Throttled` deficit.
    #[allow(dead_code)] // wired up in T020 (bandwidth throttle in copy loop)
    pub fn record_throttle(&self, direction: BandwidthDirection, micros: u64) {
        match direction {
            BandwidthDirection::In => {
                self.throttle_micros_in.fetch_add(micros, Ordering::Relaxed);
            }
            BandwidthDirection::Out => {
                self.throttle_micros_out
                    .fetch_add(micros, Ordering::Relaxed);
            }
        }
    }

    /// Replace the live-count gauge. Drain path snapshots the
    /// limiter's atomic and stores it here so the wire emit sees a
    /// coherent (rejects, gauge) tuple even under concurrent activity.
    pub fn set_active_connections(&self, n: u64) {
        self.active_connections.store(n, Ordering::Relaxed);
    }

    /// Snapshot the active-connections gauge. Test + diagnostics only.
    #[must_use]
    #[allow(dead_code)] // wired up in T019/T023 (Prometheus collector + diagnostics)
    pub fn active_connections(&self) -> u64 {
        self.active_connections.load(Ordering::Relaxed)
    }

    /// Snapshot the cumulative reject count for `reason`.
    #[must_use]
    #[allow(dead_code)] // tests + future diagnostic / introspection callers
    pub fn reject_total(&self, reason: RejectReason) -> u64 {
        self.reject_total_by_reason[reason_index(reason)].load(Ordering::Relaxed)
    }

    /// Snapshot the cumulative throttle micros for `direction`.
    #[must_use]
    #[allow(dead_code)] // tests + future diagnostic / introspection callers
    pub fn throttle_micros(&self, direction: BandwidthDirection) -> u64 {
        match direction {
            BandwidthDirection::In => self.throttle_micros_in.load(Ordering::Relaxed),
            BandwidthDirection::Out => self.throttle_micros_out.load(Ordering::Relaxed),
        }
    }

    /// Drain a wire snapshot. Returns `None` when no event has ever
    /// fired and the gauge is zero — preserves v0.10 byte-stability
    /// for uncapped rules (proto3 default-stripping). Otherwise emits
    /// a sparse `RateLimitStats` carrying only the reasons that have
    /// fired and the throttle/gauge fields when non-zero.
    #[must_use]
    pub fn drain_to_proto(&self) -> Option<proto::RateLimitStats> {
        let mut reject_total = Vec::new();
        for (idx, slot) in self.reject_total_by_reason.iter().enumerate() {
            let total = slot.load(Ordering::Relaxed);
            if total == 0 {
                continue;
            }
            reject_total.push(proto::RateLimitRejectCount {
                reason: reason_to_proto(reason_from_index(idx)) as i32,
                total,
            });
        }
        let throttle_in = self.throttle_micros_in.load(Ordering::Relaxed);
        let throttle_out = self.throttle_micros_out.load(Ordering::Relaxed);
        let active = self.active_connections.load(Ordering::Relaxed);
        if reject_total.is_empty() && throttle_in == 0 && throttle_out == 0 && active == 0 {
            return None;
        }
        let active_u32 = u32::try_from(active).unwrap_or(u32::MAX);
        Some(proto::RateLimitStats {
            reject_total,
            throttle_micros_in: throttle_in,
            throttle_micros_out: throttle_out,
            active_connections: active_u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_accumulator_drains_to_none() {
        let acc = RateLimitStatsAccumulator::new();
        assert!(acc.drain_to_proto().is_none());
    }

    #[test]
    fn record_reject_increments_per_reason_slot() {
        let acc = RateLimitStatsAccumulator::new();
        acc.record_reject(RejectReason::ConnConcurrent);
        acc.record_reject(RejectReason::ConnConcurrent);
        acc.record_reject(RejectReason::OwnerUdpFlowRate);
        assert_eq!(acc.reject_total(RejectReason::ConnConcurrent), 2);
        assert_eq!(acc.reject_total(RejectReason::ConnRate), 0);
        assert_eq!(acc.reject_total(RejectReason::OwnerUdpFlowRate), 1);
    }

    #[test]
    fn drain_emits_only_reasons_that_fired() {
        let acc = RateLimitStatsAccumulator::new();
        acc.record_reject(RejectReason::ConnRate);
        acc.record_reject(RejectReason::OwnerConcurrent);
        let stats = acc.drain_to_proto().expect("non-empty drain");
        assert_eq!(stats.reject_total.len(), 2);
        let reasons: Vec<i32> = stats.reject_total.iter().map(|c| c.reason).collect();
        assert!(reasons.contains(&(proto::RateLimitRejectReason::ConnRate as i32)));
        assert!(reasons.contains(&(proto::RateLimitRejectReason::OwnerConcurrent as i32)));
        // No UNSPECIFIED sentinel and no untouched reasons.
        assert!(!reasons.contains(&(proto::RateLimitRejectReason::Unspecified as i32)));
        assert!(!reasons.contains(&(proto::RateLimitRejectReason::UdpFlowRate as i32)));
    }

    #[test]
    fn drain_carries_throttle_and_gauge() {
        let acc = RateLimitStatsAccumulator::new();
        acc.record_throttle(BandwidthDirection::In, 12_345);
        acc.record_throttle(BandwidthDirection::Out, 67_890);
        acc.set_active_connections(7);
        let stats = acc.drain_to_proto().expect("non-empty drain");
        assert_eq!(stats.throttle_micros_in, 12_345);
        assert_eq!(stats.throttle_micros_out, 67_890);
        assert_eq!(stats.active_connections, 7);
        assert!(stats.reject_total.is_empty());
    }

    #[test]
    fn record_throttle_accumulates_per_direction() {
        let acc = RateLimitStatsAccumulator::new();
        acc.record_throttle(BandwidthDirection::In, 1_000);
        acc.record_throttle(BandwidthDirection::In, 500);
        acc.record_throttle(BandwidthDirection::Out, 2_000);
        assert_eq!(acc.throttle_micros(BandwidthDirection::In), 1_500);
        assert_eq!(acc.throttle_micros(BandwidthDirection::Out), 2_000);
    }

    #[test]
    fn drain_active_connections_clamps_to_u32_max() {
        let acc = RateLimitStatsAccumulator::new();
        acc.set_active_connections(u64::from(u32::MAX) + 1);
        let stats = acc.drain_to_proto().expect("non-empty drain");
        assert_eq!(stats.active_connections, u32::MAX);
    }

    #[test]
    fn reject_reason_index_round_trip() {
        for reason in [
            RejectReason::ConnConcurrent,
            RejectReason::ConnRate,
            RejectReason::UdpFlowRate,
            RejectReason::OwnerConcurrent,
            RejectReason::OwnerConnRate,
            RejectReason::OwnerUdpFlowRate,
        ] {
            let idx = reason_index(reason);
            let back = reason_from_index(idx);
            assert_eq!(reason, back, "round trip failed for {reason:?}");
        }
    }
}
