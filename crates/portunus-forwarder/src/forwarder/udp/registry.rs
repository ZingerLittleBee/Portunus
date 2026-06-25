//! Per-rule UDP flow registry. Replaces v0.4 `UdpFlowTable` which was
//! per-listener and silently inflated `udp_max_flows_per_rule` by
//! `range_size`. Spec: 014-udp-centralized-demux, FR-002 / FR-003 /
//! FR-014.
//!
//! Storage is a `DashMap` (sharded lockless reads + per-shard write
//! locks) rather than `Mutex<HashMap>` — the hot-path `get(key)` is
//! called once per UDP datagram and the original `tokio::sync::Mutex`
//! forced a scheduler dispatch on every packet even though the lock
//! was uncontended in steady state.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use dashmap::DashMap;
use dashmap::mapref::entry::Entry;

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
    inner: DashMap<FlowKey, Slot>,
    /// Rule-wide cap. Note: counts BOTH Pending and Live entries.
    cap: usize,
    /// Cumulative count of new-flow first-datagrams refused due to
    /// cap exhaustion (FR-003).
    dropped_overflow: AtomicU64,
    /// Total slot count (Pending + Live). Updated alongside `inner`
    /// mutations; the strict cap enforcement uses `fetch_add` + roll-back
    /// to avoid over-shoot under concurrent inserts across shards.
    occupancy: AtomicUsize,
}

/// RAII guard: dropping without `commit` removes the `Slot::Pending`
/// entry and decrements occupancy. `commit` consumes the guard.
pub struct Reservation {
    key: FlowKey,
    // Held as `Arc` so the registry stays alive long enough for Drop
    // cleanup. The registry itself is `Arc`-shared across listener /
    // demux / reaper, so retaining one more strong ref here is cheap.
    registry: Arc<UdpFlowRegistry>,
    committed: bool,
}

impl UdpFlowRegistry {
    #[must_use]
    pub fn new(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: DashMap::new(),
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
    /// This is the FR-014 `active_flows` data source. The atomic is
    /// updated alongside every insert/remove but is not transactionally
    /// consistent with `inner.len()` — under heavy churn the two may
    /// briefly differ. For the stats-pump reporting use case this is
    /// acceptable.
    #[must_use]
    pub fn len(&self) -> usize {
        self.occupancy.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// O(1) fast-path: returns an existing Live flow if present. Pure
    /// sync (no scheduler dispatch); reads go through DashMap's
    /// lockless reader path on the shard.
    #[must_use]
    pub fn get(self: &Arc<Self>, key: FlowKey) -> Option<Arc<UdpFlow>> {
        let slot = self.inner.get(&key)?;
        match slot.value() {
            Slot::Live(arc) => Some(Arc::clone(arc)),
            Slot::Pending => None,
        }
    }

    /// Reserve a slot. Returns:
    ///  - `TryGetOrReserve::Existing(existing)` if a Live flow already exists.
    ///  - `TryGetOrReserve::Reserved(Reservation)` if a new Pending slot
    ///    was created.
    ///  - `TryGetOrReserve::CapExhausted` if cap is exhausted (caller MUST
    ///    silent-drop and bump `dropped_overflow`).
    ///
    /// Cap enforcement uses an `fetch_add` + roll-back pattern so the
    /// rule-wide counter is never over-shot even when concurrent inserts
    /// land on different DashMap shards.
    pub fn try_get_or_reserve(self: &Arc<Self>, key: FlowKey) -> TryGetOrReserve {
        // Fast path: existing Live slot.
        if let Some(slot) = self.inner.get(&key) {
            match slot.value() {
                Slot::Live(arc) => return TryGetOrReserve::Existing(Arc::clone(arc)),
                Slot::Pending => {
                    // Another cold path is mid-flight for this key. Treat
                    // as "no slot available" to avoid double-reserving.
                    self.dropped_overflow.fetch_add(1, Ordering::Relaxed);
                    return TryGetOrReserve::CapExhausted;
                }
            }
        }

        // Reserve a cap slot first, then attempt to insert. If the cap is
        // exceeded, roll back. If a racing inserter beat us to this key,
        // also roll back.
        let new_occ = self.occupancy.fetch_add(1, Ordering::Relaxed) + 1;
        if new_occ > self.cap {
            self.occupancy.fetch_sub(1, Ordering::Relaxed);
            self.dropped_overflow.fetch_add(1, Ordering::Relaxed);
            return TryGetOrReserve::CapExhausted;
        }

        match self.inner.entry(key) {
            Entry::Vacant(vac) => {
                vac.insert(Slot::Pending);
                TryGetOrReserve::Reserved(Reservation {
                    key,
                    registry: Arc::clone(self),
                    committed: false,
                })
            }
            Entry::Occupied(occ) => {
                // Lost the race: someone else inserted between our `get`
                // and `entry`. Roll back the occupancy bump.
                self.occupancy.fetch_sub(1, Ordering::Relaxed);
                match occ.get() {
                    Slot::Live(arc) => TryGetOrReserve::Existing(Arc::clone(arc)),
                    Slot::Pending => {
                        self.dropped_overflow.fetch_add(1, Ordering::Relaxed);
                        TryGetOrReserve::CapExhausted
                    }
                }
            }
        }
    }

    /// Atomically convert a `Slot::Pending` to `Slot::Live`.
    /// Consumes the `Reservation` guard.
    pub fn commit(self: &Arc<Self>, mut reservation: Reservation, flow: Arc<UdpFlow>) {
        match self.inner.entry(reservation.key) {
            Entry::Occupied(mut occ) => {
                // Normal path: Pending -> Live, occupancy unchanged.
                // (Live overwrites also keep occupancy stable.)
                if matches!(occ.get(), Slot::Live(_)) {
                    tracing::warn!(event = "rule.udp_registry_commit_overwrote_live");
                }
                occ.insert(Slot::Live(flow));
            }
            Entry::Vacant(vac) => {
                // Drained between reserve and commit; restore occupancy.
                vac.insert(Slot::Live(flow));
                self.occupancy.fetch_add(1, Ordering::Relaxed);
            }
        }
        reservation.committed = true;
    }

    /// Remove a flow by key. Returns the `Arc<UdpFlow>` if present and
    /// Live. Decrements occupancy whether the slot was Pending or Live.
    pub fn remove(self: &Arc<Self>, key: FlowKey) -> Option<Arc<UdpFlow>> {
        match self.inner.remove(&key) {
            Some((_, Slot::Live(arc))) => {
                self.occupancy.fetch_sub(1, Ordering::Relaxed);
                Some(arc)
            }
            Some((_, Slot::Pending)) => {
                self.occupancy.fetch_sub(1, Ordering::Relaxed);
                None
            }
            None => None,
        }
    }

    /// Drain: remove every entry and fire flow cancel tokens.
    /// Used in supervisor shutdown step (c).
    pub fn drain(self: &Arc<Self>) {
        // Collect keys first so we don't deadlock by mutating the map
        // while iterating it.
        let keys: Vec<FlowKey> = self.inner.iter().map(|e| *e.key()).collect();
        for k in keys {
            if let Some((_, slot)) = self.inner.remove(&k) {
                if let Slot::Live(arc) = slot {
                    arc.cancel.cancel();
                }
                self.occupancy.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    /// Snapshot live flows (Live only, skips Pending) for reaper sweep.
    pub fn snapshot_live(self: &Arc<Self>) -> Vec<(FlowKey, Arc<UdpFlow>)> {
        self.inner
            .iter()
            .filter_map(|entry| match entry.value() {
                Slot::Live(a) => Some((*entry.key(), Arc::clone(a))),
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
            // Sync remove via DashMap — no async runtime dependency.
            // Only remove if the slot is still Pending (commit may have
            // converted it to Live before this Drop runs in a race).
            if let Some((_, Slot::Pending)) = self
                .registry
                .inner
                .remove_if(&self.key, |_, s| matches!(s, Slot::Pending))
            {
                self.registry.occupancy.fetch_sub(1, Ordering::Relaxed);
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
        let TryGetOrReserve::Reserved(reservation) = reg.try_get_or_reserve(k) else {
            panic!("expected reservation")
        };
        assert_eq!(reg.len(), 1, "Pending counts toward occupancy");
        let f = flow_for(k.src).await;
        reg.commit(reservation, Arc::clone(&f));
        assert_eq!(reg.len(), 1);
        let got = reg.get(k).expect("should be live");
        assert!(Arc::ptr_eq(&got, &f));
    }

    #[tokio::test]
    async fn cap_exhaustion_returns_cap_exhausted_and_bumps_counter() {
        let reg = UdpFlowRegistry::new(2);
        let TryGetOrReserve::Reserved(r1) = reg.try_get_or_reserve(key(8000, 1, 1)) else {
            panic!()
        };
        let TryGetOrReserve::Reserved(r2) = reg.try_get_or_reserve(key(8000, 2, 1)) else {
            panic!()
        };
        let r3 = reg.try_get_or_reserve(key(8000, 3, 1));
        assert!(matches!(r3, TryGetOrReserve::CapExhausted));
        assert_eq!(reg.dropped_overflow(), 1);
        drop(r1);
        drop(r2);
    }

    #[tokio::test]
    async fn drop_uncommitted_reservation_releases_slot() {
        let reg = UdpFlowRegistry::new(1);
        {
            let TryGetOrReserve::Reserved(r) = reg.try_get_or_reserve(key(8000, 1, 1)) else {
                panic!()
            };
            assert_eq!(reg.len(), 1);
            drop(r); // RAII releases.
        }
        // Sync Drop — should see effect immediately.
        assert!(reg.is_empty(), "Reservation drop did not release slot");
    }

    #[tokio::test]
    async fn concurrent_reserve_same_key_second_caller_sees_cap_exhausted() {
        // Listener loop is logically single-threaded per port, but the
        // registry API must remain sound under concurrent calls.
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let reg1 = Arc::clone(&reg);
        let reg2 = Arc::clone(&reg);
        let h1 = tokio::spawn(async move { reg1.try_get_or_reserve(k) });
        // Wait for h1 to take the Pending slot, then race h2.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let h2 = tokio::spawn(async move { reg2.try_get_or_reserve(k) });
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
        let TryGetOrReserve::Reserved(reservation) = reg.try_get_or_reserve(k) else {
            panic!()
        };
        let f = flow_for(k.src).await;
        reg.commit(reservation, Arc::clone(&f));
        let removed = reg.remove(k).expect("live entry");
        assert!(Arc::ptr_eq(&removed, &f));
        assert_eq!(reg.len(), 0);
        assert!(reg.get(k).is_none());
    }

    #[tokio::test]
    async fn snapshot_live_excludes_pending() {
        let reg = UdpFlowRegistry::new(4);
        let _res_pending = reg.try_get_or_reserve(key(8000, 1, 1));
        let TryGetOrReserve::Reserved(res_live) = reg.try_get_or_reserve(key(8001, 2, 1)) else {
            panic!()
        };
        let f = flow_for(key(8001, 2, 1).src).await;
        reg.commit(res_live, Arc::clone(&f));
        let snap = reg.snapshot_live();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].0.listen_port, 8001);
    }

    #[tokio::test]
    async fn commit_after_drain_restores_occupancy() {
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let TryGetOrReserve::Reserved(reservation) = reg.try_get_or_reserve(k) else {
            panic!("expected reservation")
        };
        // Drain while we hold the reservation; this removes the Pending
        // slot and decrements occupancy to 0.
        reg.drain();
        assert_eq!(reg.len(), 0);
        // Now commit — slot is missing, occupancy should be restored.
        let f = flow_for(k.src).await;
        reg.commit(reservation, Arc::clone(&f));
        assert_eq!(reg.len(), 1);
        let live = reg.get(k).expect("commit should re-insert");
        assert!(Arc::ptr_eq(&live, &f));
    }

    #[tokio::test]
    async fn drain_empties_registry_and_cancels_flows() {
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let TryGetOrReserve::Reserved(reservation) = reg.try_get_or_reserve(k) else {
            panic!()
        };
        let f = flow_for(k.src).await;
        reg.commit(reservation, Arc::clone(&f));
        assert!(!f.cancel.is_cancelled());
        reg.drain();
        assert_eq!(reg.len(), 0);
        assert!(f.cancel.is_cancelled());
    }

    #[test]
    fn cap_returns_configured_capacity() {
        // `cap()` is a pure accessor over the rule-wide cap.
        let reg = UdpFlowRegistry::new(7);
        assert_eq!(reg.cap(), 7);
    }

    #[tokio::test]
    async fn get_on_pending_slot_returns_none() {
        // A reserved-but-not-committed slot is Pending; the O(1) Live
        // fast-path `get` must report it as absent (no Live flow yet).
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let TryGetOrReserve::Reserved(_res) = reg.try_get_or_reserve(k) else {
            panic!("expected reservation")
        };
        assert!(
            reg.get(k).is_none(),
            "Pending slot must not surface via get"
        );
        // Occupancy still reflects the held Pending reservation.
        assert_eq!(reg.len(), 1);
    }

    #[tokio::test]
    async fn try_get_or_reserve_returns_existing_for_live_slot() {
        // Once a key is Live, the fast-path arm returns Existing with a
        // clone of the same flow rather than minting a new reservation.
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let TryGetOrReserve::Reserved(reservation) = reg.try_get_or_reserve(k) else {
            panic!("expected reservation")
        };
        let f = flow_for(k.src).await;
        reg.commit(reservation, Arc::clone(&f));
        match reg.try_get_or_reserve(k) {
            TryGetOrReserve::Existing(existing) => {
                assert!(Arc::ptr_eq(&existing, &f), "must hand back the live flow");
            }
            _ => panic!("expected Existing for an already-live key"),
        }
        // No new slot was added and no overflow was recorded.
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.dropped_overflow(), 0);
    }

    #[tokio::test]
    async fn remove_pending_slot_decrements_len_and_returns_none() {
        // Removing a Pending (reserved, uncommitted) slot decrements
        // occupancy but yields no flow.
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        // Reserve, then mark the guard committed so its RAII Drop is a
        // no-op and `remove` is the sole mutator of the Pending slot.
        let TryGetOrReserve::Reserved(mut reservation) = reg.try_get_or_reserve(k) else {
            panic!("expected reservation")
        };
        // A child test module may flip the parent type's private flag.
        reservation.committed = true;
        assert_eq!(reg.len(), 1);
        assert!(
            reg.remove(k).is_none(),
            "removing a Pending slot yields no flow"
        );
        assert_eq!(reg.len(), 0, "Pending removal decrements occupancy");
        drop(reservation);
    }

    #[tokio::test]
    async fn remove_missing_key_returns_none_without_touching_occupancy() {
        // Removing an absent key is a no-op: None, occupancy untouched.
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 9, 1234);
        assert!(reg.remove(k).is_none());
        assert_eq!(reg.len(), 0);
    }

    #[tokio::test]
    async fn commit_overwriting_live_slot_warns_and_replaces_flow() {
        // Driving `commit` when the slot is already Live exercises the
        // overwrite-warning branch. A second Reservation for the same key
        // is built directly (a child test module may touch the parent
        // module's private fields) since the public API never hands out
        // two reservations for one key.
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let TryGetOrReserve::Reserved(reservation) = reg.try_get_or_reserve(k) else {
            panic!("expected reservation")
        };
        let first = flow_for(k.src).await;
        reg.commit(reservation, Arc::clone(&first));
        assert!(Arc::ptr_eq(&reg.get(k).unwrap(), &first));

        // Build a second reservation for the same (now Live) key.
        let second_res = Reservation {
            key: k,
            registry: Arc::clone(&reg),
            committed: false,
        };
        let second = flow_for(k.src).await;
        reg.commit(second_res, Arc::clone(&second));

        // The Live slot was overwritten with the second flow; occupancy
        // is unchanged because the overwrite did not add a slot.
        let live = reg.get(k).expect("still live after overwrite");
        assert!(
            Arc::ptr_eq(&live, &second),
            "overwrite installs second flow"
        );
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn reservation_key_returns_the_keyed_flow_key() {
        // `Reservation::key()` is a plain accessor. Build one directly so
        // the test does not depend on the reserve/commit lifecycle. With
        // `committed: true` the RAII Drop is a no-op.
        let reg = UdpFlowRegistry::new(4);
        let k = key(8123, 4, 4321);
        let reservation = Reservation {
            key: k,
            registry: Arc::clone(&reg),
            committed: true,
        };
        assert_eq!(reservation.key(), k);
    }

    #[tokio::test]
    async fn drain_decrements_occupancy_for_pending_only_slot() {
        // A registry holding only a Pending slot is drained: the Live
        // branch is skipped, but occupancy is still decremented to zero.
        let reg = UdpFlowRegistry::new(4);
        let TryGetOrReserve::Reserved(reservation) = reg.try_get_or_reserve(key(8000, 1, 1)) else {
            panic!("expected reservation")
        };
        // Keep the reservation alive past the drain so its Drop sees the
        // slot already gone (remove_if returns None) and is a no-op.
        assert_eq!(reg.len(), 1);
        reg.drain();
        assert_eq!(
            reg.len(),
            0,
            "draining a Pending-only slot zeroes occupancy"
        );
        // Dropping the now-stale reservation must not underflow occupancy.
        drop(reservation);
        assert_eq!(reg.len(), 0);
    }
}
