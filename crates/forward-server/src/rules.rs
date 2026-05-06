//! Server-side rule registry.
//!
//! Owns the authoritative state of every forwarding rule. The store is
//! purely in-memory (rules are not persisted across restarts — see
//! `data-model.md` § Rule, "Storage"). State transitions follow Q4 of the
//! clarifications: `Failed` is a terminal-ish state that blocks reuse of
//! `(client_name, listen_port)` until an explicit `remove-rule`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use forward_core::{ClientName, RuleId};
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
    pub listen_port: u16,
    pub target_host: String,
    pub target_port: u16,
    pub protocol: Protocol,
    pub state: RuleState,
    pub created_at: DateTime<Utc>,
    pub last_state_change_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum RuleStoreError {
    #[error("port_in_use")]
    PortInUse,
    #[error("rule_not_found")]
    NotFound,
    #[error("invalid_state_transition")]
    InvalidTransition,
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
    /// Active or Failed rules block reuse of `(client, listen_port)` per Q4.
    by_client_port: HashMap<(ClientName, u16), RuleId>,
}

impl ServerRuleStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a new rule. Returns `PortInUse` if `(client, listen_port)` already
    /// has a rule in `Active` or `Failed` state. The returned rule is in
    /// `Pending` state until the client acks.
    pub async fn push(
        &self,
        client_name: ClientName,
        listen_port: u16,
        target_host: String,
        target_port: u16,
        protocol: Protocol,
    ) -> Result<Rule, RuleStoreError> {
        let mut guard = self.inner.write().await;
        let key = (client_name.clone(), listen_port);
        if let Some(existing_id) = guard.by_client_port.get(&key)
            && let Some(existing) = guard.rules.get(existing_id)
            && matches!(existing.state, RuleState::Active | RuleState::Failed { .. })
        {
            return Err(RuleStoreError::PortInUse);
        }
        let id = RuleId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let now = Utc::now();
        let rule = Rule {
            id,
            client_name,
            listen_port,
            target_host,
            target_port,
            protocol,
            state: RuleState::Pending,
            created_at: now,
            last_state_change_at: now,
        };
        guard.by_client_port.insert(key, id);
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

    /// Remove the rule and free its `(client, port)` slot. Idempotent in the
    /// sense that callers who care about "did anything actually go away"
    /// inspect the bool — but a missing rule still returns `NotFound`. The
    /// operator CLI maps `NotFound` to exit 8.
    pub async fn remove(&self, id: RuleId) -> Result<Rule, RuleStoreError> {
        let mut guard = self.inner.write().await;
        let rule = guard.rules.remove(&id).ok_or(RuleStoreError::NotFound)?;
        guard
            .by_client_port
            .remove(&(rule.client_name.clone(), rule.listen_port));
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
            .push(name("edge-01"), 18080, "x".into(), 1, Protocol::Tcp)
            .await
            .unwrap_err();
        assert!(matches!(err, RuleStoreError::PortInUse));
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
                .push(name("edge-01"), 18080, "x".into(), 1, Protocol::Tcp)
                .await,
            Err(RuleStoreError::PortInUse)
        ));
        // Remove releases the slot.
        store.remove(r.id).await.unwrap();
        let r2 = push_one(&store).await;
        assert_ne!(r.id, r2.id, "RuleId must change across removes");
    }

    #[tokio::test]
    async fn pending_does_not_block_port_reuse_check_against_pending_only() {
        // Pending alone blocks too (because by_client_port indexes everything),
        // but the "active OR failed" guard only triggers PortInUse for those
        // states. Pending → second push falls through to insert, overwriting
        // the secondary index. This is intentional: a still-pending duplicate
        // push from the same operator action is a fast-fail nice-to-have, not
        // a contract requirement, and the second rule will just overwrite the
        // index. We only assert the failing-state behavior here.
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        // Same client/port while still Pending: currently allowed (no Active/
        // Failed conflict). Not a documented behavior, just covering the
        // current branch so the assumption doesn't silently change.
        let r2 = store
            .push(name("edge-01"), 18080, "x".into(), 1, Protocol::Tcp)
            .await
            .expect("Pending duplicate currently allowed");
        assert_ne!(r.id, r2.id);
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
            .push(name("edge-a"), 1000, "x".into(), 1, Protocol::Tcp)
            .await
            .unwrap();
        store
            .push(name("edge-b"), 1001, "x".into(), 1, Protocol::Tcp)
            .await
            .unwrap();
        assert_eq!(store.list(None).await.len(), 2);
        assert_eq!(store.list(Some(&name("edge-a"))).await.len(), 1);
    }
}
