//! 008-sqlite-storage T049/T050 — shared store types that survived the
//! retirement of `file_store.rs` and `operator_store.rs`.
//!
//! `IdentityStoreError` is the operator-store error taxonomy; the SQLite
//! impl in `portunus-server` returns the same variants so the HTTP error
//! mapping in `operator::users` / `operator::grants` keeps working
//! unchanged.
//!
//! `UserRemoveSummary` is the `remove_user` cascade summary.
//!
//! `ProvisionedClient` is the `Authenticator::list` projection (no token
//! hash exposed).

use chrono::{DateTime, Utc};
use portunus_core::{ClientId, ClientName};
use thiserror::Error;

use crate::{CredentialId, GrantId, RbacError, UserId};

/// Loader / mutation error taxonomy. Mirrors the v0.5 file-store error
/// names so the HTTP envelope (`code`, status mapping) stays byte-stable.
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

/// Cascade summary returned by `remove_user`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserRemoveSummary {
    pub removed_credential_ids: Vec<CredentialId>,
    pub revoked_grant_ids: Vec<GrantId>,
}

/// Public projection of one provisioned client. Token hash is NOT exposed.
#[derive(Debug, Clone)]
pub struct ProvisionedClient {
    /// Stable, system-generated identity (015-client-stable-id).
    pub client_id: ClientId,
    pub client_name: ClientName,
    pub issued_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub client_address: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CredentialId, GrantId, UserId};
    use std::str::FromStr;

    fn uid() -> UserId {
        UserId::from_str("alice").unwrap()
    }

    #[test]
    fn as_rbac_maps_known_variants() {
        assert_eq!(
            IdentityStoreError::UserNotFound(uid()).as_rbac(),
            Some(RbacError::UserNotFound)
        );
        assert_eq!(
            IdentityStoreError::CredentialNotFound(CredentialId::new()).as_rbac(),
            Some(RbacError::CredentialNotFound)
        );
        assert_eq!(
            IdentityStoreError::GrantNotFound(GrantId::new()).as_rbac(),
            Some(RbacError::GrantNotFound)
        );
        assert_eq!(
            IdentityStoreError::UserAlreadyExists(uid()).as_rbac(),
            Some(RbacError::UserAlreadyExists)
        );
    }

    #[test]
    fn as_rbac_returns_none_for_unmapped_variants() {
        // A storage-internal error has no operator-facing RBAC code.
        assert!(IdentityStoreError::HashCollision.as_rbac().is_none());
        assert!(
            IdentityStoreError::InvalidPortRange { start: 10, end: 1 }
                .as_rbac()
                .is_none()
        );
        assert!(
            IdentityStoreError::WriteFailed("disk full".to_owned())
                .as_rbac()
                .is_none()
        );
    }

    #[test]
    fn identity_store_error_display_includes_context() {
        let e = IdentityStoreError::UnsupportedSchemaVersion {
            found: 9,
            expected: 2,
        };
        let s = e.to_string();
        assert!(s.contains('9') && s.contains('2'), "got {s}");
        let e = IdentityStoreError::DuplicateUserId(uid());
        assert!(e.to_string().contains("alice"));
    }

    #[test]
    fn user_remove_summary_default_is_empty() {
        let s = UserRemoveSummary::default();
        assert!(s.removed_credential_ids.is_empty());
        assert!(s.revoked_grant_ids.is_empty());
    }
}
