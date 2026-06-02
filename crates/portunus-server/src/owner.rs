//! 011-rate-limiting-qos T027 — server-side per-owner cap service.
//!
//! Wraps the SQLite owner-cap store with:
//! - validation of incoming envelopes (delegated to
//!   `portunus_core::rate_limit::validate`),
//! - server-set `updated_at_unix_ms` stamping,
//! - a GC sweep that drops the row when the owner's last rule on
//!   the client is removed (data-model.md §1.3 lifecycle).
//!
//! Also holds the in-memory snapshot consumed by:
//! - the REST handlers (T028) to serve GET / list,
//! - the gRPC `Welcome` path (T029) to push the current envelope set
//!   on (re)connect of a v0.11 client.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use portunus_core::{ClientId, RateLimit, rate_limit::RateLimitError};
use thiserror::Error;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::store::owner_cap_store::{OwnerRateLimitRow, SqliteOwnerCapStore};
use crate::store::{Store, StoreError};

/// Errors returned to operator-API callers. Mapped 1:1 to the HTTP
/// codes documented in `contracts/operator-api.md` §6.
#[derive(Debug, Error)]
pub enum OwnerCapError {
    /// Cap envelope failed validation (cap = 0, burst-without-rate,
    /// burst out of range, concurrent_connections_burst set). The
    /// REST handler renders this as `400 validation.rate_limit_*`.
    #[error("invalid_envelope: {0}")]
    InvalidEnvelope(#[from] RateLimitError),

    /// Capability gate — the client's last reported `Hello.client_version`
    /// is below `0.11.0`. REST handler renders this as
    /// `422 rate_limit_unsupported_by_client`. Detection lives in the
    /// gRPC service / capability helper; this error is what the
    /// caller surfaces.
    #[error("rate_limit_unsupported_by_client")]
    UnsupportedByClient,

    /// Underlying SQLite I/O error. REST handler renders `500
    /// internal_error`.
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

/// In-memory cache of every persisted `(client, owner) → envelope`
/// row. Hydrated at startup from the SQLite store; mutated in
/// lockstep with the store on every `upsert` / `delete`. The gRPC
/// `Welcome` path (T029) reads this snapshot to decide which
/// `OwnerRateLimitUpdate` pushes to emit on (re)connect; no extra
/// SQLite read sits on the connect critical path.
type Snapshot = HashMap<ClientId, HashMap<String, OwnerRateLimitRow>>;

/// High-level façade over the SQLite owner-cap store. Cheap to
/// clone — internal `Arc`.
#[derive(Clone, Debug)]
pub struct OwnerCapService {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    store: SqliteOwnerCapStore,
    cache: RwLock<Snapshot>,
}

impl OwnerCapService {
    /// Build a fresh service. Hydrates the in-memory snapshot from
    /// SQLite. Boot path failures bubble out as `StoreError` so the
    /// server `serve.rs` can refuse startup the same way it does for
    /// every other store hydration.
    pub fn open(store: Arc<Store>) -> Result<Self, StoreError> {
        let cap_store = SqliteOwnerCapStore::new(store);
        let mut snapshot: Snapshot = HashMap::new();
        for row in cap_store.list_all()? {
            snapshot
                .entry(row.client_id)
                .or_default()
                .insert(row.owner_id.clone(), row);
        }
        info!(
            event = "owner_cap.hydrated",
            owners = snapshot.values().map(HashMap::len).sum::<usize>(),
            clients = snapshot.len(),
        );
        Ok(Self {
            inner: Arc::new(Inner {
                store: cap_store,
                cache: RwLock::new(snapshot),
            }),
        })
    }

    /// Upsert an envelope. Validates the cap shape, stamps
    /// `updated_at_unix_ms = now()`, persists to SQLite, and updates
    /// the in-memory cache. Returns the persisted row so the caller
    /// can echo it back to the REST client.
    pub async fn upsert(
        &self,
        client_id: &ClientId,
        owner_id: &str,
        rate_limit: RateLimit,
    ) -> Result<OwnerRateLimitRow, OwnerCapError> {
        portunus_core::rate_limit::validate(&rate_limit)?;
        let updated_at_unix_ms = now_unix_ms();
        self.inner
            .store
            .upsert(client_id, owner_id, &rate_limit, updated_at_unix_ms)?;
        let row = OwnerRateLimitRow {
            client_id: *client_id,
            owner_id: owner_id.to_string(),
            rate_limit,
            updated_at_unix_ms,
        };
        self.inner
            .cache
            .write()
            .await
            .entry(*client_id)
            .or_default()
            .insert(owner_id.to_string(), row.clone());
        info!(
            event = "owner_cap.upserted",
            client_id = %client_id,
            owner_id = %owner_id,
            updated_at_unix_ms,
        );
        Ok(row)
    }

    /// Idempotent delete. Returns `true` when a row existed and was
    /// removed; `false` when nothing was there. Cache and store stay
    /// in lockstep.
    pub async fn delete(
        &self,
        client_id: &ClientId,
        owner_id: &str,
    ) -> Result<bool, OwnerCapError> {
        let removed = self.inner.store.delete(client_id, owner_id)?;
        if removed {
            let mut guard = self.inner.cache.write().await;
            if let Some(per_client) = guard.get_mut(client_id) {
                per_client.remove(owner_id);
                if per_client.is_empty() {
                    guard.remove(client_id);
                }
            }
            info!(
                event = "owner_cap.deleted",
                client_id = %client_id,
                owner_id = %owner_id,
            );
        }
        Ok(removed)
    }

    /// Snapshot of one specific envelope. Used by `GET
    /// /v1/clients/{id}/owners/{owner_id}/rate-limit`.
    pub async fn get(&self, client_id: &ClientId, owner_id: &str) -> Option<OwnerRateLimitRow> {
        self.inner
            .cache
            .read()
            .await
            .get(client_id)
            .and_then(|per_client| per_client.get(owner_id))
            .cloned()
    }

    /// Snapshot of every envelope under a client. Used by the
    /// `GET /v1/clients/{id}/owners` listing and by the gRPC
    /// `Welcome` push path (T029).
    pub async fn list_for_client(&self, client_id: &ClientId) -> Vec<OwnerRateLimitRow> {
        self.inner
            .cache
            .read()
            .await
            .get(client_id)
            .map(|per_client| per_client.values().cloned().collect())
            .unwrap_or_default()
    }

    /// GC sweep: when the owner's last rule on `client_name` is
    /// removed, drop the cap envelope (data-model §1.3 lifecycle).
    /// `rules_remaining` is the post-removal count of rules under
    /// `(client_name, owner_id)` — caller computes this from the
    /// in-memory rule store. A non-zero count is a no-op.
    pub async fn gc_after_rule_removed(
        &self,
        client_id: &ClientId,
        owner_id: &str,
        rules_remaining: usize,
    ) -> Result<bool, OwnerCapError> {
        if rules_remaining > 0 {
            return Ok(false);
        }
        match self.delete(client_id, owner_id).await {
            Ok(removed) => {
                if removed {
                    info!(
                        event = "owner_cap.gc_swept",
                        client_id = %client_id,
                        owner_id = %owner_id,
                    );
                }
                Ok(removed)
            }
            Err(e) => {
                warn!(
                    event = "owner_cap.gc_failed",
                    client_id = %client_id,
                    owner_id = %owner_id,
                    error = %e,
                );
                Err(e)
            }
        }
    }

    /// Owners that hold an envelope on at least one client. Used by
    /// the metrics readout (T032) to compute the cardinality envelope
    /// without re-walking SQLite.
    #[allow(dead_code)] // wired up by future operator-debug surface
    pub async fn known_owners(&self) -> HashSet<String> {
        let guard = self.inner.cache.read().await;
        let mut out = HashSet::new();
        for per_client in guard.values() {
            for owner_id in per_client.keys() {
                out.insert(owner_id.clone());
            }
        }
        out
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use tempfile::tempdir;

    fn open_service() -> OwnerCapService {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        std::mem::forget(dir);
        OwnerCapService::open(store).unwrap()
    }

    fn full_envelope() -> RateLimit {
        RateLimit {
            bandwidth_in_bps: Some(1_048_576),
            bandwidth_out_bps: Some(2_097_152),
            new_connections_per_sec: Some(50),
            concurrent_connections: Some(10),
            bandwidth_in_burst: None,
            bandwidth_out_burst: None,
            new_connections_burst: None,
        }
    }

    #[tokio::test]
    async fn t027_service_upsert_then_get_returns_envelope() {
        let svc = open_service();
        let client = ClientId::new();
        let row = svc
            .upsert(&client, "alice", full_envelope())
            .await
            .expect("upsert");
        assert_eq!(row.owner_id, "alice");
        assert_eq!(row.rate_limit.bandwidth_in_bps, Some(1_048_576));
        let fetched = svc.get(&client, "alice").await.expect("present");
        assert_eq!(fetched.rate_limit, row.rate_limit);
        assert_eq!(fetched.updated_at_unix_ms, row.updated_at_unix_ms);
    }

    #[tokio::test]
    async fn t027_service_delete_removes_from_cache_and_store() {
        let svc = open_service();
        let client = ClientId::new();
        svc.upsert(&client, "alice", full_envelope()).await.unwrap();
        assert!(svc.get(&client, "alice").await.is_some());
        let removed = svc.delete(&client, "alice").await.unwrap();
        assert!(removed);
        assert!(svc.get(&client, "alice").await.is_none());
        let again = svc.delete(&client, "alice").await.unwrap();
        assert!(!again, "second delete is idempotent");
    }

    #[tokio::test]
    async fn t027_service_validates_envelope() {
        let svc = open_service();
        let client = ClientId::new();
        let bad = RateLimit {
            bandwidth_in_bps: Some(0), // cap = 0 rejected
            ..Default::default()
        };
        let err = svc.upsert(&client, "alice", bad).await.unwrap_err();
        assert!(matches!(err, OwnerCapError::InvalidEnvelope(_)));
        // Store stays empty.
        assert!(svc.get(&client, "alice").await.is_none());
    }

    #[tokio::test]
    async fn t027_service_list_for_client_returns_owners_under_client() {
        let svc = open_service();
        let edge = ClientId::new();
        let core = ClientId::new();
        svc.upsert(&edge, "alice", full_envelope()).await.unwrap();
        svc.upsert(&edge, "bob", full_envelope()).await.unwrap();
        svc.upsert(&core, "alice", full_envelope()).await.unwrap();
        let edge_owners = svc.list_for_client(&edge).await;
        assert_eq!(edge_owners.len(), 2);
        let core_owners = svc.list_for_client(&core).await;
        assert_eq!(core_owners.len(), 1);
    }

    #[tokio::test]
    async fn t027_service_gc_sweeps_when_no_rules_remain() {
        let svc = open_service();
        let client = ClientId::new();
        svc.upsert(&client, "alice", full_envelope()).await.unwrap();
        // 1 rule remains — sweep is a no-op.
        let swept = svc
            .gc_after_rule_removed(&client, "alice", 1)
            .await
            .unwrap();
        assert!(!swept);
        assert!(svc.get(&client, "alice").await.is_some());
        // Last rule gone — sweep removes the cap envelope.
        let swept = svc
            .gc_after_rule_removed(&client, "alice", 0)
            .await
            .unwrap();
        assert!(swept);
        assert!(svc.get(&client, "alice").await.is_none());
    }

    #[tokio::test]
    async fn t027_service_gc_idempotent_when_envelope_missing() {
        let svc = open_service();
        let client = ClientId::new();
        // No envelope ever installed. Sweep with rules_remaining=0
        // returns false but does not error.
        let swept = svc
            .gc_after_rule_removed(&client, "ghost", 0)
            .await
            .unwrap();
        assert!(!swept);
    }

    #[tokio::test]
    async fn t027_service_hydrates_cache_from_sqlite_on_open() {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let client = ClientId::new();
        // Open svc, write, drop.
        {
            let svc = OwnerCapService::open(Arc::clone(&store)).unwrap();
            svc.upsert(&client, "alice", full_envelope()).await.unwrap();
        }
        // Re-open: cache must hydrate from the persisted row.
        let svc2 = OwnerCapService::open(Arc::clone(&store)).unwrap();
        assert!(svc2.get(&client, "alice").await.is_some());
        std::mem::forget(dir);
    }
}
