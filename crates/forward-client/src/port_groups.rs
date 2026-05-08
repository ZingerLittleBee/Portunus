//! `PortGroupManager` — single ownership root for SNI/legacy listeners.
//! Spec 009-tls-sni-routing data-model.md §2.4.
//!
//! Materialises `ClientRule`s into running listeners, keyed by
//! `listen_port`. Tracks the `(rule_id → listen_port)` reverse index
//! so `RuleUpdate(REMOVE)` (which carries only `rule_id`) can find its
//! group. Mode-Locked Lifetime invariant (R-004): a group's mode
//! (Legacy plain-TCP vs SNI dispatch) is fixed by its first member
//! and never flips while members exist.
//!
//! NOTE (Phase 1 — T003 scaffold): stubbed. T042 in Phase 3 fills in
//! the real manager.

#![allow(dead_code)]

use std::collections::HashMap;

use forward_core::RuleId;

#[derive(Debug)]
pub enum PortGroupError {
    /// A push tried to flip the mode of an active listener.
    /// Defensive client-side check; the server-side overlap rules
    /// (009-tls-sni-routing data-model.md §Overlap matrix) reject
    /// this before it reaches the wire.
    ModeChangeUnsupported,
    /// REMOVE referenced an unknown rule_id.
    UnknownRuleId(RuleId),
}

pub struct PortGroupManager {
    // Filled in T042.
    pub(crate) _groups: HashMap<u16, ()>,
    pub(crate) _rule_to_port: HashMap<RuleId, u16>,
}

impl PortGroupManager {
    pub fn new() -> Self {
        unimplemented!("009-tls-sni-routing T042")
    }
}
