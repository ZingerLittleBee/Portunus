//! Per-rule UDP flow table.
//!
//! Spec: 004-udp-forward, `data-model.md` § UdpFlowTable. The table is
//! keyed on the end-user `(addr, port)` so the listener can route a
//! datagram either through an existing flow's upstream socket or by
//! constructing a new flow on miss.
//!
//! Concurrency model: `tokio::sync::Mutex<HashMap<...>>`. The mutex is
//! held only across the lookup-or-insert and around `len()` /
//! `evict()`; the per-flow upstream socket and reply pump live outside
//! the lock so the hot path (a packet from an existing source) doesn't
//! contend on it. Holding `tokio::Mutex` (not `parking_lot`) lets
//! callers `await` arbitrary work while building a new flow — useful
//! for US2's `resolve_target` call which crosses an `.await` point.
//!
//! Bound on growth: `cap` (typically `udp_max_flows_per_rule` from
//! `Welcome`). New-flow first-datagrams that arrive when the table is
//! at cap are dropped and the per-rule `flows_dropped_overflow`
//! counter is incremented (FR-014). Existing flows are NOT evicted
//! to make room — that would let a noisy source displace a legitimate
//! long-lived flow. The idle reaper (US4 / T060) is what shrinks the
//! table over time; this enforces a hard upper bound between sweeps.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use portunus_core::RuleId;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::flow::UdpFlow;

/// Outcome returned when the table is at cap. Carries the source
/// address for diagnostic logs at the listener layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverflowDropped {
    pub source: SocketAddr,
}

#[derive(Debug)]
pub struct UdpFlowTable {
    inner: Mutex<HashMap<SocketAddr, Arc<UdpFlow>>>,
    cap: usize,
    /// Cumulative count of new-flow first-datagrams the table refused
    /// because it was at cap. Mirrors the per-rule
    /// `flows_dropped_overflow` Prometheus counter.
    pub dropped_overflow: AtomicU64,
}

impl UdpFlowTable {
    #[must_use]
    pub fn new(cap: usize) -> Self {
        // A cap of 0 would deadlock the listener (every first-packet
        // overflows). Caller (`Welcome` plumbing) is expected to have
        // validated to >= 1; we accept it here without panicking but
        // saturate to 1 so the listener doesn't cause an outage if a
        // bad Welcome ever lands.
        let cap = cap.max(1);
        Self {
            inner: Mutex::new(HashMap::new()),
            cap,
            dropped_overflow: AtomicU64::new(0),
        }
    }

    /// Look up an existing flow or build a new one via `build`. The
    /// closure runs **under the lock** only when the entry is absent —
    /// callers must keep `build` cheap and infallible after upstream
    /// resolution has succeeded. (For US2, resolution happens
    /// **outside** this call and is passed in via `build` as the
    /// already-resolved upstream addresses.)
    ///
    /// On success returns the live `Arc<UdpFlow>` (existing or fresh).
    /// On overflow returns `OverflowDropped { source }` and bumps
    /// `dropped_overflow`.
    pub async fn lookup_or_insert<F>(
        &self,
        source: SocketAddr,
        build: F,
    ) -> Result<Arc<UdpFlow>, OverflowDropped>
    where
        F: FnOnce() -> Arc<UdpFlow>,
    {
        let mut guard = self.inner.lock().await;
        if let Some(existing) = guard.get(&source) {
            return Ok(Arc::clone(existing));
        }
        if guard.len() >= self.cap {
            self.dropped_overflow.fetch_add(1, Ordering::Relaxed);
            return Err(OverflowDropped { source });
        }
        let flow = build();
        guard.insert(source, Arc::clone(&flow));
        Ok(flow)
    }

    /// Read-only existence check. Returns the live `Arc<UdpFlow>` if
    /// `source` is in the table, else `None`. Used by the listener's
    /// fast path to avoid touching the slow path (upstream socket
    /// bind) for already-established flows.
    pub async fn get(&self, source: SocketAddr) -> Option<Arc<UdpFlow>> {
        self.inner.lock().await.get(&source).map(Arc::clone)
    }

    /// Cancel and remove the flow for `source` (no-op if absent).
    /// Returns true if a flow was actually evicted. Used by the idle
    /// reaper in US4; in US1 it's only exercised by tests.
    #[allow(dead_code)]
    pub async fn evict(&self, source: SocketAddr) -> bool {
        let mut guard = self.inner.lock().await;
        if let Some(flow) = guard.remove(&source) {
            flow.cancel.cancel();
            true
        } else {
            false
        }
    }

    /// Cancel every flow and drop the map. Called by the listener
    /// during teardown (rule remove or shutdown drain).
    pub async fn drain(&self) {
        let mut guard = self.inner.lock().await;
        for (_source, flow) in guard.drain() {
            flow.cancel.cancel();
        }
    }

    /// Live entry count. Snapshotted onto the rule-level
    /// `active_flows` gauge each StatsReport tick.
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }

    #[allow(dead_code)]
    pub async fn is_empty(&self) -> bool {
        self.inner.lock().await.is_empty()
    }

    /// Cumulative count of overflow drops since construction.
    #[allow(dead_code)]
    pub fn dropped_overflow(&self) -> u64 {
        self.dropped_overflow.load(Ordering::Relaxed)
    }

    /// 004-udp-forward T060: evict every flow whose `last_seen` falls
    /// outside the `idle_window`. Each evicted flow's
    /// `CancellationToken` is fired so its reply-pump task tears down
    /// promptly. Returns the number of evicted flows (used by the
    /// reaper for log volume + by tests to assert behaviour).
    ///
    /// R-002 invariant: the table mutex is held only across the
    /// snapshot grab and the actual `remove()` calls — never across an
    /// `await` that could block on per-flow state. Per-flow
    /// `last_seen_at()` reads happen on snapshot copies, between the
    /// two short locked sections.
    pub async fn sweep(&self, idle_window: Duration, rule_id: RuleId) -> usize {
        // Step 1: snapshot (addr, Arc<UdpFlow>) pairs under the lock.
        let snapshot: Vec<(SocketAddr, Arc<UdpFlow>)> = {
            let guard = self.inner.lock().await;
            guard.iter().map(|(a, f)| (*a, Arc::clone(f))).collect()
        };
        let now = Instant::now();
        let mut stale: Vec<(SocketAddr, Duration)> = Vec::new();
        for (addr, flow) in &snapshot {
            let last = flow.last_seen_at().await;
            let age = now.checked_duration_since(last).unwrap_or_default();
            if age >= idle_window {
                stale.push((*addr, age));
            }
        }
        if stale.is_empty() {
            return 0;
        }
        // Step 2: re-lock briefly to remove the stale entries.
        let mut evicted: Vec<(Arc<UdpFlow>, Duration)> = Vec::with_capacity(stale.len());
        {
            let mut guard = self.inner.lock().await;
            for (addr, age) in &stale {
                if let Some(flow) = guard.remove(addr) {
                    evicted.push((flow, *age));
                }
            }
        }
        let count = evicted.len();
        // Step 3: cancel each evicted flow's pump task + log outside the lock.
        for (flow, age) in evicted {
            flow.cancel.cancel();
            debug!(
                event = "rule.udp_flow_evicted",
                rule_id = %rule_id,
                source = %flow.source_addr,
                flow_age_ms = u64::try_from(age.as_millis()).unwrap_or(u64::MAX),
            );
        }
        count
    }

    /// 004-udp-forward T060: spawn a background reaper task that sweeps
    /// stale flows every `idle_window / 4` (clamped to a 25 ms floor so
    /// test-only sub-second windows still tick promptly). The reaper
    /// task watches `cancel` so rule removal / shutdown tears it down
    /// alongside the listener. Idempotent for `idle_window == 0` —
    /// calls become no-ops, which is the documented "disable reaper"
    /// shape (used by tests that drive `sweep` manually).
    pub fn spawn_reaper(
        self: &Arc<Self>,
        idle_window: Duration,
        rule_id: RuleId,
        cancel: CancellationToken,
    ) {
        if idle_window.is_zero() {
            return;
        }
        let table = Arc::clone(self);
        let interval = std::cmp::max(idle_window / 4, Duration::from_millis(25));
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    () = tokio::time::sleep(interval) => {
                        table.sweep(idle_window, rule_id).await;
                    }
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::net::UdpSocket;

    async fn make_socket() -> Arc<UdpSocket> {
        Arc::new(
            UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind upstream"),
        )
    }

    fn upstream(p: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], p))
    }

    fn source(p: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], p))
    }

    /// T022: cap is honoured; the third insertion fails fast with
    /// OverflowDropped and `dropped_overflow` advances to 1. Existing
    /// entries remain valid.
    #[tokio::test]
    async fn cap_blocks_third_insert_and_bumps_counter() {
        let table = UdpFlowTable::new(2);
        let s = make_socket().await;
        let f1 = table
            .lookup_or_insert(source(50000), || {
                UdpFlow::new(source(50000), Arc::clone(&s), vec![upstream(9000)])
            })
            .await
            .expect("first insert");
        let f2 = table
            .lookup_or_insert(source(50001), || {
                UdpFlow::new(source(50001), Arc::clone(&s), vec![upstream(9000)])
            })
            .await
            .expect("second insert");
        assert_eq!(table.len().await, 2);

        let err = table
            .lookup_or_insert(source(50002), || {
                UdpFlow::new(source(50002), Arc::clone(&s), vec![upstream(9000)])
            })
            .await
            .expect_err("third insert MUST overflow at cap");
        assert_eq!(err.source, source(50002));
        assert_eq!(table.dropped_overflow(), 1);
        assert_eq!(table.len().await, 2);

        // Existing flows survive.
        assert_eq!(f1.source_addr, source(50000));
        assert_eq!(f2.source_addr, source(50001));
    }

    /// T023: lookup_or_insert returns the same Arc across calls for
    /// the same source; distinct sources get distinct Arcs.
    #[tokio::test]
    async fn lookup_or_insert_dedupes_by_source() {
        let table = UdpFlowTable::new(8);
        let s = make_socket().await;
        let first = table
            .lookup_or_insert(source(50000), || {
                UdpFlow::new(source(50000), Arc::clone(&s), vec![upstream(9000)])
            })
            .await
            .expect("first");
        let again = table
            .lookup_or_insert(source(50000), || {
                panic!("build closure must NOT run for an existing source");
            })
            .await
            .expect("hit");
        assert!(Arc::ptr_eq(&first, &again));

        let other = table
            .lookup_or_insert(source(50001), || {
                UdpFlow::new(source(50001), Arc::clone(&s), vec![upstream(9000)])
            })
            .await
            .expect("distinct source");
        assert!(!Arc::ptr_eq(&first, &other));
        assert_eq!(table.len().await, 2);
    }

    #[tokio::test]
    async fn evict_cancels_and_removes() {
        let table = UdpFlowTable::new(4);
        let s = make_socket().await;
        let flow = table
            .lookup_or_insert(source(50000), || {
                UdpFlow::new(source(50000), Arc::clone(&s), vec![upstream(9000)])
            })
            .await
            .unwrap();
        assert!(table.evict(source(50000)).await);
        assert!(flow.cancel.is_cancelled());
        assert!(table.is_empty().await);
        // Evicting a missing key is a no-op.
        assert!(!table.evict(source(50000)).await);
    }

    /// T056 (US4): the reaper algorithm — drive `sweep()` directly with
    /// a 100ms idle_window. `last_seen` is keyed off `std::time::Instant`
    /// (monotonic, NOT mockable via tokio's paused clock), so we sleep
    /// real wall-clock time to age the flows. Test budget is sub-second.
    #[tokio::test]
    async fn sweep_evicts_idle_flows_and_keeps_fresh_ones() {
        let table = Arc::new(UdpFlowTable::new(8));
        let s = make_socket().await;
        for p in 50000..50003 {
            table
                .lookup_or_insert(source(p), || {
                    UdpFlow::new(source(p), Arc::clone(&s), vec![upstream(9000)])
                })
                .await
                .unwrap();
        }
        assert_eq!(table.len().await, 3);

        // Sleep past the idle_window — every flow MUST be reaped.
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let evicted = table
            .sweep(std::time::Duration::from_millis(100), RuleId(99))
            .await;
        assert_eq!(evicted, 3, "all 3 idle flows must be evicted");
        assert!(table.is_empty().await);

        // Insert a fresh flow, sleep only 30ms, sweep — fresh flow
        // MUST survive (not idle long enough yet).
        let flow = table
            .lookup_or_insert(source(50100), || {
                UdpFlow::new(source(50100), Arc::clone(&s), vec![upstream(9000)])
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        let evicted = table
            .sweep(std::time::Duration::from_millis(100), RuleId(99))
            .await;
        assert_eq!(evicted, 0, "fresh flow MUST NOT be evicted at 30ms idle");
        assert_eq!(table.len().await, 1);
        assert!(!flow.cancel.is_cancelled());
    }

    /// T056 sister: `spawn_reaper` actually fires sweeps in the
    /// background. Uses real wall-clock sleeps to drive the
    /// `std::time::Instant`-backed last_seen check.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spawn_reaper_evicts_idle_flows_in_background() {
        let table = Arc::new(UdpFlowTable::new(8));
        let s = make_socket().await;
        for p in 50000..50002 {
            table
                .lookup_or_insert(source(p), || {
                    UdpFlow::new(source(p), Arc::clone(&s), vec![upstream(9000)])
                })
                .await
                .unwrap();
        }
        let cancel = CancellationToken::new();
        table.spawn_reaper(
            std::time::Duration::from_millis(100),
            RuleId(7),
            cancel.clone(),
        );

        // The reaper sleeps idle_window/4 (=25ms, floor) between sweeps.
        // After idle_window + 2 sweeps the table MUST be empty.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert!(
            table.is_empty().await,
            "background reaper MUST evict idle flows within 250ms"
        );
        cancel.cancel();
    }

    #[tokio::test]
    async fn drain_cancels_everything() {
        let table = UdpFlowTable::new(4);
        let s = make_socket().await;
        let f1 = table
            .lookup_or_insert(source(50000), || {
                UdpFlow::new(source(50000), Arc::clone(&s), vec![upstream(9000)])
            })
            .await
            .unwrap();
        let f2 = table
            .lookup_or_insert(source(50001), || {
                UdpFlow::new(source(50001), Arc::clone(&s), vec![upstream(9000)])
            })
            .await
            .unwrap();
        table.drain().await;
        assert!(table.is_empty().await);
        assert!(f1.cancel.is_cancelled());
        assert!(f2.cancel.is_cancelled());
    }
}
