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
use portunus_core::{ClientId, ClientName, fingerprint};

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
                    "SELECT client_id, client_name, issued_at, revoked_at, client_address \
                     FROM client_tokens \
                     ORDER BY client_name ASC",
                )
                .map_err(map_rusqlite)?;
            let rows = stmt
                .query_map([], |r| {
                    let id: String = r.get(0)?;
                    let name: String = r.get(1)?;
                    let issued: String = r.get(2)?;
                    let revoked: Option<String> = r.get(3)?;
                    let client_address: Option<String> = r.get(4)?;
                    Ok((id, name, issued, revoked, client_address))
                })
                .map_err(map_rusqlite)?;
            let mut out = Vec::new();
            for r in rows {
                let (id, name, issued, revoked, client_address) = r.map_err(map_rusqlite)?;
                let client_id = id.parse::<ClientId>().map_err(|e| StoreError::Internal {
                    message: format!("client_tokens has invalid client_id: {e}"),
                })?;
                let client_name = ClientName::new(name).map_err(|e| StoreError::Internal {
                    message: format!("client_tokens has invalid client_name: {e}"),
                })?;
                let issued_at = parse_ts(&issued)?;
                let revoked_at = revoked.map(|s| parse_ts(&s)).transpose()?;
                out.push(ProvisionedClient {
                    client_id,
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
        // 015-client-stable-id (FR-013): mint the stable identity at issuance —
        // this is the authoritative roster row, so the client first
        // materializes here (direct-issue path). Display names are free-form
        // and may collide; the id never does, so issuance does NOT reject a
        // duplicate name — it simply creates a distinct client under a fresh id.
        let client_id = ClientId::new().to_string();

        self.store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO client_tokens \
                     (client_id, client_name, token_hash, issued_at, revoked_at, client_address) \
                     VALUES (?, ?, ?, ?, NULL, ?)",
                    rusqlite::params![
                        client_id,
                        name.as_str(),
                        hash_hex,
                        issued_at,
                        client_address
                    ],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(store_err_to_auth)?;
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
                    .prepare(
                        "SELECT client_id, client_name, token_hash, revoked_at FROM client_tokens",
                    )
                    .map_err(map_rusqlite)?;
                let rows = stmt
                    .query_map([], |r| {
                        let id: String = r.get(0)?;
                        let name: String = r.get(1)?;
                        let hash_hex: String = r.get(2)?;
                        let revoked: Option<String> = r.get(3)?;
                        Ok((id, name, hash_hex, revoked))
                    })
                    .map_err(map_rusqlite)?;
                let mut matched: Option<(String, String, Option<String>)> = None;
                let needle = presented_hex.as_bytes();
                for r in rows {
                    let (id, name, hash_hex, revoked) = r.map_err(map_rusqlite)?;
                    if hash_hex.len() == needle.len()
                        && fingerprint::ct_eq(hash_hex.as_bytes(), needle)
                    {
                        matched = Some((id, name, revoked));
                    }
                }
                Ok(matched)
            })
            .map_err(store_err_to_auth)?;

        match result {
            None => Err(AuthError::Failed(AuthFailureReason::NotFound)),
            Some((_, _, Some(_))) => Err(AuthError::Failed(AuthFailureReason::Revoked)),
            Some((id, name, None)) => {
                let client_id = id.parse::<ClientId>().map_err(|e| {
                    AuthError::StoreCorrupt(format!("client_tokens invalid client_id: {e}"))
                })?;
                let client_name = ClientName::new(name).map_err(|e| {
                    AuthError::StoreCorrupt(format!("client_tokens invalid client_name: {e}"))
                })?;
                Ok(ClientIdentity {
                    client_id,
                    client_name,
                })
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

/// Outcome of updating editable client metadata.
#[derive(Debug, PartialEq, Eq)]
pub enum UpdateClientOutcome {
    Updated,
    NotFound,
}

impl SqliteTokenStore {
    /// Update editable metadata for an existing client token row.
    pub fn update_client_address(
        &self,
        name: &ClientName,
        client_address: Option<&str>,
    ) -> Result<UpdateClientOutcome, StoreError> {
        let rows = self.store.with_write_tx(|tx| {
            tx.execute(
                "UPDATE client_tokens SET client_address = ? WHERE client_name = ?",
                rusqlite::params![client_address, name.as_str()],
            )
            .map_err(map_rusqlite)
        })?;
        if rows == 0 {
            Ok(UpdateClientOutcome::NotFound)
        } else {
            Ok(UpdateClientOutcome::Updated)
        }
    }

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

    /// 015-client-stable-id (US2): change a client's free-form display
    /// name. Addresses the client by its stable `client_id`, so the
    /// rename leaves the identity — and every id-keyed row (rules,
    /// tokens, quotas, traffic history) — untouched. Returns `NotFound`
    /// when no row carries that id.
    pub fn rename(
        &self,
        client_id: ClientId,
        new_name: &ClientName,
    ) -> Result<UpdateClientOutcome, StoreError> {
        let rows = self.store.with_write_tx(|tx| {
            let n = tx
                .execute(
                    "UPDATE client_tokens SET client_name = ? WHERE client_id = ?",
                    rusqlite::params![new_name.as_str(), client_id.to_string()],
                )
                .map_err(map_rusqlite)?;
            // 015-client-stable-id: keep the denormalized `rules.client_name`
            // column in lock-step with the canonical display name so that
            // `/v1/rules` and the Web UI Rules page reflect the rename after a
            // restart/hydration (client_id is the join key).
            if n > 0 {
                tx.execute(
                    "UPDATE rules SET client_name = ? WHERE client_id = ?",
                    rusqlite::params![new_name.as_str(), client_id.to_string()],
                )
                .map_err(map_rusqlite)?;
            }
            Ok(n)
        })?;
        if rows == 0 {
            Ok(UpdateClientOutcome::NotFound)
        } else {
            Ok(UpdateClientOutcome::Updated)
        }
    }

    /// Revoke a client addressed by its stable `client_id` (idempotent).
    /// 015-client-stable-id (US3): unambiguous even under duplicate display
    /// names. Returns the number of rows revoked (0 if already revoked).
    pub fn revoke_by_id(&self, client_id: ClientId) -> Result<usize, StoreError> {
        let now = Utc::now().to_rfc3339();
        self.store.with_write_tx(|tx| {
            tx.execute(
                "UPDATE client_tokens SET revoked_at = ? \
                 WHERE client_id = ? AND revoked_at IS NULL",
                rusqlite::params![now, client_id.to_string()],
            )
            .map_err(map_rusqlite)
        })
    }

    /// Update editable metadata for a client addressed by `client_id`.
    pub fn update_client_address_by_id(
        &self,
        client_id: ClientId,
        client_address: Option<&str>,
    ) -> Result<UpdateClientOutcome, StoreError> {
        let rows = self.store.with_write_tx(|tx| {
            tx.execute(
                "UPDATE client_tokens SET client_address = ? WHERE client_id = ?",
                rusqlite::params![client_address, client_id.to_string()],
            )
            .map_err(map_rusqlite)
        })?;
        if rows == 0 {
            Ok(UpdateClientOutcome::NotFound)
        } else {
            Ok(UpdateClientOutcome::Updated)
        }
    }

    /// Permanently remove a previously-revoked client addressed by
    /// `client_id`. Refuses to touch an active row (caller must revoke
    /// first), mirroring [`Self::delete_revoked`].
    pub fn delete_revoked_by_id(&self, client_id: ClientId) -> Result<DeleteOutcome, StoreError> {
        self.store.with_write_tx(|tx| {
            let revoked_at: Option<Option<String>> = match tx.query_row(
                "SELECT revoked_at FROM client_tokens WHERE client_id = ?",
                rusqlite::params![client_id.to_string()],
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
                        "DELETE FROM client_tokens WHERE client_id = ?",
                        rusqlite::params![client_id.to_string()],
                    )
                    .map_err(map_rusqlite)?;
                    Ok(DeleteOutcome::Deleted)
                }
            }
        })
    }

    /// Look up a single provisioned client by its stable `client_id`.
    /// Unlike a name lookup this is unambiguous even when display names
    /// collide (FR-013). Returns `None` when the id is unknown.
    pub fn get_by_id(&self, client_id: ClientId) -> Result<Option<ProvisionedClient>, StoreError> {
        self.store.with_conn(|c| {
            let row = c
                .query_row(
                    "SELECT client_id, client_name, issued_at, revoked_at, client_address \
                     FROM client_tokens WHERE client_id = ?",
                    rusqlite::params![client_id.to_string()],
                    |r| {
                        let id: String = r.get(0)?;
                        let name: String = r.get(1)?;
                        let issued: String = r.get(2)?;
                        let revoked: Option<String> = r.get(3)?;
                        let client_address: Option<String> = r.get(4)?;
                        Ok((id, name, issued, revoked, client_address))
                    },
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(map_rusqlite(other)),
                })?;
            let Some((id, name, issued, revoked, client_address)) = row else {
                return Ok(None);
            };
            let client_id = id.parse::<ClientId>().map_err(|e| StoreError::Internal {
                message: format!("client_tokens has invalid client_id: {e}"),
            })?;
            let client_name = ClientName::new(name).map_err(|e| StoreError::Internal {
                message: format!("client_tokens has invalid client_name: {e}"),
            })?;
            Ok(Some(ProvisionedClient {
                client_id,
                client_name,
                issued_at: parse_ts(&issued)?,
                revoked_at: revoked.map(|s| parse_ts(&s)).transpose()?,
                client_address,
            }))
        })
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
    fn issue_allows_duplicate_display_name() {
        // 015-client-stable-id (FR-013): display names are non-unique.
        // Issuing twice for the same name yields two distinct clients,
        // each under its own stable id, with two separate tokens.
        let (_d, s) = fresh();
        let t1 = s.issue(cn("edge-01")).unwrap();
        let t2 = s.issue(cn("edge-01")).unwrap();
        assert_ne!(t1, t2, "each issuance is a distinct client/token");
        let id1 = s.verify(&t1).unwrap().client_id;
        let id2 = s.verify(&t2).unwrap().client_id;
        assert_ne!(id1, id2, "duplicate names get distinct stable ids");
        let same_name = s
            .list()
            .unwrap()
            .into_iter()
            .filter(|c| c.client_name.as_str() == "edge-01")
            .count();
        assert_eq!(same_name, 2);
    }

    #[test]
    fn verify_round_trip() {
        let (_d, s) = fresh();
        let token = s.issue(cn("edge-01")).unwrap();
        let id = s.verify(&token).unwrap();
        assert_eq!(id.client_name.as_str(), "edge-01");
    }

    #[test]
    fn rename_keeps_client_id_and_token_stable() {
        // 015-client-stable-id (US2): renaming changes only the display
        // name; the client_id and the bearer token are unchanged, so the
        // forwarder keeps authenticating and all id-keyed rows stay bound.
        let (_d, s) = fresh();
        let token = s.issue(cn("edge-01")).unwrap();
        let before = s.verify(&token).unwrap();

        let outcome = s.rename(before.client_id, &cn("Acme Prod – East")).unwrap();
        assert_eq!(outcome, UpdateClientOutcome::Updated);

        let after = s.verify(&token).unwrap();
        assert_eq!(after.client_id, before.client_id, "identity is stable");
        assert_eq!(after.client_name.as_str(), "Acme Prod – East");

        let view = s.get_by_id(before.client_id).unwrap().expect("present");
        assert_eq!(view.client_name.as_str(), "Acme Prod – East");
    }

    #[test]
    fn rename_unknown_id_is_not_found() {
        let (_d, s) = fresh();
        let outcome = s
            .rename(portunus_core::ClientId::new(), &cn("whatever"))
            .unwrap();
        assert_eq!(outcome, UpdateClientOutcome::NotFound);
        assert!(
            s.get_by_id(portunus_core::ClientId::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rename_allows_duplicate_display_names() {
        // FR-013: two distinct clients may share a display name.
        let (_d, s) = fresh();
        let t1 = s.issue(cn("alpha")).unwrap();
        let t2 = s.issue(cn("beta")).unwrap();
        let id1 = s.verify(&t1).unwrap().client_id;
        let id2 = s.verify(&t2).unwrap().client_id;
        assert_eq!(
            s.rename(id1, &cn("Shared")).unwrap(),
            UpdateClientOutcome::Updated
        );
        assert_eq!(
            s.rename(id2, &cn("Shared")).unwrap(),
            UpdateClientOutcome::Updated
        );
        assert_eq!(
            s.get_by_id(id1).unwrap().unwrap().client_name.as_str(),
            "Shared"
        );
        assert_eq!(
            s.get_by_id(id2).unwrap().unwrap().client_name.as_str(),
            "Shared"
        );
        assert_ne!(id1, id2, "distinct identities preserved under shared name");
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
    fn issue_with_address_persists_client_address() {
        let (_d, s) = fresh();
        s.issue_with_address(cn("edge-01"), Some("edge.example.com"))
            .unwrap();

        let rows = s.list().unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].client_address.as_deref(), Some("edge.example.com"));
    }

    #[test]
    fn update_client_address_replaces_existing_value() {
        let (_d, s) = fresh();
        let name = cn("edge-01");
        s.issue_with_address(name.clone(), Some("edge.example.com"))
            .unwrap();

        assert_eq!(
            s.update_client_address(&name, Some("new-edge.example.com"))
                .unwrap(),
            UpdateClientOutcome::Updated,
        );

        let rows = s.list().unwrap();
        assert_eq!(
            rows[0].client_address.as_deref(),
            Some("new-edge.example.com")
        );
    }

    #[test]
    fn update_client_address_reports_missing_client() {
        let (_d, s) = fresh();

        assert_eq!(
            s.update_client_address(&cn("missing"), Some("edge.example.com"))
                .unwrap(),
            UpdateClientOutcome::NotFound,
        );
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
