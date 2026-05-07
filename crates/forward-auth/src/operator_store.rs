//! Operator-side identity store (`identity.json`).
//!
//! Mirrors `file_store::FileTokenStore` shape — atomic-write JSON,
//! `RwLock<HashMap>` in-memory, single seam at the
//! [`crate::OperatorAuthenticator`] trait. See
//! `specs/005-multi-user-rbac/contracts/persistence.md` for the on-disk
//! schema and `data-model.md` for the entity shapes.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use chrono::Utc;
use forward_core::fingerprint;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    Credential, CredentialId, CredentialStatus, Grant, GrantId, OperatorAuthenticator,
    OperatorIdentity, OperatorRole, RbacError, User, UserId,
    token::{self, hash_token},
};

const SCHEMA_VERSION: u32 = 1;

/// On-disk schema. See `contracts/persistence.md` § "Schema (version 1)".
#[derive(Debug, Serialize, Deserialize)]
struct IdentityFile {
    version: u32,
    #[serde(default)]
    users: Vec<User>,
    #[serde(default)]
    credentials: Vec<Credential>,
    #[serde(default)]
    grants: Vec<Grant>,
}

/// Loader-side errors. See `contracts/persistence.md` §
/// "Invariants enforced at load time".
#[derive(Debug, Error)]
pub enum IdentityStoreError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid_json: {0}")]
    InvalidJson(String),
    #[error("unsupported_schema_version: {found} (this binary supports {expected})")]
    UnsupportedSchemaVersion { found: u32, expected: u32 },
    #[error("duplicate_user_id: {0}")]
    DuplicateUserId(UserId),
    #[error("duplicate_credential_id: {0}")]
    DuplicateCredentialId(CredentialId),
    #[error("duplicate_grant_id: {0}")]
    DuplicateGrantId(GrantId),
    #[error("orphan_credential: credential {cred_id} references unknown user {user_id}")]
    OrphanCredential {
        cred_id: CredentialId,
        user_id: UserId,
    },
    #[error("orphan_grant: grant {grant_id} references unknown user {user_id}")]
    OrphanGrant { grant_id: GrantId, user_id: UserId },
    #[error("invalid_port_range: start={start} end={end}")]
    InvalidPortRange { start: u16, end: u16 },
    #[error("hash_collision: two credentials share the same token_hash")]
    HashCollision,
    #[error("write_failed: {0}")]
    WriteFailed(String),
    #[error("user_already_exists: {0}")]
    UserAlreadyExists(UserId),
    #[error("user_not_found: {0}")]
    UserNotFound(UserId),
    #[error("credential_not_found: {0}")]
    CredentialNotFound(CredentialId),
    #[error("grant_not_found: {0}")]
    GrantNotFound(GrantId),
}

impl IdentityStoreError {
    /// Map storage-level errors to the operator-facing [`RbacError`] code.
    /// Unmapped variants surface as a generic internal error string.
    #[must_use]
    pub fn as_rbac(&self) -> Option<RbacError> {
        Some(match self {
            Self::UserNotFound(_) => RbacError::UserNotFound,
            Self::CredentialNotFound(_) => RbacError::CredentialNotFound,
            Self::GrantNotFound(_) => RbacError::GrantNotFound,
            Self::UserAlreadyExists(_) => RbacError::UserAlreadyExists,
            _ => return None,
        })
    }
}

#[derive(Debug, Default)]
struct Inner {
    users: HashMap<UserId, User>,
    /// All credentials (Active and Revoked) keyed by id.
    credentials: HashMap<CredentialId, Credential>,
    /// Active credentials' token hashes → CredentialId. Revoked credentials
    /// are NOT in this map (they fail verify with `credential_invalid`).
    by_active_hash: HashMap<[u8; 32], CredentialId>,
    grants: HashMap<GrantId, Grant>,
}

impl Inner {
    fn rebuild_indexes(&mut self) {
        self.by_active_hash.clear();
        for (id, cred) in &self.credentials {
            if cred.status.is_active() {
                self.by_active_hash.insert(cred.token_hash, *id);
            }
        }
    }
}

/// Cascade summary returned by `remove_user`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserRemoveSummary {
    pub removed_credential_ids: Vec<CredentialId>,
    pub revoked_grant_ids: Vec<GrantId>,
}

/// On-disk operator identity store. Cheap to clone via `Arc`. Thread-safe.
#[derive(Debug)]
pub struct FileOperatorStore {
    path: PathBuf,
    state: RwLock<Inner>,
}

impl FileOperatorStore {
    /// Open an existing store, or initialise an empty one if absent.
    /// Validates per `contracts/persistence.md` § "Invariants enforced at load time".
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, IdentityStoreError> {
        let path = path.into();
        let inner = if path.exists() {
            Self::load_from(&path)?
        } else {
            Inner::default()
        };
        Ok(Self {
            path,
            state: RwLock::new(inner),
        })
    }

    fn load_from(path: &Path) -> Result<Inner, IdentityStoreError> {
        let raw = fs::read_to_string(path)?;
        let file: IdentityFile = serde_json::from_str(&raw)
            .map_err(|e| IdentityStoreError::InvalidJson(e.to_string()))?;
        if file.version != SCHEMA_VERSION {
            return Err(IdentityStoreError::UnsupportedSchemaVersion {
                found: file.version,
                expected: SCHEMA_VERSION,
            });
        }

        let mut inner = Inner::default();

        for u in file.users {
            if inner.users.contains_key(&u.id) {
                return Err(IdentityStoreError::DuplicateUserId(u.id.clone()));
            }
            inner.users.insert(u.id.clone(), u);
        }

        let mut seen_active_hash: HashMap<[u8; 32], CredentialId> = HashMap::new();
        for c in file.credentials {
            if inner.credentials.contains_key(&c.id) {
                return Err(IdentityStoreError::DuplicateCredentialId(c.id));
            }
            if !inner.users.contains_key(&c.user_id) {
                return Err(IdentityStoreError::OrphanCredential {
                    cred_id: c.id,
                    user_id: c.user_id.clone(),
                });
            }
            if c.status.is_active()
                && let Some(prior) = seen_active_hash.insert(c.token_hash, c.id)
            {
                let _ = prior;
                return Err(IdentityStoreError::HashCollision);
            }
            inner.credentials.insert(c.id, c);
        }

        for g in file.grants {
            if inner.grants.contains_key(&g.id) {
                return Err(IdentityStoreError::DuplicateGrantId(g.id));
            }
            if !inner.users.contains_key(&g.user_id) {
                return Err(IdentityStoreError::OrphanGrant {
                    grant_id: g.id,
                    user_id: g.user_id.clone(),
                });
            }
            if g.listen_port_start == 0 || g.listen_port_start > g.listen_port_end {
                return Err(IdentityStoreError::InvalidPortRange {
                    start: g.listen_port_start,
                    end: g.listen_port_end,
                });
            }
            inner.grants.insert(g.id, g);
        }

        inner.rebuild_indexes();
        Ok(inner)
    }

    fn snapshot(inner: &Inner) -> IdentityFile {
        // Sort each collection for stable on-disk output.
        let mut users: Vec<User> = inner.users.values().cloned().collect();
        users.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));

        let mut credentials: Vec<Credential> = inner.credentials.values().cloned().collect();
        credentials.sort_by(|a, b| a.id.0.cmp(&b.id.0));

        let mut grants: Vec<Grant> = inner.grants.values().cloned().collect();
        grants.sort_by(|a, b| a.id.0.cmp(&b.id.0));

        IdentityFile {
            version: SCHEMA_VERSION,
            users,
            credentials,
            grants,
        }
    }

    /// Atomic write: tmp + fsync + rename + parent fsync. Mode 0600.
    /// Mirrors `file_store::FileTokenStore::persist` verbatim.
    fn persist(&self, inner: &Inner) -> Result<(), IdentityStoreError> {
        let snapshot = Self::snapshot(inner);
        let body = serde_json::to_vec_pretty(&snapshot)
            .map_err(|e| IdentityStoreError::WriteFailed(format!("serialize: {e}")))?;
        let parent = self.path.parent().ok_or_else(|| {
            IdentityStoreError::WriteFailed(format!("path has no parent: {}", self.path.display()))
        })?;
        fs::create_dir_all(parent)?;

        let pid = std::process::id();
        let tag: u64 = {
            use rand::RngCore;
            rand::rngs::OsRng.next_u64()
        };
        let file_name = self.path.file_name().map_or_else(
            || "identity.json".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let tmp = parent.join(format!("{file_name}.tmp.{pid}.{tag}"));

        write_tmp_then_rename(&tmp, &self.path, parent, &body)
            .map_err(|e| IdentityStoreError::WriteFailed(e.to_string()))?;
        Ok(())
    }

    /// Reload the store from disk under the write lock. Used by SIGHUP
    /// (T049) and by post-flush rollback on partial-write failure.
    /// On validation failure, the in-memory snapshot is KEPT.
    pub fn reload_from_disk(&self) -> Result<(), IdentityStoreError> {
        let new = Self::load_from(&self.path)?;
        *self.state.write().expect("poisoned") = new;
        Ok(())
    }

    // ---------------- read accessors ----------------

    pub fn list_users(&self) -> Vec<User> {
        let s = self.state.read().expect("poisoned");
        let mut out: Vec<User> = s.users.values().cloned().collect();
        out.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        out
    }

    pub fn get_user(&self, id: &UserId) -> Option<User> {
        self.state.read().expect("poisoned").users.get(id).cloned()
    }

    pub fn count_active_credentials(&self, user_id: &UserId) -> usize {
        self.state
            .read()
            .expect("poisoned")
            .credentials
            .values()
            .filter(|c| &c.user_id == user_id && c.status.is_active())
            .count()
    }

    pub fn list_credentials(&self, user_id: &UserId) -> Vec<Credential> {
        let s = self.state.read().expect("poisoned");
        let mut out: Vec<Credential> = s
            .credentials
            .values()
            .filter(|c| &c.user_id == user_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        out
    }

    pub fn list_grants(&self, user_filter: Option<&UserId>) -> Vec<Grant> {
        let s = self.state.read().expect("poisoned");
        let mut out: Vec<Grant> = s
            .grants
            .values()
            .filter(|g| user_filter.is_none_or(|u| &g.user_id == u))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        out
    }

    pub fn get_grant(&self, id: &GrantId) -> Option<Grant> {
        self.state.read().expect("poisoned").grants.get(id).cloned()
    }

    pub fn count_superadmins(&self) -> usize {
        self.state
            .read()
            .expect("poisoned")
            .users
            .values()
            .filter(|u| u.role == OperatorRole::Superadmin && !u.disabled)
            .count()
    }

    /// Atomic bootstrap path used by the `forward-server bootstrap-superadmin`
    /// CLI subcommand: insert `user` and `cred` together in a single persist
    /// call. The caller validates that no superadmin already exists; this
    /// method only enforces that `user.id` is unused and that `cred.user_id`
    /// matches `user.id`.
    pub fn bootstrap_pair(&self, user: User, cred: Credential) -> Result<(), IdentityStoreError> {
        if cred.user_id != user.id {
            return Err(IdentityStoreError::WriteFailed(
                "bootstrap_pair: cred.user_id must equal user.id".to_string(),
            ));
        }
        let mut s = self.state.write().expect("poisoned");
        if s.users.contains_key(&user.id) {
            return Err(IdentityStoreError::UserAlreadyExists(user.id));
        }
        let hash = cred.token_hash;
        s.users.insert(user.id.clone(), user);
        s.credentials.insert(cred.id, cred.clone());
        if cred.status.is_active() {
            s.by_active_hash.insert(hash, cred.id);
        }
        let snapshot = clone_inner(&s);
        drop(s);
        self.flush_or_rollback(&snapshot)
    }

    /// Atomic bootstrap path used by the `operator_token` server.toml
    /// shortcut (FR-006): if NO superadmin exists yet, create the reserved
    /// `_legacy` superadmin and stamp it with a credential whose token is
    /// the supplied raw string. Returns `AlreadyBootstrapped` if any
    /// superadmin already exists.
    ///
    /// The operator-side cascade ordering (R-006) is preserved because
    /// users + credentials are committed in a single persist call.
    pub fn bootstrap_legacy_superadmin(&self, raw_token: &str) -> Result<(), IdentityStoreError> {
        if raw_token.is_empty() || raw_token.len() > 256 {
            return Err(IdentityStoreError::WriteFailed(
                "operator_token must be 1..=256 bytes".to_string(),
            ));
        }
        let mut s = self.state.write().expect("poisoned");
        if s.users
            .values()
            .any(|u| u.role == OperatorRole::Superadmin && !u.disabled)
        {
            return Err(IdentityStoreError::UserAlreadyExists(UserId::reserved(
                "_legacy",
            )));
        }
        let user = User {
            id: UserId::reserved("_legacy"),
            display_name: "operator_token shortcut".to_string(),
            role: OperatorRole::Superadmin,
            created_at: Utc::now(),
            disabled: false,
        };
        let hash = hash_token(raw_token);
        let cred = Credential {
            id: CredentialId::new(),
            user_id: user.id.clone(),
            token_hash: hash,
            label: Some("operator_token (server.toml)".to_string()),
            created_at: Utc::now(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };
        s.users.insert(user.id.clone(), user);
        s.credentials.insert(cred.id, cred.clone());
        s.by_active_hash.insert(hash, cred.id);
        let snapshot = clone_inner(&s);
        drop(s);
        self.flush_or_rollback(&snapshot)
    }

    // ---------------- mutating ops ----------------

    /// Insert a new user. Returns error if the id already exists.
    pub fn add_user(&self, user: User) -> Result<(), IdentityStoreError> {
        let mut s = self.state.write().expect("poisoned");
        if s.users.contains_key(&user.id) {
            return Err(IdentityStoreError::UserAlreadyExists(user.id));
        }
        s.users.insert(user.id.clone(), user);
        let snapshot = clone_inner(&s);
        drop(s);
        self.flush_or_rollback(&snapshot)
    }

    /// Cascade-remove a user: revoke all their credentials, drop all their
    /// grants, then remove the user. Returns the cascade summary so
    /// callers can do the rule-removal cascade outside the auth store.
    pub fn remove_user(&self, user_id: &UserId) -> Result<UserRemoveSummary, IdentityStoreError> {
        let mut s = self.state.write().expect("poisoned");
        if !s.users.contains_key(user_id) {
            return Err(IdentityStoreError::UserNotFound(user_id.clone()));
        }

        let cred_ids: Vec<CredentialId> = s
            .credentials
            .values()
            .filter(|c| &c.user_id == user_id)
            .map(|c| c.id)
            .collect();
        let grant_ids: Vec<GrantId> = s
            .grants
            .values()
            .filter(|g| &g.user_id == user_id)
            .map(|g| g.id)
            .collect();

        for cid in &cred_ids {
            s.credentials.remove(cid);
        }
        for gid in &grant_ids {
            s.grants.remove(gid);
        }
        s.users.remove(user_id);
        s.rebuild_indexes();

        let snapshot = clone_inner(&s);
        drop(s);
        self.flush_or_rollback(&snapshot)?;
        Ok(UserRemoveSummary {
            removed_credential_ids: cred_ids,
            revoked_grant_ids: grant_ids,
        })
    }

    /// Issue a fresh credential for `user_id`. Returns `(record, raw_token)`;
    /// raw token is shown to the operator exactly once at issuance.
    pub fn issue_credential(
        &self,
        user_id: &UserId,
        label: Option<String>,
    ) -> Result<(Credential, String), IdentityStoreError> {
        let raw = token::generate_token();
        let hash = hash_token(&raw);
        let cred = Credential {
            id: CredentialId::new(),
            user_id: user_id.clone(),
            token_hash: hash,
            label,
            created_at: Utc::now(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };

        let mut s = self.state.write().expect("poisoned");
        if !s.users.contains_key(user_id) {
            return Err(IdentityStoreError::UserNotFound(user_id.clone()));
        }
        s.credentials.insert(cred.id, cred.clone());
        s.by_active_hash.insert(hash, cred.id);
        let snapshot = clone_inner(&s);
        drop(s);
        self.flush_or_rollback(&snapshot)?;
        Ok((cred, raw))
    }

    /// Revoke a credential. Idempotent on already-revoked credentials.
    /// Returns 404 only if the credential doesn't exist or doesn't belong
    /// to the named user.
    pub fn revoke_credential(
        &self,
        user_id: &UserId,
        cred_id: &CredentialId,
    ) -> Result<(), IdentityStoreError> {
        let mut s = self.state.write().expect("poisoned");
        let Some(cred) = s.credentials.get_mut(cred_id) else {
            return Err(IdentityStoreError::CredentialNotFound(*cred_id));
        };
        if &cred.user_id != user_id {
            return Err(IdentityStoreError::CredentialNotFound(*cred_id));
        }
        if cred.status.is_active() {
            cred.status = CredentialStatus::revoked(Utc::now());
        }
        let hash = cred.token_hash;
        s.by_active_hash.remove(&hash);

        let snapshot = clone_inner(&s);
        drop(s);
        self.flush_or_rollback(&snapshot)
    }

    /// Atomic rotation: revoke `cred_id`, issue a new credential for the
    /// same user. Both writes commit in one persist call.
    pub fn rotate_credential(
        &self,
        user_id: &UserId,
        cred_id: &CredentialId,
        new_label: Option<String>,
    ) -> Result<(Credential, String), IdentityStoreError> {
        let raw = token::generate_token();
        let hash = hash_token(&raw);
        let new_cred = Credential {
            id: CredentialId::new(),
            user_id: user_id.clone(),
            token_hash: hash,
            label: new_label,
            created_at: Utc::now(),
            last_used_at: None,
            status: CredentialStatus::active(),
        };

        let mut s = self.state.write().expect("poisoned");
        if !s.users.contains_key(user_id) {
            return Err(IdentityStoreError::UserNotFound(user_id.clone()));
        }
        let Some(old) = s.credentials.get_mut(cred_id) else {
            return Err(IdentityStoreError::CredentialNotFound(*cred_id));
        };
        if &old.user_id != user_id {
            return Err(IdentityStoreError::CredentialNotFound(*cred_id));
        }
        if old.status.is_active() {
            old.status = CredentialStatus::revoked(Utc::now());
        }
        let old_hash = old.token_hash;
        s.by_active_hash.remove(&old_hash);
        s.credentials.insert(new_cred.id, new_cred.clone());
        s.by_active_hash.insert(hash, new_cred.id);

        let snapshot = clone_inner(&s);
        drop(s);
        self.flush_or_rollback(&snapshot)?;
        Ok((new_cred, raw))
    }

    pub fn add_grant(&self, grant: Grant) -> Result<(), IdentityStoreError> {
        if grant.listen_port_start == 0 || grant.listen_port_start > grant.listen_port_end {
            return Err(IdentityStoreError::InvalidPortRange {
                start: grant.listen_port_start,
                end: grant.listen_port_end,
            });
        }
        let mut s = self.state.write().expect("poisoned");
        if !s.users.contains_key(&grant.user_id) {
            return Err(IdentityStoreError::UserNotFound(grant.user_id.clone()));
        }
        s.grants.insert(grant.id, grant);
        let snapshot = clone_inner(&s);
        drop(s);
        self.flush_or_rollback(&snapshot)
    }

    pub fn revoke_grant(&self, grant_id: &GrantId) -> Result<Grant, IdentityStoreError> {
        let mut s = self.state.write().expect("poisoned");
        let Some(g) = s.grants.remove(grant_id) else {
            return Err(IdentityStoreError::GrantNotFound(*grant_id));
        };
        let snapshot = clone_inner(&s);
        drop(s);
        self.flush_or_rollback(&snapshot)?;
        Ok(g)
    }

    /// Flush the snapshot; on failure, roll back the in-memory state
    /// by reloading from the prior on-disk file.
    fn flush_or_rollback(&self, snapshot: &Inner) -> Result<(), IdentityStoreError> {
        match self.persist(snapshot) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Best-effort reload; if reload also fails, surface the
                // original write error (the in-memory state may now be
                // mid-mutation but that's strictly better than panicking).
                let _ = self.reload_from_disk();
                Err(e)
            }
        }
    }
}

fn clone_inner(s: &Inner) -> Inner {
    Inner {
        users: s.users.clone(),
        credentials: s.credentials.clone(),
        by_active_hash: s.by_active_hash.clone(),
        grants: s.grants.clone(),
    }
}

#[cfg(unix)]
fn write_tmp_then_rename(
    tmp: &Path,
    dest: &Path,
    parent: &Path,
    body: &[u8],
) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)?;
    f.write_all(body)?;
    f.sync_all()?;
    drop(f);
    fs::rename(tmp, dest)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_tmp_then_rename(
    tmp: &Path,
    dest: &Path,
    _parent: &Path,
    body: &[u8],
) -> std::io::Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(tmp)?;
    f.write_all(body)?;
    f.sync_all()?;
    drop(f);
    fs::rename(tmp, dest)?;
    Ok(())
}

impl OperatorAuthenticator for FileOperatorStore {
    fn verify(&self, token: &str) -> Result<OperatorIdentity, RbacError> {
        if token.is_empty() {
            return Err(RbacError::Unauthenticated);
        }
        if token.len() > 256 {
            return Err(RbacError::CredentialInvalid);
        }
        let presented = hash_token(token);

        // Snapshot under read lock; release before any mutating op.
        let (cred_user_id, is_active, _cred_id) = {
            let s = self.state.read().expect("poisoned");
            // Constant-time scan of active credentials.
            let mut found_id: Option<CredentialId> = None;
            for (h, id) in &s.by_active_hash {
                if fingerprint::ct_eq(&presented, h) {
                    found_id = Some(*id);
                }
            }
            // Also check revoked credentials so the failure reason is
            // distinguishable (but still constant-time vs a missing token).
            let revoked_match = s
                .credentials
                .values()
                .any(|c| !c.status.is_active() && fingerprint::ct_eq(&presented, &c.token_hash));
            if let Some(id) = found_id {
                let cred = s.credentials.get(&id).cloned();
                cred.map_or((None, false, None), |c| (Some(c.user_id), true, Some(c.id)))
            } else {
                if revoked_match {
                    return Err(RbacError::CredentialInvalid);
                }
                (None, false, None)
            }
        };

        let Some(uid) = cred_user_id else {
            return Err(RbacError::CredentialInvalid);
        };
        if !is_active {
            return Err(RbacError::CredentialInvalid);
        }

        let user = {
            let s = self.state.read().expect("poisoned");
            s.users.get(&uid).cloned()
        };
        let Some(user) = user else {
            return Err(RbacError::CredentialInvalid);
        };
        if user.disabled {
            return Err(RbacError::UserDisabled);
        }

        // Best-effort last_used_at update — in-memory only; piggy-backs on
        // the next mutating flush. Holds the write lock briefly.
        if let Some(cid) = _cred_id {
            let mut s = self.state.write().expect("poisoned");
            if let Some(c) = s.credentials.get_mut(&cid) {
                c.last_used_at = Some(Utc::now());
            }
        }

        Ok(OperatorIdentity {
            user_id: user.id,
            role: user.role,
        })
    }

    fn grants_for(&self, user_id: &UserId) -> Vec<Grant> {
        let s = self.state.read().expect("poisoned");
        let mut out: Vec<Grant> = s
            .grants
            .values()
            .filter(|g| &g.user_id == user_id)
            .cloned()
            .collect();
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        out
    }

    fn has_any_superadmin(&self) -> bool {
        self.count_superadmins() > 0
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProtocolSet;
    use chrono::TimeZone;
    use forward_core::ClientName;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::thread;
    use tempfile::TempDir;

    fn fixed_ts() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, 7, 10, 0, 0).unwrap()
    }

    fn store_path(dir: &TempDir) -> PathBuf {
        dir.path().join("identity.json")
    }

    fn open_empty() -> (TempDir, FileOperatorStore) {
        let dir = TempDir::new().unwrap();
        let store = FileOperatorStore::open(store_path(&dir)).unwrap();
        (dir, store)
    }

    fn user_alice() -> User {
        User {
            id: UserId::from_str("alice").unwrap(),
            display_name: "Alice".into(),
            role: OperatorRole::User,
            created_at: fixed_ts(),
            disabled: false,
        }
    }

    fn user_superadmin() -> User {
        User {
            id: UserId::superadmin(),
            display_name: "Built-in".into(),
            role: OperatorRole::Superadmin,
            created_at: fixed_ts(),
            disabled: false,
        }
    }

    fn make_grant(user: &UserId, port: u16, protocols: ProtocolSet) -> Grant {
        Grant {
            id: GrantId::new(),
            user_id: user.clone(),
            client: crate::ClientScope::Named(ClientName::new("client-a").unwrap()),
            listen_port_start: port,
            listen_port_end: port,
            protocols,
            note: None,
            created_at: fixed_ts(),
        }
    }

    // -------- T008 schema golden + invariants --------

    #[test]
    fn schema_round_trip_via_disk() {
        let (dir, store) = open_empty();
        store.add_user(user_superadmin()).unwrap();
        store.add_user(user_alice()).unwrap();
        let (_cred, _raw) = store
            .issue_credential(&UserId::from_str("alice").unwrap(), Some("laptop".into()))
            .unwrap();
        let grant = make_grant(&UserId::from_str("alice").unwrap(), 30000, ProtocolSet::TCP);
        store.add_grant(grant).unwrap();

        // Reload — every entity round-trips.
        let store2 = FileOperatorStore::open(store_path(&dir)).unwrap();
        assert_eq!(store2.list_users().len(), 2);
        assert_eq!(
            store2
                .list_credentials(&UserId::from_str("alice").unwrap())
                .len(),
            1
        );
        assert_eq!(
            store2
                .list_grants(Some(&UserId::from_str("alice").unwrap()))
                .len(),
            1
        );
    }

    #[test]
    fn loader_rejects_unknown_schema_version() {
        let dir = TempDir::new().unwrap();
        let path = store_path(&dir);
        fs::write(
            &path,
            r#"{"version": 99, "users": [], "credentials": [], "grants": []}"#,
        )
        .unwrap();
        let err = FileOperatorStore::open(&path).unwrap_err();
        assert!(matches!(
            err,
            IdentityStoreError::UnsupportedSchemaVersion { .. }
        ));
    }

    #[test]
    fn loader_rejects_orphan_credential() {
        let dir = TempDir::new().unwrap();
        let path = store_path(&dir);
        let blob = r#"{
            "version": 1,
            "users": [],
            "credentials": [{
                "id": "01HEXY7Z2K8R0M3N9P4QABCDEF",
                "user_id": "ghost",
                "token_hash": "0000000000000000000000000000000000000000000000000000000000000000",
                "created_at": "2026-05-07T10:00:00Z",
                "status": "active"
            }],
            "grants": []
        }"#;
        fs::write(&path, blob).unwrap();
        let err = FileOperatorStore::open(&path).unwrap_err();
        assert!(matches!(err, IdentityStoreError::OrphanCredential { .. }));
    }

    #[test]
    fn loader_rejects_orphan_grant() {
        let dir = TempDir::new().unwrap();
        let path = store_path(&dir);
        let blob = r#"{
            "version": 1,
            "users": [],
            "credentials": [],
            "grants": [{
                "id": "01HFGH7Z2K8R0M3N9P4QABCDEF",
                "user_id": "ghost",
                "client": "client-a",
                "listen_port_start": 30000,
                "listen_port_end": 30010,
                "protocols": ["tcp"],
                "created_at": "2026-05-07T11:10:00Z"
            }]
        }"#;
        fs::write(&path, blob).unwrap();
        let err = FileOperatorStore::open(&path).unwrap_err();
        assert!(matches!(err, IdentityStoreError::OrphanGrant { .. }));
    }

    #[test]
    fn loader_rejects_invalid_port_range() {
        let dir = TempDir::new().unwrap();
        let path = store_path(&dir);
        let blob = r#"{
            "version": 1,
            "users": [{"id":"alice","display_name":"a","role":"user","created_at":"2026-05-07T10:00:00Z"}],
            "credentials": [],
            "grants": [{
                "id": "01HFGH7Z2K8R0M3N9P4QABCDEF",
                "user_id": "alice",
                "client": "client-a",
                "listen_port_start": 30010,
                "listen_port_end": 30000,
                "protocols": ["tcp"],
                "created_at": "2026-05-07T11:10:00Z"
            }]
        }"#;
        fs::write(&path, blob).unwrap();
        let err = FileOperatorStore::open(&path).unwrap_err();
        assert!(matches!(err, IdentityStoreError::InvalidPortRange { .. }));
    }

    // -------- T010 atomic write --------

    #[test]
    fn round_trip_add_user_flush_load() {
        let (dir, store) = open_empty();
        store.add_user(user_alice()).unwrap();
        let store2 = FileOperatorStore::open(store_path(&dir)).unwrap();
        assert_eq!(store2.list_users().len(), 1);
    }

    #[test]
    fn persisted_file_is_mode_0600() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let (dir, store) = open_empty();
            store.add_user(user_superadmin()).unwrap();
            let mode = fs::metadata(store_path(&dir)).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn concurrent_readers_during_writes_see_no_partial_state() {
        let (_dir, store) = open_empty();
        store.add_user(user_superadmin()).unwrap();
        let store = Arc::new(store);

        let writer = {
            let s = Arc::clone(&store);
            thread::spawn(move || {
                for i in 0..50 {
                    let mut u = user_alice();
                    u.id = UserId::from_str(&format!("alice{i}")).unwrap();
                    let _ = s.add_user(u);
                }
            })
        };

        let mut readers = vec![];
        for _ in 0..10 {
            let s = Arc::clone(&store);
            readers.push(thread::spawn(move || {
                for _ in 0..100 {
                    let users = s.list_users();
                    // Invariant: every listed user has a parseable id.
                    for u in users {
                        assert!(!u.id.as_str().is_empty());
                    }
                }
            }));
        }

        writer.join().unwrap();
        for r in readers {
            r.join().unwrap();
        }
        assert!(!store.list_users().is_empty());
    }

    // -------- T012 OperatorAuthenticator --------

    #[test]
    fn verify_accepts_freshly_issued_credential() {
        let (_dir, store) = open_empty();
        store.add_user(user_alice()).unwrap();
        let (_cred, raw) = store
            .issue_credential(&UserId::from_str("alice").unwrap(), None)
            .unwrap();
        let id = store.verify(&raw).unwrap();
        assert_eq!(id.user_id, UserId::from_str("alice").unwrap());
        assert_eq!(id.role, OperatorRole::User);
    }

    #[test]
    fn verify_rejects_unknown_token() {
        let (_dir, store) = open_empty();
        store.add_user(user_alice()).unwrap();
        let _ = store
            .issue_credential(&UserId::from_str("alice").unwrap(), None)
            .unwrap();
        let err = store
            .verify("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
            .unwrap_err();
        assert_eq!(err, RbacError::CredentialInvalid);
    }

    #[test]
    fn verify_rejects_revoked_credential() {
        let (_dir, store) = open_empty();
        store.add_user(user_alice()).unwrap();
        let (cred, raw) = store
            .issue_credential(&UserId::from_str("alice").unwrap(), None)
            .unwrap();
        store
            .revoke_credential(&UserId::from_str("alice").unwrap(), &cred.id)
            .unwrap();
        assert_eq!(store.verify(&raw), Err(RbacError::CredentialInvalid));
    }

    #[test]
    fn verify_rejects_disabled_user() {
        let (_dir, store) = open_empty();
        let mut alice = user_alice();
        alice.disabled = true;
        store.add_user(alice).unwrap();
        let (_cred, raw) = store
            .issue_credential(&UserId::from_str("alice").unwrap(), None)
            .unwrap();
        assert_eq!(store.verify(&raw), Err(RbacError::UserDisabled));
    }

    #[test]
    fn rotate_invalidates_old_and_issues_new() {
        let (_dir, store) = open_empty();
        store.add_user(user_alice()).unwrap();
        let (cred, old_raw) = store
            .issue_credential(&UserId::from_str("alice").unwrap(), Some("v1".into()))
            .unwrap();
        let (_new, new_raw) = store
            .rotate_credential(
                &UserId::from_str("alice").unwrap(),
                &cred.id,
                Some("v2".into()),
            )
            .unwrap();
        assert_eq!(store.verify(&old_raw), Err(RbacError::CredentialInvalid));
        let id = store.verify(&new_raw).unwrap();
        assert_eq!(id.user_id, UserId::from_str("alice").unwrap());
    }

    #[test]
    fn grants_for_returns_user_grants_in_creation_order() {
        let (_dir, store) = open_empty();
        store.add_user(user_alice()).unwrap();
        let alice = UserId::from_str("alice").unwrap();
        let g1 = make_grant(&alice, 30000, ProtocolSet::TCP);
        let g2 = make_grant(&alice, 30001, ProtocolSet::UDP);
        store.add_grant(g1.clone()).unwrap();
        store.add_grant(g2.clone()).unwrap();
        let out = store.grants_for(&alice);
        assert_eq!(out.len(), 2);
        // Both have the same created_at fixture; just assert presence.
        assert!(out.iter().any(|g| g.id == g1.id));
        assert!(out.iter().any(|g| g.id == g2.id));
    }

    #[test]
    fn user_remove_cascades_credentials_and_grants() {
        let (_dir, store) = open_empty();
        store.add_user(user_superadmin()).unwrap(); // ensure last_superadmin protection elsewhere
        store.add_user(user_alice()).unwrap();
        let alice = UserId::from_str("alice").unwrap();
        let _ = store.issue_credential(&alice, None).unwrap();
        let _ = store.issue_credential(&alice, None).unwrap();
        let g = make_grant(&alice, 30000, ProtocolSet::TCP);
        store.add_grant(g).unwrap();

        let summary = store.remove_user(&alice).unwrap();
        assert_eq!(summary.removed_credential_ids.len(), 2);
        assert_eq!(summary.revoked_grant_ids.len(), 1);
        assert!(store.get_user(&alice).is_none());
        assert_eq!(store.list_credentials(&alice).len(), 0);
        assert_eq!(store.list_grants(Some(&alice)).len(), 0);
    }

    #[test]
    fn has_any_superadmin_reflects_state() {
        let (_dir, store) = open_empty();
        assert!(!store.has_any_superadmin());
        store.add_user(user_superadmin()).unwrap();
        assert!(store.has_any_superadmin());
    }

    #[test]
    fn add_user_rejects_duplicate() {
        let (_dir, store) = open_empty();
        store.add_user(user_alice()).unwrap();
        let err = store.add_user(user_alice()).unwrap_err();
        assert!(matches!(err, IdentityStoreError::UserAlreadyExists(_)));
    }

    #[test]
    fn add_grant_rejects_inverted_range() {
        let (_dir, store) = open_empty();
        store.add_user(user_alice()).unwrap();
        let mut g = make_grant(&UserId::from_str("alice").unwrap(), 30000, ProtocolSet::TCP);
        g.listen_port_start = 30010;
        g.listen_port_end = 30000;
        assert!(matches!(
            store.add_grant(g).unwrap_err(),
            IdentityStoreError::InvalidPortRange { .. }
        ));
    }

    // -------- T049 SIGHUP-equivalent reload path --------

    #[test]
    fn reload_from_disk_picks_up_external_writes() {
        let (dir, store) = open_empty();
        store
            .bootstrap_legacy_superadmin("T049-bootstrap-token")
            .unwrap();
        // Externally append alice + a credential by writing the JSON
        // file directly (simulates an operator editing identity.json
        // out-of-band, then sending SIGHUP).
        let path = store_path(&dir);
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let new_user = serde_json::json!({
            "id": "alice",
            "display_name": "Alice",
            "role": "user",
            "created_at": fixed_ts(),
        });
        value["users"].as_array_mut().unwrap().push(new_user);
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        // Pre-reload: in-memory state still doesn't know about alice.
        assert!(
            store
                .get_user(&UserId::from_str("alice").unwrap())
                .is_none()
        );

        store.reload_from_disk().expect("reload");

        // Post-reload: alice is visible, superadmin still verifies.
        assert!(
            store
                .get_user(&UserId::from_str("alice").unwrap())
                .is_some()
        );
        assert!(store.verify("T049-bootstrap-token").is_ok());
    }

    #[test]
    fn reload_from_disk_keeps_prior_state_on_validation_failure() {
        let (dir, store) = open_empty();
        store.bootstrap_legacy_superadmin("T049-keep-old").unwrap();
        let path = store_path(&dir);

        // Corrupt the on-disk file with an invalid schema (orphan grant).
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let bad_grant = serde_json::json!({
            "id": "01HXXXXXXXXXXXXXXXXXXXXXXX",
            "user_id": "ghost",
            "client": "client-a",
            "listen_port_start": 30000,
            "listen_port_end": 30000,
            "protocols": ["tcp"],
            "created_at": fixed_ts(),
        });
        value["grants"].as_array_mut().unwrap().push(bad_grant);
        std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        let err = store.reload_from_disk().unwrap_err();
        // Either the orphan-grant or invalid-id classifier must fire.
        assert!(matches!(
            err,
            IdentityStoreError::OrphanGrant { .. }
                | IdentityStoreError::DuplicateGrantId(_)
                | IdentityStoreError::InvalidJson(_)
        ));
        // Prior in-memory state survived: bootstrap token still verifies.
        assert!(store.verify("T049-keep-old").is_ok());
    }
}
