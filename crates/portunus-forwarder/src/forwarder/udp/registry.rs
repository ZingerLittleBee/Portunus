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
    // Held weak to avoid keeping registry alive past its owner, but
    // Arc is fine here because the registry itself is `Arc`-shared
    // across listener/demux/reaper.
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

    /// Snapshot of registry size. Used by FR-014 `active_flows` gauge.
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
        // Replace Pending with Live. Use insert; if for some reason the
        // slot is not Pending (e.g. concurrent drain), accept Live insert.
        guard.insert(reservation.key, Slot::Live(flow));
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
            tokio::spawn(async move {
                let mut guard = registry.inner.lock().await;
                if let Some(Slot::Pending) = guard.get(&key) {
                    guard.remove(&key);
                    registry.occupancy.fetch_sub(1, Ordering::Relaxed);
                }
            });
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
