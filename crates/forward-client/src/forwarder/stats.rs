//! Per-rule traffic counters.
//!
//! `RuleStats` is shared between the per-rule listener (which spawns proxies
//! and increments `active_connections`) and the periodic `StatsReport`
//! sender in `control.rs`. Counters are monotonic cumulative — the server
//! computes deltas for Prometheus.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[derive(Debug, Default)]
pub struct RuleStats {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub active_connections: AtomicU32,
}

impl RuleStats {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn add_in(&self, n: u64) {
        self.bytes_in.fetch_add(n, Ordering::Relaxed);
    }

    pub fn add_out(&self, n: u64) {
        self.bytes_out.fetch_add(n, Ordering::Relaxed);
    }

    pub fn inc_active(&self) {
        self.active_connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_active(&self) {
        self.active_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// Snapshot for serialization — `(bytes_in, bytes_out, active_connections)`.
    #[must_use]
    pub fn snapshot(&self) -> (u64, u64, u32) {
        (
            self.bytes_in.load(Ordering::Relaxed),
            self.bytes_out.load(Ordering::Relaxed),
            self.active_connections.load(Ordering::Relaxed),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_and_snapshot() {
        let s = RuleStats::new();
        s.add_in(100);
        s.add_in(50);
        s.add_out(200);
        s.inc_active();
        s.inc_active();
        s.dec_active();
        assert_eq!(s.snapshot(), (150, 200, 1));
    }
}
