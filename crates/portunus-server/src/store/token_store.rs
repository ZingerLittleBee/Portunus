//! 008-sqlite-storage T043 — SQLite-backed `Authenticator`.
//!
//! Replaces `portunus_auth::file_store::FileTokenStore`. Schema lives in
//! the `client_tokens` table (V001). All mutations go through a single
//! BEGIN IMMEDIATE write transaction; reads pull from a pooled
//! connection.
//!
//! Constitution V (preserve identity through the call chain) is
//! preserved: `verify` returns `ClientIdentity` reconstructed from
//! `client_name` only — token hash never leaves the store.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use portunus_auth::{
    AuthError, AuthFailureReason, Authenticator, ClientIdentity, ProvisionedClient, token,
};
use portunus_core::{ClientName, fingerprint};

use crate::store::{Store, StoreError, map_rusqlite};

/// SQLite-backed `Authenticator`. Cheap to clone (`Arc<Store>` inside).
#[derive(Clone)]
pub struct SqliteTokenStore {
    store: Arc<Store>,
}

impl std::fmt::Debug for SqliteTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteTokenStore")
            .field("db", &self.store.db_path())
            .finish()
    }
}

impl SqliteTokenStore {
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Snapshot of provisioned clients (for `list-clients`). Mirrors
    /// `FileTokenStore::list` shape so the v0.7 CLI output is byte-stable.
    pub fn list(&self) -> Result<Vec<ProvisionedClient>, StoreError> {
        self.store.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT client_name, issued_at, revoked_at, client_address \
                     FROM client_tokens \
                     ORDER BY client_name ASC",
                )
                .map_err(map_rusqlite)?;
            let rows = stmt
                .query_map([], |r| {
                    let name: String = r.get(0)?;
                    let issued: String = r.get(1)?;
                    let revoked: Option<String> = r.get(2)?;
                    let client_address: Option<String> = r.get(3)?;
                    Ok((name, issued, revoked, client_address))
                })
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            for r in rows {
                let (name, issued, revoked, client_address) = r.map_err(map_rusqlite)?;
                let client_name = ClientName::new(name).map_err(|e| StoreError::Internal {
                    message: format!("client_tokens has invalid client_name: {e}"),
                })?;
                let issued_at = parse_ts(&issued)?;
                let revoked_at = revoked.map(|s| parse_ts(&s)).transpose()?;
                out.push(ProvisionedClient {
                    client_name,
                    issued_at,
                    revoked_at,
                    client_address,
                });
            }
            Ok(out)
        })
    }

    pub fn issue_with_address(
        &self,
        name: ClientName,
        client_address: Option<&str>,
    ) -> Result<String, AuthError> {
        self.issue_inner(name, client_address)
    }

    fn issue_inner(
        &self,
        name: ClientName,
        client_address: Option<&str>,
    ) -> Result<String, AuthError> {
        let token = token::generate_token();
        let hash_hex = fingerprint::hex(&token::hash_token(&token));
        let issued_at = Utc::now().to_rfc3339();
        let name_for_err = name.clone();

        self.store
            .with_write_tx(|tx| {
                let exists: bool = tx
                    .query_row(
                        "SELECT 1 FROM client_tokens WHERE client_name = ? LIMIT 1",
                        rusqlite::params![name.as_str()],
                        |_| Ok(true),
                    )
                    .or_else(|e| match e {
                        rusqlite::Error::QueryReturnedNoRows => Ok(false),
                        other => Err(other),
                    })
                    .map_err(map_rusqlite)?;
                if exists {
                    return Err(StoreError::Conflict {
                        detail: "client_already_exists".into(),
                    });
                }
                tx.execute(
                    "INSERT INTO client_tokens \
                     (client_name, token_hash, issued_at, revoked_at, client_address) \
                     VALUES (?, ?, ?, NULL, ?)",
                    rusqlite::params![name.as_str(), hash_hex, issued_at, client_address],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { .. } => AuthError::ClientAlreadyExists(name_for_err),
                other => store_err_to_auth(other),
            })?;
        Ok(token)
    }
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>, StoreError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| StoreError::Corruption {
            detail: format!("bad RFC3339 ts: {e}"),
        })
}

fn store_err_to_auth(e: StoreError) -> AuthError {
    match e {
        StoreError::Corruption { detail: s } => AuthError::StoreCorrupt(s),
        other => AuthError::StoreCorrupt(other.to_string()),
    }
}

impl Authenticator for SqliteTokenStore {
    fn verify(&self, token: &str) -> Result<ClientIdentity, AuthError> {
        if token.is_empty() {
            return Err(AuthError::Failed(AuthFailureReason::Missing));
        }
        if token.len() > 256 {
            return Err(AuthError::Failed(AuthFailureReason::Malformed));
        }
        let presented = token::hash_token(token);
        let presented_hex = fingerprint::hex(&presented);

        // Constitution V: full table scan with CT-equality on every row to
        // avoid leaky timing across (present vs absent) regardless of the
        // SQL planner's index choice. ≤100 rows expected — this is the
        // same shape the file store used.
        let result = self
            .store
            .with_conn(|c| {
                let mut stmt = c
                    .prepare("SELECT client_name, token_hash, revoked_at FROM client_tokens")
                    .map_err(map_rusqlite)?;
                let rows = stmt
                    .query_map([], |r| {
                        let name: String = r.get(0)?;
                        let hash_hex: String = r.get(1)?;
                        let revoked: Option<String> = r.get(2)?;
                        Ok((name, hash_hex, revoked))
                    })
                    .map_err(map_rusqlite)?;
                let mut matched: Option<(String, Option<String>)> = None;
                let needle = presented_hex.as_bytes();
                for r in rows {
                    let (name, hash_hex, revoked) = r.map_err(map_rusqlite)?;
                    if hash_hex.len() == needle.len()
                        && fingerprint::ct_eq(hash_hex.as_bytes(), needle)
                    {
                        matched = Some((name, revoked));
                    }
                }
                Ok(matched)
            })
            .map_err(store_err_to_auth)?;

        match result {
            None => Err(AuthError::Failed(AuthFailureReason::NotFound)),
            Some((_, Some(_))) => Err(AuthError::Failed(AuthFailureReason::Revoked)),
            Some((name, None)) => {
                let client_name = ClientName::new(name).map_err(|e| {
                    AuthError::StoreCorrupt(format!("client_tokens invalid client_name: {e}"))
                })?;
                Ok(ClientIdentity { client_name })
            }
        }
    }

    fn issue(&self, name: ClientName) -> Result<String, AuthError> {
        self.issue_inner(name, None)
    }

    fn revoke(&self, name: &ClientName) -> Result<(), AuthError> {
        let now = Utc::now().to_rfc3339();
        self.store
            .with_write_tx(|tx| {
                tx.execute(
                    "UPDATE client_tokens SET revoked_at = ? \
                     WHERE client_name = ? AND revoked_at IS NULL",
                    rusqlite::params![now, name.as_str()],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(store_err_to_auth)?;
        Ok(())
    }
}

/// Outcome of `SqliteTokenStore::delete_revoked`. The store refuses to
/// drop a still-active client — operators must revoke first so the
/// connected forwarder gets disconnected before its name vanishes.
#[derive(Debug, PartialEq, Eq)]
pub enum DeleteOutcome {
    Deleted,
    NotFound,
    StillActive,
}

/// Outcome of `SqliteTokenStore::reissue`. `Rotated` carries the new
/// plaintext token (server-side it's hashed before the transaction
/// commits — same one-shot disclosure contract as `issue`).
#[derive(Debug, PartialEq, Eq)]
pub enum ReissueOutcome {
    Rotated(String),
    NotFound,
}

impl SqliteTokenStore {
    /// Permanently remove a previously-revoked client row. Refuses to
    /// touch active rows so the caller can't accidentally race the
    /// data-plane disconnect path. Returns `StillActive` if the row is
    /// still live; the caller is expected to `revoke()` first.
    pub fn delete_revoked(&self, name: &ClientName) -> Result<DeleteOutcome, StoreError> {
        self.store.with_write_tx(|tx| {
            let revoked_at: Option<Option<String>> = match tx.query_row(
                "SELECT revoked_at FROM client_tokens WHERE client_name = ?",
                rusqlite::params![name.as_str()],
                |r| r.get::<_, Option<String>>(0),
            ) {
                Ok(v) => Some(v),
                Err(rusqlite::Error::QueryReturnedNoRows) => None,
                Err(e) => return Err(map_rusqlite(e)),
            };
            match revoked_at {
                None => Ok(DeleteOutcome::NotFound),
                Some(None) => Ok(DeleteOutcome::StillActive),
                Some(Some(_)) => {
                    tx.execute(
                        "DELETE FROM client_tokens WHERE client_name = ?",
                        rusqlite::params![name.as_str()],
                    )
                    .map_err(map_rusqlite)?;
                    Ok(DeleteOutcome::Deleted)
                }
            }
        })
    }

    /// Rotate a client's bearer token in place. Works on active OR
    /// revoked rows: rotation clears `revoked_at`, refreshes the hash
    /// and `issued_at`, and hands back the new plaintext exactly once.
    /// The caller is responsible for disconnecting any live session so
    /// the forwarder picks up the new credentials.
    pub fn reissue(&self, name: &ClientName) -> Result<ReissueOutcome, StoreError> {
        let token = token::generate_token();
        let hash_hex = fingerprint::hex(&token::hash_token(&token));
        let issued_at = Utc::now().to_rfc3339();
        let rows = self.store.with_write_tx(|tx| {
            tx.execute(
                "UPDATE client_tokens \
                 SET token_hash = ?, issued_at = ?, revoked_at = NULL \
                 WHERE client_name = ?",
                rusqlite::params![hash_hex, issued_at, name.as_str()],
            )
            .map_err(map_rusqlite)
        })?;
        if rows == 0 {
            Ok(ReissueOutcome::NotFound)
        } else {
            Ok(ReissueOutcome::Rotated(token))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn cn(s: &str) -> ClientName {
        ClientName::new(s).unwrap()
    }

    fn fresh() -> (tempfile::TempDir, SqliteTokenStore) {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        (dir, SqliteTokenStore::new(store))
    }

    #[test]
    fn issue_returns_unique_token() {
        let (_d, s) = fresh();
        let t1 = s.issue(cn("edge-01")).unwrap();
        let t2 = s.issue(cn("edge-02")).unwrap();
        assert_ne!(t1, t2);
        assert_eq!(t1.len(), 43);
    }

    #[test]
    fn issue_rejects_duplicate() {
        let (_d, s) = fresh();
        s.issue(cn("edge-01")).unwrap();
        let err = s.issue(cn("edge-01")).unwrap_err();
        match err {
            AuthError::ClientAlreadyExists(n) => assert_eq!(n.as_str(), "edge-01"),
            other => panic!("expected ClientAlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn verify_round_trip() {
        let (_d, s) = fresh();
        let token = s.issue(cn("edge-01")).unwrap();
        let id = s.verify(&token).unwrap();
        assert_eq!(id.client_name.as_str(), "edge-01");
    }

    #[test]
    fn verify_rejects_wrong_token() {
        let (_d, s) = fresh();
        s.issue(cn("edge-01")).unwrap();
        let err = s
            .verify("not-a-real-token-not-a-real-token-not-a-rea")
            .unwrap_err();
        assert!(matches!(
            err,
            AuthError::Failed(AuthFailureReason::NotFound)
        ));
    }

    #[test]
    fn verify_missing_when_empty() {
        let (_d, s) = fresh();
        let err = s.verify("").unwrap_err();
        assert!(matches!(err, AuthError::Failed(AuthFailureReason::Missing)));
    }

    #[test]
    fn revoke_blocks_verify() {
        let (_d, s) = fresh();
        let token = s.issue(cn("edge-01")).unwrap();
        s.revoke(&cn("edge-01")).unwrap();
        let err = s.verify(&token).unwrap_err();
        assert!(matches!(err, AuthError::Failed(AuthFailureReason::Revoked)));
    }

    #[test]
    fn revoke_is_idempotent() {
        let (_d, s) = fresh();
        s.issue(cn("edge-01")).unwrap();
        s.revoke(&cn("edge-01")).unwrap();
        s.revoke(&cn("edge-01")).unwrap(); // second call no-op
        s.revoke(&cn("nonexistent")).unwrap(); // also no-op
    }

    #[test]
    fn persists_and_reloads() {
        let dir = tempdir().unwrap();
        let token = {
            let store = Arc::new(Store::open(dir.path()).unwrap());
            let s = SqliteTokenStore::new(store.clone());
            let t = s.issue(cn("edge-01")).unwrap();
            s.issue(cn("edge-02")).unwrap();
            s.revoke(&cn("edge-02")).unwrap();
            drop(s);
            drop(store);
            t
        };
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let reopened = SqliteTokenStore::new(store);
        let id = reopened.verify(&token).unwrap();
        assert_eq!(id.client_name.as_str(), "edge-01");
    }

    #[test]
    fn delete_revoked_removes_row() {
        let (_d, s) = fresh();
        s.issue(cn("edge-01")).unwrap();
        s.revoke(&cn("edge-01")).unwrap();
        assert_eq!(
            s.delete_revoked(&cn("edge-01")).unwrap(),
            DeleteOutcome::Deleted
        );
        assert!(s.list().unwrap().is_empty());
    }

    #[test]
    fn delete_revoked_refuses_active_row() {
        let (_d, s) = fresh();
        s.issue(cn("edge-01")).unwrap();
        assert_eq!(
            s.delete_revoked(&cn("edge-01")).unwrap(),
            DeleteOutcome::StillActive
        );
        // Row must still exist after a refused delete.
        assert_eq!(s.list().unwrap().len(), 1);
    }

    #[test]
    fn delete_revoked_reports_not_found() {
        let (_d, s) = fresh();
        assert_eq!(
            s.delete_revoked(&cn("never-existed")).unwrap(),
            DeleteOutcome::NotFound
        );
    }

    #[test]
    fn reissue_rotates_token_and_invalidates_old() {
        let (_d, s) = fresh();
        let old = s.issue(cn("edge-01")).unwrap();
        let new = match s.reissue(&cn("edge-01")).unwrap() {
            ReissueOutcome::Rotated(t) => t,
            ReissueOutcome::NotFound => panic!("expected Rotated, got NotFound"),
        };
        assert_ne!(old, new);
        // Old token no longer authenticates.
        assert!(matches!(
            s.verify(&old).unwrap_err(),
            AuthError::Failed(AuthFailureReason::NotFound)
        ));
        // New token does.
        assert_eq!(s.verify(&new).unwrap().client_name.as_str(), "edge-01");
    }

    #[test]
    fn reissue_revives_revoked_client() {
        let (_d, s) = fresh();
        s.issue(cn("edge-01")).unwrap();
        s.revoke(&cn("edge-01")).unwrap();
        let new = match s.reissue(&cn("edge-01")).unwrap() {
            ReissueOutcome::Rotated(t) => t,
            ReissueOutcome::NotFound => panic!("expected Rotated, got NotFound"),
        };
        assert_eq!(s.verify(&new).unwrap().client_name.as_str(), "edge-01");
        let row = s.list().unwrap();
        assert_eq!(row.len(), 1);
        assert_eq!(row[0].revoked_at, None, "reissue must clear revoked_at");
    }

    #[test]
    fn reissue_reports_not_found() {
        let (_d, s) = fresh();
        assert!(matches!(
            s.reissue(&cn("nope")).unwrap(),
            ReissueOutcome::NotFound
        ));
    }

    #[test]
    fn issue_with_address_persists_client_address() {
        let (_d, s) = fresh();
        s.issue_with_address(cn("edge-01"), Some("edge.example.com"))
            .unwrap();

        let rows = s.list().unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].client_address.as_deref(), Some("edge.example.com"));
    }

    #[test]
    fn list_excludes_token_hash_and_sorts() {
        let (_d, s) = fresh();
        s.issue(cn("edge-02")).unwrap();
        s.issue(cn("edge-01")).unwrap();
        let rows = s.list().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].client_name.as_str(), "edge-01");
        assert_eq!(rows[1].client_name.as_str(), "edge-02");
    }
}
