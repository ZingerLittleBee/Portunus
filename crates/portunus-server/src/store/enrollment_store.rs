//! Short-lived client enrollment code store.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use portunus_auth::token;
use portunus_core::{ClientId, ClientName, fingerprint};
use rusqlite::OptionalExtension;
use thiserror::Error;

use crate::store::{Store, StoreError, map_rusqlite};

#[derive(Debug, Clone)]
pub struct ClientEnrollmentStore {
    store: Arc<Store>,
}

impl ClientEnrollmentStore {
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    pub fn create(
        &self,
        input: CreateEnrollment,
    ) -> Result<CreatedEnrollment, CreateEnrollmentError> {
        let code = token::generate_token();
        let code_hash = fingerprint::hex(&token::hash_token(&code));
        let issued_at = input.now.to_rfc3339();
        let expires_at = input.expires_at.to_rfc3339();
        let client_address = self.store.with_write_tx(|tx| {
            prune_stale_enrollments(tx, input.now)?;
            // 015-client-stable-id (FR-013): resolve the stable id this
            // enrollment will carry. A brand-new client mints a fresh id
            // NOW — a colliding display name is NOT an error, since names
            // are free-form and non-unique. A re-enrollment resolves the
            // existing client by its stable id (passed by the id-keyed
            // operator route), falling back to a name lookup only for a
            // legacy caller that supplies no id.
            let (effective_client_id, client_address): (String, Option<String>) =
                match &input.target {
                    EnrollmentTarget::New { client_address } => {
                        let id = input.client_id.unwrap_or_default();
                        (id.to_string(), client_address.clone())
                    }
                    EnrollmentTarget::Existing => {
                        let resolved = match input.client_id {
                            Some(id) => Some((id.to_string(), client_for_id(tx, id)?)),
                            None => client_for_name_with_id(tx, &input.client_name)?,
                        };
                        match resolved {
                            Some((id, ExistingClient::Present { client_address })) => {
                                (id, client_address)
                            }
                            _ => {
                                return Ok(Err(CreateEnrollmentError::ClientNotFound(
                                    input.client_name.clone(),
                                )));
                            }
                        }
                    }
                };
            // Supersede prior unconsumed enrollments for this client, keyed
            // on the now-known stable id so a renamed client's outstanding
            // codes are still invalidated.
            tx.execute(
                "UPDATE client_enrollments \
                 SET consumed_at = ? \
                 WHERE client_id = ? AND consumed_at IS NULL",
                rusqlite::params![issued_at, effective_client_id],
            )
            .map_err(map_rusqlite)?;
            tx.execute(
                "INSERT INTO client_enrollments \
                 (client_id, client_name, client_address, code_hash, issued_at, expires_at, consumed_at, advertised_endpoint) \
                 VALUES (?, ?, ?, ?, ?, ?, NULL, ?)",
                rusqlite::params![
                    effective_client_id,
                    input.client_name.as_str(),
                    client_address.as_deref(),
                    code_hash,
                    issued_at,
                    expires_at,
                    input.advertised_endpoint
                ],
            )
            .map_err(map_rusqlite)?;
            Ok(Ok(client_address))
        })??;
        Ok(CreatedEnrollment {
            client_name: input.client_name,
            client_address,
            code,
            expires_at: input.expires_at,
        })
    }

    pub fn redeem(
        &self,
        code: &str,
        now: DateTime<Utc>,
        resolve_legacy: impl FnOnce() -> Result<String, RedeemEnrollmentError>,
    ) -> Result<IssuedClientCredential, RedeemEnrollmentError> {
        if code.is_empty() || code.len() > 256 {
            return Err(RedeemEnrollmentError::InvalidCode);
        }
        let presented_hash = fingerprint::hex(&token::hash_token(code));
        let consumed_at = now.to_rfc3339();
        let client_token = token::generate_token();
        let client_token_hash = fingerprint::hex(&token::hash_token(&client_token));

        self.store.with_write_tx(|tx| {
            let mut stmt = tx
                .prepare(
                    "SELECT id, client_id, client_name, client_address, code_hash, expires_at, consumed_at, advertised_endpoint \
                         FROM client_enrollments",
                )
                .map_err(map_rusqlite)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(EnrollmentRow {
                        id: row.get(0)?,
                        client_id: row.get(1)?,
                        client_name: row.get(2)?,
                        client_address: row.get(3)?,
                        code_hash: row.get(4)?,
                        expires_at: row.get(5)?,
                        consumed_at: row.get(6)?,
                        advertised_endpoint: row.get(7)?,
                    })
                })
                .map_err(map_rusqlite)?;

            let mut matched: Option<EnrollmentRow> = None;
            for row in rows {
                let row = row.map_err(map_rusqlite)?;
                if row.code_hash.len() == presented_hash.len()
                    && fingerprint::ct_eq(row.code_hash.as_bytes(), presented_hash.as_bytes())
                {
                    matched = Some(row);
                }
            }

            let Some(row) = matched else {
                return Ok(Err(RedeemEnrollmentError::InvalidCode));
            };
            if row.consumed_at.is_some() {
                return Ok(Err(RedeemEnrollmentError::AlreadyUsed));
            }
            let expires_at = match parse_ts(&row.expires_at) {
                Ok(ts) => ts,
                Err(e) => return Ok(Err(e)),
            };
            if expires_at <= now {
                return Ok(Err(RedeemEnrollmentError::Expired));
            }
            let client_name = match ClientName::new(row.client_name.clone()) {
                Ok(name) => name,
                Err(e) => {
                    return Ok(Err(RedeemEnrollmentError::Store(StoreError::Corruption {
                        detail: format!("client_enrollments invalid client_name: {e}"),
                    })));
                }
            };

            // Resolve the effective advertised endpoint BEFORE any
            // consume / token-mint write. For a legacy pre-V010 NULL
            // row this calls the fail-closed resolver; on failure we
            // early-return here so the `UPDATE consumed_at` and the
            // `client_tokens` insert/rotate below never run — the
            // transaction commits with zero mutations and the
            // enrollment stays redeemable (idempotent / retryable
            // once the operator fixes the advertised-endpoint config).
            let effective_endpoint = match row.advertised_endpoint.clone() {
                Some(ep) => ep,
                None => match resolve_legacy() {
                    Ok(ep) => ep,
                    Err(e) => return Ok(Err(e)),
                },
            };

            // 015-client-stable-id (T014): resolve the stable id. Prefer the
            // id persisted on the enrollment row (re-enrollment) — keying the
            // token rotation on the id keeps it identity-safe across a rename
            // that happened between create and redeem. Fall back to a
            // name-based lookup for legacy rows (NULL client_id) and brand-new
            // clients, minting a fresh id only when the client truly does not
            // exist yet (U2).
            let (client_id, client_name, existing_client): (String, ClientName, bool) =
                if let Some(id) = row.client_id.clone() {
                    let current: Option<String> = tx
                        .query_row(
                            "SELECT client_name FROM client_tokens WHERE client_id = ?",
                            rusqlite::params![id],
                            |r| r.get(0),
                        )
                        .optional()
                        .map_err(map_rusqlite)?;
                    if let Some(current_name) = current {
                        // Rotate the existing client's token by id; return its
                        // CURRENT display name (post-rename).
                        tx.execute(
                            "UPDATE client_tokens \
                             SET token_hash = ?, issued_at = ?, revoked_at = NULL, \
                                 client_address = COALESCE(?, client_address) \
                             WHERE client_id = ?",
                            rusqlite::params![
                                client_token_hash,
                                consumed_at,
                                row.client_address.as_deref(),
                                id
                            ],
                        )
                        .map_err(map_rusqlite)?;
                        let resolved =
                            ClientName::new(current_name).map_err(|e| StoreError::Corruption {
                                detail: format!("client_tokens invalid client_name: {e}"),
                            })?;
                        (id, resolved, true)
                    } else {
                        // The client was deleted between create and redeem;
                        // re-materialize it under the SAME id so its rules /
                        // quotas / history (keyed on the id) reattach.
                        tx.execute(
                            "INSERT INTO client_tokens \
                                 (client_id, client_name, token_hash, issued_at, revoked_at, client_address) \
                                 VALUES (?, ?, ?, ?, NULL, ?)",
                            rusqlite::params![
                                id,
                                client_name.as_str(),
                                client_token_hash,
                                consumed_at,
                                row.client_address.as_deref()
                            ],
                        )
                        .map_err(map_rusqlite)?;
                        (id, client_name, false)
                    }
                } else {
                    let existing: bool = tx
                        .query_row(
                            "SELECT 1 FROM client_tokens WHERE client_name = ? LIMIT 1",
                            rusqlite::params![client_name.as_str()],
                            |_| Ok(true),
                        )
                        .or_else(|e| match e {
                            rusqlite::Error::QueryReturnedNoRows => Ok(false),
                            other => Err(other),
                        })
                        .map_err(map_rusqlite)?;
                    if existing {
                        let id: String = tx
                            .query_row(
                                "SELECT client_id FROM client_tokens WHERE client_name = ?",
                                rusqlite::params![client_name.as_str()],
                                |r| r.get(0),
                            )
                            .map_err(map_rusqlite)?;
                        tx.execute(
                            "UPDATE client_tokens \
                             SET token_hash = ?, issued_at = ?, revoked_at = NULL, \
                                 client_address = COALESCE(?, client_address) \
                             WHERE client_name = ?",
                            rusqlite::params![
                                client_token_hash,
                                consumed_at,
                                row.client_address.as_deref(),
                                client_name.as_str()
                            ],
                        )
                        .map_err(map_rusqlite)?;
                        (id, client_name, true)
                    } else {
                        let id = ClientId::new().to_string();
                        tx.execute(
                            "INSERT INTO client_tokens \
                                 (client_id, client_name, token_hash, issued_at, revoked_at, client_address) \
                                 VALUES (?, ?, ?, ?, NULL, ?)",
                            rusqlite::params![
                                id,
                                client_name.as_str(),
                                client_token_hash,
                                consumed_at,
                                row.client_address.as_deref()
                            ],
                        )
                        .map_err(map_rusqlite)?;
                        (id, client_name, false)
                    }
                };

            tx.execute(
                "UPDATE client_enrollments SET consumed_at = ? WHERE id = ?",
                rusqlite::params![consumed_at, row.id],
            )
            .map_err(map_rusqlite)?;

            let client_id = client_id.parse::<ClientId>().map_err(|e| {
                StoreError::Corruption {
                    detail: format!("client_tokens invalid client_id: {e}"),
                }
            })?;

            Ok(Ok(IssuedClientCredential {
                client_id,
                client_name,
                token: client_token,
                rotated_existing: existing_client,
                advertised_endpoint: Some(effective_endpoint),
            }))
        })?
    }
}

#[derive(Debug, Clone)]
pub struct CreateEnrollment {
    pub client_name: ClientName,
    /// 015-client-stable-id (T014): the stable id of the client this
    /// enrollment targets, when it already exists (re-enrollment). `None`
    /// for a brand-new client — its id is minted at redeem (U2). Persisted
    /// on the row so a rename between create and redeem stays identity-safe:
    /// redeem resolves the client by this id, not by the (now-stale) name.
    pub client_id: Option<ClientId>,
    pub target: EnrollmentTarget,
    pub expires_at: DateTime<Utc>,
    pub now: DateTime<Utc>,
    pub advertised_endpoint: String,
}

#[derive(Debug, Clone)]
pub enum EnrollmentTarget {
    New { client_address: Option<String> },
    Existing,
}

#[derive(Debug, Clone)]
pub struct CreatedEnrollment {
    pub client_name: ClientName,
    pub client_address: Option<String>,
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct IssuedClientCredential {
    pub client_id: ClientId,
    pub client_name: ClientName,
    pub token: String,
    pub rotated_existing: bool,
    pub advertised_endpoint: Option<String>,
}

#[derive(Debug, Error)]
pub enum CreateEnrollmentError {
    #[error("client_already_exists: {0}")]
    ClientAlreadyExists(ClientName),
    #[error("client_not_found: {0}")]
    ClientNotFound(ClientName),
    #[error(transparent)]
    Store(#[from] StoreError),
}

#[derive(Debug, Error)]
pub enum RedeemEnrollmentError {
    #[error("invalid_code")]
    InvalidCode,
    #[error("expired")]
    Expired,
    #[error("already_used")]
    AlreadyUsed,
    /// Legacy pre-V010 NULL-endpoint row could not be resolved
    /// fail-closed at redeem time. Surfaced BEFORE any consume / token
    /// write so the enrollment stays redeemable once the operator fixes
    /// the advertised-endpoint config (idempotent / retryable).
    #[error("advertised_endpoint: {0}")]
    AdvertisedEndpoint(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

struct EnrollmentRow {
    id: i64,
    client_id: Option<String>,
    client_name: String,
    client_address: Option<String>,
    code_hash: String,
    expires_at: String,
    consumed_at: Option<String>,
    advertised_endpoint: Option<String>,
}

enum ExistingClient {
    Present { client_address: Option<String> },
    Absent,
}

/// Resolve a client by its stable id (015-client-stable-id). This is the
/// canonical re-enrollment lookup: a renamed client — or one sharing a
/// display name with others — still resolves unambiguously.
fn client_for_id(
    tx: &rusqlite::Transaction<'_>,
    client_id: ClientId,
) -> Result<ExistingClient, StoreError> {
    let client_address = tx
        .query_row(
            "SELECT client_address FROM client_tokens WHERE client_id = ?",
            rusqlite::params![client_id.to_string()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_rusqlite)?;
    Ok(match client_address {
        Some(client_address) => ExistingClient::Present { client_address },
        None => ExistingClient::Absent,
    })
}

/// Legacy fallback for a re-enrollment that supplies no id: resolve the
/// first client with this display name, returning both its id and
/// address. Display names are non-unique (FR-013), so this is
/// `LIMIT 1` / first-match; callers that need determinism pass the id.
fn client_for_name_with_id(
    tx: &rusqlite::Transaction<'_>,
    client_name: &ClientName,
) -> Result<Option<(String, ExistingClient)>, StoreError> {
    let row = tx
        .query_row(
            "SELECT client_id, client_address FROM client_tokens WHERE client_name = ? LIMIT 1",
            rusqlite::params![client_name.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(map_rusqlite)?;
    Ok(row.map(|(id, client_address)| (id, ExistingClient::Present { client_address })))
}

fn prune_stale_enrollments(
    tx: &rusqlite::Transaction<'_>,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    let cutoff = (now - Duration::days(1)).to_rfc3339();
    tx.execute(
        "DELETE FROM client_enrollments \
         WHERE (consumed_at IS NOT NULL AND consumed_at <= ?) \
            OR (expires_at <= ?)",
        rusqlite::params![cutoff, cutoff],
    )
    .map_err(map_rusqlite)?;
    Ok(())
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>, RedeemEnrollmentError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            RedeemEnrollmentError::Store(StoreError::Corruption {
                detail: format!("bad RFC3339 ts: {e}"),
            })
        })
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use tempfile::tempdir;

    fn test_store() -> Arc<Store> {
        let dir = tempdir().unwrap();
        Arc::new(Store::open(dir.path()).unwrap())
    }

    fn consumed_at_of(store: &Store, code: &str) -> Option<String> {
        let hash = fingerprint::hex(&token::hash_token(code));
        store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT consumed_at FROM client_enrollments WHERE code_hash = ?",
                    rusqlite::params![hash],
                    |row| row.get::<_, Option<String>>(0),
                )
                .map_err(map_rusqlite)
            })
            .unwrap()
    }

    fn token_hash_of(store: &Store, client_name: &str) -> Option<String> {
        store
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT token_hash FROM client_tokens WHERE client_name = ?",
                    rusqlite::params![client_name],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(map_rusqlite)
            })
            .unwrap()
    }

    #[test]
    fn create_persists_and_redeem_returns_advertised_endpoint() {
        let store = test_store();
        let es = ClientEnrollmentStore::new(Arc::clone(&store));
        let now = Utc::now();
        let created = es
            .create(CreateEnrollment {
                client_name: ClientName::from_str("edge-1").unwrap(),
                client_id: None,
                target: EnrollmentTarget::New {
                    client_address: None,
                },
                expires_at: now + chrono::Duration::seconds(300),
                now,
                advertised_endpoint: "public.example:7443".to_string(),
            })
            .unwrap();
        // Persisted-endpoint rows must NEVER invoke the legacy resolver.
        let issued = es
            .redeem(&created.code, Utc::now(), || {
                panic!("resolver must not run for persisted-endpoint rows")
            })
            .unwrap();
        assert_eq!(
            issued.advertised_endpoint.as_deref(),
            Some("public.example:7443")
        );
    }

    /// Insert a pre-V010-style row (NULL `advertised_endpoint`) plus a
    /// pre-existing `client_tokens` row, so a redeem of this legacy code
    /// would *rotate* the existing token. Returns the legacy code.
    fn seed_legacy_existing_client(store: &Store, name: &str, code: &str) -> String {
        let now = Utc::now();
        let prior_token = "prior-token-for-existing-client";
        store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO client_tokens \
                     (client_id, client_name, token_hash, issued_at, revoked_at, client_address) \
                     VALUES (?, ?, ?, ?, NULL, NULL)",
                    rusqlite::params![
                        ClientId::new().to_string(),
                        name,
                        fingerprint::hex(&token::hash_token(prior_token)),
                        now.to_rfc3339(),
                    ],
                )
                .unwrap();
                tx.execute(
                    "INSERT INTO client_enrollments \
                     (client_name, client_address, code_hash, issued_at, expires_at, consumed_at, advertised_endpoint) \
                     VALUES (?, NULL, ?, ?, ?, NULL, NULL)",
                    rusqlite::params![
                        name,
                        fingerprint::hex(&token::hash_token(code)),
                        now.to_rfc3339(),
                        (now + chrono::Duration::seconds(300)).to_rfc3339(),
                    ],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        fingerprint::hex(&token::hash_token(prior_token))
    }

    /// 015-client-stable-id (T014): a re-enrollment created against an
    /// existing client carries its stable id. If the client is RENAMED
    /// between create and redeem, the redeem must rotate that same client's
    /// token (resolved by id) and return the CURRENT name — never fork a
    /// brand-new client under the stale name.
    #[test]
    fn reenrollment_with_id_survives_a_rename_between_create_and_redeem() {
        let store = test_store();
        let es = ClientEnrollmentStore::new(Arc::clone(&store));
        let now = Utc::now();

        // Seed an existing client "old-name" with a stable id.
        let client_id = ClientId::new();
        store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO client_tokens \
                     (client_id, client_name, token_hash, issued_at, revoked_at, client_address) \
                     VALUES (?, ?, ?, ?, NULL, NULL)",
                    rusqlite::params![
                        client_id.to_string(),
                        "old-name",
                        fingerprint::hex(&token::hash_token("seed-token")),
                        now.to_rfc3339(),
                    ],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();

        // Operator creates a re-enrollment addressed by id.
        let created = es
            .create(CreateEnrollment {
                client_name: ClientName::from_str("old-name").unwrap(),
                client_id: Some(client_id),
                target: EnrollmentTarget::Existing,
                expires_at: now + chrono::Duration::seconds(300),
                now,
                advertised_endpoint: "public.example:7443".to_string(),
            })
            .unwrap();

        // Client is renamed before the code is redeemed.
        store
            .with_write_tx(|tx| {
                tx.execute(
                    "UPDATE client_tokens SET client_name = ? WHERE client_id = ?",
                    rusqlite::params!["new-name", client_id.to_string()],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();

        let issued = es
            .redeem(&created.code, Utc::now(), || {
                panic!("persisted-endpoint row must not call the legacy resolver")
            })
            .unwrap();

        // Same identity, rotated (not forked), current display name.
        assert_eq!(issued.client_id, client_id);
        assert_eq!(issued.client_name.as_str(), "new-name");
        assert!(issued.rotated_existing);

        // Exactly one client row — no fork under the stale name.
        let client_count: i64 = store
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM client_tokens", [], |r| r.get(0))
                    .map_err(map_rusqlite)
            })
            .unwrap();
        assert_eq!(client_count, 1, "redeem must not fork a new client");
    }

    #[test]
    fn legacy_failed_resolution_leaves_enrollment_unconsumed_and_token_unrotated() {
        let store = test_store();
        let es = ClientEnrollmentStore::new(Arc::clone(&store));
        let code = "legacycode000000000000000000000000000000000000000000000000000000";
        let prior_hash = seed_legacy_existing_client(&store, "legacy-1", code);

        // Fail-closed resolver: the redeem must surface the error and
        // roll back — no consume, no token rotation.
        let err = es
            .redeem(code, Utc::now(), || {
                Err(RedeemEnrollmentError::AdvertisedEndpoint(
                    "advertised_endpoint_unresolved".to_string(),
                ))
            })
            .unwrap_err();
        assert!(matches!(err, RedeemEnrollmentError::AdvertisedEndpoint(_)));

        // Enrollment still redeemable: consumed_at IS NULL.
        assert_eq!(
            consumed_at_of(&store, code),
            None,
            "failed legacy resolution must NOT consume the enrollment"
        );
        // Existing client's token must be byte-identical (not rotated).
        assert_eq!(
            token_hash_of(&store, "legacy-1").as_deref(),
            Some(prior_hash.as_str()),
            "failed legacy resolution must NOT rotate the client token"
        );

        // Now a SUCCEEDING resolver redeems normally and consumes.
        let issued = es
            .redeem(code, Utc::now(), || Ok("public.example:7443".to_string()))
            .unwrap();
        assert_eq!(
            issued.advertised_endpoint.as_deref(),
            Some("public.example:7443")
        );
        assert!(
            consumed_at_of(&store, code).is_some(),
            "successful redeem must consume the enrollment"
        );
        assert_ne!(
            token_hash_of(&store, "legacy-1").as_deref(),
            Some(prior_hash.as_str()),
            "successful redeem must rotate the existing client token"
        );
    }

    /// Verifies the hoisted-settings-read + pure-resolver path (the C1
    /// fix in `grpc/enrollment.rs`) composes correctly through a real
    /// redeem of a legacy NULL-`advertised_endpoint` row. This test
    /// constructs the `resolve_legacy` closure exactly as the fixed
    /// `enroll` handler does (real `SqliteSettingsStore.get_advertised_endpoint()`
    /// called before `redeem`, result moved into a closure that calls the
    /// pure resolver) and exercises it against a real `ClientEnrollmentStore`
    /// with a seeded legacy NULL-`advertised_endpoint` row. Confirms:
    /// - the hoisted settings read composes correctly with the closure;
    /// - the pure resolver (`resolve_advertised_endpoint`) resolves the
    ///   override to the expected endpoint string;
    /// - `redeem` succeeds, returns `advertised_endpoint == Some("public.example:7443")`,
    ///   and the enrollment is consumed.
    ///
    /// Note: this does NOT reproduce the C1 1-vCPU nested-pool-checkout
    /// deadlock — at the default test-host pool size (> 1) this would
    /// pass even against the original buggy nested-checkout code. The
    /// deadlock is prevented structurally: the closure no longer performs
    /// any pool access (the settings read is hoisted ahead of the redeem
    /// transaction). This test guards that composition, not the deadlock.
    #[test]
    fn legacy_null_row_redeems_via_hoisted_settings_read_and_pure_resolver() {
        use crate::advertised::CertSanSet;
        use crate::store::settings_store::SqliteSettingsStore;

        const FIXTURE_PEM: &str = include_str!("../advertised/testdata/san_fixture.pem");
        const OVERRIDE_EP: &str = "public.example:7443";
        const CODE: &str = "legacyc1code0000000000000000000000000000000000000000000000000000";

        let store = test_store();
        let es = ClientEnrollmentStore::new(Arc::clone(&store));

        // Persist the operator override so the settings read returns Some.
        let settings = SqliteSettingsStore::new(Arc::clone(&store));
        settings
            .set_advertised_endpoint(Some(OVERRIDE_EP.to_string()))
            .expect("set override");

        // Seed a legacy NULL-advertised_endpoint enrollment row.
        seed_legacy_existing_client(&store, "legacy-c1", CODE);

        // Build the resolve closure EXACTLY as the fixed enroll() handler:
        // hoist the settings DB read before redeem, move the Result into
        // the closure, run the pure resolver inside.
        let cert_san = std::sync::Arc::new(CertSanSet::from_pem(FIXTURE_PEM).unwrap());
        let advertised_seed: Option<String> = None;
        let control_port: u16 = 7443;

        let pre_override = settings.get_advertised_endpoint(); // ← hoisted (no DB inside redeem tx)
        let resolve_legacy = move || -> Result<String, RedeemEnrollmentError> {
            let override_value = pre_override.map_err(RedeemEnrollmentError::Store)?;
            crate::advertised::resolve_advertised_endpoint(
                &crate::advertised::resolve::ResolveInputs {
                    override_value,
                    seed: advertised_seed,
                    req_host: None,
                    control_port,
                    san: &cert_san,
                },
            )
            .map(|r| r.endpoint)
            .map_err(|e| RedeemEnrollmentError::AdvertisedEndpoint(e.http_code().to_string()))
        };

        let issued = es
            .redeem(CODE, Utc::now(), resolve_legacy)
            .expect("redeem must succeed for legacy NULL row with valid settings");

        assert_eq!(
            issued.advertised_endpoint.as_deref(),
            Some(OVERRIDE_EP),
            "resolved endpoint must match the settings override"
        );
        assert!(
            consumed_at_of(&store, CODE).is_some(),
            "enrollment must be consumed after successful redeem"
        );
    }
}
