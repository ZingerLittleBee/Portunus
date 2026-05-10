//! Newtype wrappers for the IDs that flow through the workspace.
//!
//! All construction routes through validators so a malformed `ClientName`
//! cannot reach the auth seam, the rule store, or a log line.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use ulid::Ulid;

/// Operator-supplied client identifier. DNS-label shape.
///
/// Constraints (per `data-model.md` and FR-003):
/// - 1..=63 chars
/// - lowercase ASCII alphanumeric + hyphen
/// - first and last char alphanumeric
/// - no consecutive hyphens
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ClientName(String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ClientNameError {
    #[error("client name must be 1..=63 chars, got {0}")]
    BadLength(usize),
    #[error("client name must start and end with [a-z0-9]")]
    BadEdge,
    #[error("client name may only contain [a-z0-9-]")]
    BadChar,
    #[error("client name may not contain consecutive hyphens")]
    DoubleHyphen,
}

impl ClientName {
    pub fn new(s: impl Into<String>) -> Result<Self, ClientNameError> {
        let s = s.into();
        let bytes = s.as_bytes();
        if bytes.is_empty() || bytes.len() > 63 {
            return Err(ClientNameError::BadLength(bytes.len()));
        }
        let edge_ok = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
        if !edge_ok(bytes[0]) || !edge_ok(bytes[bytes.len() - 1]) {
            return Err(ClientNameError::BadEdge);
        }
        let mut prev_hyphen = false;
        for &b in bytes {
            match b {
                b'a'..=b'z' | b'0'..=b'9' => prev_hyphen = false,
                b'-' => {
                    if prev_hyphen {
                        return Err(ClientNameError::DoubleHyphen);
                    }
                    prev_hyphen = true;
                }
                _ => return Err(ClientNameError::BadChar),
            }
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
    fn accepts_dns_label_shapes() {
        for ok in ["edge-01", "a", "1", "edge", "node-1-2-3", "abc123"] {
            ClientName::new(ok).unwrap_or_else(|e| panic!("{ok} should parse: {e}"));
        }
    }

    #[test]
    fn rejects_bad_shapes() {
        let cases = [
            ("", ClientNameError::BadLength(0)),
            ("Edge-01", ClientNameError::BadEdge),
            ("eDge-01", ClientNameError::BadChar),
            ("-edge", ClientNameError::BadEdge),
            ("edge-", ClientNameError::BadEdge),
            ("edge--01", ClientNameError::DoubleHyphen),
            ("edge_01", ClientNameError::BadChar),
            ("edge.01", ClientNameError::BadChar),
        ];
        for (input, expected) in cases {
            assert_eq!(
                ClientName::new(input).unwrap_err(),
                expected,
                "input={input}"
            );
        }
    }

    #[test]
    fn rejects_overlong() {
        let s: String = "a".repeat(64);
        assert!(matches!(
            ClientName::new(&s).unwrap_err(),
            ClientNameError::BadLength(64)
        ));
    }

    #[test]
    fn accepts_max_length() {
        let s: String = "a".repeat(63);
        assert!(ClientName::new(&s).is_ok());
    }

    #[test]
    fn from_str_works() {
        let n: ClientName = "edge-01".parse().unwrap();
        assert_eq!(n.as_str(), "edge-01");
    }

    #[test]
    fn deserializes_via_validator() {
        let json = r#""edge-01""#;
        let n: ClientName = serde_json::from_str(json).unwrap();
        assert_eq!(n.as_str(), "edge-01");
        let bad = r#""Edge_01""#;
        assert!(serde_json::from_str::<ClientName>(bad).is_err());
    }

    #[test]
    fn request_id_is_unique() {
        let a = RequestId::new();
        let b = RequestId::new();
        assert_ne!(a, b);
    }
}
