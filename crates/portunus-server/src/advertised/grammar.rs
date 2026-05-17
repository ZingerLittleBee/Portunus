//! Authority grammar validation (host:port, no scheme/path/userinfo/IPv6).
//!
//! The value is simultaneously a URI authority AND the client's TLS
//! verification domain, so it is validated strictly: exactly `host:port`,
//! host is RFC-1123 DNS or IPv4 (IPv6 literals rejected so the client's
//! `rsplit_once(':')` host parser stays correct), port 1..=65535,
//! length ≤ 255, no scheme/path/query/fragment/userinfo/whitespace/control.

/// Returns the validated `(host, port)` borrowed from `s`.
///
/// # Errors
/// Returns a human-readable reason string on any grammar violation.
pub fn validate_authority(s: &str) -> Result<(&str, u16), String> {
    if s.is_empty() {
        return Err("empty".into());
    }
    if s.len() > 255 {
        return Err("too long (> 255)".into());
    }
    if s.contains("://") {
        return Err("must not contain a scheme".into());
    }
    if s.contains('/') {
        return Err("must not contain a path".into());
    }
    if s.contains('?') || s.contains('#') {
        return Err("must not contain query/fragment".into());
    }
    if s.contains('@') {
        return Err("must not contain userinfo".into());
    }
    if s.contains('[') || s.contains(']') {
        return Err("IPv6 literals are not supported".into());
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("must not contain whitespace/control characters".into());
    }
    let (host, port_str) = s.rsplit_once(':').ok_or("missing :port")?;
    if host.is_empty() {
        return Err("empty host".into());
    }
    let port: u16 = port_str
        .parse()
        .map_err(|_| "port must be a decimal 1..=65535".to_string())?;
    if port == 0 {
        return Err("port must be 1..=65535".into());
    }
    if !is_ipv4(host) && !is_rfc1123_hostname(host) {
        return Err("host must be an RFC-1123 hostname or IPv4 address".into());
    }
    Ok((host, port))
}

fn is_ipv4(host: &str) -> bool {
    host.parse::<std::net::Ipv4Addr>().is_ok()
}

fn is_rfc1123_hostname(host: &str) -> bool {
    if host.len() > 253 {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_dns_and_ipv4() {
        assert_eq!(
            validate_authority("public.example:7443").unwrap(),
            ("public.example", 7443)
        );
        assert_eq!(
            validate_authority("127.0.0.1:443").unwrap(),
            ("127.0.0.1", 443)
        );
        assert_eq!(validate_authority("localhost:1").unwrap(), ("localhost", 1));
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "host-only",
            "https://x:7443",
            "x/y:7443",
            "user@x:7443",
            "[::1]:7443",
            "x:bad",
            "x:0",
            "x:70000",
            "x y:7443",
            "x:7443?q=1",
            &"a".repeat(300),
        ] {
            assert!(validate_authority(bad).is_err(), "should reject {bad:?}");
        }
    }
}
