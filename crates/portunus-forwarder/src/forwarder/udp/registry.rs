//! Per-rule UDP flow registry. Replaces v0.4 `UdpFlowTable` which was
//! per-listener and silently inflated `udp_max_flows_per_rule` by
//! `range_size`. Spec: 014-udp-centralized-demux, FR-002 / FR-003 /
//! FR-014.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tokio::sync::Mutex;

use crate::forwarder::udp::flow::UdpFlow;

/// `(listen_port, src)` keying is canonical: a single client source
/// addressing two ports of the same range rule resolves to two
/// independent flows (FR-002).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub struct FlowKey {
    pub listen_port: u16,
    pub src: SocketAddr,
}

impl FlowKey {
    #[must_use]
    pub fn new(listen_port: u16, src: SocketAddr) -> Self {
        Self { listen_port, src }
    }
}

/// `Slot::Pending` guards a reservation between try_reserve and commit;
/// `Slot::Live` is a fully constructed flow.
enum Slot {
    Pending,
    Live(Arc<UdpFlow>),
}

pub struct UdpFlowRegistry {
    inner: Mutex<HashMap<FlowKey, Slot>>,
    /// Rule-wide cap. Note: counts BOTH Pending and Live entries.
    cap: usize,
    /// Cumulative count of new-flow first-datagrams refused due to
    /// cap exhaustion (FR-003).
    dropped_overflow: AtomicU64,
    /// Total slot count (Pending + Live). Used by `try_reserve`'s cap
    /// check without holding the inner lock.
    occupancy: AtomicUsize,
}

/// RAII guard: dropping without `commit` removes the `Slot::Pending`
/// entry and decrements occupancy. `commit` consumes the guard.
pub struct Reservation {
    key: FlowKey,
    // Held as `Arc` so the registry stays alive long enough for the Drop
    // cleanup task. The registry itself is `Arc`-shared across listener /
    // demux / reaper, so retaining one more strong ref here is cheap and
    // removes any risk of an orphan reservation outliving its owner.
    registry: Arc<UdpFlowRegistry>,
    committed: bool,
}

impl UdpFlowRegistry {
    #[must_use]
    pub fn new(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            cap,
            dropped_overflow: AtomicU64::new(0),
            occupancy: AtomicUsize::new(0),
        })
    }

    #[must_use]
    pub fn cap(&self) -> usize {
        self.cap
    }

    pub fn dropped_overflow(&self) -> u64 {
        self.dropped_overflow.load(Ordering::Relaxed)
    }

    /// Snapshot of registry size as observed via the `occupancy` atomic.
    ///
    /// This is the FR-014 `active_flows` data source. It reads the atomic
    /// without acquiring `inner.lock()`, so under heavy concurrent commit /
    /// remove / drain churn the value may briefly differ from
    /// `inner.lock().await.len()` by ±a few entries. For the stats-pump
    /// reporting use case this is acceptable — the gauge is sampled
    /// periodically, not transactionally.
    pub fn len(&self) -> usize {
        self.occupancy.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// O(1) fast-path: returns an existing Live flow if present.
    pub async fn get(self: &Arc<Self>, key: FlowKey) -> Option<Arc<UdpFlow>> {
        let guard = self.inner.lock().await;
        match guard.get(&key) {
            Some(Slot::Live(arc)) => Some(Arc::clone(arc)),
            _ => None,
        }
    }

    /// Reserve a slot. Returns:
    ///  - `TryGetOrReserve::Existing(existing)` if a Live flow already exists.
    ///  - `TryGetOrReserve::Reserved(Reservation)` if a new Pending slot
    ///    was created.
    ///  - `TryGetOrReserve::CapExhausted` if cap is exhausted (caller MUST
    ///    silent-drop and bump `dropped_overflow`).
    pub async fn try_get_or_reserve(self: &Arc<Self>, key: FlowKey) -> TryGetOrReserve {
        let mut guard = self.inner.lock().await;
        if let Some(slot) = guard.get(&key) {
            if let Slot::Live(arc) = slot {
                return TryGetOrReserve::Existing(Arc::clone(arc));
            }
            // Pending: another listener for the same key is mid-cold-path.
            // This is rare (same listener serializes; cross-listener uses
            // a different listen_port, hence different key). Treat as
            // "cap exhausted for this key" to avoid double-reserve.
            self.dropped_overflow.fetch_add(1, Ordering::Relaxed);
            return TryGetOrReserve::CapExhausted;
        }
        // Cap check: count Pending+Live.
        if self.occupancy.load(Ordering::Relaxed) >= self.cap {
            self.dropped_overflow.fetch_add(1, Ordering::Relaxed);
            return TryGetOrReserve::CapExhausted;
        }
        guard.insert(key, Slot::Pending);
        self.occupancy.fetch_add(1, Ordering::Relaxed);
        TryGetOrReserve::Reserved(Reservation {
            key,
            registry: Arc::clone(self),
            committed: false,
        })
    }

    /// Atomically convert a `Slot::Pending` to `Slot::Live`.
    /// Consumes the `Reservation` guard.
    pub async fn commit(self: &Arc<Self>, mut reservation: Reservation, flow: Arc<UdpFlow>) {
        let mut guard = self.inner.lock().await;
        match guard.insert(reservation.key, Slot::Live(flow)) {
            Some(Slot::Pending) => {
                // Normal path: Pending -> Live, occupancy unchanged.
            }
            None => {
                // Drained between reserve and commit; restore occupancy.
                self.occupancy.fetch_add(1, Ordering::Relaxed);
            }
            Some(Slot::Live(_old)) => {
                // Should not happen: another path committed a Live slot
                // for this key concurrently. Keep newest, count once.
                // (occupancy unchanged — old +1 → still +1)
                tracing::warn!(event = "rule.udp_registry_commit_overwrote_live");
            }
        }
        reservation.committed = true;
    }

    /// Remove a flow by key. Returns the Arc if present and Live.
    /// Decrements occupancy whether the slot was Pending or Live.
    pub async fn remove(self: &Arc<Self>, key: FlowKey) -> Option<Arc<UdpFlow>> {
        let mut guard = self.inner.lock().await;
        match guard.remove(&key) {
            Some(Slot::Live(arc)) => {
                self.occupancy.fetch_sub(1, Ordering::Relaxed);
                Some(arc)
            }
            Some(Slot::Pending) => {
                self.occupancy.fetch_sub(1, Ordering::Relaxed);
                None
            }
            None => None,
        }
    }

    /// Drain: remove every entry and fire flow cancel tokens.
    /// Used in supervisor shutdown step (c).
    pub async fn drain(self: &Arc<Self>) {
        let mut guard = self.inner.lock().await;
        for (_key, slot) in guard.drain() {
            if let Slot::Live(arc) = slot {
                arc.cancel.cancel();
            }
            self.occupancy.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Snapshot live flows (Live only, skips Pending) for reaper sweep.
    pub async fn snapshot_live(self: &Arc<Self>) -> Vec<(FlowKey, Arc<UdpFlow>)> {
        let guard = self.inner.lock().await;
        guard
            .iter()
            .filter_map(|(k, s)| match s {
                Slot::Live(a) => Some((*k, Arc::clone(a))),
                Slot::Pending => None,
            })
            .collect()
    }
}

pub enum TryGetOrReserve {
    Existing(Arc<UdpFlow>),
    Reserved(Reservation),
    CapExhausted,
}

impl Reservation {
    #[must_use]
    pub fn key(&self) -> FlowKey {
        self.key
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.committed {
            // Spawn a brief async task to remove the Pending slot. We
            // can't await in Drop, so use tokio::spawn — the registry
            // outlives the spawned task because callers hold the Arc.
            let key = self.key;
            let registry = Arc::clone(&self.registry);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let mut guard = registry.inner.lock().await;
                    if let Some(Slot::Pending) = guard.get(&key) {
                        guard.remove(&key);
                        registry.occupancy.fetch_sub(1, Ordering::Relaxed);
                    }
                });
            } else {
                // No runtime — log and accept the leaked Pending slot. Only
                // happens during pathological shutdown ordering.
                tracing::warn!(event = "rule.udp_registry_drop_outside_runtime");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn key(port: u16, ip_last_octet: u8, src_port: u16) -> FlowKey {
        FlowKey::new(
            port,
            SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(127, 0, 0, ip_last_octet)),
                src_port,
            ),
        )
    }

    async fn flow_for(src: SocketAddr) -> Arc<UdpFlow> {
        UdpFlow::for_test(src).await
    }

    #[tokio::test]
    async fn reserve_then_commit_makes_flow_live() {
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let TryGetOrReserve::Reserved(res) = reg.try_get_or_reserve(k).await else {
            panic!("expected reservation")
        };
        assert_eq!(reg.len(), 1, "Pending counts toward occupancy");
        let f = flow_for(k.src).await;
        reg.commit(res, Arc::clone(&f)).await;
        assert_eq!(reg.len(), 1);
        let got = reg.get(k).await.expect("should be live");
        assert!(Arc::ptr_eq(&got, &f));
    }

    #[tokio::test]
    async fn cap_exhaustion_returns_cap_exhausted_and_bumps_counter() {
        let reg = UdpFlowRegistry::new(2);
        let TryGetOrReserve::Reserved(r1) = reg.try_get_or_reserve(key(8000, 1, 1)).await else {
            panic!()
        };
        let TryGetOrReserve::Reserved(r2) = reg.try_get_or_reserve(key(8000, 2, 1)).await else {
            panic!()
        };
        let r3 = reg.try_get_or_reserve(key(8000, 3, 1)).await;
        assert!(matches!(r3, TryGetOrReserve::CapExhausted));
        assert_eq!(reg.dropped_overflow(), 1);
        drop(r1);
        drop(r2);
    }

    #[tokio::test]
    async fn drop_uncommitted_reservation_releases_slot() {
        let reg = UdpFlowRegistry::new(1);
        {
            let TryGetOrReserve::Reserved(r) = reg.try_get_or_reserve(key(8000, 1, 1)).await
            else {
                panic!()
            };
            assert_eq!(reg.len(), 1);
            drop(r); // RAII releases.
        }
        // Reservation::drop spawns an async task; yield a few times.
        for _ in 0..10 {
            tokio::task::yield_now().await;
            if reg.is_empty() {
                return;
            }
        }
        panic!("Reservation drop did not release slot");
    }

    #[tokio::test]
    async fn concurrent_reserve_same_key_second_caller_sees_cap_exhausted() {
        // Listener loop is logically single-threaded per port, but the
        // registry API must remain sound under concurrent calls.
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let reg1 = Arc::clone(&reg);
        let reg2 = Arc::clone(&reg);
        let h1 = tokio::spawn(async move { reg1.try_get_or_reserve(k).await });
        // Wait for h1 to take the Pending slot, then race h2.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let h2 = tokio::spawn(async move { reg2.try_get_or_reserve(k).await });
        let r1 = h1.await.unwrap();
        let r2 = h2.await.unwrap();
        match (r1, r2) {
            (TryGetOrReserve::Reserved(_), TryGetOrReserve::CapExhausted) => {}
            _ => panic!("expected first Reserved, second CapExhausted"),
        }
    }

    #[tokio::test]
    async fn remove_live_returns_arc_and_decrements_len() {
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let TryGetOrReserve::Reserved(res) = reg.try_get_or_reserve(k).await else {
            panic!()
        };
        let f = flow_for(k.src).await;
        reg.commit(res, Arc::clone(&f)).await;
        let removed = reg.remove(k).await.expect("live entry");
        assert!(Arc::ptr_eq(&removed, &f));
        assert_eq!(reg.len(), 0);
        assert!(reg.get(k).await.is_none());
    }

    #[tokio::test]
    async fn snapshot_live_excludes_pending() {
        let reg = UdpFlowRegistry::new(4);
        let _res_pending = reg.try_get_or_reserve(key(8000, 1, 1)).await;
        let TryGetOrReserve::Reserved(res_live) =
            reg.try_get_or_reserve(key(8001, 2, 1)).await
        else {
            panic!()
        };
        let f = flow_for(key(8001, 2, 1).src).await;
        reg.commit(res_live, Arc::clone(&f)).await;
        let snap = reg.snapshot_live().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].0.listen_port, 8001);
    }

    #[tokio::test]
    async fn commit_after_drain_restores_occupancy() {
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let TryGetOrReserve::Reserved(res) = reg.try_get_or_reserve(k).await else {
            panic!("expected reservation")
        };
        // Drain while we hold the reservation; this removes the Pending
        // slot and decrements occupancy to 0.
        reg.drain().await;
        assert_eq!(reg.len(), 0);
        // Now commit — slot is missing, occupancy should be restored.
        let f = flow_for(k.src).await;
        reg.commit(res, Arc::clone(&f)).await;
        assert_eq!(reg.len(), 1);
        let live = reg.get(k).await.expect("commit should re-insert");
        assert!(Arc::ptr_eq(&live, &f));
    }

    #[tokio::test]
    async fn drain_empties_registry_and_cancels_flows() {
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let TryGetOrReserve::Reserved(res) = reg.try_get_or_reserve(k).await else {
            panic!()
        };
        let f = flow_for(k.src).await;
        reg.commit(res, Arc::clone(&f)).await;
        assert!(!f.cancel.is_cancelled());
        reg.drain().await;
        assert_eq!(reg.len(), 0);
        assert!(f.cancel.is_cancelled());
    }
}
