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
    ActiveTag, ClientScope, Credential, CredentialId, CredentialStatus, Grant, GrantId,
    IdentityStoreError, OperatorAuthenticator, OperatorIdentity, OperatorRole, ProtocolSet,
    RbacError, RevokedDetails, User, UserId, UserRemoveSummary, token::hash_token,
};
use portunus_core::{ClientName, fingerprint};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::operator::{
    sessions,
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

    pub fn count_active_credentials(&self, user_id: &UserId) -> usize {
        self.store
            .with_conn(|c| {
                let n: i64 = c
                    .query_row(
                        "SELECT COUNT(*) FROM credentials \
                         WHERE user_id = ? AND status = 'active'",
                        params![user_id.as_str()],
                        |r| r.get(0),
                    )
                    .map_err(map_rusqlite)?;
                Ok(n as usize)
            })
            .unwrap_or(0)
    }

    pub fn list_credentials(&self, user_id: &UserId) -> Vec<Credential> {
        self.store
            .with_conn(|c| {
                let mut stmt = c
                    .prepare(
                        "SELECT credential_id, user_id, hash, label, status, issued_at, revoked_at, last_used_at \
                         FROM credentials WHERE user_id = ? ORDER BY credential_id ASC",
                    )
                    .map_err(map_rusqlite)?;
                let mut out = Vec::new();
                let rows = stmt
                    .query_map(params![user_id.as_str()], row_to_credential)
                    .map_err(map_rusqlite)?;
                for r in rows {
                    out.push(r.map_err(map_rusqlite)?);
                }
                Ok(out)
            })
            .unwrap_or_default()
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

    pub fn issue_credential(
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

    pub fn revoke_credential(
        &self,
        user_id: &UserId,
        cred_id: &CredentialId,
    ) -> Result<(), IdentityStoreError> {
        let cred_for_err = *cred_id;
        let now = Utc::now().to_rfc3339();
        self.store
            .with_write_tx(|tx| {
                let owner: Option<String> = tx
                    .query_row(
                        "SELECT user_id FROM credentials WHERE credential_id = ?",
                        params![cred_id.to_string()],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(map_rusqlite)?;
                let Some(owner_uid) = owner else {
                    return Err(StoreError::Conflict {
                        detail: "credential_not_found".into(),
                    });
                };
                if owner_uid != user_id.as_str() {
                    return Err(StoreError::Conflict {
                        detail: "credential_not_found".into(),
                    });
                }
                tx.execute(
                    "UPDATE credentials SET status = 'revoked', revoked_at = ? \
                     WHERE credential_id = ? AND status = 'active'",
                    params![now, cred_id.to_string()],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "credential_not_found" => {
                    IdentityStoreError::CredentialNotFound(cred_for_err)
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })
    }

    pub fn rotate_credential(
        &self,
        user_id: &UserId,
        cred_id: &CredentialId,
        new_label: Option<String>,
    ) -> Result<(Credential, String), IdentityStoreError> {
        let raw = portunus_auth::token::generate_token();
        let new_cred = Credential {
            id: CredentialId::new(),
            user_id: user_id.clone(),
            token_hash: hash_token(&raw),
            label: new_label,
            created_at: Utc::now(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };
        let cred_for_err = *cred_id;
        let uid_for_err = user_id.clone();
        let new_cred_clone = new_cred.clone();
        let now = Utc::now().to_rfc3339();
        self.store
            .with_write_tx(|tx| {
                if !user_exists(tx, user_id)? {
                    return Err(StoreError::Conflict {
                        detail: "user_not_found".into(),
                    });
                }
                let owner: Option<String> = tx
                    .query_row(
                        "SELECT user_id FROM credentials WHERE credential_id = ?",
                        params![cred_id.to_string()],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(map_rusqlite)?;
                let Some(owner_uid) = owner else {
                    return Err(StoreError::Conflict {
                        detail: "credential_not_found".into(),
                    });
                };
                if owner_uid != user_id.as_str() {
                    return Err(StoreError::Conflict {
                        detail: "credential_not_found".into(),
                    });
                }
                tx.execute(
                    "UPDATE credentials SET status = 'revoked', revoked_at = ? \
                     WHERE credential_id = ? AND status = 'active'",
                    params![now, cred_id.to_string()],
                )
                .map_err(map_rusqlite)?;
                insert_credential(tx, &new_cred_clone)?;
                Ok(())
            })
            .map_err(|e| match e {
                StoreError::Conflict { detail } if detail == "user_not_found" => {
                    IdentityStoreError::UserNotFound(uid_for_err)
                }
                StoreError::Conflict { detail } if detail == "credential_not_found" => {
                    IdentityStoreError::CredentialNotFound(cred_for_err)
                }
                other => IdentityStoreError::WriteFailed(other.to_string()),
            })?;
        Ok((new_cred, raw))
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

fn row_to_credential(r: &Row<'_>) -> rusqlite::Result<Credential> {
    let credential_id: String = r.get(0)?;
    let user_id: String = r.get(1)?;
    let hash: String = r.get(2)?;
    let label: Option<String> = r.get(3)?;
    let status: String = r.get(4)?;
    let issued_at: String = r.get(5)?;
    let revoked_at: Option<String> = r.get(6)?;
    let last_used_at: Option<String> = r.get(7)?;

    let id = ulid::Ulid::from_string(&credential_id).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad ULID: {e}"),
            )),
        )
    })?;
    let token_hash = hex_to_32(&hash).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "bad token hash hex",
            )),
        )
    })?;
    let uid = parse_user_id(&user_id, 1)?;
    let cs = match status.as_str() {
        "active" => CredentialStatus::Active(ActiveTag::Active),
        "revoked" => {
            let ts = revoked_at.ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    6,
                    rusqlite::types::Type::Text,
                    Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "revoked status missing revoked_at",
                    )),
                )
            })?;
            CredentialStatus::Revoked {
                revoked: RevokedDetails {
                    revoked_at: parse_ts_rusqlite(&ts, 6)?,
                },
            }
        }
        other => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown credential status {other}"),
                )),
            ));
        }
    };
    let last_used_at = last_used_at.map(|s| parse_ts_rusqlite(&s, 7)).transpose()?;
    Ok(Credential {
        id: CredentialId(id),
        user_id: uid,
        token_hash,
        label,
        status: cs,
        created_at: parse_ts_rusqlite(&issued_at, 5)?,
        last_used_at,
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

fn hex_to_32(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    for i in 0..32 {
        let hi = nibble(bytes[i * 2])?;
        let lo = nibble(bytes[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
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
    fn issue_credential_then_verify_round_trip() {
        let (_d, s) = fresh();
        s.add_user(alice()).unwrap();
        let (_, raw) = s
            .issue_credential(&UserId::from_str("alice").unwrap(), Some("laptop".into()))
            .unwrap();
        let id = s.verify(&raw).unwrap();
        assert_eq!(id.user_id.as_str(), "alice");
        assert_eq!(id.role, OperatorRole::User);
    }

    #[test]
    fn revoke_credential_blocks_verify() {
        let (_d, s) = fresh();
        s.add_user(alice()).unwrap();
        let (cred, raw) = s
            .issue_credential(&UserId::from_str("alice").unwrap(), None)
            .unwrap();
        s.revoke_credential(&UserId::from_str("alice").unwrap(), &cred.id)
            .unwrap();
        let err = s.verify(&raw).unwrap_err();
        assert_eq!(err, RbacError::CredentialInvalid);
    }

    #[test]
    fn remove_user_cascades_credentials_and_grants() {
        let (_d, s) = fresh();
        s.add_user(alice()).unwrap();
        let (_, _) = s
            .issue_credential(&UserId::from_str("alice").unwrap(), None)
            .unwrap();
        let g = Grant {
            id: GrantId::new(),
            user_id: UserId::from_str("alice").unwrap(),
            client: ClientScope::Any,
            listen_port_start: 30000,
            listen_port_end: 30010,
            protocols: ProtocolSet::TCP,
            note: None,
            created_at: fixed_ts(),
        };
        s.add_grant(g).unwrap();

        let summary = s.remove_user(&UserId::from_str("alice").unwrap()).unwrap();
        assert_eq!(summary.removed_credential_ids.len(), 1);
        assert_eq!(summary.revoked_grant_ids.len(), 1);
        assert!(s.list_users().is_empty());
        assert!(
            s.list_credentials(&UserId::from_str("alice").unwrap())
                .is_empty()
        );
        assert!(s.list_grants(None).is_empty());
    }

    #[test]
    fn rotate_credential_swaps_active() {
        let (_d, s) = fresh();
        s.add_user(alice()).unwrap();
        let (cred, raw1) = s
            .issue_credential(&UserId::from_str("alice").unwrap(), None)
            .unwrap();
        let (_new_cred, raw2) = s
            .rotate_credential(&UserId::from_str("alice").unwrap(), &cred.id, None)
            .unwrap();
        assert_ne!(raw1, raw2);
        assert!(s.verify(&raw1).is_err());
        assert!(s.verify(&raw2).is_ok());
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
}
