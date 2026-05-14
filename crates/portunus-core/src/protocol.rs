//! Authoritative `Protocol` enum used by all crates in the workspace.
//! Phase 1 of the standalone-forwarder spec; replaces the per-crate
//! `Protocol` types in `portunus-proto`, `portunus-server`, and the
//! data-plane modules.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown protocol {0:?}; expected one of: tcp, udp")]
pub struct ParseProtocolError(pub String);

impl FromStr for Protocol {
    type Err = ParseProtocolError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tcp" => Ok(Self::Tcp),
            "udp" => Ok(Self::Udp),
            other => Err(ParseProtocolError(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn serde_round_trips_lowercase() {
        let cases = [(Protocol::Tcp, "\"tcp\""), (Protocol::Udp, "\"udp\"")];
        for (p, json) in cases {
            assert_eq!(serde_json::to_string(&p).unwrap(), json);
            assert_eq!(serde_json::from_str::<Protocol>(json).unwrap(), p);
        }
    }

    #[test]
    fn from_str_accepts_lowercase_only() {
        assert_eq!(Protocol::from_str("tcp").unwrap(), Protocol::Tcp);
        assert_eq!(Protocol::from_str("udp").unwrap(), Protocol::Udp);
        assert!(Protocol::from_str("TCP").is_err());
        assert!(Protocol::from_str("http").is_err());
    }

    #[test]
    fn display_matches_serde_repr() {
        assert_eq!(Protocol::Tcp.to_string(), "tcp");
        assert_eq!(Protocol::Udp.to_string(), "udp");
    }
}
