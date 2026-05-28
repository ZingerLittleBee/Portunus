//! Per-rule traffic counters.
//!
//! `RuleStats` is shared between the per-rule listener (which spawns proxies
//! and increments `active_connections`) and the periodic `StatsReport`
//! sender in `control.rs`. Counters are monotonic cumulative — the server
//! computes deltas for Prometheus.
//!
//! Range rules (002-port-range-forward) additionally maintain per-port
//! counters in `per_port`. The aggregate counters always reflect the sum
//! across every port; per-port detail is reported on the existing bidi
//! stream and surfaced only when an operator passes `--per-port`. The
//! per-port slot is intentionally NOT re-exported as Prometheus series
//! (SC-002 — cardinality budget). Single-port rules ship with `per_port`
//! empty (graceful degradation: `record_in` and friends update the
//! aggregate only).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use portunus_core::PortRange;

/// Wire-neutral mirror of `proto::v1::RateLimitRejectReason` — variants
/// listed in the **same order** as the proto enum so per-variant numeric
/// codes stay stable when the client crate translates via `From`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RateLimitRejectReason {
    Unspecified,
    ConnConcurrent,
    ConnRate,
    UdpFlowRate,
    OwnerConcurrent,
    OwnerConnRate,
    OwnerUdpFlowRate,
}

/// Wire-neutral mirror of `proto::v1::RateLimitStats` (spec §5.2). Lives
/// in forwarder so the data plane is proto-free; the client crate
/// translates via `From<RateLimitStatsSnapshot> for proto::v1::RateLimitStats`.
#[derive(Clone, Debug, Default)]
pub struct RateLimitStatsSnapshot {
    pub reject_total: Vec<(RateLimitRejectReason, u64)>,
    pub throttle_micros_in: u64,
    pub throttle_micros_out: u64,
    pub active_connections: u32,
}

/// Wire-neutral mirror of `proto::v1::OwnerRateLimitStats`. Pairs an owner
/// identifier with its drained `RateLimitStatsSnapshot` so the client crate
/// can translate via `From<OwnerRateLimitStatsSnapshot>` without touching
/// proto types in the data plane.
#[derive(Clone, Debug, Default)]
pub struct OwnerRateLimitStatsSnapshot {
    pub owner_id: String,
    pub stats: RateLimitStatsSnapshot,
}

/// Wire-neutral mirror of `proto::v1::SniListenerStats` (full peek
/// histogram fields included). Client translates via
/// `From<SniListenerStatsSnapshot> for proto::v1::SniListenerStats`.
#[derive(Clone, Debug, Default)]
pub struct SniListenerStatsSnapshot {
    pub listen_port: u16,
    pub sni_route_miss_total: u64,
    pub client_hello_parse_failures_total: u64,
    /// Bucket counts in order of `portunus_core::PEEK_HISTOGRAM_BUCKETS_SECS`.
    pub client_hello_peek_bucket_counts: Vec<u64>,
    pub client_hello_peek_sum_micros: u64,
    pub client_hello_peek_count: u64,
}

impl RateLimitRejectReason {
    /// Stable mapping to proto enum integer values. Lets the data plane
    /// key a `[u64; N]` counter array without pulling in proto types.
    #[must_use]
    pub fn as_index(self) -> usize {
        match self {
            Self::Unspecified => 0,
            Self::ConnConcurrent => 1,
            Self::ConnRate => 2,
            Self::UdpFlowRate => 3,
            Self::OwnerConcurrent => 4,
            Self::OwnerConnRate => 5,
            Self::OwnerUdpFlowRate => 6,
        }
    }
}

#[derive(Debug, Default)]
pub struct PerPortCounters {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub active_connections: AtomicU32,
    /// 004-udp-forward T053: per-port datagram counters for UDP range
    /// rules. Always 0 for TCP entries.
    pub datagrams_in: AtomicU64,
    pub datagrams_out: AtomicU64,
}

/// 015-standalone-stats-tui: per-rule failure event counters.
/// Each `AtomicU64` is bumped from the existing `tracing::warn!` /
/// `tracing::info!` call site for the matching event name. Counters
/// are cumulative since the rule was activated.
#[derive(Debug, Default)]
pub struct ErrorCounters {
    /// `rule.failed` (port_in_use) — TCP bind failure.
    pub port_in_use: AtomicU64,
    /// `rule.udp_upstream_connect_failed` — connect(2) on a UDP
    /// upstream socket failed before the flow was installed.
    pub upstream_connect_failed: AtomicU64,
    /// `rule.udp_flow_evicted_icmp` — kernel returned an ICMP
    /// error on the connected upstream socket; flow evicted.
    pub icmp_evict: AtomicU64,
    /// `rule.udp_emsgsize` — datagram too large for the path MTU.
    pub emsgsize: AtomicU64,
    /// `rule.udp_reply_wouldblock` — reply send_to returned
    /// WouldBlock; datagram dropped.
    pub wouldblock: AtomicU64,
    /// `rule.udp_addflow_dropped` — new-flow datagram dropped
    /// because the per-rule flow table is at capacity.
    pub addflow_dropped: AtomicU64,
}

impl ErrorCounters {
    pub fn inc_port_in_use(&self) {
        self.port_in_use.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_upstream_connect_failed(&self) {
        self.upstream_connect_failed.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_icmp_evict(&self) {
        self.icmp_evict.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_emsgsize(&self) {
        self.emsgsize.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_wouldblock(&self) {
        self.wouldblock.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_addflow_dropped(&self) {
        self.addflow_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot all six counters at once.
    #[must_use]
    pub fn snapshot(&self) -> ErrorSnapshot {
        ErrorSnapshot {
            port_in_use: self.port_in_use.load(Ordering::Relaxed),
            upstream_connect_failed: self.upstream_connect_failed.load(Ordering::Relaxed),
            icmp_evict: self.icmp_evict.load(Ordering::Relaxed),
            emsgsize: self.emsgsize.load(Ordering::Relaxed),
            wouldblock: self.wouldblock.load(Ordering::Relaxed),
            addflow_dropped: self.addflow_dropped.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ErrorSnapshot {
    pub port_in_use: u64,
    pub upstream_connect_failed: u64,
    pub icmp_evict: u64,
    pub emsgsize: u64,
    pub wouldblock: u64,
    pub addflow_dropped: u64,
}

#[derive(Debug, Default)]
pub struct RuleStats {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub active_connections: AtomicU32,
    /// 015-standalone-stats-tui: monotonic count of accepted TCP
    /// connections per rule. UDP rules carry the field but never
    /// increment it (use `datagrams_in` / `flows_active` instead).
    pub connections_total: AtomicU64,
    /// 015-standalone-stats-tui: failure-event counters paired with
    /// existing tracing call sites.
    pub errors: ErrorCounters,
    /// 003-domain-name-forward (FR-008): monotonic count of end-user
    /// connections that ultimately failed to dial because of DNS —
    /// either resolution returned an error, the answer was empty, or
    /// every resolved address was unreachable. For IP-target rules
    /// this counter never increments (the resolver layer is
    /// short-circuited); the wire emit (`StatsReport`) skips the
    /// proto field when the value is 0 to keep v0.2.0 byte-compat
    /// (verified by `dns_wire_compat::v0_2_0_rule_stats_byte_compatible_when_dns_failures_zero`).
    pub dns_failures: AtomicU64,
    /// 004-udp-forward T029: monotonic count of UDP datagrams the rule's
    /// listener received from end-users (across all ports for range
    /// rules). Always 0 for TCP rules.
    pub datagrams_in: AtomicU64,
    /// Cumulative count of UDP datagrams the rule sent back to end-users.
    /// Always 0 for TCP rules.
    pub datagrams_out: AtomicU64,
    /// Current live UDP flows aggregated across all ports. Snapshotted
    /// each `StatsReport` tick from the per-rule `UdpFlowTable::len()`
    /// (or the sum across per-port tables for range rules). Always 0
    /// for TCP rules.
    pub active_flows: AtomicU32,
    /// Cumulative count of new-flow first-datagrams dropped because the
    /// per-rule flow table was at `udp_max_flows_per_rule`. Always 0 for
    /// TCP rules.
    pub flows_dropped_overflow: AtomicU64,
    /// Per-port counters keyed on the listen-side port. Populated at
    /// construction by [`RuleStats::for_range`]; empty when constructed
    /// via [`RuleStats::new`]. Lookup misses are silent — the aggregate
    /// is always updated regardless.
    pub per_port: BTreeMap<u16, PerPortCounters>,
    /// 009-tls-sni-routing T077: monotonic per-rule SNI hit counters.
    /// Bumped from `SniListener::handle_accept` BEFORE the dispatch.
    /// Always 0 for legacy plain-TCP rules and for UDP rules; the
    /// wire emit (`StatsReport`) folds them into proto fields 13/14/15
    /// (`RuleStats.sni_route_*_total`). The slot is populated for SNI
    /// rules from the listener's `SniRuleSlot`; the same `Arc<AtomicU64>`
    /// trio is shared by reference between the slot and this struct
    /// so both readers see the same totals.
    pub sni_route_exact_total: Arc<AtomicU64>,
    pub sni_route_wildcard_total: Arc<AtomicU64>,
    pub sni_route_fallback_total: Arc<AtomicU64>,
    /// 015-standalone-stats-tui: aggregate count of failover events
    /// across the rule's target list. Always present; for
    /// single-target rules the counter stays 0. The `Arc` is shared
    /// with the failover supervisor in `failover_path.rs` so both
    /// the failover hot path and the stats server observe one value.
    pub target_failovers_total: Arc<AtomicU64>,
}

impl RuleStats {
    /// Aggregate-only counters. Used by unit tests in this module and
    /// by callers that don't have a `PortRange` handy (e.g., the
    /// `record_on_unknown_port_updates_aggregate_only` regression
    /// test). Production rule construction goes through
    /// [`RuleStats::for_range`].
    #[must_use]
    #[cfg(test)]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Range-aware constructor. Initialises one `PerPortCounters` slot
    /// per port in `range`. For range size 1 the per-port slot is still
    /// allocated so single-port rules can report a one-element
    /// `per_port` slot if a future caller wants it (the client today
    /// only emits `per_port` when `range.len() > 1`).
    #[must_use]
    pub fn for_range(range: PortRange) -> Arc<Self> {
        let mut per_port = BTreeMap::new();
        for port in range.iter() {
            per_port.insert(port, PerPortCounters::default());
        }
        Arc::new(Self {
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            active_connections: AtomicU32::new(0),
            connections_total: AtomicU64::new(0),
            errors: ErrorCounters::default(),
            dns_failures: AtomicU64::new(0),
            datagrams_in: AtomicU64::new(0),
            datagrams_out: AtomicU64::new(0),
            active_flows: AtomicU32::new(0),
            flows_dropped_overflow: AtomicU64::new(0),
            per_port,
            sni_route_exact_total: Arc::new(AtomicU64::new(0)),
            sni_route_wildcard_total: Arc::new(AtomicU64::new(0)),
            sni_route_fallback_total: Arc::new(AtomicU64::new(0)),
            target_failovers_total: Arc::new(AtomicU64::new(0)),
        })
    }

    // ----- 004-udp-forward T029: UDP counters -----

    /// Bump the inbound-datagram counter (and the per-port slot if present).
    /// `n` is the byte count of the datagram payload — also folded into
    /// `bytes_in` for protocol-agnostic byte-count reporting.
    pub fn inc_datagram_in(&self, port: u16, n: u64) {
        self.datagrams_in.fetch_add(1, Ordering::Relaxed);
        self.bytes_in.fetch_add(n, Ordering::Relaxed);
        if let Some(slot) = self.per_port.get(&port) {
            slot.datagrams_in.fetch_add(1, Ordering::Relaxed);
            slot.bytes_in.fetch_add(n, Ordering::Relaxed);
        }
    }

    pub fn inc_datagram_out(&self, port: u16, n: u64) {
        self.datagrams_out.fetch_add(1, Ordering::Relaxed);
        self.bytes_out.fetch_add(n, Ordering::Relaxed);
        if let Some(slot) = self.per_port.get(&port) {
            slot.datagrams_out.fetch_add(1, Ordering::Relaxed);
            slot.bytes_out.fetch_add(n, Ordering::Relaxed);
        }
    }

    /// Replace the live-flow gauge. Called on each StatsReport tick by
    /// the listener path with the current `UdpFlowTable::len()`.
    pub fn set_active_flows(&self, n: u32) {
        self.active_flows.store(n, Ordering::Relaxed);
    }

    pub fn inc_flow_dropped_overflow(&self) {
        self.flows_dropped_overflow.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot_datagrams_in(&self) -> u64 {
        self.datagrams_in.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn snapshot_datagrams_out(&self) -> u64 {
        self.datagrams_out.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn snapshot_active_flows(&self) -> u32 {
        self.active_flows.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn snapshot_flows_dropped_overflow(&self) -> u64 {
        self.flows_dropped_overflow.load(Ordering::Relaxed)
    }

    /// Per-port snapshot in port order, including UDP datagram counts.
    /// Returns `(listen_port, bytes_in, bytes_out, active_connections,
    ///           datagrams_in, datagrams_out)`.
    #[must_use]
    pub fn snapshot_per_port_with_udp(&self) -> Vec<(u16, u64, u64, u32, u64, u64)> {
        self.per_port
            .iter()
            .map(|(port, c)| {
                (
                    *port,
                    c.bytes_in.load(Ordering::Relaxed),
                    c.bytes_out.load(Ordering::Relaxed),
                    c.active_connections.load(Ordering::Relaxed),
                    c.datagrams_in.load(Ordering::Relaxed),
                    c.datagrams_out.load(Ordering::Relaxed),
                )
            })
            .collect()
    }

    /// 015-standalone-stats-tui: bump on each accepted TCP connection.
    /// TCP-only call sites; UDP listener path does not invoke this.
    pub fn inc_connection(&self) {
        self.connections_total.fetch_add(1, Ordering::Relaxed);
    }

    /// 003-domain-name-forward (FR-008): one DNS failure (NXDOMAIN,
    /// SERVFAIL, timeout, all-addrs-unreachable, stale-served-then-still-failing).
    /// Per-rule cardinality only; no per-port breakdown — the
    /// resolver works at the rule level.
    pub fn inc_dns_failure(&self) {
        self.dns_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot the DNS-failure counter. Used by `StatsReport` and
    /// the test harness; the wire emit conditionally skips the proto
    /// field when this is 0 (v0.2.0 byte-compat).
    #[must_use]
    pub fn snapshot_dns_failures(&self) -> u64 {
        self.dns_failures.load(Ordering::Relaxed)
    }

    /// Record `n` inbound bytes on `port`. The aggregate counter is
    /// always incremented; the per-port slot is incremented if it
    /// exists (range rules) and silently ignored otherwise (legacy
    /// aggregate-only callers).
    pub fn record_in(&self, port: u16, n: u64) {
        self.bytes_in.fetch_add(n, Ordering::Relaxed);
        if let Some(slot) = self.per_port.get(&port) {
            slot.bytes_in.fetch_add(n, Ordering::Relaxed);
        }
    }

    pub fn record_out(&self, port: u16, n: u64) {
        self.bytes_out.fetch_add(n, Ordering::Relaxed);
        if let Some(slot) = self.per_port.get(&port) {
            slot.bytes_out.fetch_add(n, Ordering::Relaxed);
        }
    }

    pub fn inc_active(&self, port: u16) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
        if let Some(slot) = self.per_port.get(&port) {
            slot.active_connections.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn dec_active(&self, port: u16) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
        if let Some(slot) = self.per_port.get(&port) {
            slot.active_connections.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Aggregate snapshot — `(bytes_in, bytes_out, active_connections)`.
    #[must_use]
    pub fn snapshot(&self) -> (u64, u64, u32) {
        (
            self.bytes_in.load(Ordering::Relaxed),
            self.bytes_out.load(Ordering::Relaxed),
            self.active_connections.load(Ordering::Relaxed),
        )
    }

    /// Per-port snapshot in port order. Empty for aggregate-only
    /// constructions (`RuleStats::new`). Retained for legacy callers /
    /// tests that don't need the UDP datagram columns; the wire emit
    /// uses `snapshot_per_port_with_udp`.
    #[must_use]
    #[allow(dead_code)]
    pub fn snapshot_per_port(&self) -> Vec<(u16, u64, u64, u32)> {
        self.per_port
            .iter()
            .map(|(port, c)| {
                (
                    *port,
                    c.bytes_in.load(Ordering::Relaxed),
                    c.bytes_out.load(Ordering::Relaxed),
                    c.active_connections.load(Ordering::Relaxed),
                )
            })
            .collect()
    }
}

use portunus_core::RuleId;

/// Per-port detail for range rules. Single-port rules still emit one slot
/// for symmetry. Empty `per_port` keeps proto3 default-strip semantics
/// at the wire-translation layer.
#[derive(Clone, Debug, Default)]
pub struct PerPortStatsSnapshot {
    pub listen_port: u16,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
}

/// Per-target detail for multi-target rules. Lives in this struct because
/// the wire translation (`From<PerTargetStatsSnapshot> for proto::v1::PerTargetStats`)
/// needs to encode `health` as a `uint32` via `TargetHealth::as_wire()`.
#[derive(Clone, Debug, Default)]
pub struct PerTargetStatsSnapshot {
    pub index: u32,
    pub host: String,
    pub port: u16,
    pub priority: u32,
    pub health: TargetHealth,
    pub consecutive_failures: u32,
    pub last_failure_at_unix_ms: u64,
    pub last_success_at_unix_ms: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub connections_accepted: u64,
}

/// Mirrors `forwarder::failover::Health` for the wire snapshot path.
/// Healthy=0, Failed=1 — kept stable so `proto::v1::PerTargetStats.health: uint32`
/// stays byte-identical across the client and standalone reporters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetHealth {
    #[default]
    Healthy,
    Failed,
}

impl TargetHealth {
    /// Stable uint32 mapping written to the wire.
    #[must_use]
    pub fn as_wire(self) -> u32 {
        match self {
            Self::Healthy => 0,
            Self::Failed => 1,
        }
    }
}

/// Basic per-rule counters owned by `RuleStats`. Excludes per-target,
/// rate-limit, and target-failovers state — those live alongside on
/// the client `RuleSlot` and are stitched together in
/// `portunus-client::control::build_rule_stats_snapshot`.
#[derive(Clone, Debug, Default)]
pub struct RuleStatsSnapshotBasic {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
    pub per_port: Vec<PerPortStatsSnapshot>,
    pub dns_failures: u64,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub active_flows: u32,
    pub flows_dropped_overflow: u64,
    pub sni_route_exact_total: u64,
    pub sni_route_wildcard_total: u64,
    pub sni_route_fallback_total: u64,
}

/// Complete per-rule snapshot used by the wire-translation layer.
/// Assembled by `build_rule_stats_snapshot(rule_id, &slot)` in
/// portunus-client::control; the standalone reporter constructs an
/// equivalent value without needing a client `RuleSlot`.
#[derive(Clone, Debug)]
pub struct RuleStatsSnapshot {
    pub rule_id: RuleId,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
    pub per_port: Vec<PerPortStatsSnapshot>,
    pub dns_failures: u64,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub active_flows: u32,
    pub flows_dropped_overflow: u64,
    pub target_failovers_total: u64,
    pub per_target: Vec<PerTargetStatsSnapshot>,
    pub sni_route_exact_total: u64,
    pub sni_route_wildcard_total: u64,
    pub sni_route_fallback_total: u64,
    pub rate_limit: Option<RateLimitStatsSnapshot>,
}

impl RuleStats {
    /// proto-free snapshot of this struct's basic counters. Excludes
    /// per-target / rate-limit / target-failovers (assembled by caller).
    #[must_use]
    pub fn snapshot_basic(&self) -> RuleStatsSnapshotBasic {
        let (bytes_in, bytes_out, active_connections) = self.snapshot();
        let per_port = self
            .snapshot_per_port_with_udp()
            .into_iter()
            .map(
                |(listen_port, bin, bout, active, dgin, dgout)| PerPortStatsSnapshot {
                    listen_port,
                    bytes_in: bin,
                    bytes_out: bout,
                    active_connections: active,
                    datagrams_in: dgin,
                    datagrams_out: dgout,
                },
            )
            .collect();
        RuleStatsSnapshotBasic {
            bytes_in,
            bytes_out,
            active_connections,
            per_port,
            dns_failures: self.snapshot_dns_failures(),
            datagrams_in: self.snapshot_datagrams_in(),
            datagrams_out: self.snapshot_datagrams_out(),
            active_flows: self.snapshot_active_flows(),
            flows_dropped_overflow: self.snapshot_flows_dropped_overflow(),
            sni_route_exact_total: self.sni_route_exact_total.load(Ordering::Relaxed),
            sni_route_wildcard_total: self.sni_route_wildcard_total.load(Ordering::Relaxed),
            sni_route_fallback_total: self.sni_route_fallback_total.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_counters_accumulate_and_snapshot() {
        let s = RuleStats::new();
        s.record_in(0, 100);
        s.record_in(0, 50);
        s.record_out(0, 200);
        s.inc_active(0);
        s.inc_active(0);
        s.dec_active(0);
        assert_eq!(s.snapshot(), (150, 200, 1));
        // Aggregate-only construction → no per-port slot.
        assert!(s.snapshot_per_port().is_empty());
    }

    #[test]
    fn per_port_counters_track_independently() {
        let range = PortRange::new(30000, 30002).unwrap();
        let s = RuleStats::for_range(range);
        s.record_in(30000, 100);
        s.record_in(30001, 50);
        s.record_in(30002, 25);
        s.record_out(30000, 1);
        s.inc_active(30001);

        let agg = s.snapshot();
        assert_eq!(agg.0, 175, "aggregate bytes_in = sum of per-port");
        assert_eq!(agg.1, 1, "aggregate bytes_out includes 30000");
        assert_eq!(agg.2, 1);

        let per_port = s.snapshot_per_port();
        assert_eq!(per_port.len(), 3);
        assert_eq!(per_port[0], (30000, 100, 1, 0));
        assert_eq!(per_port[1], (30001, 50, 0, 1));
        assert_eq!(per_port[2], (30002, 25, 0, 0));
    }

    #[test]
    fn record_on_unknown_port_updates_aggregate_only() {
        let range = PortRange::new(30000, 30001).unwrap();
        let s = RuleStats::for_range(range);
        // Port 99 isn't in the range — aggregate still ticks; no per-port
        // entry is created on the fly.
        s.record_in(99, 7);
        let agg = s.snapshot();
        assert_eq!(agg.0, 7);
        let per_port = s.snapshot_per_port();
        assert_eq!(per_port.len(), 2);
        assert_eq!(per_port[0].1, 0);
        assert_eq!(per_port[1].1, 0);
    }

    #[test]
    fn connections_total_starts_at_zero_and_increments() {
        let s = RuleStats::new();
        assert_eq!(s.connections_total.load(Ordering::Relaxed), 0);
        s.inc_connection();
        s.inc_connection();
        assert_eq!(s.connections_total.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn snapshot_basic_zeroed_for_fresh_stats() {
        use portunus_core::PortRange;
        let stats = RuleStats::for_range(PortRange::single(8080));
        let snap = stats.snapshot_basic();
        assert_eq!(snap.bytes_in, 0);
        assert_eq!(snap.bytes_out, 0);
        assert_eq!(snap.active_connections, 0);
        assert_eq!(snap.dns_failures, 0);
        assert_eq!(snap.datagrams_in, 0);
        assert_eq!(
            snap.per_port.len(),
            1,
            "single-port rule still allocates one per-port slot"
        );
        assert_eq!(snap.per_port[0].listen_port, 8080);
    }

    #[test]
    fn error_counters_default_zero_and_bump() {
        let s = RuleStats::new();
        assert_eq!(s.errors.port_in_use.load(Ordering::Relaxed), 0);
        assert_eq!(s.errors.upstream_connect_failed.load(Ordering::Relaxed), 0);
        s.errors.inc_port_in_use();
        s.errors.inc_upstream_connect_failed();
        s.errors.inc_upstream_connect_failed();
        assert_eq!(s.errors.port_in_use.load(Ordering::Relaxed), 1);
        assert_eq!(s.errors.upstream_connect_failed.load(Ordering::Relaxed), 2);
    }
}
