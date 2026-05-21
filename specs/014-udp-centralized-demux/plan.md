# UDP Centralized Reply Demux — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace per-flow UDP reply-pump tokio tasks (each holding a 64 KiB receive buffer) with one centralized demux task per rule, and correct `udp_max_flows_per_rule` semantics from per-port-listener to true per-rule.

**Architecture:** A new `UdpRuleRuntime` owns a per-rule `UdpFlowRegistry` (shared across all listeners of a range rule), a single `ReplyDemuxTask` (multiplexes upstream sockets via `FuturesUnordered<ReadWait>`), one `RuleReaper` task, and N `PerListenerLoop` tasks (one per listen port). A supervisor task with a Running/ShuttingDownIntentional/ShuttingDownAfterFailure state machine owns the `JoinSet` and drives ordered shutdown. Upstream sockets are `bind(0) + connect(target)`.

**Tech Stack:** Rust 2024, tokio 1.x (existing), futures-util 0.3 (NEW direct dep on `portunus-forwarder`; transitive already), nix 0.30 (existing).

**Spec:** `specs/014-udp-centralized-demux/spec.md`

**Out of scope for this plan:**
- Multi-target UDP rules (failover) — they keep their existing path in `forwarder/failover_path.rs::run_udp`. Migrating that is a follow-up.
- `recvmmsg`/`sendmmsg` batching — see spec § Out of Scope.

---

## Pre-flight

### Task 0.1: Confirm branch and clean tree

- [ ] **Step 1: Check current branch**

```bash
git branch --show-current
```
Expected: `014-udp-centralized-demux`

- [ ] **Step 2: Confirm tree is clean**

```bash
git status --short
```
Expected: empty output.

- [ ] **Step 3: Sanity-build current code**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder -p portunus-client 2>&1 | tail -5
```
Expected: `Finished ...` line, no errors. (Tests may take longer; just confirm the build is green before changing anything.)

---

## Phase 1: Add `futures-util` dependency

### Task 1.1: Add `futures-util` to `portunus-forwarder`

**Files:**
- Modify: `crates/portunus-forwarder/Cargo.toml`

- [ ] **Step 1: Read current dependencies block**

```bash
sed -n '/^\[dependencies\]/,/^\[/p' crates/portunus-forwarder/Cargo.toml | head -30
```

- [ ] **Step 2: Add the dependency**

Use `Edit` to insert `futures-util = "0.3"` into the `[dependencies]` section, placed alphabetically near other `futures*` or `f` entries. If no such anchor exists, add at the end of the block, sorted.

- [ ] **Step 3: Verify build**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder 2>&1 | tail -3
```
Expected: `Finished ...`

- [ ] **Step 4: Commit**

```bash
git add crates/portunus-forwarder/Cargo.toml Cargo.lock
git commit -m "deps(014): add futures-util to portunus-forwarder

Needed for FuturesUnordered<ReadWait> in the upcoming per-rule UDP
demux task. Already transitive via tokio-stream/tonic — no new
transitive deps."
```

---

## Phase 2: `UdpFlowRegistry` (Slot/Reservation/registry-wide cap)

### Task 2.1: Create `udp/registry.rs` skeleton + tests-only skeleton

**Files:**
- Create: `crates/portunus-forwarder/src/forwarder/udp/registry.rs`
- Modify: `crates/portunus-forwarder/src/forwarder/udp/mod.rs` (add `pub mod registry;`)

**Goal:** stand up the type signatures so subsequent tasks can write tests against them without compile errors.

- [ ] **Step 1: Add module declaration**

In `crates/portunus-forwarder/src/forwarder/udp/mod.rs`, near other `pub mod` lines (look for `pub mod flow; pub mod table;`), add:

```rust
pub mod registry;
```

- [ ] **Step 2: Write the skeleton file**

```rust
// crates/portunus-forwarder/src/forwarder/udp/registry.rs
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
    pub fn new(listen_port: u16, src: SocketAddr) -> Self {
        Self { listen_port, src }
    }
}

/// Slot::Pending guards a reservation between try_reserve and commit;
/// Slot::Live is a fully constructed flow.
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
    pub fn new(cap: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            cap,
            dropped_overflow: AtomicU64::new(0),
            occupancy: AtomicUsize::new(0),
        })
    }

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
    ///  - `Either::Left(existing)` if a Live flow already exists.
    ///  - `Either::Right(Reservation)` if a new Pending slot was created.
    ///  - `None` if cap is exhausted (caller MUST silent-drop and bump
    ///    `dropped_overflow`).
    pub async fn try_get_or_reserve(
        self: &Arc<Self>,
        key: FlowKey,
    ) -> TryGetOrReserve {
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
    pub async fn commit(
        self: &Arc<Self>,
        mut reservation: Reservation,
        flow: Arc<UdpFlow>,
    ) {
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
                _ => None,
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
    pub fn key(&self) -> FlowKey {
        self.key
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.committed {
            // Spawn a brief async task to remove the Pending slot. We
            // can't await in Drop, so use blocking_lock if possible or
            // tokio::spawn. The simplest correct path: tokio::spawn.
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
    // tests added in Task 2.2-2.5
}
```

- [ ] **Step 3: Note on the `UdpFlow::cancel` field**

The skeleton uses `flow.cancel.cancel()` in `drain()`. The existing `UdpFlow` in `flow.rs:34..` has a `pub cancel: CancellationToken` field — confirm by reading `crates/portunus-forwarder/src/forwarder/udp/flow.rs` around line 40-90. If the field exists with that name, no change needed. If the field is named differently, adjust the call site.

```bash
grep -n "cancel" crates/portunus-forwarder/src/forwarder/udp/flow.rs | head -8
```

- [ ] **Step 4: Verify compile**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder 2>&1 | tail -5
```
Expected: `Finished ...`. Compile errors here are real — fix before proceeding.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-forwarder/src/forwarder/udp/registry.rs crates/portunus-forwarder/src/forwarder/udp/mod.rs
git commit -m "feat(014): UdpFlowRegistry skeleton with Slot/Reservation

Per-rule shared flow table with cap and Pending/Live state machine.
Tests follow in subsequent tasks (TDD)."
```

### Task 2.2: Write registry happy-path tests

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/udp/registry.rs` (replace `#[cfg(test)] mod tests`)

- [ ] **Step 1: Add helper for synthetic UdpFlow**

A real `UdpFlow` requires bind + connect + several fields. For unit tests we want a lightweight constructor. If `UdpFlow::new` is too heavy, add a `#[cfg(test)] pub fn for_test(src: SocketAddr) -> Arc<Self>` helper in `flow.rs` that constructs a flow with a bound-but-not-connected socket. Implementation note: bind `0.0.0.0:0`, store a fresh `CancellationToken`, leave quota/target empty. Add minimal helpers as needed.

Sub-agent: inspect `UdpFlow::new` signature first; choose the lightest viable construction. Confirm `flow.cancel` is the public field name.

- [ ] **Step 2: Write tests for happy path**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn key(port: u16, ip_last_octet: u8, src_port: u16) -> FlowKey {
        FlowKey::new(
            port,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, ip_last_octet)), src_port),
        )
    }

    async fn flow_for(_src: SocketAddr) -> Arc<UdpFlow> {
        // Use UdpFlow::for_test (added in Task 2.2 Step 1).
        UdpFlow::for_test(_src).await
    }

    #[tokio::test]
    async fn reserve_then_commit_makes_flow_live() {
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let res = match reg.try_get_or_reserve(k).await {
            TryGetOrReserve::Reserved(r) => r,
            _ => panic!("expected reservation"),
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
        let r1 = match reg.try_get_or_reserve(key(8000, 1, 1)).await {
            TryGetOrReserve::Reserved(r) => r,
            _ => panic!(),
        };
        let r2 = match reg.try_get_or_reserve(key(8000, 2, 1)).await {
            TryGetOrReserve::Reserved(r) => r,
            _ => panic!(),
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
            let r = match reg.try_get_or_reserve(key(8000, 1, 1)).await {
                TryGetOrReserve::Reserved(r) => r,
                _ => panic!(),
            };
            assert_eq!(reg.len(), 1);
            drop(r); // RAII releases.
        }
        // Reservation::drop spawns an async task; yield a few times.
        for _ in 0..10 {
            tokio::task::yield_now().await;
            if reg.len() == 0 {
                return;
            }
        }
        panic!("Reservation drop did not release slot");
    }

    #[tokio::test]
    async fn drain_empties_registry_and_cancels_flows() {
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let res = match reg.try_get_or_reserve(k).await {
            TryGetOrReserve::Reserved(r) => r,
            _ => panic!(),
        };
        let f = flow_for(k.src).await;
        reg.commit(res, Arc::clone(&f)).await;
        assert!(!f.cancel.is_cancelled());
        reg.drain().await;
        assert_eq!(reg.len(), 0);
        assert!(f.cancel.is_cancelled());
    }
}
```

- [ ] **Step 3: Run tests**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::udp::registry::tests 2>&1 | tail -20
```
Expected: all four pass. If `UdpFlow::for_test` is missing, add it now per Step 1.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "test(014): UdpFlowRegistry reserve/commit/drain/cap tests"
```

### Task 2.3: Concurrent reservation race + remove tests

- [ ] **Step 1: Add tests covering concurrent semantics**

Append to the `tests` module:

```rust
    #[tokio::test]
    async fn concurrent_reserve_same_key_second_caller_sees_cap_exhausted() {
        // Listener loop is logically single-threaded per port, but the
        // registry API must remain sound under concurrent calls.
        let reg = UdpFlowRegistry::new(4);
        let k = key(8000, 1, 50000);
        let reg1 = Arc::clone(&reg);
        let reg2 = Arc::clone(&reg);
        let h1 = tokio::spawn(async move {
            reg1.try_get_or_reserve(k).await
        });
        // Wait for h1 to take the Pending slot, then race h2.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let h2 = tokio::spawn(async move {
            reg2.try_get_or_reserve(k).await
        });
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
        let res = match reg.try_get_or_reserve(k).await {
            TryGetOrReserve::Reserved(r) => r,
            _ => panic!(),
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
        let res_live = match reg.try_get_or_reserve(key(8001, 2, 1)).await {
            TryGetOrReserve::Reserved(r) => r,
            _ => panic!(),
        };
        let f = flow_for(key(8001, 2, 1).src).await;
        reg.commit(res_live, Arc::clone(&f)).await;
        let snap = reg.snapshot_live().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].0.listen_port, 8001);
    }
```

- [ ] **Step 2: Run + commit**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::udp::registry::tests 2>&1 | tail -25
```
Expected: 7 total passing.

```bash
git add -A
git commit -m "test(014): UdpFlowRegistry concurrent + remove + snapshot tests"
```

---

## Phase 3: `classify_udp_error` helper

### Task 3.1: Add error classifier with synthetic-error tests

**Files:**
- Create: `crates/portunus-forwarder/src/forwarder/udp/error.rs`
- Modify: `crates/portunus-forwarder/src/forwarder/udp/mod.rs` (add `pub mod error;`)

**Goal:** Pure-function classification of `io::Error` into an `Action` enum. Lets demux and listener share one decision rule and lets unit tests use synthetic errors instead of provoking real ICMP.

- [ ] **Step 1: Write the helper**

```rust
// crates/portunus-forwarder/src/forwarder/udp/error.rs
//! Classification of UDP socket errors into eviction-or-drop actions
//! (FR-006 / FR-007). Pure function so unit tests can feed synthetic
//! `io::Error`s without provoking real ICMP / PMTU events.

use std::io;

/// What the caller should do with a connection after observing this error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UdpAction {
    /// `WouldBlock`: drop datagram, keep flow.
    Wouldblock,
    /// `EMSGSIZE`: drop datagram, keep flow (no PMTU bisection here).
    MessageTooLarge,
    /// Terminal/ICMP-class errors: evict flow.
    Evict,
    /// Other transient (`EINTR` etc.): drop datagram, keep flow.
    Transient,
}

pub fn classify_udp_error(e: &io::Error) -> UdpAction {
    use io::ErrorKind;
    match e.kind() {
        ErrorKind::WouldBlock => UdpAction::Wouldblock,
        ErrorKind::ConnectionRefused
        | ErrorKind::ConnectionAborted
        | ErrorKind::ConnectionReset
        | ErrorKind::HostUnreachable
        | ErrorKind::NetworkUnreachable => UdpAction::Evict,
        ErrorKind::Interrupted => UdpAction::Transient,
        _ => {
            // EMSGSIZE has no stable ErrorKind on stable Rust; check os
            // errno when available.
            if let Some(raw) = e.raw_os_error() {
                if raw == libc::EMSGSIZE {
                    return UdpAction::MessageTooLarge;
                }
                if raw == libc::EHOSTUNREACH
                    || raw == libc::ENETUNREACH
                    || raw == libc::ECONNREFUSED
                {
                    return UdpAction::Evict;
                }
            }
            UdpAction::Transient
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_from_raw(raw: i32) -> io::Error {
        io::Error::from_raw_os_error(raw)
    }

    #[test]
    fn wouldblock_is_wouldblock() {
        let e = io::Error::new(io::ErrorKind::WouldBlock, "x");
        assert_eq!(classify_udp_error(&e), UdpAction::Wouldblock);
    }

    #[test]
    fn econnrefused_evicts() {
        let e = err_from_raw(libc::ECONNREFUSED);
        assert_eq!(classify_udp_error(&e), UdpAction::Evict);
    }

    #[test]
    fn ehostunreach_evicts() {
        let e = err_from_raw(libc::EHOSTUNREACH);
        assert_eq!(classify_udp_error(&e), UdpAction::Evict);
    }

    #[test]
    fn enetunreach_evicts() {
        let e = err_from_raw(libc::ENETUNREACH);
        assert_eq!(classify_udp_error(&e), UdpAction::Evict);
    }

    #[test]
    fn emsgsize_message_too_large() {
        let e = err_from_raw(libc::EMSGSIZE);
        assert_eq!(classify_udp_error(&e), UdpAction::MessageTooLarge);
    }

    #[test]
    fn eintr_transient() {
        let e = err_from_raw(libc::EINTR);
        assert_eq!(classify_udp_error(&e), UdpAction::Transient);
    }
}
```

- [ ] **Step 2: Add `libc` dep if missing**

```bash
grep '^libc' crates/portunus-forwarder/Cargo.toml || echo "MISSING"
```
If missing, add `libc = "0.2"` to `[dependencies]`. If `nix` already pulls it transitively, you can still add it as a direct dep — it's load-bearing here.

- [ ] **Step 3: Wire module**

In `udp/mod.rs`, add `pub mod error;` near `pub mod registry;`.

- [ ] **Step 4: Run tests + commit**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::udp::error 2>&1 | tail -10
git add -A
git commit -m "feat(014): UDP error classifier with synthetic-errno tests

classify_udp_error is the single decision point for FR-006/FR-007.
Pure function — tests use io::Error::from_raw_os_error so no actual
ICMP/PMTU is needed in CI."
```

---

## Phase 4: `ReplyDemuxTask`

### Task 4.1: Demux module skeleton + `DemuxCommand` + `ReadOutcome`

**Files:**
- Create: `crates/portunus-forwarder/src/forwarder/udp/demux.rs`
- Modify: `crates/portunus-forwarder/src/forwarder/udp/mod.rs` (add `pub mod demux;`)

- [ ] **Step 1: Skeleton + command enum**

```rust
// crates/portunus-forwarder/src/forwarder/udp/demux.rs
//! Per-rule reply demux task. Multiplexes all live upstream sockets
//! via FuturesUnordered<ReadWait>. Spec: 014-udp-centralized-demux,
//! FR-008 / FR-009 / FR-011 (drain step d/e).

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};

use crate::forwarder::stats::RuleStats;
use crate::forwarder::udp::error::{UdpAction, classify_udp_error};
use crate::forwarder::udp::flow::UdpFlow;
use crate::forwarder::udp::registry::{FlowKey, UdpFlowRegistry};

/// Demux drains at most this many datagrams per Ready before re-arming
/// the readable future, to keep one chatty flow from starving others
/// (FR-008).
pub const DEMUX_FAIRNESS_BUDGET: usize = 32;

/// Sized at protocol max so try_recv never truncates.
const RECV_BUFFER_BYTES: usize = 65_535;

pub enum DemuxCommand {
    AddFlow {
        key: FlowKey,
        flow: Arc<UdpFlow>,
    },
    Shutdown,
}

pub struct DemuxConfig {
    pub registry: Arc<UdpFlowRegistry>,
    pub listener_sockets: Arc<HashMap<u16, Arc<UdpSocket>>>,
    pub stats: Arc<RuleStats>,
}

pub async fn run_demux(
    cfg: DemuxConfig,
    mut rx: mpsc::Receiver<DemuxCommand>,
) {
    let mut buf = vec![0u8; RECV_BUFFER_BYTES];
    let mut readables: FuturesUnordered<ReadWaitFut> = FuturesUnordered::new();

    loop {
        tokio::select! {
            biased;
            cmd = rx.recv() => match cmd {
                Some(DemuxCommand::AddFlow { key, flow }) => {
                    readables.push(read_wait(key, flow));
                }
                Some(DemuxCommand::Shutdown) | None => break,
            },
            Some(outcome) = readables.next(), if !readables.is_empty() => match outcome {
                ReadOutcome::Ready { key, flow } => {
                    drain_one_flow(&cfg, key, &flow, &mut buf).await;
                    // Re-arm unless this flow has been cancelled during drain
                    // (terminal error path) — cancel will surface next iter.
                    if !flow.cancel.is_cancelled() {
                        readables.push(read_wait(key, flow));
                    }
                }
                ReadOutcome::Cancelled { key: _ } => {
                    // Drop the Arc; no re-arm.
                }
            },
        }
    }
}

async fn drain_one_flow(
    cfg: &DemuxConfig,
    key: FlowKey,
    flow: &Arc<UdpFlow>,
    buf: &mut [u8],
) {
    let Some(listener) = cfg.listener_sockets.get(&key.listen_port).cloned() else {
        // Listener gone — shouldn't happen during normal operation.
        warn!(event = "rule.udp_demux_missing_listener", listen_port = key.listen_port);
        return;
    };
    for _ in 0..DEMUX_FAIRNESS_BUDGET {
        match flow.upstream_socket.try_recv(buf) {
            Ok(n) => {
                match listener.try_send_to(&buf[..n], key.src) {
                    Ok(_) => {
                        flow.bump_outbound(n as u64).await;
                        cfg.stats.inc_datagram_out(key.listen_port, n);
                        flow.quota_consume_after_send(n as u64);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        trace!(event = "rule.udp_reply_wouldblock", listen_port = key.listen_port);
                        // Drop reply; flow continues.
                    }
                    Err(e) => {
                        warn!(
                            event = "rule.udp_reply_send_failed",
                            listen_port = key.listen_port,
                            error = %e,
                        );
                        // Listener socket error is rule-level — best effort: log + continue.
                    }
                }
            }
            Err(e) => match classify_udp_error(&e) {
                UdpAction::Wouldblock => return,
                UdpAction::Evict => {
                    info!(
                        event = "rule.udp_flow_evicted_icmp",
                        listen_port = key.listen_port,
                        error = %e,
                    );
                    let _ = cfg.registry.remove(key).await;
                    flow.cancel.cancel();
                    return;
                }
                UdpAction::MessageTooLarge => {
                    debug!(event = "rule.udp_emsgsize", listen_port = key.listen_port);
                    return;
                }
                UdpAction::Transient => {
                    return;
                }
            },
        }
    }
}

enum ReadOutcome {
    Ready { key: FlowKey, flow: Arc<UdpFlow> },
    Cancelled { key: FlowKey },
}

type ReadWaitFut = std::pin::Pin<Box<dyn std::future::Future<Output = ReadOutcome> + Send>>;

fn read_wait(key: FlowKey, flow: Arc<UdpFlow>) -> ReadWaitFut {
    Box::pin(async move {
        tokio::select! {
            _ = flow.cancel.cancelled() => ReadOutcome::Cancelled { key },
            r = flow.upstream_socket.readable() => match r {
                Ok(()) => ReadOutcome::Ready { key, flow },
                Err(_) => ReadOutcome::Cancelled { key },
            }
        }
    })
}

#[cfg(test)]
mod tests {
    // Tests in Task 4.2-4.4.
}
```

**Note for sub-agent:** The skeleton references `flow.upstream_socket` and `flow.bump_outbound`/`quota_consume_after_send`. Confirm these against `flow.rs`:

```bash
grep -nE 'upstream_socket|bump_outbound|quota_consume_after_send' crates/portunus-forwarder/src/forwarder/udp/flow.rs | head -10
```
Adjust field/method names as needed. The connect()-on-upstream change happens in the listener (Task 6); for demux purposes the field name and `try_recv` signature are what matter.

- [ ] **Step 2: Wire module + verify compile**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder 2>&1 | tail -8
```
Expected: clean build. Fix any name mismatches before continuing.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat(014): ReplyDemuxTask skeleton with FuturesUnordered<ReadWait>"
```

### Task 4.2: Demux unit tests with loopback sockets

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/udp/demux.rs` (tests module)

**Goal:** verify (a) AddFlow → reply forwarding works, (b) fairness budget caps drain at 32, (c) cancel during ReadWait drops the flow cleanly.

- [ ] **Step 1: Test helpers + first test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Duration;

    async fn bind_loopback_udp() -> (Arc<UdpSocket>, SocketAddr) {
        let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = s.local_addr().unwrap();
        (Arc::new(s), addr)
    }

    /// End-to-end: stand up a listener socket, an upstream socket, an
    /// UdpFlow that owns the upstream, then send a "reply" from the
    /// upstream's peer and verify the demux forwards it via the listener
    /// to the original client.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn add_flow_then_reply_reaches_client() {
        // (1) listener socket the demux will use to send_to client
        let (listener_sock, listener_addr) = bind_loopback_udp().await;
        let mut listener_map = HashMap::new();
        listener_map.insert(listener_addr.port(), Arc::clone(&listener_sock));

        // (2) "client" socket that will receive the forwarded reply
        let (client_sock, client_addr) = bind_loopback_udp().await;

        // (3) "target" socket impersonates upstream
        let (target_sock, target_addr) = bind_loopback_udp().await;

        // (4) flow.upstream_socket: bind ephemeral, connect to target
        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);

        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        let key = FlowKey::new(listener_addr.port(), client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = Arc::new(RuleStats::single());
        let cfg = DemuxConfig {
            registry: Arc::clone(&registry),
            listener_sockets: Arc::new(listener_map),
            stats: Arc::clone(&stats),
        };
        let (tx, rx) = mpsc::channel(8);
        let h = tokio::spawn(run_demux(cfg, rx));

        tx.send(DemuxCommand::AddFlow { key, flow: Arc::clone(&flow) }).await.unwrap();

        // Send "reply" from target to upstream
        target_sock.send_to(b"hello", upstream.local_addr().unwrap()).await.unwrap();

        let mut buf = [0u8; 64];
        let (n, src) = tokio::time::timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf))
            .await
            .expect("client should receive forwarded reply within 2s")
            .unwrap();
        assert_eq!(&buf[..n], b"hello");
        assert_eq!(src, listener_addr);

        tx.send(DemuxCommand::Shutdown).await.unwrap();
        h.await.unwrap();
    }
}
```

**Note:** This test needs a `UdpFlow::for_test_with_socket` helper that takes the pre-built socket. Add it to `flow.rs` alongside the earlier `for_test`. Construct a minimal flow with cancel token, stats, etc.

`RuleStats::single()` is a constructor placeholder — match the actual signature in `stats.rs`. If `RuleStats::for_range` is the only public constructor, use it with a single-port range.

- [ ] **Step 2: Test for cancel-during-readwait**

Append:

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_flow_drops_arc_without_re_arm() {
        let (listener_sock, listener_addr) = bind_loopback_udp().await;
        let mut listener_map = HashMap::new();
        listener_map.insert(listener_addr.port(), Arc::clone(&listener_sock));

        let (_target_sock, target_addr) = bind_loopback_udp().await;

        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);

        let client_addr: SocketAddr = "127.0.0.1:50000".parse().unwrap();
        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        let key = FlowKey::new(listener_addr.port(), client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = Arc::new(RuleStats::single());
        let (tx, rx) = mpsc::channel(8);
        let h = tokio::spawn(run_demux(
            DemuxConfig { registry, listener_sockets: Arc::new(listener_map), stats },
            rx,
        ));

        tx.send(DemuxCommand::AddFlow { key, flow: Arc::clone(&flow) }).await.unwrap();
        // Give demux a tick to push the ReadWait future.
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Now cancel the flow.
        flow.cancel.cancel();
        // Strong-ref count goes 2 (test + demux). After cancel and re-poll, demux drops its ref → 1.
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if Arc::strong_count(&flow) == 1 {
                break;
            }
        }
        assert_eq!(Arc::strong_count(&flow), 1, "demux must drop its Arc after cancel");

        tx.send(DemuxCommand::Shutdown).await.unwrap();
        h.await.unwrap();
    }
```

- [ ] **Step 3: Run tests**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::udp::demux 2>&1 | tail -15
```
Expected: 2 passing.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "test(014): demux add-flow + cancel-readwait unit tests"
```

### Task 4.3: Fairness budget test

- [ ] **Step 1: Add test**

Append to demux tests:

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_budget_caps_at_32_datagrams_per_ready() {
        // Send 100 reply datagrams in one go, observe demux processes
        // them in batches of <=32 (the fairness budget). We can't observe
        // batches directly; instead check that all 100 eventually arrive
        // and the demux did not panic / starve.
        let (listener_sock, listener_addr) = bind_loopback_udp().await;
        let mut listener_map = HashMap::new();
        listener_map.insert(listener_addr.port(), Arc::clone(&listener_sock));

        let (client_sock, client_addr) = bind_loopback_udp().await;
        let (target_sock, target_addr) = bind_loopback_udp().await;

        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);
        let upstream_local = upstream.local_addr().unwrap();

        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        let key = FlowKey::new(listener_addr.port(), client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = Arc::new(RuleStats::single());
        let (tx, rx) = mpsc::channel(8);
        let h = tokio::spawn(run_demux(
            DemuxConfig { registry, listener_sockets: Arc::new(listener_map), stats },
            rx,
        ));
        tx.send(DemuxCommand::AddFlow { key, flow }).await.unwrap();

        for i in 0..100u8 {
            target_sock.send_to(&[i], upstream_local).await.unwrap();
        }

        let mut received = std::collections::HashSet::new();
        let mut buf = [0u8; 64];
        for _ in 0..100 {
            let (n, _) = tokio::time::timeout(Duration::from_secs(3), client_sock.recv_from(&mut buf))
                .await
                .expect("100 replies should arrive within 3s")
                .unwrap();
            assert_eq!(n, 1);
            received.insert(buf[0]);
        }
        assert_eq!(received.len(), 100);

        tx.send(DemuxCommand::Shutdown).await.unwrap();
        h.await.unwrap();
    }
```

- [ ] **Step 2: Run + commit**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::udp::demux 2>&1 | tail -15
git add -A
git commit -m "test(014): demux fairness budget — 100 datagrams arrive intact"
```

---

## Phase 5: `RuleReaper`

### Task 5.1: Per-rule reaper module + idle test

**Files:**
- Create: `crates/portunus-forwarder/src/forwarder/udp/reaper.rs`
- Modify: `crates/portunus-forwarder/src/forwarder/udp/mod.rs`

- [ ] **Step 1: Skeleton**

```rust
// crates/portunus-forwarder/src/forwarder/udp/reaper.rs
//! Per-rule idle-flow reaper. Replaces v0.4 per-listener
//! `spawn_reaper`. Sweeps every `idle_window / 4`; evicts flows whose
//! `last_seen` exceeds `idle_window`. Spec: 014, FR-010.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::forwarder::udp::registry::UdpFlowRegistry;
use portunus_core::RuleId;

pub async fn run_reaper(
    registry: Arc<UdpFlowRegistry>,
    idle_window: Duration,
    rule_id: RuleId,
    cancel: CancellationToken,
) {
    let mut ticker = interval(idle_window / 4);
    ticker.tick().await; // skip the immediate tick
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = ticker.tick() => {
                let now = Instant::now();
                let snap = registry.snapshot_live().await;
                let mut evicted = 0usize;
                for (key, flow) in snap {
                    let last = flow.last_seen_at().await;
                    if now.saturating_duration_since(last) > idle_window {
                        if registry.remove(key).await.is_some() {
                            flow.cancel.cancel();
                            evicted += 1;
                            info!(
                                event = "rule.udp_flow_closed_idle",
                                rule_id = %rule_id,
                                listen_port = key.listen_port,
                                source = %key.src,
                            );
                        }
                    }
                }
                let _ = evicted;
            }
        }
    }
}
```

- [ ] **Step 2: Idle-eviction test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarder::udp::flow::UdpFlow;
    use crate::forwarder::udp::registry::FlowKey;
    use std::net::SocketAddr;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn idle_flows_are_evicted_after_window() {
        let reg = UdpFlowRegistry::new(4);
        let src: SocketAddr = "127.0.0.1:50000".parse().unwrap();
        let key = FlowKey::new(8000, src);
        let res = match reg.try_get_or_reserve(key).await {
            crate::forwarder::udp::registry::TryGetOrReserve::Reserved(r) => r,
            _ => panic!(),
        };
        let flow = UdpFlow::for_test(src).await;
        // Backdate last_seen to before "now - 1s" so it's idle vs a
        // 100ms idle_window.
        flow.force_last_seen(Instant::now() - Duration::from_secs(60)).await;
        reg.commit(res, Arc::clone(&flow)).await;

        let cancel = CancellationToken::new();
        let reg_ref = Arc::clone(&reg);
        let cancel_ref = cancel.clone();
        let h = tokio::spawn(async move {
            run_reaper(reg_ref, Duration::from_millis(100), RuleId::from(1u64), cancel_ref).await;
        });

        // Wait up to 1s for reaper to evict.
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if reg.len() == 0 { break; }
        }
        assert_eq!(reg.len(), 0);
        assert!(flow.cancel.is_cancelled());

        cancel.cancel();
        h.await.unwrap();
    }
}
```

`UdpFlow::force_last_seen` is a `#[cfg(test)]` helper — add to `flow.rs`.

- [ ] **Step 3: Wire module + test + commit**

```bash
# Add `pub mod reaper;` to udp/mod.rs (next to pub mod demux;)
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::udp::reaper 2>&1 | tail -10
git add -A
git commit -m "feat(014): per-rule RuleReaper + idle-eviction test"
```

---

## Phase 6: `PerListenerLoop` (cold + fast path)

### Task 6.1: Listener loop module skeleton + cold-path order

**Files:**
- Create: `crates/portunus-forwarder/src/forwarder/udp/listener.rs`
- Modify: `crates/portunus-forwarder/src/forwarder/udp/mod.rs`

**Note for sub-agent:** This is the largest single file. Read `udp/mod.rs::run_listener` (line 459 onward) AND `relay_existing_flow` / `relay_new_flow` (around 320-440) before writing this. The new listener is the per-port version, but with:
1. flow key = `(listen_port, src)` — uses the shared per-rule registry
2. cold-path order per FR-004 (cap check BEFORE rate-limit, etc.)
3. multi-A fallback at `connect()` seam (Task 6.2)
4. fast-path adds `if flow.cancel.is_cancelled() { fall through to cold path }` cheap defensive check
5. Rate-limit, quota, DNS plumbing identical to v0.4 — same Arc handles passed in

**Required APIs from listener.rs:**
```rust
pub struct ListenerConfig {
    pub rule_id: RuleId,
    pub listen_port: u16,
    pub target: Target,
    pub target_port: u16,
    pub prefer_ipv6: bool,
    pub idle_window: Duration,
    pub registry: Arc<UdpFlowRegistry>,
    pub demux_tx: mpsc::Sender<DemuxCommand>,
    pub stats: Arc<RuleStats>,
    pub resolver: Arc<LiveResolver<R>>,  // generic R
    pub rate_limit: Option<Arc<RuleRateLimitHandle>>,
    pub rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    pub owner_rate_limit: Option<Arc<OwnerRateLimitHandle>>,
    pub owner_rate_limit_stats: Option<Arc<OwnerRateLimitStatsAccumulator>>,
    pub quota: Option<Arc<QuotaHandle>>,
    pub cancel: CancellationToken,
}

pub async fn run_listener<R: Resolve + 'static>(
    cfg: ListenerConfig<R>,
    listener_socket: Arc<UdpSocket>,
) {...}
```

The `listener_socket` is passed in (already bound by runtime). Listener does NOT bind — runtime probe-binds and shares the `Arc<UdpSocket>` with both listener and demux via the runtime's listener_sockets map.

Implementation guidance:
- Reuse the recv loop shape from v0.4 `run_listener` (mod.rs:447-540).
- Replace the `flow_table.lookup_or_insert(...)` call site with the FR-004 path. Use `registry.try_get_or_reserve` (since registry is per-rule, the cap check is per-rule).
- Drop the per-listener `spawn_reaper`, per-listener `inc_active_flows` calls.
- After `commit`, do **`demux_tx.try_send(AddFlow{key, flow})`** — on full: `registry.remove + flow.cancel.cancel() + drop payload`, emit `rule.udp_addflow_dropped`.
- Then `upstream.try_send(payload)` — classify per `classify_udp_error`. On `Evict`: `registry.remove + flow.cancel.cancel()`, no `datagram_in` count, no quota consume. On `Wouldblock`/`MessageTooLarge`/`Transient`: drop datagram but keep flow.

- [ ] **Step 1: Write the skeleton** — sub-agent: study v0.4 `run_listener` first, then write the new one. Keep the function signature stable; the runtime call site (Phase 8) depends on `ListenerConfig`.

- [ ] **Step 2: Wire module + compile**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder 2>&1 | tail -10
```

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat(014): PerListenerLoop with FR-004 cold-path order"
```

### Task 6.2: Cold-path connect-time multi-A fallback

- [ ] **Step 1: Implement the resolver-ordered walk inside listener.rs cold path**

```rust
// Pseudocode inside cold path:
let resolved: Vec<SocketAddr> = match cfg.resolver.resolve_target(rule_id, &target, target_port, prefer_ipv6).await {
    Ok((addrs, _src)) => addrs,
    Err(_) => {
        cfg.stats.inc_dns_failed();
        return; // silent drop
    }
};

let mut selected: Option<(Arc<UdpSocket>, SocketAddr)> = None;
for addr in &resolved {
    let bind_addr: SocketAddr = match addr {
        SocketAddr::V4(_) => "0.0.0.0:0".parse().unwrap(),
        SocketAddr::V6(_) => "[::]:0".parse().unwrap(),
    };
    let sock = match UdpSocket::bind(bind_addr).await {
        Ok(s) => s,
        Err(e) => {
            warn!(event = "rule.udp_upstream_bind_failed", error = %e);
            continue;
        }
    };
    match sock.connect(*addr).await {
        Ok(()) => {
            selected = Some((Arc::new(sock), *addr));
            break;
        }
        Err(e) => {
            warn!(event = "rule.udp_upstream_connect_failed", target = %addr, error = %e);
            continue;
        }
    }
}
let Some((upstream_socket, chosen_target)) = selected else {
    cfg.stats.inc_dns_failed(); // FR-016 — counted under dns_failures
    return;
};
```

- [ ] **Step 2: Add a unit test using TcpListener... wait, UDP**

Test the resolver fallback by injecting a synthetic resolver. If `LiveResolver` is hard to inject, build the test with a real `LiveResolver` instance configured to return a known list — or use a trait-mock if `Resolve` is already a trait. Use a list where the first address is `0.0.0.0:0` (which `connect()` will accept but `try_send` may not — easier: use `127.0.0.1` to an unbound port to force `ECONNREFUSED` later).

Sub-agent decision: write a unit test only if `Resolve` can be mocked cleanly. Otherwise rely on the integration test in Phase 10 to cover the fallback walk.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat(014): listener cold-path multi-A connect-time fallback"
```

### Task 6.3: Listener fast-path with `is_cancelled` defensive check

- [ ] **Step 1: Add the defensive check inside listener.rs fast path**

```rust
if let Some(flow) = cfg.registry.get(key).await {
    if flow.cancel.is_cancelled() {
        // Race vs reaper — fall through to cold path.
    } else {
        // quota.check_in
        if let Some(q) = &cfg.quota {
            if !q.quota_allows() {
                return; // silent drop
            }
        }
        // try_send classified per FR-006/FR-007
        match flow.upstream_socket.try_send(payload) {
            Ok(_) => {
                flow.bump_inbound(n as u64).await;
                cfg.stats.inc_datagram_in(listen_port, n);
                if let Some(q) = &cfg.quota {
                    q.quota_consume_after_send(n as u64);
                }
            }
            Err(e) => match classify_udp_error(&e) {
                UdpAction::Evict => {
                    let _ = cfg.registry.remove(key).await;
                    flow.cancel.cancel();
                }
                _ => { /* drop datagram, keep flow */ }
            },
        }
        return;
    }
}
// fall through to cold path
```

- [ ] **Step 2: Commit**

```bash
git add -A
git commit -m "feat(014): listener fast-path with cancel-guard defensive check"
```

---

## Phase 7: `UdpRuleRuntime` + Supervisor

### Task 7.1: Runtime skeleton + supervisor state machine

**Files:**
- Create: `crates/portunus-forwarder/src/forwarder/udp/runtime.rs`
- Modify: `crates/portunus-forwarder/src/forwarder/udp/mod.rs`

- [ ] **Step 1: Skeleton**

```rust
// crates/portunus-forwarder/src/forwarder/udp/runtime.rs
//! Per-rule UDP runtime. Owns the registry, listener socket map, and a
//! supervisor task that drives ordered shutdown.
//! Spec: 014, FR-001 / FR-011 / FR-012.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::forwarder::stats::RuleStats;
use crate::forwarder::udp::demux::{DemuxCommand, DemuxConfig, run_demux};
use crate::forwarder::udp::listener::{ListenerConfig, run_listener};
use crate::forwarder::udp::reaper::run_reaper;
use crate::forwarder::udp::registry::UdpFlowRegistry;
use crate::resolver::{LiveResolver, Resolve};
use portunus_core::RuleId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Running,
    ShuttingDownIntentional,
    ShuttingDownAfterFailure,
}

#[derive(Clone, Copy, Debug)]
enum Role {
    Listener(u16),
    Demux,
    Reaper,
}

pub struct UdpRuleRuntime {
    registry: Arc<UdpFlowRegistry>,
    listener_sockets: Arc<HashMap<u16, Arc<UdpSocket>>>,
    rule_cancel: CancellationToken,
    shutdown_tx: mpsc::Sender<()>,
    supervisor_handle: Option<tokio::task::JoinHandle<()>>,
    completion_rx: tokio::sync::watch::Receiver<Option<ShutdownOutcome>>,
}

#[derive(Clone, Debug)]
pub enum ShutdownOutcome {
    Ok,
    UnexpectedExitsDuringDrain { roles: Vec<String>, count: usize },
}

pub struct UdpRuntimeConfig<R: Resolve + 'static> {
    pub rule_id: RuleId,
    pub listen_ports: std::ops::RangeInclusive<u16>,
    pub target: crate::forwarder::Target,
    pub target_ports: std::ops::RangeInclusive<u16>,
    pub prefer_ipv6: bool,
    pub rule_cap: usize,
    pub idle_window: Duration,
    pub stats: Arc<RuleStats>,
    pub resolver: Arc<LiveResolver<R>>,
    pub rate_limit: Option<Arc<crate::forwarder::rate_limit::scope::RuleRateLimitHandle>>,
    pub rate_limit_stats: Option<Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>>,
    pub owner_rate_limit: Option<Arc<crate::forwarder::rate_limit::scope::OwnerRateLimitHandle>>,
    pub owner_rate_limit_stats: Option<Arc<crate::forwarder::rate_limit::stats::OwnerRateLimitStatsAccumulator>>,
    pub quota: Option<Arc<crate::forwarder::quota::QuotaHandle>>,
    pub failed_callback: Box<dyn Fn(String) + Send + Sync>,
}

impl UdpRuleRuntime {
    pub async fn start<R: Resolve + 'static>(
        cfg: UdpRuntimeConfig<R>,
        rule_cancel: CancellationToken,
    ) -> Result<Self, UdpRuntimeStartError> {
        // (1) probe-bind every port; rollback on partial failure
        let mut sockets: HashMap<u16, Arc<UdpSocket>> = HashMap::new();
        for port in cfg.listen_ports.clone() {
            match UdpSocket::bind(("0.0.0.0", port)).await {
                Ok(s) => {
                    sockets.insert(port, Arc::new(s));
                }
                Err(e) => {
                    drop(sockets);
                    return Err(UdpRuntimeStartError::BindFailed { port, error: e });
                }
            }
        }
        let listener_sockets = Arc::new(sockets);

        // (2) registry + channels
        let registry = UdpFlowRegistry::new(cfg.rule_cap);
        let (demux_tx, demux_rx) = mpsc::channel::<DemuxCommand>(1024);
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
        let (completion_tx, completion_rx) = tokio::sync::watch::channel::<Option<ShutdownOutcome>>(None);

        // (3) child cancel tokens
        let listener_token = rule_cancel.child_token();
        let reaper_token = rule_cancel.child_token();

        // (4) emit rule.udp_runtime_started
        let range_size = cfg.listen_ports.end() - cfg.listen_ports.start() + 1;
        info!(
            event = "rule.udp_runtime_started",
            rule_id = %cfg.rule_id,
            listen_port_start = cfg.listen_ports.start(),
            listen_port_end = cfg.listen_ports.end(),
            range_size = range_size,
            rule_cap = cfg.rule_cap,
            cap_scope = "per_rule",
        );

        // (5) spawn supervisor (holds JoinSet, owns demux_tx clone for Shutdown)
        let supervisor = Supervisor {
            joinset: JoinSet::new(),
            state: State::Running,
            registry: Arc::clone(&registry),
            shutdown_rx,
            completion_tx,
            rule_cancel: rule_cancel.clone(),
            listener_token: listener_token.clone(),
            reaper_token: reaper_token.clone(),
            demux_tx_for_shutdown: demux_tx.clone(),
            failed_callback: cfg.failed_callback,
            unexpected_during_drain: Vec::new(),
        };

        let supervisor_handle = supervisor.spawn_all_and_run(
            // dispatch payload
            cfg,
            Arc::clone(&listener_sockets),
            Arc::clone(&registry),
            demux_tx.clone(),
            demux_rx,
            listener_token,
            reaper_token,
        );

        Ok(Self {
            registry,
            listener_sockets,
            rule_cancel,
            shutdown_tx,
            supervisor_handle: Some(supervisor_handle),
            completion_rx,
        })
    }

    /// FR-012: idempotent signal-and-wait. The supervisor performs the
    /// ordered drain.
    pub async fn shutdown(&mut self) -> ShutdownOutcome {
        let _ = self.shutdown_tx.send(()).await; // ignore SendError (already closed)
        let mut rx = self.completion_rx.clone();
        loop {
            if let Some(outcome) = rx.borrow().clone() {
                return outcome;
            }
            if rx.changed().await.is_err() {
                // supervisor task dropped sender — assume Ok if no outcome was set
                return ShutdownOutcome::Ok;
            }
        }
    }

    pub fn registry(&self) -> &Arc<UdpFlowRegistry> {
        &self.registry
    }
}

#[derive(Debug)]
pub enum UdpRuntimeStartError {
    BindFailed { port: u16, error: std::io::Error },
}

struct Supervisor {
    joinset: JoinSet<(Role, Result<(), tokio::task::JoinError>)>,
    state: State,
    registry: Arc<UdpFlowRegistry>,
    shutdown_rx: mpsc::Receiver<()>,
    completion_tx: tokio::sync::watch::Sender<Option<ShutdownOutcome>>,
    rule_cancel: CancellationToken,
    listener_token: CancellationToken,
    reaper_token: CancellationToken,
    demux_tx_for_shutdown: mpsc::Sender<DemuxCommand>,
    failed_callback: Box<dyn Fn(String) + Send + Sync>,
    unexpected_during_drain: Vec<String>,
}

impl Supervisor {
    fn spawn_all_and_run<R: Resolve + 'static>(
        mut self,
        cfg: UdpRuntimeConfig<R>,
        listener_sockets: Arc<HashMap<u16, Arc<UdpSocket>>>,
        registry: Arc<UdpFlowRegistry>,
        demux_tx_for_listeners: mpsc::Sender<DemuxCommand>,
        demux_rx: mpsc::Receiver<DemuxCommand>,
        listener_token: CancellationToken,
        reaper_token: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        // spawn demux
        let demux_cfg = DemuxConfig {
            registry: Arc::clone(&registry),
            listener_sockets: Arc::clone(&listener_sockets),
            stats: Arc::clone(&cfg.stats),
        };
        self.joinset.spawn(async move {
            run_demux(demux_cfg, demux_rx).await;
            (Role::Demux, Ok(()))
        });
        // spawn reaper
        let reg_for_reaper = Arc::clone(&registry);
        let reaper_token_for_task = reaper_token.clone();
        let rule_id_for_reaper = cfg.rule_id;
        let idle_window = cfg.idle_window;
        self.joinset.spawn(async move {
            run_reaper(reg_for_reaper, idle_window, rule_id_for_reaper, reaper_token_for_task).await;
            (Role::Reaper, Ok(()))
        });
        // spawn one listener per port
        for port in cfg.listen_ports.clone() {
            let Some(target_port) = port_map(port, &cfg.listen_ports, &cfg.target_ports) else {
                continue;
            };
            let lcfg = ListenerConfig {
                rule_id: cfg.rule_id,
                listen_port: port,
                target: cfg.target.clone(),
                target_port,
                prefer_ipv6: cfg.prefer_ipv6,
                idle_window: cfg.idle_window,
                registry: Arc::clone(&registry),
                demux_tx: demux_tx_for_listeners.clone(),
                stats: Arc::clone(&cfg.stats),
                resolver: Arc::clone(&cfg.resolver),
                rate_limit: cfg.rate_limit.clone(),
                rate_limit_stats: cfg.rate_limit_stats.clone(),
                owner_rate_limit: cfg.owner_rate_limit.clone(),
                owner_rate_limit_stats: cfg.owner_rate_limit_stats.clone(),
                quota: cfg.quota.clone(),
                cancel: listener_token.clone(),
            };
            let sock = Arc::clone(listener_sockets.get(&port).expect("bound earlier"));
            self.joinset.spawn(async move {
                run_listener(lcfg, sock).await;
                (Role::Listener(port), Ok(()))
            });
        }
        tokio::spawn(self.run())
    }

    async fn run(mut self) {
        let mut listener_cancel_fired = false;
        let mut reaper_cancel_fired = false;
        let mut demux_shutdown_sent = false;

        // Phase A: Running — watch for unexpected exits OR shutdown signal.
        loop {
            tokio::select! {
                biased;
                Some(_) = self.shutdown_rx.recv() => {
                    if matches!(self.state, State::Running) {
                        self.state = State::ShuttingDownIntentional;
                    }
                    break;
                }
                Some(res) = self.joinset.join_next() => {
                    // In Running, any exit is unexpected.
                    if matches!(self.state, State::Running) {
                        let role_str = format!("{:?}", res.as_ref().map(|(r, _)| r).unwrap_or(&Role::Demux));
                        self.state = State::ShuttingDownAfterFailure;
                        self.rule_cancel.cancel();
                        (self.failed_callback)(format!("unexpected_task_exit:{role_str}"));
                        break;
                    }
                }
            }
        }

        // Phase B: Ordered drain (per FR-011).
        // (a) cancel listener token, await all Listener tags
        self.listener_token.cancel();
        listener_cancel_fired = true;
        let _ = self.drain_until(|r| matches!(r, Role::Listener(_))).await;
        // (b) cancel reaper token, await Reaper tag
        self.reaper_token.cancel();
        reaper_cancel_fired = true;
        let _ = self.drain_until(|r| matches!(r, Role::Reaper)).await;
        // (c) registry.drain
        self.registry.drain().await;
        // (d) send DemuxCommand::Shutdown (FR-011: explicit, not channel-close)
        let _ = self.demux_tx_for_shutdown.send(DemuxCommand::Shutdown).await;
        demux_shutdown_sent = true;
        // (e) await Demux
        let _ = self.drain_until(|r| matches!(r, Role::Demux)).await;
        // Loop to consume any stragglers.
        while let Some(res) = self.joinset.join_next().await {
            self.handle_drain_result(res);
        }

        let _ = listener_cancel_fired;
        let _ = reaper_cancel_fired;
        let _ = demux_shutdown_sent;

        // Phase C: emit completion
        let outcome = if self.unexpected_during_drain.is_empty() {
            ShutdownOutcome::Ok
        } else {
            ShutdownOutcome::UnexpectedExitsDuringDrain {
                count: self.unexpected_during_drain.len(),
                roles: self.unexpected_during_drain,
            }
        };
        let _ = self.completion_tx.send(Some(outcome));
    }

    async fn drain_until<F: Fn(&Role) -> bool>(&mut self, target: F) -> usize {
        let mut got = 0usize;
        while let Some(res) = self.joinset.join_next().await {
            self.handle_drain_result_inner(res.as_ref().ok(), &res);
            if let Ok((role, _)) = &res {
                if target(role) {
                    got += 1;
                    // We may have multiple of the target (e.g. several listeners);
                    // continue draining until joinset has no more target-tagged.
                    // Simpler: just collect all current ready items; bail when no more match.
                }
            }
            // Break once we've drained at least one match and the joinset
            // is either empty or its next ready won't be target. Implementation
            // simplification: just return; outer loop will keep draining.
            if got >= 1 {
                return got;
            }
        }
        got
    }

    fn handle_drain_result(&mut self, res: Result<(Role, Result<(), tokio::task::JoinError>), tokio::task::JoinError>) {
        self.handle_drain_result_inner(res.as_ref().ok(), &res);
    }

    fn handle_drain_result_inner(
        &mut self,
        ok: Option<&(Role, Result<(), tokio::task::JoinError>)>,
        full: &Result<(Role, Result<(), tokio::task::JoinError>), tokio::task::JoinError>,
    ) {
        if let Some((role, _)) = ok {
            let role_cancelled = match role {
                Role::Listener(_) => self.listener_token.is_cancelled(),
                Role::Reaper => self.reaper_token.is_cancelled(),
                Role::Demux => false, // demux exits on explicit Shutdown command; we don't track via a token
            };
            if !role_cancelled {
                self.unexpected_during_drain.push(format!("{:?}", role));
                warn!(event = "rule.udp_shutdown_unexpected_exit", role = ?role);
            }
        } else if let Err(je) = full {
            warn!(event = "rule.udp_shutdown_unexpected_exit", join_error = %je);
            self.unexpected_during_drain.push(format!("JoinError:{je}"));
        }
    }
}

fn port_map(port: u16, listen: &std::ops::RangeInclusive<u16>, target: &std::ops::RangeInclusive<u16>) -> Option<u16> {
    let offset = port.checked_sub(*listen.start())?;
    let target_port = (*target.start()).checked_add(offset)?;
    if target_port <= *target.end() { Some(target_port) } else { None }
}
```

**Sub-agent note:** The `drain_until` implementation above is simplified. The correct semantics for FR-011 ordered drain: cancel the target role's token, then drain `joinset.join_next()` until either (a) all entries with that role tag have been joined, or (b) the joinset is empty. Sibling-role exits (cancel or panic) during this drain are classified as "unexpected" because their own cancel token hasn't fired yet. Sub-agent: refine `drain_until` to track per-role expected counts (e.g. `listener_count - listener_joined`), and complete when no more of the target role exists. Provide a non-naive implementation.

- [ ] **Step 2: Compile-only check**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder 2>&1 | tail -15
```

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat(014): UdpRuleRuntime skeleton with supervisor state machine"
```

### Task 7.2: Supervisor tests — state transitions

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/udp/runtime.rs`

- [ ] **Step 1: Add tests for state transitions + drain ordering + shutdown idempotency**

Write at least three tests:
1. `intentional_shutdown_emits_Ok_no_failed_callback`
2. `panic_in_listener_during_Running_transitions_to_AfterFailure_and_emits_failed_once`
3. `second_shutdown_call_is_idempotent`

For these we need to inject controllable tasks. Two options:
- Build a real `UdpRuleRuntime` with no flows and observe shutdown outcome.
- Extract the supervisor into a testable subroutine that takes pre-spawned task handles.

Recommended: write test variant of the supervisor that takes a pre-populated `JoinSet<(Role, Result<(), JoinError>)>` and a shutdown channel, exposed via `#[cfg(test)]` constructor. Test against the supervisor logic directly without spawning real listeners.

- [ ] **Step 2: Run + commit**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::udp::runtime 2>&1 | tail -15
git add -A
git commit -m "test(014): supervisor state machine + idempotent shutdown tests"
```

---

## Phase 8: Wire into `run_udp` (forwarder/mod.rs)

### Task 8.1: Replace `run_udp` single-target body with `UdpRuleRuntime::start`

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/mod.rs` (lines 561-end of run_udp)
- Modify: `crates/portunus-forwarder/src/forwarder/udp/mod.rs` (delete or hide v0.4 `run_listener`, `run_listener_multi_target`, helpers)

**Important:** Multi-target UDP rules continue to use `failover_path::run_udp` — leave that branch (lines 568-574) unchanged.

- [ ] **Step 1: Modify run_udp single-target branch**

After the existing multi-target early return (line 574), replace the body with:

```rust
    let cap = resolve_udp_cap(rule.udp_max_flows);
    let idle_window = resolve_udp_idle_window(rule.udp_flow_idle_secs);

    let (failed_tx, mut failed_rx) = tokio::sync::mpsc::channel::<String>(1);
    let failed_callback: Box<dyn Fn(String) + Send + Sync> = Box::new(move |reason| {
        let _ = failed_tx.try_send(reason);
    });

    let cfg = udp::runtime::UdpRuntimeConfig {
        rule_id: rule.rule_id,
        listen_ports: rule.listen_range.start()..=rule.listen_range.end(),
        target: rule.target.clone(),
        target_ports: rule.target_range.start()..=rule.target_range.end(),
        prefer_ipv6: rule.prefer_ipv6,
        rule_cap: cap as usize,
        idle_window,
        stats: Arc::clone(&stats),
        resolver: Arc::clone(&resolver),
        rate_limit: rule.rate_limit.clone(),
        rate_limit_stats: rule.rate_limit_stats.clone(),
        owner_rate_limit: rule.owner_rate_limit.clone(),
        owner_rate_limit_stats: rule.owner_rate_limit_stats.clone(),
        quota: rule.quota.clone(),
        failed_callback,
    };

    let mut runtime = match udp::runtime::UdpRuleRuntime::start(cfg, cancel.clone()).await {
        Ok(rt) => rt,
        Err(udp::runtime::UdpRuntimeStartError::BindFailed { port, error: _ }) => {
            let reason = if range_size == 1 {
                "port_in_use".to_string()
            } else {
                format!("port_in_use:{port}")
            };
            let _ = status_tx.send(RuleStatusEvent::Failed { rule_id: rule.rule_id, reason }).await;
            return;
        }
    };

    info!(
        event = "rule.activated",
        rule_id = %rule.rule_id,
        listen_port = listen_start,
        listen_port_end = listen_end,
        range_size = range_size,
        protocol = "udp",
        target = %format!("{}:{}-{}", rule.target_host, target_first, rule.target_range.end()),
    );
    let _ = status_tx.send(RuleStatusEvent::Activated { rule_id: rule.rule_id }).await;

    // Block until cancel or supervisor-reported failure.
    tokio::select! {
        _ = cancel.cancelled() => {}
        Some(reason) = failed_rx.recv() => {
            let _ = status_tx.send(RuleStatusEvent::Failed { rule_id: rule.rule_id, reason }).await;
        }
    }

    let _outcome = runtime.shutdown().await;

    info!(
        event = "rule.deactivated",
        rule_id = %rule.rule_id,
        protocol = "udp",
    );
```

- [ ] **Step 2: Delete v0.4 functions**

In `crates/portunus-forwarder/src/forwarder/udp/mod.rs`:
- Delete `pub async fn run_listener<R: Resolve + 'static>(...)` (line 459 to end of function)
- Delete `pub async fn run_listener_multi_target<R: Resolve + 'static>(...)` (line 64 to end)
- Delete `relay_existing_flow`, `relay_new_flow`, `build_or_lookup_flow`, `spawn_admit_guard`, `spawn_reply_pump` if no other callers
- Delete `pub mod table;` — replaced by `registry`
- Delete `crates/portunus-forwarder/src/forwarder/udp/table.rs` if it has no other callers

Sub-agent: use `cargo build` errors as a TODO list. Each error points at code that still references the deleted symbol; either delete the caller or rewire it to the new types.

- [ ] **Step 3: Compile check + commit**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder 2>&1 | tail -25
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-client 2>&1 | tail -10
git add -A
git commit -m "feat(014): wire UdpRuleRuntime into forwarder::run_udp; remove v0.4 path

Multi-target UDP (failover_path::run_udp) continues unchanged."
```

---

## Phase 9: Stats correction (`active_flows`)

### Task 9.1: Replace per-listener `set_active_flows` with registry-driven snapshot

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/stats.rs` if needed
- Modify: `crates/portunus-forwarder/src/forwarder/udp/runtime.rs` to expose `active_flows()` or have runtime periodically write into `stats`

**FR-014**: `active_flows` MUST equal `registry.len()`.

- [ ] **Step 1: Decide reporting mechanism**

Two options:
(a) UdpRuleRuntime spawns a tiny ticker task that calls `stats.set_active_flows(registry.len())` every 1s.
(b) Stats reporter in `control.rs::send_stats_report` reads `runtime.registry().len()` directly when collecting per-rule stats.

(a) is simpler and doesn't change call-site shapes. Use (a).

- [ ] **Step 2: Spawn a stats-pump task in UdpRuleRuntime::start**

Add to the supervisor's spawn list as a 4th role `Role::StatsPump` that loops `tokio::time::sleep(1s)` + `stats.set_active_flows(registry.len() as u32)`. Exits on `rule_cancel.cancelled()`.

- [ ] **Step 3: Commit**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::udp 2>&1 | tail -15
git add -A
git commit -m "feat(014): active_flows reflects registry.len() (FR-014)

v1.4 last-writer-wins behaviour replaced by per-rule stats-pump."
```

---

## Phase 10: Integration tests

### Task 10.1: Delete v0.4 per-listener round-trip tests

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/udp/mod.rs` (delete tests module, lines 928-1398)

- [ ] **Step 1: Inventory**

```bash
grep -n '^\s*#\[tokio::test\]\|^\s*fn ' crates/portunus-forwarder/src/forwarder/udp/mod.rs
```

Identify tests coupled to `UdpFlowTable` (per-listener), `spawn_reply_pump`, or `build_or_lookup_flow`. These get deleted.

- [ ] **Step 2: Delete or relocate**

Any test whose subject is purely behavioural (round-trip, two-source isolation, overflow on cap, idle eviction) → rewrite under Tasks 10.2-10.5.

Any test whose subject is implementation-coupled (per-listener table len, etc.) → delete.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "test(014): drop v0.4 impl-coupled UDP unit tests

Behavioural coverage rewrites follow."
```

### Task 10.2: `udp_rule_round_trip_byte_equal`

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/udp/mod.rs` or new `udp/integration_tests.rs`

- [ ] **Step 1: Write the test**

End-to-end inside a single tokio runtime: spawn an upstream echo server, build a `UdpRuleRuntime` with a single listener port, send a payload through it, verify exact byte match on the round trip.

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_rule_round_trip_byte_equal() {
    // 1. echo server on ephemeral upstream port
    let echo = tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    let echo_h = tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let Ok((n, src)) = echo.recv_from(&mut buf).await else { return; };
            let _ = echo.send_to(&buf[..n], src).await;
        }
    });

    // 2. build a runtime targeting that echo
    // (use helper to construct UdpRuntimeConfig with stub rate_limit/quota etc.)
    let (runtime, listen_port) = build_runtime_target(echo_addr).await;

    // 3. client sends 4 different payloads
    let client = tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
    let payloads: [&[u8]; 4] = [b"hello", b"world!", &[0u8; 1000], b""];
    for p in &payloads {
        client.send_to(p, ("127.0.0.1", listen_port)).await.unwrap();
        let mut buf = [0u8; 2048];
        let (n, _) = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv_from(&mut buf)).await.unwrap().unwrap();
        assert_eq!(&buf[..n], *p);
    }

    drop(runtime); // or call shutdown()
    echo_h.abort();
}
```

The `build_runtime_target` helper is a test fixture you write in a `#[cfg(test)]` module. Stub resolver returning `vec![echo_addr]`, stub rate_limit/quota = None, etc.

- [ ] **Step 2: Run + commit**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib udp_rule_round_trip 2>&1 | tail -15
git add -A
git commit -m "test(014): udp_rule_round_trip_byte_equal (replaces v0.4 round-trip)"
```

### Task 10.3: `udp_range_rule_cap_is_per_rule`

- [ ] **Step 1: Test**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_range_rule_cap_is_per_rule() {
    // Build a runtime with listen_ports = 9000..=9003 (4 ports), rule_cap = 3.
    // Send first datagrams from 4 distinct client source ports, each to a
    // different listen port. Assert: exactly 3 flows commit, the 4th is
    // dropped + dropped_overflow counter advances.
}
```

- [ ] **Step 2: Run + commit**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib udp_range_rule_cap_is_per_rule 2>&1 | tail -10
git add -A
git commit -m "test(014): udp_range_rule_cap_is_per_rule (SC-002)"
```

### Task 10.4: `udp_cross_listener_same_src_distinct_flows`

- [ ] **Step 1: Test**

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn udp_cross_listener_same_src_distinct_flows() {
    // Same client src sends to two listen ports of a 2-port range rule.
    // Assert: registry.len() == 2, both flows have distinct upstream sockets.
}
```

- [ ] **Step 2: Run + commit**

```bash
git add -A
git commit -m "test(014): udp_cross_listener_same_src_distinct_flows (FR-002)"
```

### Task 10.5: Rewrite `udp_overflow_on_cap`

- [ ] **Step 1: Test**

Single-port rule with `rule_cap = 2`. Send first datagrams from 3 distinct client sources. Assert 2 succeed, 1 dropped silently, `dropped_overflow_total` = 1.

- [ ] **Step 2: Commit**

```bash
git add -A
git commit -m "test(014): udp_overflow_on_cap rewritten for per-rule semantics"
```

---

## Phase 11: E2E tests (`portunus-e2e`)

### Task 11.1: Inspect existing UDP e2e

```bash
ls crates/portunus-e2e/tests/ | grep -i udp
grep -l 'udp' crates/portunus-e2e/tests/*.rs
```

Identify `udp_smoke.rs` or similar. Read it to learn the e2e fixture pattern (subprocess spawn? in-process? real socket?).

### Task 11.2: `udp_smoke_per_rule_cap`

- [ ] **Step 1: Add to e2e suite**

Following the existing fixture style, add a test that:
1. Pushes a UDP range rule listen=9000..=9003, cap=3
2. Sends first datagrams from 4 distinct clients to 4 different ports
3. Asserts 3 succeed, 1 drop. Inspect `rule-stats` HTTP and assert `flows_dropped_overflow_total = 1`.

- [ ] **Step 2: Commit**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-e2e udp_smoke_per_rule_cap 2>&1 | tail -10
git add -A
git commit -m "test(014/e2e): udp_smoke_per_rule_cap (SC-002 end-to-end)"
```

### Task 11.3: `udp_smoke_icmp_evict`

- [ ] **Step 1: Test sketch**

This e2e is hard on macOS (where the dev box is). The test:
1. Spawns a UDP echo subprocess as target
2. Pushes a UDP rule pointing at it
3. Sends datagrams; verifies replies arrive
4. Kills the echo subprocess
5. Sends one more datagram from client → triggers ICMP port unreachable
6. Asserts the flow is evicted within 2 RTTs (check `rule-stats` or wait until next-packet rebuild succeeds when echo is restarted)

If the test is too flaky on macOS due to ICMP delivery quirks, gate with `#[cfg(target_os = "linux")]` and document in the test.

- [ ] **Step 2: Commit**

```bash
git add -A
git commit -m "test(014/e2e): udp_smoke_icmp_evict (SC-005)

Linux-only — macOS ICMP delivery semantics make this flaky."
```

---

## Phase 12: Benchmark addition

### Task 12.1: Add `udp_high_flow_count` scenario

**Files:**
- Modify: `crates/portunus-forwarder/benches/udp_data_plane.rs`

- [ ] **Step 1: Read existing bench**

```bash
sed -n '1,80p' crates/portunus-forwarder/benches/udp_data_plane.rs
```

- [ ] **Step 2: Add scenario**

A criterion bench group `udp_high_flow_count` that:
1. Spawns 1000 source-port clients
2. Drives 1 datagram from each within the idle window
3. Measures peak RSS delta (read `/proc/self/status` `VmRSS` if on Linux)
4. Reports the delta as a metric

This is **not** a CI gate (Linux perf host only). Sub-agent: add `cfg!(target_os = "linux")` gate around the RSS measurement.

- [ ] **Step 3: Build + commit**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo bench -p portunus-forwarder --bench udp_data_plane --no-run 2>&1 | tail -5
git add -A
git commit -m "bench(014): udp_high_flow_count scenario for SC-001a"
```

---

## Phase 13: Documentation

### Task 13.1: CHANGELOG.md release notes

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add an Unreleased section heading + entry**

```markdown
## Unreleased — UDP runtime correction (014)

### Behavior corrections

- **UDP rule flow cap is now true per-rule (was per-port-listener).**
  Previously, `udp_max_flows_per_rule` was applied independently to
  each port of a range rule. A 100-port range rule with cap=1024
  actually permitted up to 102 400 concurrent flows. The cap is now
  correctly applied across the entire rule.

  Migration:
  - Single-port UDP rules: no change in effective capacity
  - Range UDP rules: effective capacity drops by a factor of
    `range_size`. Operators relying on the inflated capacity should
    either (a) raise `udp_max_flows_per_rule` proportionally, or (b)
    split the range into smaller rules. Note: `udp_max_flows_per_rule`
    has an upper bound of 65535; if `cap × range_size` previously
    exceeded that, the rule MUST be split.

- **`portunus_rule_active_flows` now reports the true rule-wide live
  UDP flow count.** Previously the gauge could under-report for range
  rules because each port listener overwrote a shared `AtomicU32`
  independently (last-writer-wins).

- **UDP flows may be evicted early on ICMP errors.** Upstream sockets
  are now `connect()`-ed to the chosen target, enabling Linux to
  reflect ICMP `port unreachable`, `host unreachable`, and `network
  unreachable` errors back to the client process. Affected flows are
  evicted immediately (well before `idle_window`); the next datagram
  rebuilds the flow, which may select a different multi-A target.

### Performance

- Per-rule UDP receive-buffer memory dropped from `O(flows) × 64 KiB`
  to `O(1) × 64 KiB`. A 1000-flow workload now uses ~64 KiB of
  receive buffer instead of ~64 MiB (factor scales with flow count).

### Removed

- v0.4's first-packet send-level multi-A fallback. Multi-A is now
  performed at the `connect()` seam during flow creation; once a
  flow is committed, its target is locked. Mid-flow fallback was
  removed because the semantics on connected upstream sockets are
  ambiguous and the existing ICMP-driven eviction provides
  equivalent coarse-grained failover.
- Tracing events `rule.udp_send_to_fallback` and
  `rule.udp_send_to_exhausted` (consequence of the above).

### Added tracing events

`rule.udp_upstream_connect_failed`, `rule.udp_addflow_dropped`,
`rule.udp_flow_evicted_icmp`, `rule.udp_reply_wouldblock`,
`rule.udp_emsgsize`, `rule.udp_runtime_started`,
`rule.udp_shutdown_unexpected_exit`. No new Prometheus metrics.
```

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs(014): CHANGELOG entry for UDP per-rule correction"
```

### Task 13.2: Refresh CLAUDE.md active-feature pointer

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Update the "Active feature" block**

Find and replace the v0.12 / 012-tcp-zero-copy-splice block (lines 199-260ish in CLAUDE.md) with a 014 pointer. Preserve the inherited-baselines list and append v1.4 (013 traffic quotas) + v1.5/x (014 this work) at the top.

Sub-agent: read CLAUDE.md "Active feature:" section first to match its existing tone.

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs(014): point CLAUDE.md active-feature block at 014"
```

---

## Phase 14: Local verification gate

### Task 14.1: Full workspace test + clippy

- [ ] **Step 1: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace 2>&1 | tail -30
PORTUNUS_SKIP_WEBUI=1 cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo fmt --all --check
```

- [ ] **Step 2: Fix any failures**

For each failure: read the error, locate the root cause, fix it. If a test is flaky (timeouts), increase the wait window with a comment explaining why. **Never** `#[ignore]` a failing test without writing down what's wrong.

- [ ] **Step 3: Commit fixes**

If fixes were necessary, commit them with `fix(014): ...` messages.

---

## Phase 15: VPS verification (SC-001a, SC-004, SC-005)

VPS: `207.241.173.217`, user `root`, port 22. Per goal: real Linux host needed for ICMP and meaningful throughput numbers.

### Task 15.1: Prep VPS

- [ ] **Step 1: Verify SSH access**

```bash
ssh -o StrictHostKeyChecking=accept-new root@207.241.173.217 'uname -a && rustc --version 2>/dev/null || echo "rustc missing"'
```

- [ ] **Step 2: Install rustup if missing**

```bash
ssh root@207.241.173.217 'command -v rustup || curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable'
```

- [ ] **Step 3: Push the working tree to VPS**

Use `rsync` (preserves .git) since the goal forbids `git push`:

```bash
rsync -avz --delete --exclude target --exclude node_modules --exclude webui/dist \
  ./ root@207.241.173.217:/root/forward-rs/
```

### Task 15.2: Build on VPS

```bash
ssh root@207.241.173.217 'cd /root/forward-rs && source $HOME/.cargo/env && PORTUNUS_SKIP_WEBUI=1 cargo build --release -p portunus-forwarder -p portunus-client 2>&1 | tail -15'
```
Expected: `Finished release ...`

### Task 15.3: Run new integration tests on VPS

```bash
ssh root@207.241.173.217 'cd /root/forward-rs && source $HOME/.cargo/env && PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --release udp_ 2>&1 | tail -30'
```
Expected: all `udp_*` tests pass.

### Task 15.4: Run e2e ICMP evict on VPS

```bash
ssh root@207.241.173.217 'cd /root/forward-rs && source $HOME/.cargo/env && PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-e2e --release udp_smoke_icmp_evict 2>&1 | tail -20'
```
Expected: PASS (SC-005 — ICMP evict latency ≤ 2 × RTT).

### Task 15.5: Run benchmark + record SC-001a baseline

```bash
ssh root@207.241.173.217 'cd /root/forward-rs && source $HOME/.cargo/env && PORTUNUS_SKIP_WEBUI=1 cargo bench -p portunus-forwarder --bench udp_data_plane udp_high_flow_count 2>&1 | tail -30'
```

Record:
- RSS delta at 1000 concurrent flows (target: ≪ 64 MiB — SC-001a)
- Existing scenarios stay within ±5 % (SC-004)

Capture output to a file:

```bash
ssh root@207.241.173.217 'cd /root/forward-rs && source $HOME/.cargo/env && PORTUNUS_SKIP_WEBUI=1 cargo bench -p portunus-forwarder --bench udp_data_plane 2>&1' > /tmp/vps-bench.txt
```

Bring back locally and store under `specs/014-udp-centralized-demux/perf-artifacts/` (matches the v0.12 splice convention).

### Task 15.6: Document VPS results

**Files:**
- Create: `specs/014-udp-centralized-demux/perf-artifacts/vps-results-YYYY-MM-DD.md`

Include:
- VPS hardware (`lscpu` output snippet, `uname -a`)
- Test invocations
- Numbers vs spec SC-001a / SC-004 / SC-005 expectations
- Pass/fail per SC

```bash
git add specs/014-udp-centralized-demux/perf-artifacts/
git commit -m "perf(014): VPS verification results"
```

---

## Final Checklist

Before declaring complete, verify:

- [ ] Branch `014-udp-centralized-demux` checked out (no worktree per goal)
- [ ] All FR-001..FR-017 have at least one task implementing or testing them (spec self-review against this plan)
- [ ] SC-001a measured on VPS; receive-buffer memory is O(1)/rule
- [ ] SC-002 verified by `udp_range_rule_cap_is_per_rule`
- [ ] SC-003 verified by `cargo test --workspace` green
- [ ] SC-004 measured: existing benches within ±5 % on VPS
- [ ] SC-005 verified by `udp_smoke_icmp_evict` on VPS
- [ ] SC-006 (channel saturation) optionally tested; spec allows informational
- [ ] CHANGELOG entry written
- [ ] No `git push` performed (goal forbids it)
- [ ] `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test --workspace` all green
