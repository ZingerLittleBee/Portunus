//! Short-lived client enrollment code store.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use portunus_auth::token;
use portunus_core::{ClientName, fingerprint};
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
            let existing_client = client_for(tx, &input.client_name)?;
            let client_address = match &input.target {
                EnrollmentTarget::New { client_address } => {
                    if matches!(existing_client, ExistingClient::Present { .. }) {
                        return Ok(Err(CreateEnrollmentError::ClientAlreadyExists(
                            input.client_name.clone(),
                        )));
                    }
                    client_address.clone()
                }
                EnrollmentTarget::Existing => match existing_client {
                    ExistingClient::Present { client_address } => client_address,
                    ExistingClient::Absent => {
                        return Ok(Err(CreateEnrollmentError::ClientNotFound(
                            input.client_name.clone(),
                        )));
                    }
                },
            };
            tx.execute(
                "UPDATE client_enrollments \
                 SET consumed_at = ? \
                 WHERE client_name = ? AND consumed_at IS NULL",
                rusqlite::params![issued_at, input.client_name.as_str()],
            )
            .map_err(map_rusqlite)?;
            tx.execute(
                "INSERT INTO client_enrollments \
                 (client_name, client_address, code_hash, issued_at, expires_at, consumed_at, advertised_endpoint) \
                 VALUES (?, ?, ?, ?, ?, NULL, ?)",
                rusqlite::params![
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
                    "SELECT id, client_name, client_address, code_hash, expires_at, consumed_at, advertised_endpoint \
                         FROM client_enrollments",
                )
                .map_err(map_rusqlite)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok(EnrollmentRow {
                        id: row.get(0)?,
                        client_name: row.get(1)?,
                        client_address: row.get(2)?,
                        code_hash: row.get(3)?,
                        expires_at: row.get(4)?,
                        consumed_at: row.get(5)?,
                        advertised_endpoint: row.get(6)?,
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

            let existing_client: bool = tx
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
            if existing_client {
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
            } else {
                tx.execute(
                    "INSERT INTO client_tokens \
                         (client_name, token_hash, issued_at, revoked_at, client_address) \
                         VALUES (?, ?, ?, NULL, ?)",
                    rusqlite::params![
                        client_name.as_str(),
                        client_token_hash,
                        consumed_at,
                        row.client_address.as_deref()
                    ],
                )
                .map_err(map_rusqlite)?;
            }

            tx.execute(
                "UPDATE client_enrollments SET consumed_at = ? WHERE id = ?",
                rusqlite::params![consumed_at, row.id],
            )
            .map_err(map_rusqlite)?;

            Ok(Ok(IssuedClientCredential {
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

fn client_for(
    tx: &rusqlite::Transaction<'_>,
    client_name: &ClientName,
) -> Result<ExistingClient, StoreError> {
    let client_address = tx
        .query_row(
            "SELECT client_address FROM client_tokens WHERE client_name = ? LIMIT 1",
            rusqlite::params![client_name.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(map_rusqlite)?;
    Ok(match client_address {
        Some(client_address) => ExistingClient::Present { client_address },
        None => ExistingClient::Absent,
    })
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
                     (client_name, token_hash, issued_at, revoked_at, client_address) \
                     VALUES (?, ?, ?, NULL, NULL)",
                    rusqlite::params![
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
}
