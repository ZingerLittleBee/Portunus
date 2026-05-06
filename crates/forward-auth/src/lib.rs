//! Authentication seam.
//!
//! Per Constitution Principle I (FR-023), every code path that decides
//! "is this caller authorised to act as `<client_name>`?" routes through
//! the [`Authenticator`] trait defined in this crate. Swapping the auth
//! scheme (e.g., adding mTLS later) means writing a new `Authenticator`
//! impl, not touching `forward-server` or `forward-client`.

pub mod file_store;
pub mod token;

use std::fmt;

use forward_core::ClientName;
use thiserror::Error;

/// Identity recovered from a verified credential.
///
/// Inserted into Tonic request extensions by the auth interceptor; every
/// request handler reads it via `req.extensions().get::<ClientIdentity>()`.
/// Carrying identity through this struct (rather than re-deriving it inside
/// handlers) is what satisfies Constitution V's preservation requirement.
#[derive(Debug, Clone)]
pub struct ClientIdentity {
    pub client_name: ClientName,
    // Future: pub tenant_id: TenantId  (Constitution V, post-MVP).
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFailureReason {
    Missing,
    Malformed,
    NotFound,
    Revoked,
}

impl fmt::Display for AuthFailureReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "missing_token",
            Self::Malformed => "malformed_token",
            Self::NotFound => "token_not_found",
            Self::Revoked => "token_revoked",
        })
    }
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("auth_failed: {0}")]
    Failed(AuthFailureReason),
    #[error("client_already_exists: {0}")]
    ClientAlreadyExists(ClientName),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("store_corrupt: {0}")]
    StoreCorrupt(String),
}

/// The single auth seam. All callers MUST go through this trait.
pub trait Authenticator: Send + Sync + 'static {
    /// Verify a presented bearer token. Returns the recovered identity on
    /// success; returns `AuthError::Failed(reason)` on every failure mode
    /// so that the gRPC interceptor can map to a `Status::unauthenticated`
    /// with a stable `reason` string.
    fn verify(&self, token: &str) -> Result<ClientIdentity, AuthError>;

    /// Provision a new client and return the plaintext token (returned to
    /// the operator exactly once — never persisted).
    fn issue(&self, name: ClientName) -> Result<String, AuthError>;

    /// Mark a client's token revoked. Idempotent. Errors only on I/O.
    fn revoke(&self, name: &ClientName) -> Result<(), AuthError>;
}
