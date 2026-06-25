//! 008-sqlite-storage T044 — SQLite-backed `OperatorAuthenticator`.
//!
//! Replaces `portunus_auth::operator_store::FileOperatorStore`. Mirrors
//! the FileOperatorStore public API so callers keep the same signatures.
//! All multi-table mutations (delete-cascade, rotate, bootstrap pair)
//! commit in a single `BEGIN IMMEDIATE` transaction (R-014).
//!
//! Token hash storage: hex(blake3) — same encoding the file store used,
//! preserving forensic-trail compatibility for any operator who reads
//! the column out-of-band.

use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use portunus_auth::{
    ClientScope, Credential, CredentialId, CredentialStatus, Grant, GrantId, IdentityStoreError,
    OperatorAuthenticator, OperatorIdentity, OperatorRole, ProtocolSet, RbacError, User, UserId,
    UserRemoveSummary, token::hash_token,
};
use portunus_core::{ClientName, fingerprint};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::operator::{
    audit::AuditEntry,
    sessions,
    setup_token::{DEFAULT_SETUP_TOKEN_TTL, SetupTokenRecord},
    throttle::{AuthThrottleAction, ThrottleDecision},
};
use crate::store::{Store, StoreError, map_rusqlite};

#[derive(Clone)]
pub struct SqliteOperatorStore {
    store: Arc<Store>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct PasswordState {
    pub hash: String,
    pub password_change_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct WebSession {
    pub user_id: UserId,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub absolute_expires_at: DateTime<Utc>,
    pub remote_addr: Option<String>,
    pub user_agent: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PasswordResetSummary {
    pub sessions_revoked: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OnboardingError {
    AlreadyBootstrapped,
    InvalidSetupToken,
    UserAlreadyExists(UserId),
    Store(String),
}

impl std::fmt::Debug for SqliteOperatorStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteOperatorStore")
            .field("db", &self.store.db_path())
            .finish()
    }
}

impl SqliteOperatorStore {
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    // ---------------- read accessors ----------------

    pub fn list_users(&self) -> Vec<User> {
        self.store
            .with_conn(|c| {
                let mut stmt = c
                    .prepare(
                        "SELECT user_id, role, display_name, disabled, created_at \
                         FROM users ORDER BY user_id ASC",
                    )
                    .map_err(map_rusqlite)?;
                let mut out = Vec::new();
                let rows = stmt.query_map([], row_to_user).map_err(map_rusqlite)?;
                for r in rows {
                    out.push(r.map_err(map_rusqlite)?);
                }
                Ok(out)
            })
            .unwrap_or_default()
    }

    pub fn get_user(&self, id: &UserId) -> Option<User> {
        self.store
            .with_conn(|c| {
                let row = c
                    .query_row(
                        "SELECT user_id, role, display_name, disabled, created_at \
                         FROM users WHERE user_id = ?",
                        params![id.as_str()],
                        row_to_user,
                    )
                    .optional()
                    .map_err(map_rusqlite)?;
                Ok(row)
            })
            .unwrap_or(None)
    }

    pub fn list_grants(&self, user_filter: Option<&UserId>) -> Vec<Grant> {
        self.store
            .with_conn(|c| {
                let mut out = Vec::new();
                if let Some(uid) = user_filter {
                    let mut stmt = c
                        .prepare(
                            "SELECT grant_id, user_id, client, listen_port_start, listen_port_end, \
                                    protocols, note, created_at FROM grants WHERE user_id = ? \
                             ORDER BY grant_id ASC",
                        )
                        .map_err(map_rusqlite)?;
                    let rows = stmt
                        .query_map(params![uid.as_str()], row_to_grant)
                        .map_err(map_rusqlite)?;
                    for r in rows {
                        out.push(r.map_err(map_rusqlite)?);
                    }
                } else {
                    let mut stmt = c
                        .prepare(
                            "SELECT grant_id, user_id, client, listen_port_start, listen_port_end, \
                                    protocols, note, created_at FROM grants ORDER BY grant_id ASC",
                        )
                        .map_err(map_rusqlite)?;
                    let rows = stmt.query_map([], row_to_grant).map_err(map_rusqlite)?;
                    for r in rows {
                        out.push(r.map_err(map_rusqlite)?);
                    }
                }
                Ok(out)
            })
            .unwrap_or_default()
    }

    pub fn get_grant(&self, id: &GrantId) -> Option<Grant> {
        self.store
            .with_conn(|c| {
                let row = c
                    .query_row(
                        "SELECT grant_id, user_id, client, listen_port_start, listen_port_end, \
                                protocols, note, created_at FROM grants WHERE grant_id = ?",
                        params![id.to_string()],
                        row_to_grant,
                    )
                    .optional()
                    .map_err(map_rusqlite)?;
                Ok(row)
            })
            .unwrap_or(None)
    }

    pub fn count_superadmins(&self) -> usize {
        self.store
            .with_conn(|c| {
                let n: i64 = c
                    .query_row(
                        "SELECT COUNT(*) FROM users WHERE role = 'superadmin' AND disabled = 0",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(map_rusqlite)?;
                Ok(n as usize)
            })
            .unwrap_or(0)
    }

    pub(crate) fn has_active_superadmin(&self) -> Result<bool, IdentityStoreError> {
        self.store
            .with_conn(|c| {
                let n: i64 = c
                    .query_row(
                        "SELECT COUNT(*) FROM users WHERE role = 'superadmin' AND disabled = 0",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(map_rusqlite)?;
                Ok(n > 0)
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))
    }

    #[allow(dead_code)]
    pub(crate) fn set_password_hash(
        &self,
        user_id: &UserId,
        hash: &str,
        password_change_required: bool,
    ) -> Result<(), IdentityStoreError> {
        let uid_for_err = user_id.clone();
        self.store
            .with_write_tx(|tx| {
                let changed = tx
                    .execute(
                        "UPDATE users SET password_hash = ?, password_change_required = ? \
                         WHERE user_id = ?",
                        params![hash, i32::from(password_change_required), user_id.as_str()],
                    )
                    .map_err(map_rusqlite)?;
                if changed == 0 {
                    return Err(StoreError::Conflict {
                        detail: "user_not_found".into(),
                    });
                }
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "user_not_found" => {
                    IdentityStoreError::UserNotFound(uid_for_err)
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    pub(crate) fn reset_password_state(
        &self,
        user_id: &UserId,
        hash: &str,
        password_change_required: bool,
        revoke_sessions: bool,
    ) -> Result<PasswordResetSummary, IdentityStoreError> {
        let uid_for_err = user_id.clone();
        let now = Utc::now().to_rfc3339();
        self.store
            .with_write_tx(|tx| {
                if !user_exists(tx, user_id)? {
                    return Err(StoreError::Conflict {
                        detail: "user_not_found".into(),
                    });
                }
                tx.execute(
                    "UPDATE users SET password_hash = ?, password_change_required = ? \
                     WHERE user_id = ?",
                    params![hash, i32::from(password_change_required), user_id.as_str()],
                )
                .map_err(map_rusqlite)?;

                let sessions_revoked = if revoke_sessions {
                    tx.execute(
                        "UPDATE web_sessions \
                         SET revoked_at = COALESCE(revoked_at, ?) \
                         WHERE user_id = ? AND revoked_at IS NULL",
                        params![now, user_id.as_str()],
                    )
                    .map_err(map_rusqlite)?
                } else {
                    0
                };

                Ok(PasswordResetSummary { sessions_revoked })
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "user_not_found" => {
                    IdentityStoreError::UserNotFound(uid_for_err)
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    #[allow(dead_code)]
    pub(crate) fn password_state(
        &self,
        user_id: &UserId,
    ) -> Result<Option<PasswordState>, IdentityStoreError> {
        let uid_for_err = user_id.clone();
        self.store
            .with_conn(|c| {
                let row = c
                    .query_row(
                        "SELECT password_hash, password_change_required \
                         FROM users WHERE user_id = ?",
                        params![user_id.as_str()],
                        |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i32>(1)? != 0)),
                    )
                    .optional()
                    .map_err(map_rusqlite)?;
                match row {
                    None => Err(StoreError::Conflict {
                        detail: "user_not_found".into(),
                    }),
                    Some((None, _)) => Ok(None),
                    Some((Some(hash), password_change_required)) => Ok(Some(PasswordState {
                        hash,
                        password_change_required,
                    })),
                }
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "user_not_found" => {
                    IdentityStoreError::UserNotFound(uid_for_err)
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    #[allow(dead_code)]
    pub(crate) fn create_web_session(
        &self,
        session_hash: &str,
        user_id: &UserId,
        created_at: DateTime<Utc>,
        absolute_expires_at: DateTime<Utc>,
        remote_addr: Option<String>,
        user_agent: Option<String>,
    ) -> Result<(), IdentityStoreError> {
        let uid_for_err = user_id.clone();
        self.store
            .with_write_tx(|tx| {
                if !user_exists(tx, user_id)? {
                    return Err(StoreError::Conflict {
                        detail: "user_not_found".into(),
                    });
                }
                tx.execute(
                    "INSERT INTO web_sessions \
                        (session_hash, user_id, created_at, last_seen_at, absolute_expires_at, revoked_at, remote_addr, user_agent) \
                     VALUES (?, ?, ?, ?, ?, NULL, ?, ?)",
                    params![
                        session_hash,
                        user_id.as_str(),
                        created_at.to_rfc3339(),
                        created_at.to_rfc3339(),
                        absolute_expires_at.to_rfc3339(),
                        remote_addr,
                        user_agent,
                    ],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "user_not_found" => {
                    IdentityStoreError::UserNotFound(uid_for_err)
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    #[allow(dead_code)]
    pub(crate) fn verify_web_session(
        &self,
        session_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<WebSession>, IdentityStoreError> {
        self.store
            .with_write_tx(|tx| {
                let row = tx
                    .query_row(
                        "SELECT user_id, created_at, last_seen_at, absolute_expires_at, revoked_at, remote_addr, user_agent \
                         FROM web_sessions WHERE session_hash = ?",
                        params![session_hash],
                        row_to_web_session_record,
                    )
                    .optional()
                    .map_err(map_rusqlite)?;

                let Some((mut session, revoked_at)) = row else {
                    return Ok(None);
                };

                if revoked_at.is_some()
                    || sessions::session_is_expired(
                        session.last_seen_at,
                        session.absolute_expires_at,
                        now,
                    )
                {
                    return Ok(None);
                }

                tx.execute(
                    "UPDATE web_sessions SET last_seen_at = ? \
                     WHERE session_hash = ? AND revoked_at IS NULL",
                    params![now.to_rfc3339(), session_hash],
                )
                .map_err(map_rusqlite)?;
                session.last_seen_at = now;

                Ok(Some(session))
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))
    }

    #[cfg(test)]
    fn delete_web_session_for_test(&self, session_hash: &str) -> Result<usize, IdentityStoreError> {
        self.store
            .with_write_tx(|tx| {
                tx.execute(
                    "DELETE FROM web_sessions WHERE session_hash = ?",
                    params![session_hash],
                )
                .map_err(map_rusqlite)
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))
    }

    #[allow(dead_code)]
    pub(crate) fn revoke_web_session(&self, session_hash: &str) -> Result<(), IdentityStoreError> {
        let revoked_at = Utc::now().to_rfc3339();
        self.store
            .with_write_tx(|tx| {
                tx.execute(
                    "UPDATE web_sessions \
                     SET revoked_at = COALESCE(revoked_at, ?) \
                     WHERE session_hash = ?",
                    params![revoked_at, session_hash],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))
    }

    #[allow(dead_code)]
    pub(crate) fn revoke_web_sessions_for_user(
        &self,
        user_id: &UserId,
    ) -> Result<(), IdentityStoreError> {
        let uid_for_err = user_id.clone();
        let revoked_at = Utc::now().to_rfc3339();
        self.store
            .with_write_tx(|tx| {
                if !user_exists(tx, user_id)? {
                    return Err(StoreError::Conflict {
                        detail: "user_not_found".into(),
                    });
                }
                tx.execute(
                    "UPDATE web_sessions \
                     SET revoked_at = COALESCE(revoked_at, ?) \
                     WHERE user_id = ?",
                    params![revoked_at, user_id.as_str()],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "user_not_found" => {
                    IdentityStoreError::UserNotFound(uid_for_err)
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    #[allow(dead_code)]
    pub(crate) fn prune_expired_web_sessions(
        &self,
        now: DateTime<Utc>,
    ) -> Result<usize, IdentityStoreError> {
        let idle_cutoff = (now - sessions::IDLE_TIMEOUT).to_rfc3339();
        let absolute_cutoff = now.to_rfc3339();
        self.store
            .with_write_tx(|tx| {
                let deleted = tx
                    .execute(
                        "DELETE FROM web_sessions \
                         WHERE revoked_at IS NOT NULL \
                            OR absolute_expires_at < ? \
                            OR last_seen_at < ?",
                        params![absolute_cutoff, idle_cutoff],
                    )
                    .map_err(map_rusqlite)?;
                Ok(deleted)
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))
    }

    pub(crate) fn rotate_onboarding_setup_token(
        &self,
        now: DateTime<Utc>,
    ) -> Result<String, IdentityStoreError> {
        let (raw, record) = SetupTokenRecord::new(now, DEFAULT_SETUP_TOKEN_TTL);
        self.store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO onboarding_setup (id, token_hash, issued_at, expires_at) \
                     VALUES (1, ?, ?, ?) \
                     ON CONFLICT(id) DO UPDATE SET \
                        token_hash = excluded.token_hash, \
                        issued_at = excluded.issued_at, \
                        expires_at = excluded.expires_at",
                    params![
                        record.hash_hex(),
                        now.to_rfc3339(),
                        record.expires_at().to_rfc3339(),
                    ],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))?;
        Ok(raw)
    }

    pub(crate) fn insert_audit_entry(&self, entry: &AuditEntry) -> Result<(), IdentityStoreError> {
        self.store
            .with_write_tx(|tx| {
                let action = entry
                    .action
                    .clone()
                    .unwrap_or_else(|| format!("{} {}", entry.method, entry.path));
                let mut details = serde_json::json!({
                    "role": entry.role,
                    "reason": entry.reason,
                });
                if let Some(extra) = entry.details.as_ref()
                    && let (Some(base), Some(extra)) = (details.as_object_mut(), extra.as_object())
                {
                    for (key, value) in extra {
                        base.insert(key.clone(), value.clone());
                    }
                }
                tx.execute(
                    "INSERT INTO audit \
                     (ts, user_id, outcome, action, resource_kind, resource_value, correlation_id, details_json) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        entry.timestamp.to_rfc3339(),
                        if entry.actor.is_empty() {
                            None
                        } else {
                            Some(entry.actor.as_str())
                        },
                        entry.outcome.as_str(),
                        action,
                        entry.resource_kind.as_deref(),
                        entry.resource_value.as_deref(),
                        "",
                        details.to_string(),
                    ],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))
    }

    #[allow(dead_code)]
    pub(crate) fn verify_onboarding_setup_token(
        &self,
        candidate_token: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, IdentityStoreError> {
        self.store
            .with_conn(|c| {
                let stored_token = c
                    .query_row(
                        "SELECT token_hash, expires_at FROM onboarding_setup WHERE id = 1",
                        [],
                        row_to_setup_token_record,
                    )
                    .optional()
                    .map_err(map_rusqlite)?;
                Ok(stored_token.is_some_and(|record| record.verify(candidate_token, now)))
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))
    }

    #[allow(dead_code)]
    pub(crate) fn clear_onboarding_setup_token(&self) -> Result<(), IdentityStoreError> {
        self.store
            .with_write_tx(|tx| {
                tx.execute("DELETE FROM onboarding_setup WHERE id = 1", [])
                    .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))
    }

    #[allow(dead_code)]
    pub(crate) fn login_attempt_state(
        &self,
        subject: &str,
        remote_addr: &str,
        action: AuthThrottleAction,
        now: DateTime<Utc>,
    ) -> Result<ThrottleDecision, IdentityStoreError> {
        self.store
            .with_conn(|c| {
                let row = c
                    .query_row(
                        "SELECT failures, first_failed_at, last_failed_at, locked_until \
                         FROM login_attempts \
                         WHERE subject = ? AND remote_addr = ? AND action = ?",
                        params![subject, remote_addr, action.as_db_str()],
                        row_to_login_attempt,
                    )
                    .optional()
                    .map_err(map_rusqlite)?;
                Ok(row.unwrap_or_default().effective_at(now))
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))
    }

    #[allow(dead_code)]
    pub(crate) fn record_login_attempt_failure(
        &self,
        subject: &str,
        remote_addr: &str,
        action: AuthThrottleAction,
        now: DateTime<Utc>,
    ) -> Result<ThrottleDecision, IdentityStoreError> {
        self.store
            .with_write_tx(|tx| {
                let mut state = tx
                    .query_row(
                        "SELECT failures, first_failed_at, last_failed_at, locked_until \
                         FROM login_attempts \
                         WHERE subject = ? AND remote_addr = ? AND action = ?",
                        params![subject, remote_addr, action.as_db_str()],
                        row_to_login_attempt,
                    )
                    .optional()
                    .map_err(map_rusqlite)?
                    .unwrap_or_default();
                state.record_failure(now);

                tx.execute(
                    "INSERT INTO login_attempts \
                        (subject, remote_addr, action, failures, first_failed_at, last_failed_at, locked_until) \
                     VALUES (?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT(subject, remote_addr, action) DO UPDATE SET \
                        failures = excluded.failures, \
                        first_failed_at = excluded.first_failed_at, \
                        last_failed_at = excluded.last_failed_at, \
                        locked_until = excluded.locked_until",
                    params![
                        subject,
                        remote_addr,
                        action.as_db_str(),
                        i64::from(state.failures),
                        state.first_failed_at.map(|ts| ts.to_rfc3339()),
                        state.last_failed_at.map(|ts| ts.to_rfc3339()),
                        state.locked_until.map(|ts| ts.to_rfc3339()),
                    ],
                )
                .map_err(map_rusqlite)?;

                Ok(state)
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))
    }

    #[allow(dead_code)]
    pub(crate) fn clear_login_attempts(
        &self,
        subject: &str,
        remote_addr: &str,
        action: AuthThrottleAction,
    ) -> Result<(), IdentityStoreError> {
        self.store
            .with_write_tx(|tx| {
                tx.execute(
                    "DELETE FROM login_attempts \
                     WHERE subject = ? AND remote_addr = ? AND action = ?",
                    params![subject, remote_addr, action.as_db_str()],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))
    }

    // ---------------- atomic bootstrap paths ----------------

    pub fn bootstrap_pair(&self, user: User, cred: Credential) -> Result<(), IdentityStoreError> {
        if cred.user_id != user.id {
            return Err(IdentityStoreError::WriteFailed(
                "bootstrap_pair: cred.user_id must equal user.id".into(),
            ));
        }
        let user_for_err = user.id.clone();
        self.store
            .with_write_tx(|tx| {
                if user_exists(tx, &user.id)? {
                    return Err(StoreError::Conflict {
                        detail: "user_already_exists".into(),
                    });
                }
                insert_user(tx, &user)?;
                insert_credential(tx, &cred)?;
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { .. } => IdentityStoreError::UserAlreadyExists(user_for_err),
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    pub fn bootstrap_legacy_superadmin(&self, raw_token: &str) -> Result<(), IdentityStoreError> {
        if raw_token.is_empty() || raw_token.len() > 256 {
            return Err(IdentityStoreError::WriteFailed(
                "operator_token must be 1..=256 bytes".into(),
            ));
        }
        let user = User {
            id: UserId::reserved("_legacy"),
            display_name: "operator_token shortcut".into(),
            role: OperatorRole::Superadmin,
            created_at: Utc::now(),
            disabled: false,
        };
        let cred = Credential {
            id: CredentialId::new(),
            user_id: user.id.clone(),
            token_hash: hash_token(raw_token),
            label: Some("operator_token (server.toml)".into()),
            created_at: Utc::now(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };

        self.store
            .with_write_tx(|tx| {
                let n: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM users WHERE role = 'superadmin' AND disabled = 0",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(map_rusqlite)?;
                if n > 0 {
                    return Err(StoreError::Conflict {
                        detail: "already_bootstrapped".into(),
                    });
                }
                insert_user(tx, &user)?;
                insert_credential(tx, &cred)?;
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { .. } => {
                    IdentityStoreError::UserAlreadyExists(UserId::reserved("_legacy"))
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    pub(crate) fn onboard_first_superadmin(
        &self,
        user: User,
        password_hash: &str,
        setup_token: &str,
        now: DateTime<Utc>,
    ) -> Result<(), OnboardingError> {
        if user.role != OperatorRole::Superadmin {
            return Err(OnboardingError::Store(
                "onboard_first_superadmin requires a superadmin user".into(),
            ));
        }
        let user_for_err = user.id.clone();
        self.store
            .with_write_tx(|tx| {
                let active_superadmins: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM users WHERE role = 'superadmin' AND disabled = 0",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(map_rusqlite)?;
                if active_superadmins > 0 {
                    return Err(StoreError::Conflict {
                        detail: "already_bootstrapped".into(),
                    });
                }

                let setup = tx
                    .query_row(
                        "SELECT token_hash, expires_at FROM onboarding_setup WHERE id = 1",
                        [],
                        row_to_setup_token_record,
                    )
                    .optional()
                    .map_err(map_rusqlite)?;
                if !setup.is_some_and(|record| record.verify(setup_token, now)) {
                    return Err(StoreError::Conflict {
                        detail: "setup_token_invalid".into(),
                    });
                }

                if user_exists(tx, &user.id)? {
                    return Err(StoreError::Conflict {
                        detail: "user_already_exists".into(),
                    });
                }
                insert_user(tx, &user)?;
                tx.execute(
                    "UPDATE users SET password_hash = ?, password_change_required = 0 \
                     WHERE user_id = ?",
                    params![password_hash, user.id.as_str()],
                )
                .map_err(map_rusqlite)?;
                tx.execute("DELETE FROM onboarding_setup WHERE id = 1", [])
                    .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "already_bootstrapped" => {
                    OnboardingError::AlreadyBootstrapped
                }
                StoreError::Conflict { detail } if detail == "setup_token_invalid" => {
                    OnboardingError::InvalidSetupToken
                }
                StoreError::Conflict { detail } if detail == "user_already_exists" => {
                    OnboardingError::UserAlreadyExists(user_for_err)
                }
                other => OnboardingError::Store(other.to_string()),
            })
    }

    // ---------------- mutating ops ----------------

    pub fn add_user(&self, user: User) -> Result<(), IdentityStoreError> {
        let user_for_err = user.id.clone();
        self.store
            .with_write_tx(|tx| {
                if user_exists(tx, &user.id)? {
                    return Err(StoreError::Conflict {
                        detail: "user_already_exists".into(),
                    });
                }
                insert_user(tx, &user)?;
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { .. } => IdentityStoreError::UserAlreadyExists(user_for_err),
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    pub(crate) fn add_user_with_password(
        &self,
        user: User,
        password_hash: Option<&str>,
        password_change_required: bool,
    ) -> Result<(), IdentityStoreError> {
        let user_for_err = user.id.clone();
        self.store
            .with_write_tx(|tx| {
                if user_exists(tx, &user.id)? {
                    return Err(StoreError::Conflict {
                        detail: "user_already_exists".into(),
                    });
                }
                insert_user(tx, &user)?;
                if let Some(hash) = password_hash {
                    tx.execute(
                        "UPDATE users SET password_hash = ?, password_change_required = ? \
                         WHERE user_id = ?",
                        params![hash, i32::from(password_change_required), user.id.as_str()],
                    )
                    .map_err(map_rusqlite)?;
                }
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { .. } => IdentityStoreError::UserAlreadyExists(user_for_err),
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    pub fn remove_user(&self, user_id: &UserId) -> Result<UserRemoveSummary, IdentityStoreError> {
        let uid_for_err = user_id.clone();
        self.store
            .with_write_tx(|tx| {
                if !user_exists(tx, user_id)? {
                    return Err(StoreError::Conflict {
                        detail: "user_not_found".into(),
                    });
                }

                let cred_ids = collect_credential_ids(tx, user_id)?;
                let grant_ids = collect_grant_ids(tx, user_id)?;

                // FK CASCADE handles the actual row deletion; we only need
                // to remove the parent row.
                tx.execute(
                    "DELETE FROM users WHERE user_id = ?",
                    params![user_id.as_str()],
                )
                .map_err(map_rusqlite)?;

                Ok(UserRemoveSummary {
                    removed_credential_ids: cred_ids,
                    revoked_grant_ids: grant_ids,
                })
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "user_not_found" => {
                    IdentityStoreError::UserNotFound(uid_for_err)
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    /// Test-only seam: the user-facing credential-issuance surface (HTTP/CLI/UI)
    /// was removed. This mints a bearer token by inserting a credential row
    /// directly (same mechanism `bootstrap_legacy_superadmin` uses) so in-process
    /// integration tests can authenticate a seeded operator user. NOT a product API.
    #[doc(hidden)]
    pub fn seed_credential_for_test(
        &self,
        user_id: &UserId,
        label: Option<String>,
    ) -> Result<(Credential, String), IdentityStoreError> {
        let raw = portunus_auth::token::generate_token();
        let cred = Credential {
            id: CredentialId::new(),
            user_id: user_id.clone(),
            token_hash: hash_token(&raw),
            label,
            created_at: Utc::now(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };
        let uid_for_err = user_id.clone();
        let cred_clone = cred.clone();
        self.store
            .with_write_tx(|tx| {
                if !user_exists(tx, user_id)? {
                    return Err(StoreError::Conflict {
                        detail: "user_not_found".into(),
                    });
                }
                insert_credential(tx, &cred_clone)?;
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "user_not_found" => {
                    IdentityStoreError::UserNotFound(uid_for_err)
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })?;
        Ok((cred, raw))
    }

    pub fn add_grant(&self, grant: Grant) -> Result<(), IdentityStoreError> {
        if grant.listen_port_start == 0 || grant.listen_port_start > grant.listen_port_end {
            return Err(IdentityStoreError::InvalidPortRange {
                start: grant.listen_port_start,
                end: grant.listen_port_end,
            });
        }
        let uid_for_err = grant.user_id.clone();
        self.store
            .with_write_tx(|tx| {
                if !user_exists(tx, &grant.user_id)? {
                    return Err(StoreError::Conflict {
                        detail: "user_not_found".into(),
                    });
                }
                insert_grant(tx, &grant)?;
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "user_not_found" => {
                    IdentityStoreError::UserNotFound(uid_for_err)
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    pub fn revoke_grant(&self, grant_id: &GrantId) -> Result<Grant, IdentityStoreError> {
        let gid_for_err = *grant_id;
        self.store
            .with_write_tx(|tx| {
                let g: Option<Grant> = tx
                    .query_row(
                        "SELECT grant_id, user_id, client, listen_port_start, listen_port_end, \
                                protocols, note, created_at FROM grants WHERE grant_id = ?",
                        params![grant_id.to_string()],
                        row_to_grant,
                    )
                    .optional()
                    .map_err(map_rusqlite)?;
                let Some(grant) = g else {
                    return Err(StoreError::Conflict {
                        detail: "grant_not_found".into(),
                    });
                };
                tx.execute(
                    "DELETE FROM grants WHERE grant_id = ?",
                    params![grant_id.to_string()],
                )
                .map_err(map_rusqlite)?;
                Ok(grant)
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "grant_not_found" => {
                    IdentityStoreError::GrantNotFound(gid_for_err)
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }
}

// ---- shared SQL helpers (BEGIN IMMEDIATE tx) ----

fn user_exists(tx: &Connection, id: &UserId) -> Result<bool, StoreError> {
    let n: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM users WHERE user_id = ?",
            params![id.as_str()],
            |r| r.get(0),
        )
        .map_err(map_rusqlite)?;
    Ok(n > 0)
}

fn insert_user(tx: &Connection, user: &User) -> Result<(), StoreError> {
    let role = match user.role {
        OperatorRole::Superadmin => "superadmin",
        OperatorRole::User => "user",
    };
    tx.execute(
        "INSERT INTO users (user_id, role, display_name, disabled, created_at) \
         VALUES (?, ?, ?, ?, ?)",
        params![
            user.id.as_str(),
            role,
            user.display_name,
            i32::from(user.disabled),
            user.created_at.to_rfc3339(),
        ],
    )
    .map_err(map_rusqlite)?;
    Ok(())
}

fn insert_credential(tx: &Connection, cred: &Credential) -> Result<(), StoreError> {
    let (status, revoked_at) = match &cred.status {
        CredentialStatus::Active(_) => ("active", None),
        CredentialStatus::Revoked { revoked } => ("revoked", Some(revoked.revoked_at.to_rfc3339())),
    };
    tx.execute(
        "INSERT INTO credentials \
            (credential_id, user_id, hash, label, status, issued_at, revoked_at, last_used_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            cred.id.to_string(),
            cred.user_id.as_str(),
            fingerprint::hex(&cred.token_hash),
            cred.label,
            status,
            cred.created_at.to_rfc3339(),
            revoked_at,
            cred.last_used_at.map(|t| t.to_rfc3339()),
        ],
    )
    .map_err(map_rusqlite)?;
    Ok(())
}

fn insert_grant(tx: &Connection, g: &Grant) -> Result<(), StoreError> {
    let client = match &g.client {
        ClientScope::Any => "*".to_string(),
        ClientScope::Named(n) => n.as_str().to_string(),
    };
    tx.execute(
        "INSERT INTO grants \
            (grant_id, user_id, client, listen_port_start, listen_port_end, protocols, note, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            g.id.to_string(),
            g.user_id.as_str(),
            client,
            g.listen_port_start,
            g.listen_port_end,
            g.protocols.bits(),
            g.note,
            g.created_at.to_rfc3339(),
        ],
    )
    .map_err(map_rusqlite)?;
    Ok(())
}

fn collect_credential_ids(
    tx: &Connection,
    user_id: &UserId,
) -> Result<Vec<CredentialId>, StoreError> {
    let mut stmt = tx
        .prepare("SELECT credential_id FROM credentials WHERE user_id = ?")
        .map_err(map_rusqlite)?;
    let rows = stmt
        .query_map(params![user_id.as_str()], |r| r.get::<_, String>(0))
        .map_err(map_rusqlite)?;
    let mut out = Vec::new();
    for r in rows {
        let s = r.map_err(map_rusqlite)?;
        let ulid = ulid::Ulid::from_string(&s).map_err(|e| StoreError::Corruption {
            detail: format!("bad CredentialId {s}: {e}"),
        })?;
        out.push(CredentialId(ulid));
    }
    Ok(out)
}

fn collect_grant_ids(tx: &Connection, user_id: &UserId) -> Result<Vec<GrantId>, StoreError> {
    let mut stmt = tx
        .prepare("SELECT grant_id FROM grants WHERE user_id = ?")
        .map_err(map_rusqlite)?;
    let rows = stmt
        .query_map(params![user_id.as_str()], |r| r.get::<_, String>(0))
        .map_err(map_rusqlite)?;
    let mut out = Vec::new();
    for r in rows {
        let s = r.map_err(map_rusqlite)?;
        let ulid = ulid::Ulid::from_string(&s).map_err(|e| StoreError::Corruption {
            detail: format!("bad GrantId {s}: {e}"),
        })?;
        out.push(GrantId(ulid));
    }
    Ok(out)
}

// ---- row mappers ----

fn row_to_user(r: &Row<'_>) -> rusqlite::Result<User> {
    let user_id: String = r.get(0)?;
    let role: String = r.get(1)?;
    let display_name: String = r.get(2)?;
    let disabled: i32 = r.get(3)?;
    let created_at: String = r.get(4)?;

    let role = match role.as_str() {
        "superadmin" => OperatorRole::Superadmin,
        "user" => OperatorRole::User,
        other => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown role {other}"),
                )),
            ));
        }
    };
    let id = if user_id.starts_with('_') {
        UserId::reserved(user_id)
    } else {
        UserId::from_str(&user_id).map_err(|_e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid user_id: {user_id}"),
                )),
            )
        })?
    };
    Ok(User {
        id,
        display_name,
        role,
        created_at: parse_ts_rusqlite(&created_at, 4)?,
        disabled: disabled != 0,
    })
}

fn row_to_grant(r: &Row<'_>) -> rusqlite::Result<Grant> {
    let grant_id: String = r.get(0)?;
    let user_id: String = r.get(1)?;
    let client: String = r.get(2)?;
    let listen_port_start: u16 = r.get::<_, i64>(3)? as u16;
    let listen_port_end: u16 = r.get::<_, i64>(4)? as u16;
    let protocols_bits: u8 = r.get::<_, i64>(5)? as u8;
    let note: Option<String> = r.get(6)?;
    let created_at: String = r.get(7)?;

    let id = ulid::Ulid::from_string(&grant_id).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad ULID: {e}"),
            )),
        )
    })?;
    let uid = parse_user_id(&user_id, 1)?;
    let scope = if client == "*" {
        ClientScope::Any
    } else {
        ClientScope::Named(ClientName::new(client.clone()).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("bad client name {client}: {e}"),
                )),
            )
        })?)
    };
    let protocols = ProtocolSet::from_bits(protocols_bits).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad protocol bits {protocols_bits}"),
            )),
        )
    })?;
    Ok(Grant {
        id: GrantId(id),
        user_id: uid,
        client: scope,
        listen_port_start,
        listen_port_end,
        protocols,
        note,
        created_at: parse_ts_rusqlite(&created_at, 7)?,
    })
}

fn row_to_web_session_record(r: &Row<'_>) -> rusqlite::Result<(WebSession, Option<DateTime<Utc>>)> {
    let user_id: String = r.get(0)?;
    let created_at: String = r.get(1)?;
    let last_seen_at: String = r.get(2)?;
    let absolute_expires_at: String = r.get(3)?;
    let revoked_at: Option<String> = r.get(4)?;
    let remote_addr: Option<String> = r.get(5)?;
    let user_agent: Option<String> = r.get(6)?;

    Ok((
        WebSession {
            user_id: parse_user_id(&user_id, 0)?,
            created_at: parse_ts_rusqlite(&created_at, 1)?,
            last_seen_at: parse_ts_rusqlite(&last_seen_at, 2)?,
            absolute_expires_at: parse_ts_rusqlite(&absolute_expires_at, 3)?,
            remote_addr,
            user_agent,
        },
        revoked_at.map(|ts| parse_ts_rusqlite(&ts, 4)).transpose()?,
    ))
}

fn row_to_login_attempt(r: &Row<'_>) -> rusqlite::Result<ThrottleDecision> {
    let failures = r.get::<_, i64>(0)?;
    let first_failed_at: Option<String> = r.get(1)?;
    let last_failed_at: Option<String> = r.get(2)?;
    let locked_until: Option<String> = r.get(3)?;

    let failures = u32::try_from(failures).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad failures count: {e}"),
            )),
        )
    })?;

    Ok(ThrottleDecision {
        failures,
        first_failed_at: first_failed_at
            .map(|ts| parse_ts_rusqlite(&ts, 1))
            .transpose()?,
        last_failed_at: last_failed_at
            .map(|ts| parse_ts_rusqlite(&ts, 2))
            .transpose()?,
        locked_until: locked_until
            .map(|ts| parse_ts_rusqlite(&ts, 3))
            .transpose()?,
    })
}

#[allow(dead_code)]
fn row_to_setup_token_record(r: &Row<'_>) -> rusqlite::Result<SetupTokenRecord> {
    let hash_hex: String = r.get(0)?;
    let expires_at: String = r.get(1)?;
    Ok(SetupTokenRecord::from_stored(
        hash_hex,
        parse_ts_rusqlite(&expires_at, 1)?,
    ))
}

fn parse_user_id(s: &str, col: usize) -> rusqlite::Result<UserId> {
    if s.starts_with('_') {
        Ok(UserId::reserved(s))
    } else {
        UserId::from_str(s).map_err(|_e| {
            rusqlite::Error::FromSqlConversionFailure(
                col,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("invalid user_id: {s}"),
                )),
            )
        })
    }
}

fn parse_ts_rusqlite(s: &str, col: usize) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                col,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("bad RFC3339 ts: {e}"),
                )),
            )
        })
}

// ---- OperatorAuthenticator impl ----

impl OperatorAuthenticator for SqliteOperatorStore {
    fn verify(&self, token: &str) -> Result<OperatorIdentity, RbacError> {
        if token.is_empty() {
            return Err(RbacError::Unauthenticated);
        }
        if token.len() > 256 {
            return Err(RbacError::CredentialInvalid);
        }
        let presented_hex = fingerprint::hex(&hash_token(token));

        // Constitution V: full active-credential scan with constant-time
        // hex equality. ≤O(N) credentials in the store.
        let result = self
            .store
            .with_conn(|c| {
                let mut stmt = c
                    .prepare("SELECT credential_id, user_id, hash, status FROM credentials")
                    .map_err(map_rusqlite)?;
                let rows = stmt
                    .query_map([], |r| {
                        let id: String = r.get(0)?;
                        let uid: String = r.get(1)?;
                        let hash: String = r.get(2)?;
                        let status: String = r.get(3)?;
                        Ok((id, uid, hash, status))
                    })
                    .map_err(map_rusqlite)?;

                let needle = presented_hex.as_bytes();
                let mut matched_active: Option<(String, String)> = None;
                let mut matched_revoked = false;
                for r in rows {
                    let (id, uid, hash, status) = r.map_err(map_rusqlite)?;
                    if hash.len() == needle.len() && fingerprint::ct_eq(hash.as_bytes(), needle) {
                        match status.as_str() {
                            "active" => matched_active = Some((id, uid)),
                            _ => matched_revoked = true,
                        }
                    }
                }
                Ok((matched_active, matched_revoked))
            })
            .map_err(|_| RbacError::Unauthenticated)?;

        let (matched_active, matched_revoked) = result;
        let Some((cred_id, owner_uid)) = matched_active else {
            if matched_revoked {
                return Err(RbacError::CredentialInvalid);
            }
            return Err(RbacError::CredentialInvalid);
        };

        // Look up the user.
        let user = self
            .store
            .with_conn(|c| {
                let row = c
                    .query_row(
                        "SELECT user_id, role, display_name, disabled, created_at \
                         FROM users WHERE user_id = ?",
                        params![owner_uid],
                        row_to_user,
                    )
                    .optional()
                    .map_err(map_rusqlite)?;
                Ok(row)
            })
            .map_err(|_| RbacError::CredentialInvalid)?;
        let Some(user) = user else {
            return Err(RbacError::CredentialInvalid);
        };
        if user.disabled {
            return Err(RbacError::UserDisabled);
        }

        // Best-effort last_used_at update; failure here MUST NOT fail
        // the verify call.
        let now = Utc::now().to_rfc3339();
        let _ = self.store.with_write_tx(|tx| {
            tx.execute(
                "UPDATE credentials SET last_used_at = ? WHERE credential_id = ?",
                params![now, cred_id],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        });

        Ok(OperatorIdentity {
            user_id: user.id,
            role: user.role,
        })
    }

    fn grants_for(&self, user_id: &UserId) -> Vec<Grant> {
        // Sort by created_at to match FileOperatorStore ordering.
        let mut grants = self
            .store
            .with_conn(|c| {
                let mut stmt = c
                    .prepare(
                        "SELECT grant_id, user_id, client, listen_port_start, listen_port_end, \
                                protocols, note, created_at FROM grants WHERE user_id = ?",
                    )
                    .map_err(map_rusqlite)?;
                let rows = stmt
                    .query_map(params![user_id.as_str()], row_to_grant)
                    .map_err(map_rusqlite)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r.map_err(map_rusqlite)?);
                }
                Ok(out)
            })
            .unwrap_or_default();
        grants.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        grants
    }

    fn has_any_superadmin(&self) -> bool {
        self.count_superadmins() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn fixed_ts() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 7, 10, 0, 0).unwrap()
    }

    fn fresh() -> (tempfile::TempDir, SqliteOperatorStore) {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        (dir, SqliteOperatorStore::new(store))
    }

    fn alice() -> User {
        User {
            id: UserId::from_str("alice").unwrap(),
            display_name: "Alice".into(),
            role: OperatorRole::User,
            created_at: fixed_ts(),
            disabled: false,
        }
    }

    fn bob() -> User {
        User {
            id: UserId::from_str("bob").unwrap(),
            display_name: "Bob".into(),
            role: OperatorRole::User,
            created_at: fixed_ts(),
            disabled: false,
        }
    }

    fn superadmin() -> User {
        User {
            id: UserId::superadmin(),
            display_name: "Built-in".into(),
            role: OperatorRole::Superadmin,
            created_at: fixed_ts(),
            disabled: false,
        }
    }

    #[test]
    fn add_user_round_trip() {
        let (_d, s) = fresh();
        s.add_user(alice()).unwrap();
        let got = s.get_user(&UserId::from_str("alice").unwrap()).unwrap();
        assert_eq!(got.display_name, "Alice");
    }

    #[test]
    fn add_user_rejects_duplicate() {
        let (_d, s) = fresh();
        s.add_user(alice()).unwrap();
        let err = s.add_user(alice()).unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserAlreadyExists(_)));
    }

    #[test]
    fn seed_credential_then_verify_round_trip() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        // Seed a User-role credential via the shared low-level helper (the
        // public issue_credential was removed); verify it authenticates.
        let raw = "alice-laptop-tok";
        let cred = Credential {
            id: CredentialId::new(),
            user_id: alice_id.clone(),
            token_hash: hash_token(raw),
            label: Some("laptop".into()),
            created_at: fixed_ts(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };
        s.store
            .with_write_tx(|tx| insert_credential(tx, &cred))
            .unwrap();
        let id = s.verify(raw).unwrap();
        assert_eq!(id.user_id.as_str(), "alice");
        assert_eq!(id.role, OperatorRole::User);
    }

    #[test]
    fn verify_rejects_revoked_credential() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        // Seed a revoked credential row directly (insert_credential
        // serializes the revoked case); verify() must reject it.
        let raw = "alice-revoked-tok";
        let cred = Credential {
            id: CredentialId::new(),
            user_id: alice_id.clone(),
            token_hash: hash_token(raw),
            label: None,
            created_at: fixed_ts(),
            last_used_at: None,
            status: CredentialStatus::revoked(fixed_ts()),
        };
        s.store
            .with_write_tx(|tx| insert_credential(tx, &cred))
            .unwrap();
        let err = s.verify(raw).unwrap_err();
        assert_eq!(err, RbacError::CredentialInvalid);
    }

    #[test]
    fn remove_user_cascades_credentials_and_grants() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        // Seed a credential via the shared low-level helper so the cascade
        // has a row to remove.
        let cred = Credential {
            id: CredentialId::new(),
            user_id: alice_id.clone(),
            token_hash: hash_token("alice-tok"),
            label: None,
            created_at: fixed_ts(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };
        s.store
            .with_write_tx(|tx| insert_credential(tx, &cred))
            .unwrap();
        let g = Grant {
            id: GrantId::new(),
            user_id: alice_id.clone(),
            client: ClientScope::Any,
            listen_port_start: 30000,
            listen_port_end: 30010,
            protocols: ProtocolSet::TCP,
            note: None,
            created_at: fixed_ts(),
        };
        s.add_grant(g).unwrap();

        let summary = s.remove_user(&alice_id).unwrap();
        assert_eq!(summary.removed_credential_ids.len(), 1);
        assert_eq!(summary.revoked_grant_ids.len(), 1);
        assert!(s.list_users().is_empty());
        // The credential rows are gone (FK CASCADE on user delete).
        let remaining_creds: i64 = s
            .store
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM credentials WHERE user_id = ?",
                    params![alice_id.as_str()],
                    |r| r.get(0),
                )
                .map_err(map_rusqlite)
            })
            .unwrap();
        assert_eq!(remaining_creds, 0);
        assert!(s.list_grants(None).is_empty());
    }

    #[test]
    fn bootstrap_legacy_then_blocks_second_bootstrap() {
        let (_d, s) = fresh();
        s.bootstrap_legacy_superadmin("super-secret-token").unwrap();
        let err = s.bootstrap_legacy_superadmin("another-token").unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserAlreadyExists(_)));
        let id = s.verify("super-secret-token").unwrap();
        assert_eq!(id.role, OperatorRole::Superadmin);
    }

    #[test]
    fn count_superadmins_works() {
        let (_d, s) = fresh();
        assert_eq!(s.count_superadmins(), 0);
        s.add_user(superadmin()).unwrap();
        assert_eq!(s.count_superadmins(), 1);
    }

    #[test]
    fn password_hash_round_trips_without_exposing_in_user_view() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();

        s.set_password_hash(&alice_id, "$argon2id$fake", true)
            .unwrap();
        let state = s.password_state(&alice_id).unwrap().unwrap();
        assert_eq!(state.hash, "$argon2id$fake");
        assert!(state.password_change_required);

        let public_user = s.get_user(&alice_id).unwrap();
        assert_eq!(public_user.id, alice_id);
        assert_eq!(s.list_users(), vec![public_user]);
    }

    #[test]
    fn password_state_distinguishes_unset_from_missing_user() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();

        assert_eq!(s.password_state(&alice_id).unwrap(), None);

        let missing = UserId::from_str("missing").unwrap();
        let err = s.password_state(&missing).unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserNotFound(id) if id == missing));

        let err = s
            .set_password_hash(&missing, "$argon2id$fake", false)
            .unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserNotFound(id) if id == missing));
    }

    #[test]
    fn web_session_create_verify_revoke_round_trip() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let raw = crate::operator::sessions::generate_session_secret();
        let hash = crate::operator::sessions::hash_session_secret(&raw);
        let created_at = fixed_ts();
        let absolute_expires_at = created_at + chrono::Duration::days(7);

        s.create_web_session(
            &hash,
            &alice_id,
            created_at,
            absolute_expires_at,
            Some("127.0.0.1".into()),
            Some("test-agent".into()),
        )
        .unwrap();

        let session = s.verify_web_session(&hash, created_at).unwrap().unwrap();
        assert_eq!(session.user_id, alice_id);
        assert_eq!(session.created_at, created_at);
        assert_eq!(session.absolute_expires_at, absolute_expires_at);
        assert_eq!(session.remote_addr.as_deref(), Some("127.0.0.1"));
        assert_eq!(session.user_agent.as_deref(), Some("test-agent"));

        s.revoke_web_session(&hash).unwrap();
        assert!(s.verify_web_session(&hash, created_at).unwrap().is_none());
    }

    #[test]
    fn web_session_idle_expired_returns_none() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let hash = crate::operator::sessions::hash_session_secret("idle-secret");
        let created_at = fixed_ts();

        s.create_web_session(
            &hash,
            &alice_id,
            created_at,
            created_at + chrono::Duration::days(7),
            None,
            None,
        )
        .unwrap();

        let now =
            created_at + crate::operator::sessions::IDLE_TIMEOUT + chrono::Duration::seconds(1);
        assert!(s.verify_web_session(&hash, now).unwrap().is_none());
    }

    #[test]
    fn web_session_verify_persists_last_seen_refresh() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let hash = crate::operator::sessions::hash_session_secret("refresh-secret");
        let created_at = fixed_ts();

        s.create_web_session(
            &hash,
            &alice_id,
            created_at,
            created_at + chrono::Duration::days(7),
            None,
            None,
        )
        .unwrap();

        let refreshed_at = created_at + chrono::Duration::hours(7);
        let refreshed = s.verify_web_session(&hash, refreshed_at).unwrap().unwrap();
        assert_eq!(refreshed.last_seen_at, refreshed_at);

        let still_valid_after_original_idle_deadline = created_at + chrono::Duration::hours(14);
        assert!(
            s.verify_web_session(&hash, still_valid_after_original_idle_deadline)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn web_session_absolute_expired_returns_none() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let hash = crate::operator::sessions::hash_session_secret("absolute-secret");
        let created_at = fixed_ts();
        let absolute_expires_at = created_at + chrono::Duration::hours(1);

        s.create_web_session(
            &hash,
            &alice_id,
            created_at,
            absolute_expires_at,
            None,
            None,
        )
        .unwrap();

        let now = absolute_expires_at + chrono::Duration::seconds(1);
        assert!(s.verify_web_session(&hash, now).unwrap().is_none());
    }

    #[test]
    fn web_session_missing_after_delete_returns_none() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let hash = crate::operator::sessions::hash_session_secret("delete-secret");
        let created_at = fixed_ts();

        s.create_web_session(
            &hash,
            &alice_id,
            created_at,
            created_at + chrono::Duration::days(7),
            None,
            None,
        )
        .unwrap();

        assert_eq!(s.delete_web_session_for_test(&hash).unwrap(), 1);
        assert!(s.verify_web_session(&hash, created_at).unwrap().is_none());
    }

    #[test]
    fn onboarding_setup_token_rotation_rejects_old_token() {
        let (_d, s) = fresh();
        let now = fixed_ts();

        let first = s.rotate_onboarding_setup_token(now).unwrap();
        let second = s
            .rotate_onboarding_setup_token(now + chrono::Duration::minutes(1))
            .unwrap();

        assert!(
            s.verify_onboarding_setup_token(&second, now + chrono::Duration::minutes(1))
                .unwrap()
        );
        assert!(!s.verify_onboarding_setup_token(&first, now).unwrap());
    }

    #[test]
    fn onboarding_setup_token_stores_hash_and_expires() {
        let (_d, s) = fresh();
        let now = fixed_ts();

        let raw = s.rotate_onboarding_setup_token(now).unwrap();

        let stored_hash: String = s
            .store
            .with_conn(|c| {
                let hash = c
                    .query_row(
                        "SELECT token_hash FROM onboarding_setup WHERE id = 1",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(map_rusqlite)?;
                Ok(hash)
            })
            .unwrap();
        assert_ne!(stored_hash, raw);
        assert!(
            s.verify_onboarding_setup_token(&raw, now + chrono::Duration::minutes(29))
                .unwrap()
        );
        assert!(
            !s.verify_onboarding_setup_token(&raw, now + chrono::Duration::minutes(30))
                .unwrap()
        );
    }

    #[test]
    fn onboarding_setup_token_clear_and_missing_row_return_false() {
        let (_d, s) = fresh();
        let now = fixed_ts();

        assert!(!s.verify_onboarding_setup_token("missing", now).unwrap());

        let raw = s.rotate_onboarding_setup_token(now).unwrap();
        assert!(s.verify_onboarding_setup_token(&raw, now).unwrap());

        s.clear_onboarding_setup_token().unwrap();
        assert!(!s.verify_onboarding_setup_token(&raw, now).unwrap());
    }

    #[test]
    fn revoke_web_sessions_for_user_revokes_only_target_user() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        let bob_id = UserId::from_str("bob").unwrap();
        s.add_user(alice()).unwrap();
        s.add_user(bob()).unwrap();
        let created_at = fixed_ts();
        let absolute_expires_at = created_at + chrono::Duration::days(7);

        let alice_hash_a = crate::operator::sessions::hash_session_secret("alice-secret-a");
        let alice_hash_b = crate::operator::sessions::hash_session_secret("alice-secret-b");
        let bob_hash = crate::operator::sessions::hash_session_secret("bob-secret");

        s.create_web_session(
            &alice_hash_a,
            &alice_id,
            created_at,
            absolute_expires_at,
            None,
            None,
        )
        .unwrap();
        s.create_web_session(
            &alice_hash_b,
            &alice_id,
            created_at,
            absolute_expires_at,
            None,
            None,
        )
        .unwrap();
        s.create_web_session(
            &bob_hash,
            &bob_id,
            created_at,
            absolute_expires_at,
            None,
            None,
        )
        .unwrap();

        s.revoke_web_sessions_for_user(&alice_id).unwrap();

        assert!(
            s.verify_web_session(&alice_hash_a, created_at)
                .unwrap()
                .is_none()
        );
        assert!(
            s.verify_web_session(&alice_hash_b, created_at)
                .unwrap()
                .is_none()
        );
        assert!(
            s.verify_web_session(&bob_hash, created_at)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn web_session_prune_removes_revoked_and_expired_sessions() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let created_at = fixed_ts();
        let now = created_at + chrono::Duration::days(8);
        let revoked_hash = crate::operator::sessions::hash_session_secret("revoked-secret");
        let expired_hash = crate::operator::sessions::hash_session_secret("expired-secret");
        let live_hash = crate::operator::sessions::hash_session_secret("live-secret");

        s.create_web_session(
            &revoked_hash,
            &alice_id,
            created_at,
            created_at + chrono::Duration::days(30),
            None,
            None,
        )
        .unwrap();
        s.create_web_session(
            &expired_hash,
            &alice_id,
            created_at,
            created_at + chrono::Duration::days(1),
            None,
            None,
        )
        .unwrap();
        s.create_web_session(
            &live_hash,
            &alice_id,
            now,
            now + chrono::Duration::days(7),
            None,
            None,
        )
        .unwrap();
        s.revoke_web_session(&revoked_hash).unwrap();

        assert_eq!(s.prune_expired_web_sessions(now).unwrap(), 2);
        assert!(s.verify_web_session(&revoked_hash, now).unwrap().is_none());
        assert!(s.verify_web_session(&expired_hash, now).unwrap().is_none());
        assert!(s.verify_web_session(&live_hash, now).unwrap().is_some());
    }

    #[test]
    fn web_session_create_and_bulk_revoke_reject_missing_user() {
        let (_d, s) = fresh();
        let missing = UserId::from_str("missing").unwrap();
        let hash = crate::operator::sessions::hash_session_secret("missing-secret");
        let created_at = fixed_ts();

        let err = s
            .create_web_session(
                &hash,
                &missing,
                created_at,
                created_at + chrono::Duration::days(7),
                None,
                None,
            )
            .unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserNotFound(id) if id == missing));

        let err = s.revoke_web_sessions_for_user(&missing).unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserNotFound(id) if id == missing));
    }

    #[test]
    fn login_attempt_failures_persist_lockout() {
        let (_d, s) = fresh();
        let subject = "alice";
        let remote_addr = "127.0.0.1";
        let now = fixed_ts();

        for offset in 0..crate::operator::throttle::LOCK_AFTER_FAILURES {
            s.record_login_attempt_failure(
                subject,
                remote_addr,
                crate::operator::throttle::AuthThrottleAction::Login,
                now + chrono::Duration::seconds(i64::from(offset)),
            )
            .unwrap();
        }

        let state = s
            .login_attempt_state(
                subject,
                remote_addr,
                crate::operator::throttle::AuthThrottleAction::Login,
                now,
            )
            .unwrap();
        assert_eq!(
            state.failures,
            crate::operator::throttle::LOCK_AFTER_FAILURES
        );
        assert!(state.locked_until.is_some());
    }

    #[test]
    fn login_attempt_success_clear_removes_lockout() {
        let (_d, s) = fresh();
        let now = fixed_ts();

        s.record_login_attempt_failure(
            "alice",
            "127.0.0.1",
            crate::operator::throttle::AuthThrottleAction::Login,
            now,
        )
        .unwrap();
        s.clear_login_attempts(
            "alice",
            "127.0.0.1",
            crate::operator::throttle::AuthThrottleAction::Login,
        )
        .unwrap();

        let state = s
            .login_attempt_state(
                "alice",
                "127.0.0.1",
                crate::operator::throttle::AuthThrottleAction::Login,
                now,
            )
            .unwrap();
        assert_eq!(
            state,
            crate::operator::throttle::ThrottleDecision::default()
        );
    }

    #[test]
    fn login_attempt_expired_lockout_starts_new_burst() {
        let (_d, s) = fresh();
        let subject = "alice";
        let remote_addr = "127.0.0.1";
        let now = fixed_ts();

        let mut state = crate::operator::throttle::ThrottleDecision::default();
        for offset in 0..crate::operator::throttle::LOCK_AFTER_FAILURES {
            state = s
                .record_login_attempt_failure(
                    subject,
                    remote_addr,
                    crate::operator::throttle::AuthThrottleAction::Login,
                    now + chrono::Duration::seconds(i64::from(offset)),
                )
                .unwrap();
        }

        let after_lockout = state.locked_until.unwrap() + chrono::Duration::seconds(1);
        let state = s
            .record_login_attempt_failure(
                subject,
                remote_addr,
                crate::operator::throttle::AuthThrottleAction::Login,
                after_lockout,
            )
            .unwrap();

        assert_eq!(state.failures, 1);
        assert_eq!(state.first_failed_at, Some(after_lockout));
        assert_eq!(state.last_failed_at, Some(after_lockout));
        assert_eq!(state.locked_until, None);

        let persisted = s
            .login_attempt_state(
                subject,
                remote_addr,
                crate::operator::throttle::AuthThrottleAction::Login,
                after_lockout,
            )
            .unwrap();
        assert_eq!(persisted, state);
    }

    #[test]
    fn login_attempt_failures_expire_after_window() {
        let (_d, s) = fresh();
        let subject = "alice";
        let remote_addr = "127.0.0.1";
        let now = fixed_ts();
        let after_window =
            now + chrono::Duration::seconds(crate::operator::throttle::FAILURE_WINDOW_SECONDS + 1);

        s.record_login_attempt_failure(
            subject,
            remote_addr,
            crate::operator::throttle::AuthThrottleAction::Login,
            now,
        )
        .unwrap();
        let state = s
            .record_login_attempt_failure(
                subject,
                remote_addr,
                crate::operator::throttle::AuthThrottleAction::Login,
                after_window,
            )
            .unwrap();

        assert_eq!(state.failures, 1);
        assert_eq!(state.first_failed_at, Some(after_window));
        assert_eq!(state.last_failed_at, Some(after_window));
        assert_eq!(state.locked_until, None);

        let persisted = s
            .login_attempt_state(
                subject,
                remote_addr,
                crate::operator::throttle::AuthThrottleAction::Login,
                after_window,
            )
            .unwrap();
        assert_eq!(persisted, state);
    }

    #[test]
    fn login_attempt_unknown_subject_round_trip() {
        let (_d, s) = fresh();
        let now = fixed_ts();

        let state = s
            .record_login_attempt_failure(
                crate::operator::throttle::UNKNOWN_AUTH_SUBJECT,
                "127.0.0.1",
                crate::operator::throttle::AuthThrottleAction::Login,
                now,
            )
            .unwrap();

        assert_eq!(state.failures, 1);
        let persisted = s
            .login_attempt_state(
                crate::operator::throttle::UNKNOWN_AUTH_SUBJECT,
                "127.0.0.1",
                crate::operator::throttle::AuthThrottleAction::Login,
                now,
            )
            .unwrap();
        assert_eq!(persisted.failures, 1);
    }

    #[test]
    fn login_attempt_buckets_are_independent_by_remote_and_action() {
        let (_d, s) = fresh();
        let now = fixed_ts();

        for offset in 0..crate::operator::throttle::LOCK_AFTER_FAILURES {
            s.record_login_attempt_failure(
                "alice",
                "127.0.0.1",
                crate::operator::throttle::AuthThrottleAction::Login,
                now + chrono::Duration::seconds(i64::from(offset)),
            )
            .unwrap();
        }
        s.record_login_attempt_failure(
            "alice",
            "10.0.0.5",
            crate::operator::throttle::AuthThrottleAction::Login,
            now,
        )
        .unwrap();
        s.record_login_attempt_failure(
            "alice",
            "127.0.0.1",
            crate::operator::throttle::AuthThrottleAction::PasswordReset,
            now,
        )
        .unwrap();

        let login_local = s
            .login_attempt_state(
                "alice",
                "127.0.0.1",
                crate::operator::throttle::AuthThrottleAction::Login,
                now,
            )
            .unwrap();
        let login_remote = s
            .login_attempt_state(
                "alice",
                "10.0.0.5",
                crate::operator::throttle::AuthThrottleAction::Login,
                now,
            )
            .unwrap();
        let reset_local = s
            .login_attempt_state(
                "alice",
                "127.0.0.1",
                crate::operator::throttle::AuthThrottleAction::PasswordReset,
                now,
            )
            .unwrap();

        assert_eq!(
            login_local.failures,
            crate::operator::throttle::LOCK_AFTER_FAILURES
        );
        assert!(login_local.locked_until.is_some());
        assert_eq!(login_remote.failures, 1);
        assert!(login_remote.locked_until.is_none());
        assert_eq!(reset_local.failures, 1);
        assert!(reset_local.locked_until.is_none());
    }

    #[test]
    fn v013_predicate_purges_user_creds_keeps_reserved() {
        let (_d, s) = fresh();

        // Seed the reserved bootstrap credential via the dedicated path.
        s.bootstrap_legacy_superadmin("legacy-tok").unwrap();

        // Seed a regular user credential directly via the shared helper so
        // this test is not coupled to issue_credential (removed in a later
        // task).
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let alice_cred = Credential {
            id: CredentialId::new(),
            user_id: alice_id.clone(),
            token_hash: hash_token("alice-tok"),
            label: None,
            created_at: fixed_ts(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };
        s.store
            .with_write_tx(|tx| insert_credential(tx, &alice_cred))
            .unwrap();

        // Run the V013 predicate.
        s.store
            .with_write_tx(|tx| {
                tx.execute(
                    r"DELETE FROM credentials WHERE user_id NOT LIKE '\_%' ESCAPE '\'",
                    [],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .unwrap();

        // Reserved credential survives and still authenticates as superadmin.
        let id = s.verify("legacy-tok").unwrap();
        assert_eq!(id.role, OperatorRole::Superadmin);

        // Regular user credential is gone.
        let err = s.verify("alice-tok").unwrap_err();
        assert_eq!(err, RbacError::CredentialInvalid);
    }

    #[test]
    fn grants_for_filters_and_orders() {
        let (_d, s) = fresh();
        s.add_user(alice()).unwrap();
        let g1 = Grant {
            id: GrantId::new(),
            user_id: UserId::from_str("alice").unwrap(),
            client: ClientScope::Any,
            listen_port_start: 30000,
            listen_port_end: 30000,
            protocols: ProtocolSet::TCP,
            note: None,
            created_at: fixed_ts(),
        };
        let g2 = Grant {
            id: GrantId::new(),
            user_id: UserId::from_str("alice").unwrap(),
            client: ClientScope::Named(ClientName::new("client-a").unwrap()),
            listen_port_start: 31000,
            listen_port_end: 31010,
            protocols: ProtocolSet::TCP | ProtocolSet::UDP,
            note: Some("multi".into()),
            created_at: fixed_ts() + chrono::Duration::seconds(1),
        };
        s.add_grant(g1.clone()).unwrap();
        s.add_grant(g2.clone()).unwrap();
        let got = s.grants_for(&UserId::from_str("alice").unwrap());
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, g1.id); // earliest created_at first
    }

    fn sample_grant(user: &UserId, start: u16, end: u16) -> Grant {
        Grant {
            id: GrantId::new(),
            user_id: user.clone(),
            client: ClientScope::Any,
            listen_port_start: start,
            listen_port_end: end,
            protocols: ProtocolSet::TCP,
            note: None,
            created_at: fixed_ts(),
        }
    }

    #[test]
    fn list_grants_filters_by_user_and_lists_all() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        let bob_id = UserId::from_str("bob").unwrap();
        s.add_user(alice()).unwrap();
        s.add_user(bob()).unwrap();
        let alice_grant = sample_grant(&alice_id, 30000, 30000);
        let bob_grant = sample_grant(&bob_id, 31000, 31000);
        s.add_grant(alice_grant.clone()).unwrap();
        s.add_grant(bob_grant.clone()).unwrap();

        // Filtered listing returns only the target user's grants.
        let filtered = s.list_grants(Some(&alice_id));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, alice_grant.id);

        // Unfiltered listing returns every grant.
        assert_eq!(s.list_grants(None).len(), 2);
    }

    #[test]
    fn get_grant_returns_some_and_none() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let grant = sample_grant(&alice_id, 30000, 30010);
        s.add_grant(grant.clone()).unwrap();

        let got = s.get_grant(&grant.id).unwrap();
        assert_eq!(got.id, grant.id);
        assert_eq!(got.listen_port_end, 30010);

        assert!(s.get_grant(&GrantId::new()).is_none());
    }

    #[test]
    fn add_grant_rejects_invalid_port_range() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();

        // start == 0 is rejected before any DB access.
        let err = s.add_grant(sample_grant(&alice_id, 0, 10)).unwrap_err();
        assert!(matches!(
            err,
            IdentityStoreError::InvalidPortRange { start: 0, end: 10 }
        ));

        // start > end is rejected.
        let err = s.add_grant(sample_grant(&alice_id, 50, 40)).unwrap_err();
        assert!(matches!(
            err,
            IdentityStoreError::InvalidPortRange { start: 50, end: 40 }
        ));
    }

    #[test]
    fn add_grant_rejects_missing_user() {
        let (_d, s) = fresh();
        let missing = UserId::from_str("missing").unwrap();
        let err = s
            .add_grant(sample_grant(&missing, 30000, 30001))
            .unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserNotFound(id) if id == missing));
    }

    #[test]
    fn revoke_grant_returns_grant_and_then_not_found() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let grant = sample_grant(&alice_id, 30000, 30000);
        s.add_grant(grant.clone()).unwrap();

        let revoked = s.revoke_grant(&grant.id).unwrap();
        assert_eq!(revoked.id, grant.id);
        assert!(s.get_grant(&grant.id).is_none());

        // A second revoke of the same (now absent) grant reports not found.
        let err = s.revoke_grant(&grant.id).unwrap_err();
        assert!(matches!(err, IdentityStoreError::GrantNotFound(id) if id == grant.id));
    }

    #[test]
    fn has_active_superadmin_reflects_store_state() {
        let (_d, s) = fresh();
        assert!(!s.has_active_superadmin().unwrap());
        s.add_user(superadmin()).unwrap();
        assert!(s.has_active_superadmin().unwrap());
    }

    #[test]
    fn reset_password_state_updates_hash_and_revokes_sessions() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let created_at = fixed_ts();
        let hash = crate::operator::sessions::hash_session_secret("reset-secret");
        s.create_web_session(
            &hash,
            &alice_id,
            created_at,
            created_at + chrono::Duration::days(7),
            None,
            None,
        )
        .unwrap();

        let summary = s
            .reset_password_state(&alice_id, "$argon2id$reset", false, true)
            .unwrap();
        assert_eq!(summary.sessions_revoked, 1);
        assert!(s.verify_web_session(&hash, created_at).unwrap().is_none());

        let state = s.password_state(&alice_id).unwrap().unwrap();
        assert_eq!(state.hash, "$argon2id$reset");
        assert!(!state.password_change_required);
    }

    #[test]
    fn reset_password_state_without_session_revoke_keeps_sessions() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let created_at = fixed_ts();
        let hash = crate::operator::sessions::hash_session_secret("keep-secret");
        s.create_web_session(
            &hash,
            &alice_id,
            created_at,
            created_at + chrono::Duration::days(7),
            None,
            None,
        )
        .unwrap();

        let summary = s
            .reset_password_state(&alice_id, "$argon2id$keep", true, false)
            .unwrap();
        assert_eq!(summary.sessions_revoked, 0);
        assert!(s.verify_web_session(&hash, created_at).unwrap().is_some());
    }

    #[test]
    fn reset_password_state_rejects_missing_user() {
        let (_d, s) = fresh();
        let missing = UserId::from_str("missing").unwrap();
        let err = s
            .reset_password_state(&missing, "$argon2id$x", false, true)
            .unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserNotFound(id) if id == missing));
    }

    #[test]
    fn add_user_with_password_persists_hash_and_rejects_duplicate() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user_with_password(alice(), Some("$argon2id$awp"), true)
            .unwrap();
        let state = s.password_state(&alice_id).unwrap().unwrap();
        assert_eq!(state.hash, "$argon2id$awp");
        assert!(state.password_change_required);

        let err = s.add_user_with_password(alice(), None, false).unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserAlreadyExists(_)));
    }

    #[test]
    fn add_user_with_password_none_leaves_hash_unset() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user_with_password(alice(), None, false).unwrap();
        // No password row was written, so password_state is None (not an error).
        assert_eq!(s.password_state(&alice_id).unwrap(), None);
    }

    #[test]
    fn seed_credential_for_test_authenticates_and_rejects_missing_user() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();

        let (cred, raw) = s
            .seed_credential_for_test(&alice_id, Some("seeded".into()))
            .unwrap();
        assert_eq!(cred.label.as_deref(), Some("seeded"));
        let id = s.verify(&raw).unwrap();
        assert_eq!(id.user_id, alice_id);

        let missing = UserId::from_str("missing").unwrap();
        let err = s.seed_credential_for_test(&missing, None).unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserNotFound(id) if id == missing));
    }

    #[test]
    fn bootstrap_pair_inserts_user_and_credential() {
        let (_d, s) = fresh();
        let admin = superadmin();
        let raw = "bootstrap-pair-tok";
        let cred = Credential {
            id: CredentialId::new(),
            user_id: admin.id.clone(),
            token_hash: hash_token(raw),
            label: Some("pair".into()),
            created_at: fixed_ts(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };
        s.bootstrap_pair(admin.clone(), cred).unwrap();

        let id = s.verify(raw).unwrap();
        assert_eq!(id.role, OperatorRole::Superadmin);

        // A second pair for an existing user id is rejected.
        let dup_cred = Credential {
            id: CredentialId::new(),
            user_id: admin.id.clone(),
            token_hash: hash_token("other"),
            label: None,
            created_at: fixed_ts(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };
        let err = s.bootstrap_pair(admin, dup_cred).unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserAlreadyExists(_)));
    }

    #[test]
    fn bootstrap_pair_rejects_mismatched_user_id() {
        let (_d, s) = fresh();
        let cred = Credential {
            id: CredentialId::new(),
            user_id: UserId::from_str("bob").unwrap(),
            token_hash: hash_token("x"),
            label: None,
            created_at: fixed_ts(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };
        let err = s.bootstrap_pair(alice(), cred).unwrap_err();
        assert!(matches!(err, IdentityStoreError::WriteFailed(_)));
    }

    #[test]
    fn bootstrap_legacy_rejects_invalid_token_length() {
        let (_d, s) = fresh();
        let err = s.bootstrap_legacy_superadmin("").unwrap_err();
        assert!(matches!(err, IdentityStoreError::WriteFailed(_)));

        let too_long = "x".repeat(257);
        let err = s.bootstrap_legacy_superadmin(&too_long).unwrap_err();
        assert!(matches!(err, IdentityStoreError::WriteFailed(_)));
    }

    #[test]
    fn onboard_first_superadmin_happy_path() {
        let (_d, s) = fresh();
        let now = fixed_ts();
        let setup_token = s.rotate_onboarding_setup_token(now).unwrap();

        s.onboard_first_superadmin(superadmin(), "$argon2id$onboard", &setup_token, now)
            .unwrap();

        // The onboarded superadmin exists and the setup token was consumed.
        assert!(s.has_active_superadmin().unwrap());
        assert!(!s.verify_onboarding_setup_token(&setup_token, now).unwrap());
        let state = s.password_state(&UserId::superadmin()).unwrap().unwrap();
        assert_eq!(state.hash, "$argon2id$onboard");
        assert!(!state.password_change_required);
    }

    #[test]
    fn onboard_first_superadmin_rejects_non_superadmin_role() {
        let (_d, s) = fresh();
        let now = fixed_ts();
        let err = s
            .onboard_first_superadmin(alice(), "$argon2id$x", "tok", now)
            .unwrap_err();
        assert!(matches!(err, OnboardingError::Store(_)));
    }

    #[test]
    fn onboard_first_superadmin_rejects_when_already_bootstrapped() {
        let (_d, s) = fresh();
        let now = fixed_ts();
        let setup_token = s.rotate_onboarding_setup_token(now).unwrap();
        s.add_user(superadmin()).unwrap();

        let err = s
            .onboard_first_superadmin(superadmin(), "$argon2id$x", &setup_token, now)
            .unwrap_err();
        assert_eq!(err, OnboardingError::AlreadyBootstrapped);
    }

    #[test]
    fn onboard_first_superadmin_rejects_invalid_setup_token() {
        let (_d, s) = fresh();
        let now = fixed_ts();
        // No setup token row at all -> verification fails.
        let err = s
            .onboard_first_superadmin(superadmin(), "$argon2id$x", "wrong", now)
            .unwrap_err();
        assert_eq!(err, OnboardingError::InvalidSetupToken);

        // A rotated token that does not match the candidate also fails.
        s.rotate_onboarding_setup_token(now).unwrap();
        let err = s
            .onboard_first_superadmin(superadmin(), "$argon2id$x", "still-wrong", now)
            .unwrap_err();
        assert_eq!(err, OnboardingError::InvalidSetupToken);
    }

    #[test]
    fn insert_audit_entry_merges_details_and_persists() {
        let (_d, s) = fresh();
        let entry = AuditEntry {
            timestamp: fixed_ts(),
            actor: "alice".into(),
            role: Some(OperatorRole::Superadmin),
            method: "POST".into(),
            path: "/v1/users".into(),
            outcome: crate::operator::audit::AuditOutcome::Allow,
            reason: None,
            action: Some("user.create".into()),
            resource_kind: Some("user".into()),
            resource_value: Some("alice".into()),
            details: Some(serde_json::json!({ "extra_key": "extra_val" })),
        };
        s.insert_audit_entry(&entry).unwrap();

        let (action, details): (String, String) = s
            .store
            .with_conn(|c| {
                c.query_row(
                    "SELECT action, details_json FROM audit WHERE user_id = 'alice'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(map_rusqlite)
            })
            .unwrap();
        assert_eq!(action, "user.create");
        let parsed: serde_json::Value = serde_json::from_str(&details).unwrap();
        assert_eq!(parsed["extra_key"], "extra_val");
        assert!(parsed.get("role").is_some());
    }

    #[test]
    fn insert_audit_entry_empty_actor_stores_null_and_derives_action() {
        let (_d, s) = fresh();
        let entry = AuditEntry {
            timestamp: fixed_ts(),
            actor: String::new(),
            role: None,
            method: "GET".into(),
            path: "/v1/health".into(),
            outcome: crate::operator::audit::AuditOutcome::Deny,
            reason: Some("unauthenticated".into()),
            action: None,
            resource_kind: None,
            resource_value: None,
            details: None,
        };
        s.insert_audit_entry(&entry).unwrap();

        let (user_id, action): (Option<String>, String) = s
            .store
            .with_conn(|c| {
                c.query_row("SELECT user_id, action FROM audit LIMIT 1", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .map_err(map_rusqlite)
            })
            .unwrap();
        assert_eq!(user_id, None); // empty actor maps to NULL
        assert_eq!(action, "GET /v1/health"); // derived from method + path
    }

    #[test]
    fn verify_rejects_empty_and_oversize_tokens() {
        let (_d, s) = fresh();
        assert_eq!(s.verify("").unwrap_err(), RbacError::Unauthenticated);
        let too_long = "x".repeat(257);
        assert_eq!(
            s.verify(&too_long).unwrap_err(),
            RbacError::CredentialInvalid
        );
    }

    #[test]
    fn verify_unknown_token_is_credential_invalid() {
        let (_d, s) = fresh();
        s.add_user(alice()).unwrap();
        assert_eq!(
            s.verify("never-issued").unwrap_err(),
            RbacError::CredentialInvalid
        );
    }

    #[test]
    fn verify_rejects_disabled_user() {
        let (_d, s) = fresh();
        let disabled_user = User {
            id: UserId::from_str("alice").unwrap(),
            display_name: "Alice".into(),
            role: OperatorRole::User,
            created_at: fixed_ts(),
            disabled: true,
        };
        // Insert the disabled user directly (add_user accepts disabled flag).
        s.store
            .with_write_tx(|tx| insert_user(tx, &disabled_user))
            .unwrap();
        let raw = "disabled-tok";
        let cred = Credential {
            id: CredentialId::new(),
            user_id: disabled_user.id.clone(),
            token_hash: hash_token(raw),
            label: None,
            created_at: fixed_ts(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };
        s.store
            .with_write_tx(|tx| insert_credential(tx, &cred))
            .unwrap();

        assert_eq!(s.verify(raw).unwrap_err(), RbacError::UserDisabled);
    }

    #[test]
    fn verify_credential_with_orphan_user_is_credential_invalid() {
        let (_d, s) = fresh();
        // Bootstrap creates the reserved _legacy user + active credential.
        s.bootstrap_legacy_superadmin("orphan-tok").unwrap();
        // Delete only the user row with FK enforcement disabled for this
        // single pooled connection, leaving an orphan active credential. This
        // forces verify()'s user-lookup to miss (the "user not found" arm).
        s.store
            .with_conn(|c| {
                c.pragma_update(None, "foreign_keys", "OFF")
                    .map_err(map_rusqlite)?;
                c.execute(
                    "DELETE FROM users WHERE user_id = ?",
                    params![UserId::reserved("_legacy").as_str()],
                )
                .map_err(map_rusqlite)?;
                c.pragma_update(None, "foreign_keys", "ON")
                    .map_err(map_rusqlite)?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            s.verify("orphan-tok").unwrap_err(),
            RbacError::CredentialInvalid
        );
    }

    #[test]
    fn list_users_skips_corrupt_timestamp_row() {
        let (_d, s) = fresh();
        s.add_user(alice()).unwrap();
        // A non-RFC3339 created_at makes row_to_user fail (parse_ts_rusqlite);
        // the read accessors degrade to an empty Vec / None via unwrap_or_default.
        s.store
            .with_write_tx(|tx| {
                tx.execute(
                    "UPDATE users SET created_at = 'not-a-timestamp' WHERE user_id = 'alice'",
                    [],
                )
                .map_err(map_rusqlite)
            })
            .unwrap();
        assert!(s.list_users().is_empty());
        assert!(s.get_user(&UserId::from_str("alice").unwrap()).is_none());
    }

    #[test]
    fn list_grants_skips_corrupt_timestamp_row() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let grant = sample_grant(&alice_id, 30000, 30000);
        s.add_grant(grant.clone()).unwrap();
        // A non-RFC3339 created_at makes row_to_grant fail (parse_ts_rusqlite);
        // the read accessors degrade to empty / None via unwrap_or_default.
        s.store
            .with_write_tx(|tx| {
                tx.execute(
                    "UPDATE grants SET created_at = 'not-a-timestamp' WHERE user_id = 'alice'",
                    [],
                )
                .map_err(map_rusqlite)
            })
            .unwrap();
        assert!(s.list_grants(None).is_empty());
        assert!(s.grants_for(&alice_id).is_empty());
        assert!(s.get_grant(&grant.id).is_none());
    }

    #[test]
    fn revoke_grant_propagates_corrupt_row_as_write_failure() {
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let grant = sample_grant(&alice_id, 30000, 30000);
        s.add_grant(grant.clone()).unwrap();
        // Corrupt the grant_id so row_to_grant's ULID parse fails inside the
        // revoke transaction; the error surfaces as WriteFailed (not GrantNotFound).
        s.store
            .with_write_tx(|tx| {
                tx.execute(
                    "UPDATE grants SET created_at = 'bad-ts' WHERE grant_id = ?",
                    params![grant.id.to_string()],
                )
                .map_err(map_rusqlite)
            })
            .unwrap();
        let err = s.revoke_grant(&grant.id).unwrap_err();
        assert!(matches!(err, IdentityStoreError::WriteFailed(_)));
    }

    #[test]
    fn list_users_empty_on_fresh_store() {
        // The unfiltered read accessor returns an empty Vec when no users
        // exist yet (exercises the empty-loop path before any push).
        let (_d, s) = fresh();
        assert!(s.list_users().is_empty());
    }

    #[test]
    fn get_user_returns_none_for_missing_user() {
        // The single-row query_row optional() path returns None for an
        // absent user without erroring.
        let (_d, s) = fresh();
        s.add_user(alice()).unwrap();
        assert!(s.get_user(&UserId::from_str("ghost").unwrap()).is_none());
    }

    #[test]
    fn list_grants_returns_empty_when_no_grants() {
        // Both the filtered and unfiltered branches return an empty Vec when
        // the user has no grants / the store has no grants at all.
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        assert!(s.list_grants(Some(&alice_id)).is_empty());
        assert!(s.list_grants(None).is_empty());
        assert!(s.grants_for(&alice_id).is_empty());
    }

    #[test]
    fn count_superadmins_excludes_disabled_superadmin() {
        // The COUNT query filters on disabled = 0, so a disabled superadmin
        // is not counted and has_active_superadmin / has_any_superadmin agree.
        let (_d, s) = fresh();
        let disabled_admin = User {
            id: UserId::superadmin(),
            display_name: "Built-in".into(),
            role: OperatorRole::Superadmin,
            created_at: fixed_ts(),
            disabled: true,
        };
        s.store
            .with_write_tx(|tx| insert_user(tx, &disabled_admin))
            .unwrap();
        assert_eq!(s.count_superadmins(), 0);
        assert!(!s.has_active_superadmin().unwrap());
        assert!(!s.has_any_superadmin());
    }

    #[test]
    fn grants_for_reads_back_reserved_user_grant() {
        // A grant owned by the reserved `_legacy` user round-trips through
        // row_to_grant -> parse_user_id's reserved (`_`-prefixed) branch.
        let (_d, s) = fresh();
        s.bootstrap_legacy_superadmin("legacy-tok").unwrap();
        let legacy_id = UserId::reserved("_legacy");
        let grant = sample_grant(&legacy_id, 40000, 40000);
        s.add_grant(grant.clone()).unwrap();

        let got = s.grants_for(&legacy_id);
        assert_eq!(got.len(), 1);
        assert!(got[0].user_id.is_reserved());
        // The unfiltered listing and single-row lookup also resolve the
        // reserved owner without error.
        assert_eq!(s.list_grants(None).len(), 1);
        let one = s.get_grant(&grant.id).unwrap();
        assert_eq!(one.user_id, legacy_id);
    }

    #[test]
    fn list_grants_skips_row_with_empty_client_name() {
        // An empty `client` string is neither "*" (Any) nor a valid
        // ClientName, so row_to_grant's Named branch errors (ClientName::new
        // rejects empty); the read accessors degrade to empty via
        // unwrap_or_default.
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let grant = sample_grant(&alice_id, 30000, 30000);
        s.add_grant(grant).unwrap();
        s.store
            .with_write_tx(|tx| {
                tx.execute("UPDATE grants SET client = '' WHERE user_id = 'alice'", [])
                    .map_err(map_rusqlite)
            })
            .unwrap();
        assert!(s.list_grants(None).is_empty());
        assert!(s.grants_for(&alice_id).is_empty());
    }

    #[test]
    fn list_grants_skips_row_with_corrupt_grant_id() {
        // A non-ULID grant_id makes row_to_grant's ULID parse fail; the read
        // accessors degrade to empty rather than surfacing the error.
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let grant = sample_grant(&alice_id, 30000, 30000);
        s.add_grant(grant.clone()).unwrap();
        s.store
            .with_write_tx(|tx| {
                tx.execute(
                    "UPDATE grants SET grant_id = 'not-a-ulid' WHERE user_id = 'alice'",
                    [],
                )
                .map_err(map_rusqlite)
            })
            .unwrap();
        assert!(s.list_grants(Some(&alice_id)).is_empty());
        assert!(s.list_grants(None).is_empty());
    }

    #[test]
    fn list_grants_skips_row_with_corrupt_user_id() {
        // A non-reserved, non-parseable user_id makes parse_user_id's error
        // branch fire inside row_to_grant; the read accessors degrade to empty.
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let grant = sample_grant(&alice_id, 30000, 30000);
        s.add_grant(grant).unwrap();
        // FK enforcement would block pointing user_id at a non-existent row,
        // so disable it on this pooled connection for the single UPDATE.
        s.store
            .with_conn(|c| {
                c.pragma_update(None, "foreign_keys", "OFF")
                    .map_err(map_rusqlite)?;
                c.execute(
                    "UPDATE grants SET user_id = 'BAD ID' WHERE client = '*'",
                    [],
                )
                .map_err(map_rusqlite)?;
                c.pragma_update(None, "foreign_keys", "ON")
                    .map_err(map_rusqlite)?;
                Ok(())
            })
            .unwrap();
        assert!(s.list_grants(None).is_empty());
    }

    #[test]
    fn remove_user_rejects_missing_user() {
        // remove_user maps the internal user_not_found conflict to the typed
        // UserNotFound error.
        let (_d, s) = fresh();
        let missing = UserId::from_str("missing").unwrap();
        let err = s.remove_user(&missing).unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserNotFound(id) if id == missing));
    }

    #[test]
    fn remove_user_with_no_credentials_or_grants_returns_empty_summary() {
        // The cascade-id collectors return empty Vecs when the user owns
        // nothing, and the summary reflects that.
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let summary = s.remove_user(&alice_id).unwrap();
        assert!(summary.removed_credential_ids.is_empty());
        assert!(summary.revoked_grant_ids.is_empty());
        assert!(s.list_users().is_empty());
    }

    #[test]
    fn add_user_with_password_reserved_owner_round_trips() {
        // Seeding a reserved-id user with a password hash exercises the
        // Some(hash) UPDATE branch and parse_user_id's reserved read-back.
        let (_d, s) = fresh();
        let reserved = UserId::reserved("_seed");
        let user = User {
            id: reserved.clone(),
            display_name: "Seed".into(),
            role: OperatorRole::Superadmin,
            created_at: fixed_ts(),
            disabled: false,
        };
        s.add_user_with_password(user, Some("$argon2id$seed"), false)
            .unwrap();
        let got = s.get_user(&reserved).unwrap();
        assert!(got.id.is_reserved());
        let state = s.password_state(&reserved).unwrap().unwrap();
        assert_eq!(state.hash, "$argon2id$seed");
        assert!(!state.password_change_required);
    }

    #[test]
    fn verify_updates_last_used_at_on_success() {
        // A successful verify() performs the best-effort last_used_at write;
        // read the column back to confirm it transitioned from NULL.
        let (_d, s) = fresh();
        let alice_id = UserId::from_str("alice").unwrap();
        s.add_user(alice()).unwrap();
        let raw = "last-used-tok";
        let cred = Credential {
            id: CredentialId::new(),
            user_id: alice_id.clone(),
            token_hash: hash_token(raw),
            label: None,
            created_at: fixed_ts(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };
        let cred_id = cred.id.to_string();
        s.store
            .with_write_tx(|tx| insert_credential(tx, &cred))
            .unwrap();

        s.verify(raw).unwrap();

        let last_used: Option<String> = s
            .store
            .with_conn(|c| {
                c.query_row(
                    "SELECT last_used_at FROM credentials WHERE credential_id = ?",
                    params![cred_id],
                    |r| r.get(0),
                )
                .map_err(map_rusqlite)
            })
            .unwrap();
        assert!(last_used.is_some());
    }

    #[test]
    fn debug_impl_includes_db_path() {
        // The custom Debug impl renders the struct name and the db path field.
        let (_d, s) = fresh();
        let rendered = format!("{s:?}");
        assert!(rendered.contains("SqliteOperatorStore"));
        assert!(rendered.contains("db"));
    }
}
