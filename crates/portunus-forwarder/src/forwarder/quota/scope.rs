//! 013-traffic-quotas v1.4.0 client registry: maps `user_id` →
//! `Arc<QuotaHandle>`. Pattern mirrors v0.11
//! `OwnerRateLimitScopeManager` (forwarder/rate_limit/scope.rs).
//!
//! The accept loop and copy hooks look up by `rule.owner_user_id`;
//! the spec's PK is `(user_id, client_name)` but the `client_name`
//! part is implicit ("this client" — known from boot config), so
//! the registry only carries `user_id` as the key.
//!
//! `install` is idempotent. If a handle already exists, its state is
//! `replace()`d atomically in-place so any in-flight forwarder that
//! cloned the `Arc` observes the new budget immediately. Removing a
//! quota only deregisters the entry; any forwarder still holding the
//! `Arc` continues using the last installed state until it finishes —
//! the next `lookup` returns `None`.

#![allow(
    dead_code,
    reason = "Consumed by D3 control-loop dispatch + E-phase data-plane hooks."
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use super::{QuotaHandle, QuotaState};

#[derive(Default)]
pub struct QuotaScopeManager {
    inner: RwLock<HashMap<String, Arc<QuotaHandle>>>,
}

impl QuotaScopeManager {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn lookup(&self, user_id: &str) -> Option<Arc<QuotaHandle>> {
        self.inner.read().ok().and_then(|m| m.get(user_id).cloned())
    }

    /// Insert or replace handle state atomically.
    pub fn install(&self, user_id: &str, client_name: &str, state: QuotaState) -> Arc<QuotaHandle> {
        let mut m = self.inner.write().expect("quota scope poisoned");
        if let Some(existing) = m.get(user_id) {
            existing.replace(state);
            return Arc::clone(existing);
        }
        let handle = Arc::new(QuotaHandle::new(
            user_id.to_string(),
            client_name.to_string(),
            state,
        ));
        m.insert(user_id.to_string(), Arc::clone(&handle));
        handle
    }

    pub fn remove(&self, user_id: &str) -> Option<Arc<QuotaHandle>> {
        self.inner.write().ok().and_then(|mut m| m.remove(user_id))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().map(|m| m.len()).unwrap_or(0)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarder::quota::ConsumeOutcome;

    fn state(remaining: i64) -> QuotaState {
        QuotaState {
            monthly_bytes: 1_000,
            budget_remaining_bytes: remaining,
            exhausted: false,
        }
    }

    #[test]
    fn lookup_returns_installed_handle() {
        let m = QuotaScopeManager::new();
        m.install("alice", "edge-01", state(500));
        let h = m.lookup("alice").unwrap();
        assert_eq!(h.consume(200), ConsumeOutcome::Granted);
        assert_eq!(h.remaining(), 300);
    }

    #[test]
    fn lookup_missing_returns_none() {
        let m = QuotaScopeManager::new();
        assert!(m.lookup("ghost").is_none());
    }

    #[test]
    fn install_twice_updates_state_in_place_and_preserves_arc() {
        let m = QuotaScopeManager::new();
        let h1 = m.install("alice", "edge-01", state(100));
        let _ = h1.consume(50);
        // Raising the cap reseeds remaining + clears exhausted; the
        // returned Arc is the SAME allocation, so in-flight consumers
        // see the new state without losing their handle.
        let h2 = m.install("alice", "edge-01", state(1_000));
        assert!(Arc::ptr_eq(&h1, &h2));
        assert_eq!(h1.remaining(), 1_000);
        assert!(!h1.is_exhausted());
    }

    #[test]
    fn remove_drops_handle_from_registry() {
        let m = QuotaScopeManager::new();
        m.install("alice", "edge-01", state(500));
        let removed = m.remove("alice");
        assert!(removed.is_some());
        assert!(m.lookup("alice").is_none());
        assert!(m.is_empty());
    }
}
