//! In-memory SNI routing table. Spec 009-tls-sni-routing data-model.md §2.2.
//!
//! Built from a snapshot of `ClientRule`s sharing the same `(client,
//! listen_port)` SNI listener. Hot-path lookup; rebuilds happen in the
//! control task and are swapped into the listener via
//! `tokio::sync::watch::Sender::send_replace` (R-002 / R-007 in
//! `research.md`).
//!
//! NOTE (Phase 1 — T002 scaffold): bodies stubbed. T037 / T052 / T060
//! in Phases 3..5 fill in the real implementation.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use forward_core::RuleId;

#[derive(Debug, PartialEq, Eq)]
pub enum SniMatchKind {
    Exact,
    Wildcard,
    Fallback,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SniMatch {
    Hit { rule_id: RuleId, kind: SniMatchKind },
    Miss,
}

#[derive(Debug, Default)]
pub struct SniRoutingTable {
    /// Exact hostname → rule_id. O(1) lookup.
    pub(crate) exact: HashMap<String, RuleId>,
    /// Wildcard suffixes (the part *after* `*.`), sorted longest-first.
    pub(crate) wildcards: Vec<(String, RuleId)>,
    /// At most one fallback (sni_pattern = NULL).
    pub(crate) fallback: Option<RuleId>,
}

impl SniRoutingTable {
    /// Build a table from a snapshot of group members.
    /// T037: implement (exact only); T052/T060 layer wildcards + fallback.
    pub fn from_members(_members: &[(&Option<String>, RuleId)]) -> Arc<Self> {
        unimplemented!("009-tls-sni-routing T037")
    }

    /// Look up an SNI value (or its absence). Hot-path target.
    /// T037: implement (exact only); T053/T061 extend.
    pub fn lookup(&self, _sni: Option<&str>) -> SniMatch {
        unimplemented!("009-tls-sni-routing T037")
    }
}
