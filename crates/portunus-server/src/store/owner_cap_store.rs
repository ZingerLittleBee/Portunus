//! 011-rate-limiting-qos T027 — SQLite CRUD for the per-owner cap table.
//!
//! `rate_limit_owner` is keyed `(client_name, owner_id)` and carries
//! the same eight optional cap fields as the per-rule envelope. Caps
//! are independently nullable; a row with every cap NULL is a
//! degenerate "uncapped" envelope (kept on purpose so an explicit
//! PUT with no body lands as a tombstone and the GC sweep / explicit
//! DELETE remove it).
//!
//! See `specs/011-rate-limiting-qos/data-model.md` §1.3 and the
//! migration `V005__add_rate_limit_columns.sql`.

use std::sync::Arc;

use portunus_core::{ClientName, RateLimit};
use rusqlite::params;

use crate::store::{Store, StoreError, map_rusqlite};

/// One row of the `rate_limit_owner` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerRateLimitRow {
    pub client_name: ClientName,
    pub owner_id: String,
    pub rate_limit: RateLimit,
    /// Server-set on every PUT. Wire-shape `updated_at_unix_ms` so the
    /// existing audit / Prometheus rendering paths don't need a
    /// timezone conversion.
    pub updated_at_unix_ms: u64,
}

/// SQLite-backed CRUD for the per-owner cap table.
#[derive(Clone, Debug)]
pub struct SqliteOwnerCapStore {
    store: Arc<Store>,
}

impl SqliteOwnerCapStore {
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Idempotent upsert: insert a fresh `(client, owner)` row or
    /// replace the existing one's caps and bump `updated_at_unix_ms`.
    pub fn upsert(
        &self,
        client_name: &ClientName,
        owner_id: &str,
        rl: &RateLimit,
        updated_at_unix_ms: u64,
    ) -> Result<(), StoreError> {
        self.store.with_write_tx(|tx| {
            tx.execute(
                "INSERT INTO rate_limit_owner (
                    client_name, owner_id,
                    rl_bandwidth_in_bps, rl_bandwidth_out_bps,
                    rl_new_connections_per_sec, rl_concurrent_connections,
                    rl_bandwidth_in_burst, rl_bandwidth_out_burst,
                    rl_new_connections_burst, updated_at_unix_ms
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(client_name, owner_id) DO UPDATE SET
                    rl_bandwidth_in_bps        = excluded.rl_bandwidth_in_bps,
                    rl_bandwidth_out_bps       = excluded.rl_bandwidth_out_bps,
                    rl_new_connections_per_sec = excluded.rl_new_connections_per_sec,
                    rl_concurrent_connections  = excluded.rl_concurrent_connections,
                    rl_bandwidth_in_burst      = excluded.rl_bandwidth_in_burst,
                    rl_bandwidth_out_burst     = excluded.rl_bandwidth_out_burst,
                    rl_new_connections_burst   = excluded.rl_new_connections_burst,
                    updated_at_unix_ms         = excluded.updated_at_unix_ms",
                params![
                    client_name.as_str(),
                    owner_id,
                    rl.bandwidth_in_bps,
                    rl.bandwidth_out_bps,
                    rl.new_connections_per_sec,
                    rl.concurrent_connections,
                    rl.bandwidth_in_burst,
                    rl.bandwidth_out_burst,
                    rl.new_connections_burst,
                    i64::try_from(updated_at_unix_ms).unwrap_or(i64::MAX),
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    /// Idempotent delete; returns `true` when the row existed.
    pub fn delete(&self, client_name: &ClientName, owner_id: &str) -> Result<bool, StoreError> {
        self.store.with_write_tx(|tx| {
            let n = tx
                .execute(
                    "DELETE FROM rate_limit_owner WHERE client_name = ? AND owner_id = ?",
                    params![client_name.as_str(), owner_id],
                )
                .map_err(map_rusqlite)?;
            Ok(n > 0)
        })
    }

    /// Snapshot one specific envelope. Returns `None` when no row
    /// exists; the caller maps that to an HTTP 404.
    pub fn get(
        &self,
        client_name: &ClientName,
        owner_id: &str,
    ) -> Result<Option<OwnerRateLimitRow>, StoreError> {
        self.store.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT client_name, owner_id,
                            rl_bandwidth_in_bps, rl_bandwidth_out_bps,
                            rl_new_connections_per_sec, rl_concurrent_connections,
                            rl_bandwidth_in_burst, rl_bandwidth_out_burst,
                            rl_new_connections_burst, updated_at_unix_ms
                     FROM rate_limit_owner
                     WHERE client_name = ? AND owner_id = ?",
                )
                .map_err(map_rusqlite)?;
            let mut rows = stmt
                .query(params![client_name.as_str(), owner_id])
                .map_err(map_rusqlite)?;
            let Some(row) = rows.next().map_err(map_rusqlite)? else {
                return Ok(None);
            };
            Ok(Some(row_to_envelope(row)?))
        })
    }

    /// Snapshot every envelope under `client_name`. Used by the
    /// `GET /v1/clients/{id}/owners` listing (T028) plus the gRPC
    /// reconnect-replay path (T029).
    pub fn list_for_client(
        &self,
        client_name: &ClientName,
    ) -> Result<Vec<OwnerRateLimitRow>, StoreError> {
        self.store.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT client_name, owner_id,
                            rl_bandwidth_in_bps, rl_bandwidth_out_bps,
                            rl_new_connections_per_sec, rl_concurrent_connections,
                            rl_bandwidth_in_burst, rl_bandwidth_out_burst,
                            rl_new_connections_burst, updated_at_unix_ms
                     FROM rate_limit_owner
                     WHERE client_name = ?
                     ORDER BY owner_id ASC",
                )
                .map_err(map_rusqlite)?;
            let mut rows = stmt
                .query(params![client_name.as_str()])
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_envelope(row)?);
            }
            Ok(out)
        })
    }

    /// Snapshot every envelope across every client. Used at server
    /// startup to hydrate the in-memory cache that the gRPC `Welcome`
    /// path reads to decide which `OwnerRateLimitUpdate` pushes to
    /// emit (T029).
    pub fn list_all(&self) -> Result<Vec<OwnerRateLimitRow>, StoreError> {
        self.store.with_conn(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT client_name, owner_id,
                            rl_bandwidth_in_bps, rl_bandwidth_out_bps,
                            rl_new_connections_per_sec, rl_concurrent_connections,
                            rl_bandwidth_in_burst, rl_bandwidth_out_burst,
                            rl_new_connections_burst, updated_at_unix_ms
                     FROM rate_limit_owner
                     ORDER BY client_name ASC, owner_id ASC",
                )
                .map_err(map_rusqlite)?;
            let mut rows = stmt.query([]).map_err(map_rusqlite)?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(map_rusqlite)? {
                out.push(row_to_envelope(row)?);
            }
            Ok(out)
        })
    }
}

fn row_to_envelope(row: &rusqlite::Row<'_>) -> Result<OwnerRateLimitRow, StoreError> {
    let client_name =
        ClientName::new(row.get::<_, String>(0).map_err(map_rusqlite)?).map_err(|e| {
            StoreError::Internal {
                message: format!("client_name: {e}"),
            }
        })?;
    let owner_id: String = row.get(1).map_err(map_rusqlite)?;
    let rate_limit = RateLimit {
        bandwidth_in_bps: row.get(2).map_err(map_rusqlite)?,
        bandwidth_out_bps: row.get(3).map_err(map_rusqlite)?,
        new_connections_per_sec: row.get(4).map_err(map_rusqlite)?,
        concurrent_connections: row.get(5).map_err(map_rusqlite)?,
        bandwidth_in_burst: row.get(6).map_err(map_rusqlite)?,
        bandwidth_out_burst: row.get(7).map_err(map_rusqlite)?,
        new_connections_burst: row.get(8).map_err(map_rusqlite)?,
    };
    let updated_at_i64: i64 = row.get(9).map_err(map_rusqlite)?;
    let updated_at_unix_ms = u64::try_from(updated_at_i64).unwrap_or(0);
    Ok(OwnerRateLimitRow {
        client_name,
        owner_id,
        rate_limit,
        updated_at_unix_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use tempfile::tempdir;

    fn open_store() -> Arc<Store> {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        // Leak the tempdir intentionally — the Store holds an
        // exclusive flock on it, and dropping the dir while the
        // Store is alive races the lock release on some platforms.
        std::mem::forget(dir);
        Arc::new(store)
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

    #[test]
    fn t027_upsert_inserts_then_replaces() {
        let store = open_store();
        let cap_store = SqliteOwnerCapStore::new(store);
        let client = ClientName::new("edge-01").unwrap();
        cap_store
            .upsert(&client, "alice", &full_envelope(), 1_700_000_000_000)
            .expect("first upsert");
        let row = cap_store.get(&client, "alice").unwrap().expect("present");
        assert_eq!(row.rate_limit.bandwidth_in_bps, Some(1_048_576));
        assert_eq!(row.updated_at_unix_ms, 1_700_000_000_000);

        // Second upsert with a tighter cap and a newer timestamp
        // replaces the row in place.
        let mut tighter = full_envelope();
        tighter.concurrent_connections = Some(2);
        cap_store
            .upsert(&client, "alice", &tighter, 1_700_000_001_000)
            .expect("replace");
        let row = cap_store.get(&client, "alice").unwrap().unwrap();
        assert_eq!(row.rate_limit.concurrent_connections, Some(2));
        assert_eq!(row.updated_at_unix_ms, 1_700_000_001_000);
    }

    #[test]
    fn t027_delete_removes_row_and_is_idempotent() {
        let store = open_store();
        let cap_store = SqliteOwnerCapStore::new(store);
        let client = ClientName::new("edge-01").unwrap();
        cap_store
            .upsert(&client, "alice", &full_envelope(), 1_700_000_000_000)
            .unwrap();
        let removed = cap_store.delete(&client, "alice").unwrap();
        assert!(removed);
        // Idempotent: second delete returns false but does not error.
        let removed_again = cap_store.delete(&client, "alice").unwrap();
        assert!(!removed_again);
        assert!(cap_store.get(&client, "alice").unwrap().is_none());
    }

    #[test]
    fn t027_get_returns_none_when_absent() {
        let store = open_store();
        let cap_store = SqliteOwnerCapStore::new(store);
        let client = ClientName::new("edge-01").unwrap();
        assert!(cap_store.get(&client, "ghost").unwrap().is_none());
    }

    #[test]
    fn t027_list_for_client_orders_by_owner_id() {
        let store = open_store();
        let cap_store = SqliteOwnerCapStore::new(store);
        let client = ClientName::new("edge-01").unwrap();
        cap_store
            .upsert(&client, "carol", &full_envelope(), 1)
            .unwrap();
        cap_store
            .upsert(&client, "alice", &full_envelope(), 2)
            .unwrap();
        cap_store
            .upsert(&client, "bob", &full_envelope(), 3)
            .unwrap();
        let rows = cap_store.list_for_client(&client).unwrap();
        let owners: Vec<_> = rows.iter().map(|r| r.owner_id.as_str()).collect();
        assert_eq!(owners, vec!["alice", "bob", "carol"]);
    }

    #[test]
    fn t027_list_for_client_isolates_by_client_name() {
        let store = open_store();
        let cap_store = SqliteOwnerCapStore::new(store);
        let edge = ClientName::new("edge-01").unwrap();
        let core = ClientName::new("core-01").unwrap();
        cap_store
            .upsert(&edge, "alice", &full_envelope(), 1)
            .unwrap();
        cap_store
            .upsert(&core, "alice", &full_envelope(), 2)
            .unwrap();
        let edge_rows = cap_store.list_for_client(&edge).unwrap();
        assert_eq!(edge_rows.len(), 1);
        assert_eq!(edge_rows[0].owner_id, "alice");
        let core_rows = cap_store.list_for_client(&core).unwrap();
        assert_eq!(core_rows.len(), 1);
    }

    #[test]
    fn t027_list_all_returns_every_client_owner_pair() {
        let store = open_store();
        let cap_store = SqliteOwnerCapStore::new(store);
        let edge = ClientName::new("edge-01").unwrap();
        let core = ClientName::new("core-01").unwrap();
        cap_store
            .upsert(&edge, "alice", &full_envelope(), 1)
            .unwrap();
        cap_store.upsert(&edge, "bob", &full_envelope(), 2).unwrap();
        cap_store
            .upsert(&core, "alice", &full_envelope(), 3)
            .unwrap();
        let all = cap_store.list_all().unwrap();
        assert_eq!(all.len(), 3);
        // Ordered by client_name ASC then owner_id ASC.
        assert_eq!(all[0].client_name.as_str(), "core-01");
        assert_eq!(all[0].owner_id, "alice");
        assert_eq!(all[1].client_name.as_str(), "edge-01");
        assert_eq!(all[1].owner_id, "alice");
        assert_eq!(all[2].client_name.as_str(), "edge-01");
        assert_eq!(all[2].owner_id, "bob");
    }

    #[test]
    fn t027_check_constraint_rejects_zero_caps() {
        let store = open_store();
        let cap_store = SqliteOwnerCapStore::new(store);
        let client = ClientName::new("edge-01").unwrap();
        // Zero is rejected at the API boundary (T016) but the SQLite
        // CHECK constraint is the second line of defence — a hand-
        // crafted INSERT bypassing the validator must still fail.
        let bad = RateLimit {
            bandwidth_in_bps: Some(0),
            ..Default::default()
        };
        let err = cap_store.upsert(&client, "alice", &bad, 1).unwrap_err();
        assert!(
            format!("{err:?}").to_lowercase().contains("constraint"),
            "expected CHECK violation, got {err:?}"
        );
    }
}
