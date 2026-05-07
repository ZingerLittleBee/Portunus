//! Discriminated classification of a rule's target host string.
//! Spec: `003-domain-name-forward` `data-model.md` § `Target`.
//!
//! Disambiguation order matches spec § Edge Cases:
//!   1. `Ipv4Addr` parse → `Target::Ip(V4)`
//!   2. bracketed `[…]` → `Ipv6Addr` parse → `Target::Ip(V6)`
//!   3. fall through to RFC 1123 hostname validator → `Target::Dns`
//!
//! Bare unbracketed IPv6 in an operator-supplied host string is
//! rejected here so callers don't have to guess where the port ends
//! and the address begins.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use thiserror::Error;

use crate::hostname::{Hostname, HostnameError};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target {
    Ip(IpAddr),
    Dns(Hostname),
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum TargetError {
    #[error("invalid_target_host: bare unbracketed IPv6 literal {0:?} (use [...])")]
    UnbracketedIpv6(String),

    #[error("invalid_target_host: bracketed value {0:?} is not a valid IPv6 literal")]
    BracketedNotIpv6(String),

    #[error(transparent)]
    Hostname(#[from] HostnameError),
}

impl Target {
    pub fn parse(input: &str) -> Result<Self, TargetError> {
        if let Ok(v4) = input.parse::<Ipv4Addr>() {
            return Ok(Self::Ip(IpAddr::V4(v4)));
        }

        if let Some(inner) = input.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            return inner
                .parse::<Ipv6Addr>()
                .map(|v6| Self::Ip(IpAddr::V6(v6)))
                .map_err(|_| TargetError::BracketedNotIpv6(input.to_string()));
        }

        // Heuristic: an unbracketed colon is almost certainly an IPv6
        // address the operator forgot to bracket. Reject before the
        // hostname validator (which would reject ':' as an invalid
        // char anyway) so the error message points at the real fix.
        if input.contains(':') {
            return Err(TargetError::UnbracketedIpv6(input.to_string()));
        }

        let host = Hostname::new(input)?;
        Ok(Self::Dns(host))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ipv4_literal() {
        let t = Target::parse("127.0.0.1").unwrap();
        assert_eq!(t, Target::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)));
    }

    #[test]
    fn parses_bracketed_ipv6() {
        let t = Target::parse("[::1]").unwrap();
        assert_eq!(t, Target::Ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn parses_bracketed_full_ipv6() {
        let t = Target::parse("[2001:db8::1]").unwrap();
        match t {
            Target::Ip(IpAddr::V6(_)) => {}
            other @ (Target::Ip(IpAddr::V4(_)) | Target::Dns(_)) => {
                panic!("expected IPv6, got {other:?}")
            }
        }
    }

    #[test]
    fn parses_hostname() {
        let t = Target::parse("api.example.com").unwrap();
        match t {
            Target::Dns(h) => assert_eq!(h.as_str(), "api.example.com"),
            other @ Target::Ip(_) => panic!("expected DNS, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unbracketed_ipv6() {
        let err = Target::parse("::1").unwrap_err();
        assert!(matches!(err, TargetError::UnbracketedIpv6(_)));

        let err = Target::parse("2001:db8::1").unwrap_err();
        assert!(matches!(err, TargetError::UnbracketedIpv6(_)));
    }

    #[test]
    fn rejects_bracketed_garbage() {
        let err = Target::parse("[not-an-ip]").unwrap_err();
        assert!(matches!(err, TargetError::BracketedNotIpv6(_)));
    }

    #[test]
    fn rejects_invalid_hostname() {
        let err = Target::parse("foo_bar.example").unwrap_err();
        assert!(matches!(
            err,
            TargetError::Hostname(HostnameError::InvalidChar { .. })
        ));
    }

    #[test]
    fn all_numeric_is_not_a_hostname() {
        // "12345" doesn't parse as Ipv4Addr (it has no dots), so it
        // would otherwise reach the hostname validator. The validator
        // rejects all-numeric inputs so the IP-literal classifier
        // remains the only owner of numeric forms.
        let err = Target::parse("12345").unwrap_err();
        assert!(matches!(
            err,
            TargetError::Hostname(HostnameError::AllNumeric(_))
        ));
    }
}
