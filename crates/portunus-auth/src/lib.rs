//! Authentication seam.
//!
//! Per Constitution Principle I (FR-023), every code path that decides
//! "is this caller authorised to act as `<client_name>`?" routes through
//! the [`Authenticator`] trait defined in this crate. Swapping the auth
//! scheme (e.g., adding mTLS later) means writing a new `Authenticator`
//! impl, not touching `portunus-server` or `portunus-client`.

pub mod store_types;
pub mod token;

pub use store_types::{IdentityStoreError, ProvisionedClient, UserRemoveSummary};

use std::fmt;

use portunus_core::{ClientId, ClientName};
use thiserror::Error;

/// Identity recovered from a verified credential.
///
/// Inserted into Tonic request extensions by the auth interceptor; every
/// request handler reads it via `req.extensions().get::<ClientIdentity>()`.
/// Carrying identity through this struct (rather than re-deriving it inside
/// handlers) is what satisfies Constitution V's preservation requirement.
#[derive(Debug, Clone)]
pub struct ClientIdentity {
    /// Stable opaque identity (canonical key; survives a display-name change).
    pub client_id: ClientId,
    /// Free-form display label (015-client-stable-id). Not an identity.
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

// ============================================================================
// 005-multi-user-rbac (T004 + T005 + T006): operator-side identity model.
//
// This is a NEW seam, distinct from `Authenticator` above. `Authenticator`
// covers the client→server (gRPC) channel. `OperatorAuthenticator` below
// covers the operator→server (HTTP) channel — separate file store, separate
// lock, zero shared state. See `specs/005-multi-user-rbac/data-model.md`
// § "Mapping to existing v0.4.0 types".
// ============================================================================

use std::str::FromStr;

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

/// User identifier. Stable, opaque to the operator after creation.
///
/// Validation per `data-model.md` § User: regex `^[a-z][a-z0-9_-]{0,31}$`
/// for user-issued IDs. Reserved IDs starting with `_` are accepted only
/// via [`UserId::reserved`] (private constructor — only the bootstrap and
/// migration code paths can mint them).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserId(String);

impl UserId {
    /// Sentinel ID for the built-in superadmin minted by the
    /// `bootstrap-superadmin` CLI subcommand or the `operator_token`
    /// config-shortcut (FR-006 / FR-017).
    #[must_use]
    pub fn superadmin() -> Self {
        Self("_superadmin".to_owned())
    }

    /// Constructor for reserved (`_`-prefixed) IDs. Used by bootstrap
    /// flows and by the SQLite-backed identity store when reading stored
    /// rows back (008-sqlite-storage T044).
    pub fn reserved(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// True iff the ID begins with `_` (reserved namespace).
    #[must_use]
    pub fn is_reserved(&self) -> bool {
        self.0.starts_with('_')
    }
}

impl fmt::Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for UserId {
    type Err = RbacError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() || s.len() > 32 {
            return Err(RbacError::InvalidUserId);
        }
        let bytes = s.as_bytes();
        // Reserved namespace check FIRST so "_admin" reports the more
        // specific reason rather than the generic "not lowercase letter".
        if bytes[0] == b'_' {
            return Err(RbacError::ReservedUserId);
        }
        // First char: lowercase letter.
        if !bytes[0].is_ascii_lowercase() {
            return Err(RbacError::InvalidUserId);
        }
        // Rest: lowercase letters, digits, `-`, `_`.
        for &b in &bytes[1..] {
            if !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_') {
                return Err(RbacError::InvalidUserId);
            }
        }
        Ok(Self(s.to_owned()))
    }
}

/// Credential identifier — opaque ULID (sortable by creation time).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialId(pub ulid::Ulid);

impl CredentialId {
    #[must_use]
    pub fn new() -> Self {
        Self(ulid::Ulid::new())
    }
}

impl Default for CredentialId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for CredentialId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Grant identifier — opaque ULID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GrantId(pub ulid::Ulid);

impl GrantId {
    #[must_use]
    pub fn new() -> Self {
        Self(ulid::Ulid::new())
    }
}

impl Default for GrantId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for GrantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OperatorRole {
    Superadmin,
    User,
}

/// All authorisation outcomes for the operator surface.
///
/// Each variant maps to a stable string code consumed by the operator CLI
/// and surfaced in HTTP error bodies. See
/// `specs/005-multi-user-rbac/contracts/operator-api.md` §
/// "Authentication" / "Authorization (RBAC) reasons" tables.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RbacError {
    // --- Authentication failures (HTTP 401) ---
    #[error("unauthenticated")]
    Unauthenticated,
    #[error("credential_invalid")]
    CredentialInvalid,
    #[error("user_disabled")]
    UserDisabled,

    // --- Authorisation failures (HTTP 403) ---
    #[error("client_not_granted")]
    ClientNotGranted,
    #[error("port_outside_grant")]
    PortOutsideGrant,
    #[error("protocol_not_granted")]
    ProtocolNotGranted,
    #[error("not_owner")]
    NotOwner,
    #[error("role_required")]
    RoleRequired,

    // --- Bootstrap state (HTTP 503) ---
    #[error("bootstrap_required")]
    BootstrapRequired,
    #[error("already_bootstrapped")]
    AlreadyBootstrapped,

    // --- Validation (HTTP 422) ---
    #[error("invalid_user_id")]
    InvalidUserId,
    #[error("invalid_display_name")]
    InvalidDisplayName,
    #[error("reserved_user_id")]
    ReservedUserId,
    #[error("invalid_port_range")]
    InvalidPortRange,
    #[error("empty_protocol_set")]
    EmptyProtocolSet,
    #[error("invalid_client")]
    InvalidClient,

    // --- State (HTTP 409) ---
    #[error("user_already_exists")]
    UserAlreadyExists,
    #[error("user_not_found")]
    UserNotFound,
    #[error("credential_not_found")]
    CredentialNotFound,
    #[error("grant_not_found")]
    GrantNotFound,
    #[error("cannot_remove_self")]
    CannotRemoveSelf,
    #[error("last_superadmin")]
    LastSuperadmin,
}

impl RbacError {
    /// Stable machine-readable code for the operator CLI / HTTP body.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::CredentialInvalid => "credential_invalid",
            Self::UserDisabled => "user_disabled",
            Self::ClientNotGranted => "client_not_granted",
            Self::PortOutsideGrant => "port_outside_grant",
            Self::ProtocolNotGranted => "protocol_not_granted",
            Self::NotOwner => "not_owner",
            Self::RoleRequired => "role_required",
            Self::BootstrapRequired => "bootstrap_required",
            Self::AlreadyBootstrapped => "already_bootstrapped",
            Self::InvalidUserId => "invalid_user_id",
            Self::InvalidDisplayName => "invalid_display_name",
            Self::ReservedUserId => "reserved_user_id",
            Self::InvalidPortRange => "invalid_port_range",
            Self::EmptyProtocolSet => "empty_protocol_set",
            Self::InvalidClient => "invalid_client",
            Self::UserAlreadyExists => "user_already_exists",
            Self::UserNotFound => "user_not_found",
            Self::CredentialNotFound => "credential_not_found",
            Self::GrantNotFound => "grant_not_found",
            Self::CannotRemoveSelf => "cannot_remove_self",
            Self::LastSuperadmin => "last_superadmin",
        }
    }
}

/// Operator-side user record. Persisted in `identity.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub display_name: String,
    pub role: OperatorRole,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub disabled: bool,
}

/// Operator-side credential record. The raw bearer token is NEVER stored;
/// only the blake3 hash. See `data-model.md` § Credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credential {
    pub id: CredentialId,
    pub user_id: UserId,
    /// 32-byte blake3 hash of the raw token. Hex-encoded on disk via
    /// `portunus_core::fingerprint::hex`.
    #[serde(with = "hash_hex")]
    pub token_hash: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub status: CredentialStatus,
}

/// Untagged-enum serialisation matches the wire format from
/// `contracts/persistence.md`: either the literal string `"active"` OR
/// the object `{"revoked": {"revoked_at": "..."}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CredentialStatus {
    Active(ActiveTag),
    Revoked { revoked: RevokedDetails },
}

/// Helper for the untagged "active" string variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActiveTag {
    #[serde(rename = "active")]
    Active,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokedDetails {
    pub revoked_at: chrono::DateTime<chrono::Utc>,
}

impl CredentialStatus {
    #[must_use]
    pub fn active() -> Self {
        Self::Active(ActiveTag::Active)
    }

    #[must_use]
    pub fn revoked(revoked_at: chrono::DateTime<chrono::Utc>) -> Self {
        Self::Revoked {
            revoked: RevokedDetails { revoked_at },
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Active(_))
    }
}

/// Per-user grant. Persisted in `identity.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: GrantId,
    pub user_id: UserId,
    /// Either a specific `ClientName` or the wildcard `"*"`.
    pub client: ClientScope,
    pub listen_port_start: u16,
    pub listen_port_end: u16,
    pub protocols: ProtocolSet,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClientScope {
    Any,
    Named(ClientName),
}

impl Serialize for ClientScope {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Any => ser.serialize_str("*"),
            Self::Named(n) => ser.serialize_str(n.as_str()),
        }
    }
}

impl<'de> Deserialize<'de> for ClientScope {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let s = String::deserialize(de)?;
        if s == "*" {
            Ok(Self::Any)
        } else {
            ClientName::new(s)
                .map(Self::Named)
                .map_err(serde::de::Error::custom)
        }
    }
}

bitflags! {
    /// Non-empty subset of {TCP, UDP}. Constructed via [`ProtocolSet::non_empty`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ProtocolSet: u8 {
        const TCP = 0b01;
        const UDP = 0b10;
    }
}

impl ProtocolSet {
    /// Validating constructor — rejects the empty set.
    pub fn non_empty(bits: Self) -> Result<Self, RbacError> {
        if bits.is_empty() {
            Err(RbacError::EmptyProtocolSet)
        } else {
            Ok(bits)
        }
    }
}

impl Serialize for ProtocolSet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(None)?;
        if self.contains(Self::TCP) {
            seq.serialize_element("tcp")?;
        }
        if self.contains(Self::UDP) {
            seq.serialize_element("udp")?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for ProtocolSet {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let raw: Vec<String> = Vec::deserialize(de)?;
        let mut out = Self::empty();
        for p in raw {
            match p.as_str() {
                "tcp" => out |= Self::TCP,
                "udp" => out |= Self::UDP,
                other => {
                    return Err(serde::de::Error::custom(format!(
                        "unknown protocol: {other}"
                    )));
                }
            }
        }
        if out.is_empty() {
            return Err(serde::de::Error::custom("empty_protocol_set"));
        }
        Ok(out)
    }
}

/// Identity recovered from a verified operator credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperatorIdentity {
    pub user_id: UserId,
    pub role: OperatorRole,
}

/// The single seam for operator-side authentication + grant lookup.
///
/// Constitution Principle I requires this be the only path that decides
/// "is this caller authorised to act as `<user_id>`?". Swapping the auth
/// scheme later (e.g., adding OIDC) means writing a new impl, not editing
/// every handler.
pub trait OperatorAuthenticator: Send + Sync + 'static {
    /// Verify a presented bearer token. Returns the recovered identity on
    /// success, or one of the authentication-failure variants of
    /// [`RbacError`] on failure.
    fn verify(&self, token: &str) -> Result<OperatorIdentity, RbacError>;

    /// Return all grants owned by the named user. Cheap clone — expected
    /// O(10) per user. Returns empty vec if user has no grants OR doesn't
    /// exist (callers MUST verify identity first via [`verify`](Self::verify)
    /// before consulting grants).
    fn grants_for(&self, user_id: &UserId) -> Vec<Grant>;

    /// True iff at least one user with role [`OperatorRole::Superadmin`]
    /// exists in the store. Drives the bootstrap-required check.
    fn has_any_superadmin(&self) -> bool;
}

mod hash_hex {
    //! Serde adapter for `[u8; 32]` ↔ 64-char lowercase hex.
    //! Mirrors the hex(blake3) encoding the v0.7 `file_store` used.
    use portunus_core::fingerprint;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&fingerprint::hex(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(de)?;
        if s.len() != 64 {
            return Err(serde::de::Error::custom(format!(
                "token_hash must be 64 hex chars, got {}",
                s.len()
            )));
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
            let hi = nibble(chunk[0]).ok_or_else(|| serde::de::Error::custom("bad hex char"))?;
            let lo = nibble(chunk[1]).ok_or_else(|| serde::de::Error::custom("bad hex char"))?;
            out[i] = (hi << 4) | lo;
        }
        Ok(out)
    }

    fn nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
}

#[cfg(test)]
mod rbac_types_tests {
    use super::*;

    #[test]
    fn user_id_accepts_valid_ids() {
        for valid in &[
            "alice",
            "a1",
            "a-_z",
            "a",
            "abcdefghijklmnopqrstuvwxyz012345",
        ] {
            UserId::from_str(valid).unwrap_or_else(|e| panic!("rejected {valid}: {e:?}"));
        }
    }

    #[test]
    fn user_id_rejects_invalid() {
        for invalid in &[
            "",
            "Alice",
            "1alice",
            "a~",
            "a@",
            "abcdefghijklmnopqrstuvwxyz0123456",
        ] {
            // ^ 33 chars is 1 over the 32 ceiling.
            assert!(
                UserId::from_str(invalid).is_err(),
                "should reject {invalid}"
            );
        }
    }

    #[test]
    fn user_id_rejects_reserved_via_public_path() {
        assert_eq!(UserId::from_str("_admin"), Err(RbacError::ReservedUserId));
        assert_eq!(
            UserId::from_str("_superadmin"),
            Err(RbacError::ReservedUserId)
        );
    }

    #[test]
    fn user_id_reserved_via_private_constructor() {
        let id = UserId::reserved("_superadmin");
        assert_eq!(id.as_str(), "_superadmin");
        assert!(id.is_reserved());
    }

    #[test]
    fn user_id_serde_transparent() {
        let id = UserId::from_str("alice").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""alice""#);
        let back: UserId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn protocol_set_non_empty_validates() {
        assert!(ProtocolSet::non_empty(ProtocolSet::empty()).is_err());
        assert!(ProtocolSet::non_empty(ProtocolSet::TCP).is_ok());
        assert_eq!(
            ProtocolSet::non_empty(ProtocolSet::TCP | ProtocolSet::UDP).unwrap(),
            ProtocolSet::TCP | ProtocolSet::UDP
        );
    }

    #[test]
    fn protocol_set_serde_round_trip() {
        let set = ProtocolSet::TCP | ProtocolSet::UDP;
        let json = serde_json::to_string(&set).unwrap();
        assert_eq!(json, r#"["tcp","udp"]"#);
        let back: ProtocolSet = serde_json::from_str(&json).unwrap();
        assert_eq!(set, back);
        // Single-protocol round-trip.
        let just_udp = ProtocolSet::UDP;
        assert_eq!(serde_json::to_string(&just_udp).unwrap(), r#"["udp"]"#);
        // Empty rejected.
        assert!(serde_json::from_str::<ProtocolSet>("[]").is_err());
        // Unknown variant rejected.
        assert!(serde_json::from_str::<ProtocolSet>(r#"["sctp"]"#).is_err());
    }

    #[test]
    fn client_scope_serde() {
        let any = ClientScope::Any;
        assert_eq!(serde_json::to_string(&any).unwrap(), r#""*""#);
        let back: ClientScope = serde_json::from_str(r#""*""#).unwrap();
        assert_eq!(any, back);
        let named = ClientScope::Named(ClientName::new("client-a".to_owned()).unwrap());
        let json = serde_json::to_string(&named).unwrap();
        assert_eq!(json, r#""client-a""#);
        let back: ClientScope = serde_json::from_str(&json).unwrap();
        assert_eq!(named, back);
    }

    #[test]
    fn credential_status_untagged_serde() {
        let active = CredentialStatus::active();
        let json = serde_json::to_string(&active).unwrap();
        assert_eq!(json, r#""active""#);
        let back: CredentialStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(active, back);

        let now = chrono::DateTime::parse_from_rfc3339("2026-05-07T11:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let revoked = CredentialStatus::revoked(now);
        let json = serde_json::to_string(&revoked).unwrap();
        assert!(json.contains("revoked"));
        assert!(json.contains("2026-05-07T11:00:00Z"));
        let back: CredentialStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(revoked, back);
    }

    #[test]
    fn rbac_error_codes_stable() {
        // Sanity-check the externally-visible code strings used by the HTTP
        // surface and CLI exit-code mapper.
        assert_eq!(RbacError::Unauthenticated.code(), "unauthenticated");
        assert_eq!(RbacError::PortOutsideGrant.code(), "port_outside_grant");
        assert_eq!(
            RbacError::AlreadyBootstrapped.code(),
            "already_bootstrapped"
        );
        assert_eq!(RbacError::EmptyProtocolSet.code(), "empty_protocol_set");
    }

    #[test]
    fn rbac_error_code_matches_display_for_every_variant() {
        // `code()` is the machine-readable contract; for these variants it is
        // byte-identical to the `Display` impl, so iterate every variant and
        // assert the two never drift apart.
        let all = [
            RbacError::Unauthenticated,
            RbacError::CredentialInvalid,
            RbacError::UserDisabled,
            RbacError::ClientNotGranted,
            RbacError::PortOutsideGrant,
            RbacError::ProtocolNotGranted,
            RbacError::NotOwner,
            RbacError::RoleRequired,
            RbacError::BootstrapRequired,
            RbacError::AlreadyBootstrapped,
            RbacError::InvalidUserId,
            RbacError::InvalidDisplayName,
            RbacError::ReservedUserId,
            RbacError::InvalidPortRange,
            RbacError::EmptyProtocolSet,
            RbacError::InvalidClient,
            RbacError::UserAlreadyExists,
            RbacError::UserNotFound,
            RbacError::CredentialNotFound,
            RbacError::GrantNotFound,
            RbacError::CannotRemoveSelf,
            RbacError::LastSuperadmin,
        ];
        for e in all {
            assert_eq!(e.code(), e.to_string(), "code/Display drift for {e:?}");
        }
    }

    #[test]
    fn auth_failure_reason_display_strings() {
        assert_eq!(AuthFailureReason::Missing.to_string(), "missing_token");
        assert_eq!(AuthFailureReason::Malformed.to_string(), "malformed_token");
        assert_eq!(AuthFailureReason::NotFound.to_string(), "token_not_found");
        assert_eq!(AuthFailureReason::Revoked.to_string(), "token_revoked");
    }

    #[test]
    fn auth_error_display_wraps_reason() {
        let e = AuthError::Failed(AuthFailureReason::Revoked);
        assert_eq!(e.to_string(), "auth_failed: token_revoked");
        let name = ClientName::new("edge-a").unwrap();
        let e = AuthError::ClientAlreadyExists(name);
        assert_eq!(e.to_string(), "client_already_exists: edge-a");
        let e = AuthError::StoreCorrupt("bad".to_owned());
        assert_eq!(e.to_string(), "store_corrupt: bad");
    }

    #[test]
    fn credential_id_default_display_and_serde() {
        // Default mints a fresh ULID per call.
        assert_ne!(CredentialId::default(), CredentialId::default());
        let id = CredentialId::new();
        // Display delegates to the inner ULID (26-char canonical form).
        assert_eq!(id.to_string().len(), 26);
        let json = serde_json::to_string(&id).unwrap();
        let back: CredentialId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn grant_id_default_display_and_serde() {
        assert_ne!(GrantId::default(), GrantId::default());
        let id = GrantId::new();
        assert_eq!(id.to_string().len(), 26);
        let json = serde_json::to_string(&id).unwrap();
        let back: GrantId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn credential_status_is_active_predicate() {
        assert!(CredentialStatus::active().is_active());
        let now = chrono::Utc::now();
        assert!(!CredentialStatus::revoked(now).is_active());
    }

    #[test]
    fn client_scope_named_rejects_invalid_name() {
        // A control character makes the inner ClientName invalid, so the
        // ClientScope deserializer surfaces a custom error (not `*`).
        let bad = "\"bad\\u0007name\"";
        assert!(serde_json::from_str::<ClientScope>(bad).is_err());
    }

    #[test]
    fn protocol_set_tcp_only_serializes() {
        assert_eq!(
            serde_json::to_string(&ProtocolSet::TCP).unwrap(),
            r#"["tcp"]"#
        );
        // Duplicate entries collapse to the set; still valid.
        let set: ProtocolSet = serde_json::from_str(r#"["tcp","tcp"]"#).unwrap();
        assert_eq!(set, ProtocolSet::TCP);
    }

    #[test]
    fn operator_role_serde_lowercase() {
        assert_eq!(
            serde_json::to_string(&OperatorRole::Superadmin).unwrap(),
            r#""superadmin""#
        );
        assert_eq!(
            serde_json::to_string(&OperatorRole::User).unwrap(),
            r#""user""#
        );
    }

    fn sample_credential() -> Credential {
        Credential {
            id: CredentialId::new(),
            user_id: UserId::from_str("alice").unwrap(),
            token_hash: [0xab; 32],
            label: Some("laptop".to_owned()),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            last_used_at: None,
            status: CredentialStatus::active(),
        }
    }

    #[test]
    fn credential_token_hash_hex_roundtrip() {
        let cred = sample_credential();
        let json = serde_json::to_string(&cred).unwrap();
        // token_hash serialises as 64 lowercase hex chars (0xab -> "ab").
        assert!(json.contains(&"ab".repeat(32)));
        let back: Credential = serde_json::from_str(&json).unwrap();
        assert_eq!(cred, back);
    }

    #[test]
    fn credential_token_hash_accepts_uppercase_hex() {
        let id = CredentialId::new();
        let json = format!(
            r#"{{"id":"{id}","user_id":"alice","token_hash":"{}","created_at":"2026-01-01T00:00:00Z","status":"active"}}"#,
            "AB".repeat(32)
        );
        let cred: Credential = serde_json::from_str(&json).unwrap();
        assert_eq!(cred.token_hash, [0xab; 32]);
    }

    #[test]
    fn credential_token_hash_rejects_bad_input() {
        let id = CredentialId::new();
        // Wrong length (4 hex chars instead of 64).
        let short = format!(
            r#"{{"id":"{id}","user_id":"alice","token_hash":"abcd","created_at":"2026-01-01T00:00:00Z","status":"active"}}"#
        );
        assert!(serde_json::from_str::<Credential>(&short).is_err());
        // Correct length but a non-hex character ('z').
        let bad = format!(
            r#"{{"id":"{id}","user_id":"alice","token_hash":"{}","created_at":"2026-01-01T00:00:00Z","status":"active"}}"#,
            "z".repeat(64)
        );
        assert!(serde_json::from_str::<Credential>(&bad).is_err());
    }
}
