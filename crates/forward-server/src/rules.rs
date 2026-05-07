//! Server-side rule registry.
//!
//! Owns the authoritative state of every forwarding rule. The store is
//! purely in-memory (rules are not persisted across restarts — see
//! `data-model.md` § Rule, "Storage"). State transitions follow Q4 of the
//! clarifications: `Failed` is a terminal-ish state that blocks reuse of
//! `(client_name, listen_port)` until an explicit `remove-rule`.
//!
//! Range support (002-port-range-forward): rules may now span a
//! contiguous listen-port range. Single-port rules are the degenerate
//! case where `listen_port_end == None` (or equivalently
//! `listen_range().len() == 1`). Conflict detection covers
//! single↔single, single↔range, range↔range overlap symmetrically via
//! a per-client `BTreeMap<u16, RuleId>` index keyed on each rule's
//! listen-range start.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use forward_core::{ClientName, PortRange, PortRangeError, RuleId};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    Tcp,
}

impl Protocol {
    #[must_use]
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleState {
    Pending,
    Active,
    Failed { reason: String },
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    pub client_name: ClientName,
    /// Range start (inclusive). For single-port rules this is also the
    /// only port (`listen_port_end == None`).
    pub listen_port: u16,
    /// Range end (inclusive). `None` for single-port rules
    /// (preserves v0.1.0 persistence verbatim — FR-005).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_port_end: Option<u16>,
    pub target_host: String,
    pub target_port: u16,
    /// Range end on the target side (symmetric to `listen_port_end`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_port_end: Option<u16>,
    /// Address-family preference for DNS-target rules
    /// (003-domain-name-forward, FR-007). Absent → IPv4-first.
    /// `Some(true)` → IPv6-first. Silently ignored for IP-literal
    /// targets. Skipped on serialize when absent so v0.2.0 rule
    /// payloads stay byte-identical (FR-009 / FR-010).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer_ipv6: Option<bool>,
    pub protocol: Protocol,
    pub state: RuleState,
    pub created_at: DateTime<Utc>,
    pub last_state_change_at: DateTime<Utc>,
}

impl Rule {
    /// Listen-side range. For single-port rules this is a range of
    /// size 1 (`PortRange::single`).
    #[must_use]
    pub fn listen_range(&self) -> PortRange {
        match self.listen_port_end {
            Some(end) if end > self.listen_port => PortRange::new(self.listen_port, end)
                .unwrap_or_else(|_| PortRange::single(self.listen_port)),
            _ => PortRange::single(self.listen_port),
        }
    }

    /// Target-side range. Symmetric to `listen_range`. Currently
    /// unused on the server side (the gRPC handler reconstructs
    /// `target_range` from the proto), but kept for parity with
    /// `listen_range` and for future server-side validation.
    #[must_use]
    #[allow(dead_code)]
    pub fn target_range(&self) -> PortRange {
        match self.target_port_end {
            Some(end) if end > self.target_port => PortRange::new(self.target_port, end)
                .unwrap_or_else(|_| PortRange::single(self.target_port)),
            _ => PortRange::single(self.target_port),
        }
    }

    /// Number of listen ports in this rule (1 for single-port rules).
    /// Currently unused outside tests; surfaced for `--per-port`
    /// rendering helpers we expect to add.
    #[must_use]
    #[allow(dead_code)]
    pub fn range_size(&self) -> u32 {
        self.listen_range().len()
    }

    /// `true` iff the rule actually spans more than one port. Reserved
    /// for diagnostics that haven't shipped yet.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_range(&self) -> bool {
        self.range_size() > 1
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RuleStoreError {
    /// A pushed rule overlaps an existing `Active` or `Failed` rule on
    /// the same client. `offending_port` names one port that is in
    /// conflict (the first one inside the overlap region). The HTTP /
    /// CLI surfaces include this in the error message so operators can
    /// pinpoint the collision (FR-010, US4).
    #[error("port_in_use: port {offending_port} already in use")]
    PortInUse { offending_port: u16 },

    #[error("rule_not_found")]
    NotFound,

    #[error("invalid_state_transition")]
    InvalidTransition,

    /// Pushed range size exceeds the operator-configured cap (FR-008).
    #[error("exceeds_cap: requested={requested} cap={cap}")]
    ExceedsCap { requested: u32, cap: u32 },

    /// Range failed structural validation (inverted, length mismatch, etc.).
    #[error("range_invalid: {0}")]
    RangeInvalid(PortRangeError),
}

/// In-memory rule store. Cheap to clone (`Arc` internal); thread-safe via
/// `tokio::sync::RwLock`.
#[derive(Debug, Clone, Default)]
pub struct ServerRuleStore {
    inner: Arc<RwLock<Inner>>,
    next_id: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct Inner {
    rules: HashMap<RuleId, Rule>,
    /// Per-client interval index keyed on each rule's listen-range
    /// `start` port. Walks `range(..=candidate.end)` to find candidate
    /// overlaps in O(log N + matches). Tracks rules in `Active` or
    /// `Failed` state per Q4 (002-port-range-forward extends the same
    /// semantics to ranges).
    by_client_listen_start: HashMap<ClientName, BTreeMap<u16, RuleId>>,
}

impl ServerRuleStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a single-port rule (v0.1.0 compat shim). Equivalent to
    /// [`push_range`] with `PortRange::single` on both sides. Kept as
    /// a convenience for legacy tests and future single-port callers.
    #[allow(dead_code)]
    pub async fn push(
        &self,
        client_name: ClientName,
        listen_port: u16,
        target_host: String,
        target_port: u16,
        protocol: Protocol,
        prefer_ipv6: Option<bool>,
    ) -> Result<Rule, RuleStoreError> {
        self.push_range(
            client_name,
            PortRange::single(listen_port),
            target_host,
            PortRange::single(target_port),
            protocol,
            prefer_ipv6,
            // No cap enforcement on the legacy single-port path —
            // size 1 is always under any positive cap. We pass
            // u32::MAX so callers that don't know the cap (tests,
            // legacy paths) aren't artificially blocked.
            u32::MAX,
        )
        .await
    }

    /// Push a (potentially range) rule. Validates structure, enforces
    /// the configured cap, and rejects overlaps with any existing
    /// `Active`/`Failed` rule on the same client.
    pub async fn push_range(
        &self,
        client_name: ClientName,
        listen: PortRange,
        target_host: String,
        target: PortRange,
        protocol: Protocol,
        prefer_ipv6: Option<bool>,
        range_cap: u32,
    ) -> Result<Rule, RuleStoreError> {
        // Structural validation (length match etc.).
        let (listen, target) =
            PortRange::pair(listen, target).map_err(RuleStoreError::RangeInvalid)?;

        let size = listen.len();
        if size > range_cap {
            return Err(RuleStoreError::ExceedsCap {
                requested: size,
                cap: range_cap,
            });
        }

        let mut guard = self.inner.write().await;

        // Conflict check via the per-client interval index. We walk
        // every entry whose `start <= candidate.end` and inspect the
        // associated rule. Any rule whose listen_range overlaps the
        // candidate AND is in Active/Failed state blocks the push.
        if let Some(index) = guard.by_client_listen_start.get(&client_name) {
            for (_start, existing_id) in index.range(..=listen.end()) {
                let Some(existing) = guard.rules.get(existing_id) else {
                    continue;
                };
                let existing_range = existing.listen_range();
                if existing_range.overlaps(listen)
                    && matches!(existing.state, RuleState::Active | RuleState::Failed { .. })
                {
                    let offending = listen.start().max(existing_range.start());
                    return Err(RuleStoreError::PortInUse {
                        offending_port: offending,
                    });
                }
            }
        }

        let id = RuleId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let now = Utc::now();
        let listen_port_end = if listen.len() > 1 {
            Some(listen.end())
        } else {
            None
        };
        let target_port_end = if target.len() > 1 {
            Some(target.end())
        } else {
            None
        };
        let rule = Rule {
            id,
            client_name: client_name.clone(),
            listen_port: listen.start(),
            listen_port_end,
            target_host,
            target_port: target.start(),
            target_port_end,
            prefer_ipv6,
            protocol,
            state: RuleState::Pending,
            created_at: now,
            last_state_change_at: now,
        };
        guard
            .by_client_listen_start
            .entry(client_name)
            .or_default()
            .insert(listen.start(), id);
        guard.rules.insert(id, rule.clone());
        Ok(rule)
    }

    pub async fn mark_active(&self, id: RuleId) -> Result<(), RuleStoreError> {
        let mut guard = self.inner.write().await;
        let rule = guard.rules.get_mut(&id).ok_or(RuleStoreError::NotFound)?;
        match rule.state {
            RuleState::Pending => {
                rule.state = RuleState::Active;
                rule.last_state_change_at = Utc::now();
                Ok(())
            }
            _ => Err(RuleStoreError::InvalidTransition),
        }
    }

    pub async fn mark_failed(&self, id: RuleId, reason: String) -> Result<(), RuleStoreError> {
        let mut guard = self.inner.write().await;
        let rule = guard.rules.get_mut(&id).ok_or(RuleStoreError::NotFound)?;
        match rule.state {
            RuleState::Pending => {
                rule.state = RuleState::Failed { reason };
                rule.last_state_change_at = Utc::now();
                Ok(())
            }
            _ => Err(RuleStoreError::InvalidTransition),
        }
    }

    /// Remove the rule and free its conflict-index entry. Returns
    /// `NotFound` if the id is unknown — callers (the operator CLI)
    /// map that to exit 8.
    pub async fn remove(&self, id: RuleId) -> Result<Rule, RuleStoreError> {
        let mut guard = self.inner.write().await;
        let rule = guard.rules.remove(&id).ok_or(RuleStoreError::NotFound)?;
        if let Some(index) = guard.by_client_listen_start.get_mut(&rule.client_name) {
            index.remove(&rule.listen_port);
            if index.is_empty() {
                guard.by_client_listen_start.remove(&rule.client_name);
            }
        }
        Ok(rule)
    }

    pub async fn get(&self, id: RuleId) -> Option<Rule> {
        self.inner.read().await.rules.get(&id).cloned()
    }

    /// Snapshot of every rule. `client_filter` narrows by owner.
    pub async fn list(&self, client_filter: Option<&ClientName>) -> Vec<Rule> {
        let guard = self.inner.read().await;
        let mut out: Vec<Rule> = guard
            .rules
            .values()
            .filter(|r| client_filter.is_none_or(|c| &r.client_name == c))
            .cloned()
            .collect();
        out.sort_by_key(|r| r.id.0);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn name(s: &str) -> ClientName {
        ClientName::from_str(s).unwrap()
    }

    async fn push_one(store: &ServerRuleStore) -> Rule {
        store
            .push(
                name("edge-01"),
                18080,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn push_initial_state_is_pending() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        assert!(matches!(r.state, RuleState::Pending));
        assert_eq!(r.client_name, name("edge-01"));
        assert_eq!(r.listen_port, 18080);
        assert_eq!(r.listen_port_end, None);
        assert_eq!(r.target_port_end, None);
        assert_eq!(r.range_size(), 1);
        assert!(!r.is_range());
    }

    #[tokio::test]
    async fn pending_can_become_active() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        store.mark_active(r.id).await.unwrap();
        let after = store.get(r.id).await.unwrap();
        assert!(matches!(after.state, RuleState::Active));
        assert!(after.last_state_change_at >= r.last_state_change_at);
    }

    #[tokio::test]
    async fn pending_can_become_failed() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        store.mark_failed(r.id, "port_in_use".into()).await.unwrap();
        let after = store.get(r.id).await.unwrap();
        match after.state {
            RuleState::Failed { reason } => assert_eq!(reason, "port_in_use"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn active_cannot_become_failed_or_active_again() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        store.mark_active(r.id).await.unwrap();
        assert!(matches!(
            store.mark_active(r.id).await,
            Err(RuleStoreError::InvalidTransition)
        ));
        assert!(matches!(
            store.mark_failed(r.id, "x".into()).await,
            Err(RuleStoreError::InvalidTransition)
        ));
    }

    #[tokio::test]
    async fn duplicate_active_blocks_push() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        store.mark_active(r.id).await.unwrap();
        let err = store
            .push(name("edge-01"), 18080, "x".into(), 1, Protocol::Tcp, None)
            .await
            .unwrap_err();
        match err {
            RuleStoreError::PortInUse { offending_port } => assert_eq!(offending_port, 18080),
            other => panic!("expected PortInUse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn failed_blocks_port_until_removed() {
        // Q4: Failed rules block port reuse until explicitly removed.
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        store.mark_failed(r.id, "port_in_use".into()).await.unwrap();
        // Re-push: blocked.
        assert!(matches!(
            store
                .push(name("edge-01"), 18080, "x".into(), 1, Protocol::Tcp, None)
                .await,
            Err(RuleStoreError::PortInUse { .. })
        ));
        // Remove releases the slot.
        store.remove(r.id).await.unwrap();
        let r2 = push_one(&store).await;
        assert_ne!(r.id, r2.id, "RuleId must change across removes");
    }

    #[tokio::test]
    async fn remove_unknown_returns_not_found() {
        let store = ServerRuleStore::new();
        assert!(matches!(
            store.remove(RuleId(999)).await,
            Err(RuleStoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn list_filters_by_client() {
        let store = ServerRuleStore::new();
        store
            .push(name("edge-a"), 1000, "x".into(), 1, Protocol::Tcp, None)
            .await
            .unwrap();
        store
            .push(name("edge-b"), 1001, "x".into(), 1, Protocol::Tcp, None)
            .await
            .unwrap();
        assert_eq!(store.list(None).await.len(), 2);
        assert_eq!(store.list(Some(&name("edge-a"))).await.len(), 1);
    }

    // --- T015 / T020: range push behavior ---

    async fn push_range(
        store: &ServerRuleStore,
        client: &str,
        l: u16,
        le: u16,
        t: u16,
        te: u16,
    ) -> Result<Rule, RuleStoreError> {
        store
            .push_range(
                name(client),
                PortRange::new(l, le).unwrap(),
                "10.0.0.5".into(),
                PortRange::new(t, te).unwrap(),
                Protocol::Tcp,
                None,
                1024,
            )
            .await
    }

    #[tokio::test]
    async fn push_range_rule_returns_single_id() {
        let store = ServerRuleStore::new();
        let r = push_range(&store, "edge-01", 30000, 30050, 30000, 30050)
            .await
            .unwrap();
        assert_eq!(r.range_size(), 51);
        assert!(r.is_range());
        assert_eq!(r.listen_port, 30000);
        assert_eq!(r.listen_port_end, Some(30050));
        assert_eq!(r.target_port_end, Some(30050));
        assert_eq!(store.list(None).await.len(), 1);
    }

    #[tokio::test]
    async fn push_range_assigns_pending_state() {
        let store = ServerRuleStore::new();
        let r = push_range(&store, "edge-01", 30000, 30050, 40000, 40050)
            .await
            .unwrap();
        assert!(matches!(r.state, RuleState::Pending));
    }

    #[tokio::test]
    async fn push_inverted_range_rejected_with_range_invalid() {
        // Constructing the PortRange itself fails — caller catches.
        // Here we exercise the store path: an explicit length-mismatch
        // is the structural error the store reports as RangeInvalid.
        let store = ServerRuleStore::new();
        let err = store
            .push_range(
                name("edge-01"),
                PortRange::new(30000, 30050).unwrap(),
                "10.0.0.5".into(),
                PortRange::new(40000, 40000).unwrap(), // length 1 vs 51
                Protocol::Tcp,
                None,
                1024,
            )
            .await
            .unwrap_err();
        match err {
            RuleStoreError::RangeInvalid(PortRangeError::LengthMismatch {
                listen_len,
                target_len,
            }) => {
                assert_eq!(listen_len, 51);
                assert_eq!(target_len, 1);
            }
            other => panic!("expected RangeInvalid(LengthMismatch), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn push_length_mismatch_rejected() {
        // Same shape as the above, just renaming the test for the spec
        // mapping (T015).
        let store = ServerRuleStore::new();
        let err = store
            .push_range(
                name("edge-01"),
                PortRange::new(30000, 30002).unwrap(),
                "h".into(),
                PortRange::new(40000, 40005).unwrap(),
                Protocol::Tcp,
                None,
                1024,
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            RuleStoreError::RangeInvalid(PortRangeError::LengthMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn push_exceeds_cap_rejected_with_named_limit() {
        let store = ServerRuleStore::new();
        let err = store
            .push_range(
                name("edge-01"),
                PortRange::new(30000, 30100).unwrap(),
                "h".into(),
                PortRange::new(40000, 40100).unwrap(),
                Protocol::Tcp,
                None,
                50,
            )
            .await
            .unwrap_err();
        match err {
            RuleStoreError::ExceedsCap { requested, cap } => {
                assert_eq!(requested, 101);
                assert_eq!(cap, 50);
            }
            other => panic!("expected ExceedsCap, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn push_range_size_1_behaves_like_single_port() {
        // Degenerate range with start == end → no listen_port_end.
        let store = ServerRuleStore::new();
        let r = store
            .push_range(
                name("edge-01"),
                PortRange::single(18080),
                "10.0.0.5".into(),
                PortRange::single(8080),
                Protocol::Tcp,
                None,
                1024,
            )
            .await
            .unwrap();
        assert_eq!(r.range_size(), 1);
        assert_eq!(r.listen_port_end, None);
        assert_eq!(r.target_port_end, None);
        assert!(!r.is_range());
    }

    // --- T049 (US4): overlap detection ---

    #[tokio::test]
    async fn range_overlapping_existing_range_returns_port_in_use_with_offending_port() {
        let store = ServerRuleStore::new();
        let a = push_range(&store, "edge-01", 30000, 30010, 40000, 40010)
            .await
            .unwrap();
        store.mark_active(a.id).await.unwrap();
        let err = push_range(&store, "edge-01", 30005, 30015, 40005, 40015)
            .await
            .unwrap_err();
        match err {
            RuleStoreError::PortInUse { offending_port } => {
                assert_eq!(offending_port, 30005);
            }
            other => panic!("expected PortInUse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn range_overlapping_existing_single_port_returns_port_in_use() {
        let store = ServerRuleStore::new();
        let single = push_one(&store).await; // listen_port = 18080
        store.mark_active(single.id).await.unwrap();
        let err = push_range(&store, "edge-01", 18075, 18085, 28075, 28085)
            .await
            .unwrap_err();
        match err {
            RuleStoreError::PortInUse { offending_port } => {
                // Overlap region is [18080, 18080]; offending port is
                // max(existing.start=18080, candidate.start=18075) = 18080.
                assert_eq!(offending_port, 18080);
            }
            other => panic!("expected PortInUse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn range_adjacent_no_overlap_succeeds() {
        // 30000-30010 is active; 30011-30020 is adjacent but disjoint.
        let store = ServerRuleStore::new();
        let a = push_range(&store, "edge-01", 30000, 30010, 40000, 40010)
            .await
            .unwrap();
        store.mark_active(a.id).await.unwrap();
        let b = push_range(&store, "edge-01", 30011, 30020, 40011, 40020)
            .await
            .unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(store.list(None).await.len(), 2);
    }

    #[tokio::test]
    async fn re_push_after_remove_succeeds() {
        // T034: removing a range frees ALL its ports for reuse.
        let store = ServerRuleStore::new();
        let a = push_range(&store, "edge-01", 30000, 30010, 40000, 40010)
            .await
            .unwrap();
        store.mark_active(a.id).await.unwrap();
        store.remove(a.id).await.unwrap();
        // A subset of the freed range should push successfully.
        let b = push_range(&store, "edge-01", 30005, 30008, 40005, 40008)
            .await
            .unwrap();
        assert_eq!(b.range_size(), 4);
    }

    #[tokio::test]
    async fn ranges_on_different_clients_do_not_conflict() {
        let store = ServerRuleStore::new();
        let a = push_range(&store, "edge-a", 30000, 30010, 40000, 40010)
            .await
            .unwrap();
        store.mark_active(a.id).await.unwrap();
        // Same listen ports on a DIFFERENT client should succeed.
        let b = push_range(&store, "edge-b", 30000, 30010, 40000, 40010)
            .await
            .unwrap();
        assert_ne!(a.id, b.id);
    }
}
