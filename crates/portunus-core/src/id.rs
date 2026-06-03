//! Newtype wrappers for the IDs that flow through the workspace.
//!
//! All construction routes through validators so a malformed `ClientName`
//! cannot reach the auth seam, the rule store, or a log line.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use ulid::Ulid;

/// Maximum byte length of a client display name (FR-003).
pub const CLIENT_NAME_MAX_BYTES: usize = 255;

/// Operator-supplied client **display name**.
///
/// As of feature `015-client-stable-id` the name is a free-form display field —
/// identity is carried by [`ClientId`], not the name. Validation is intentionally
/// minimal (FR-003 / R-006):
/// - not empty or whitespace-only
/// - no Unicode control characters
/// - at most [`CLIENT_NAME_MAX_BYTES`] bytes
///
/// Uppercase, spaces, `.`, `_`, `-` (in any position), and non-Latin Unicode are all
/// accepted, and the value is stored verbatim (no case-folding or normalization).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ClientName(String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ClientNameError {
    #[error("client name must not be empty or whitespace-only")]
    Empty,
    #[error("client name must be at most {CLIENT_NAME_MAX_BYTES} bytes, got {0}")]
    TooLong(usize),
    #[error("client name must not contain control characters")]
    ControlChar,
}

impl ClientName {
    pub fn new(s: impl Into<String>) -> Result<Self, ClientNameError> {
        let s = s.into();
        if s.trim().is_empty() {
            return Err(ClientNameError::Empty);
        }
        if s.len() > CLIENT_NAME_MAX_BYTES {
            return Err(ClientNameError::TooLong(s.len()));
        }
        if s.chars().any(char::is_control) {
            return Err(ClientNameError::ControlChar);
        }
        Ok(Self(s))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Display for ClientName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ClientName {
    type Err = ClientNameError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl<'de> Deserialize<'de> for ClientName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::new(s).map_err(serde::de::Error::custom)
    }
}

/// Server-assigned, stable, opaque client identifier (ULID).
///
/// Introduced by feature `015-client-stable-id`. This — not [`ClientName`] — is the
/// canonical key for a client across persistence, the connected-client registry, the
/// operator API/CLI/Web-UI, and internal log/metric correlation. Assigned once at
/// creation/enrollment and immutable for the client's lifetime. Sortable by creation
/// time (ULID property).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ClientId(pub Ulid);

impl ClientId {
    /// Mint a fresh client id.
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for ClientId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for ClientId {
    type Err = ulid::DecodeError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ulid::from_string(s).map(Self)
    }
}

/// Server-assigned rule identifier. Stable for the lifetime of the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuleId(pub u64);

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Per-operator-action correlation ID, threaded through every log line that
/// touches the action — both server-side and (via gRPC `request_id` field)
/// client-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub Ulid);

impl RequestId {
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new())
    }
}

impl Default for RequestId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_free_form_display_names() {
        // Everything the old DNS-label rule rejected is now valid display text.
        for ok in [
            "edge-01",
            "Acme Prod – East",
            "edge_01.lab",
            "北京边缘节点",
            "UPPER",
            "a.b.c",
            "node--double--hyphen",
            "-leading-and-trailing-",
            "with spaces and 123",
            "🚀 rocket",
        ] {
            ClientName::new(ok).unwrap_or_else(|e| panic!("{ok:?} should parse: {e}"));
        }
    }

    #[test]
    fn stores_verbatim() {
        let n = ClientName::new("Acme Prod – East").unwrap();
        assert_eq!(n.as_str(), "Acme Prod – East");
    }

    #[test]
    fn rejects_empty_or_whitespace_only() {
        for bad in ["", "   ", "\t", " \u{3000} "] {
            // note: "\t" is also a control char; the empty/whitespace check runs first
            assert!(
                matches!(
                    ClientName::new(bad).unwrap_err(),
                    ClientNameError::Empty | ClientNameError::ControlChar
                ),
                "input={bad:?}"
            );
        }
        assert_eq!(ClientName::new("").unwrap_err(), ClientNameError::Empty);
        assert_eq!(ClientName::new("   ").unwrap_err(), ClientNameError::Empty);
    }

    #[test]
    fn rejects_control_characters() {
        assert_eq!(
            ClientName::new("bad\u{0007}name").unwrap_err(),
            ClientNameError::ControlChar
        );
        assert_eq!(
            ClientName::new("line\nbreak").unwrap_err(),
            ClientNameError::ControlChar
        );
    }

    #[test]
    fn rejects_overlong() {
        let s: String = "a".repeat(CLIENT_NAME_MAX_BYTES + 1);
        assert_eq!(
            ClientName::new(&s).unwrap_err(),
            ClientNameError::TooLong(CLIENT_NAME_MAX_BYTES + 1)
        );
    }

    #[test]
    fn accepts_max_length() {
        let s: String = "a".repeat(CLIENT_NAME_MAX_BYTES);
        assert!(ClientName::new(&s).is_ok());
    }

    #[test]
    fn from_str_works() {
        let n: ClientName = "Acme Prod".parse().unwrap();
        assert_eq!(n.as_str(), "Acme Prod");
    }

    #[test]
    fn deserializes_via_validator() {
        let json = r#""Acme Prod – East""#;
        let n: ClientName = serde_json::from_str(json).unwrap();
        assert_eq!(n.as_str(), "Acme Prod – East");
        let bad = r#""""#;
        assert!(serde_json::from_str::<ClientName>(bad).is_err());
    }

    #[test]
    fn client_id_roundtrips_display_and_fromstr() {
        let id = ClientId::new();
        let s = id.to_string();
        assert_eq!(s.len(), 26, "ULID canonical form is 26 chars");
        let parsed: ClientId = s.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn client_id_rejects_bad_string() {
        assert!("not-a-ulid".parse::<ClientId>().is_err());
    }

    #[test]
    fn client_id_serde_transparent_roundtrip() {
        let id = ClientId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: ClientId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn client_id_is_unique_and_sortable() {
        let a = ClientId::new();
        let b = ClientId::new();
        assert_ne!(a, b);
        // ULIDs minted later sort >= earlier ones (monotonic-ish by time).
        let mut v = [b, a];
        v.sort();
        assert!(v[0] <= v[1]);
    }

    #[test]
    fn request_id_is_unique() {
        let a = RequestId::new();
        let b = RequestId::new();
        assert_ne!(a, b);
    }
}
