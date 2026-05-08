//! SNI-mode TCP listener. Spec 009-tls-sni-routing data-model.md §2.3.
//!
//! Owns the bound `TcpListener`, the `watch::Receiver<Arc<SniRoutingTable>>`,
//! the cancellation token, and an `Arc<SniListenerCounters>`. On each
//! accept it peeks the ClientHello, looks up the SNI in a snapshot of
//! the routing table, and dispatches into the existing `proxy::proxy`.
//!
//! NOTE (Phase 1 — T002 scaffold): bodies stubbed. T040 in Phase 3
//! fills in the real listener.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

#[derive(Default, Debug)]
pub struct SniListenerCounters {
    pub miss: AtomicU64,
    pub parse_failures: AtomicU64,
}

pub struct SniListener {
    // Filled in T040.
    pub(crate) _counters: Arc<SniListenerCounters>,
}
