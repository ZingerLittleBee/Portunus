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

use forward_core::PortRange;

#[derive(Debug, Default)]
pub struct PerPortCounters {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub active_connections: AtomicU32,
}

#[derive(Debug, Default)]
pub struct RuleStats {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub active_connections: AtomicU32,
    /// Per-port counters keyed on the listen-side port. Populated at
    /// construction by [`RuleStats::for_range`]; empty when constructed
    /// via [`RuleStats::new`]. Lookup misses are silent — the aggregate
    /// is always updated regardless.
    pub per_port: BTreeMap<u16, PerPortCounters>,
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
            per_port,
        })
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
    /// constructions (`RuleStats::new`).
    #[must_use]
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
}
