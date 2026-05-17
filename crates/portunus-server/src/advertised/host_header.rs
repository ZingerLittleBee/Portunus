//! HTTP `Host` header → bare host extraction for tier-3 auto-derive.
//!
//! Strips the optional *browser* port and discards it (the control-plane
//! port comes from the server's resolved `control_listen`). Rejects
//! scheme/path/userinfo/whitespace/IPv6-literal headers (→ tier 3 skipped).

/// Build `host:control_port` from a raw `Host` header, or `None` if the
/// header is unusable for tier-3 derivation.
#[must_use]
pub fn host_from_header(raw: &str, control_port: u16) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 255 {
        return None;
    }
    if raw.contains("://")
        || raw.contains('/')
        || raw.contains('@')
        || raw.contains('[')
        || raw.contains(']')
        || raw.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    // Strip optional browser port.
    let host = match raw.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => h,
        Some(_) => return None,
        None => raw,
    };
    if host.is_empty() {
        return None;
    }
    let candidate = format!("{host}:{control_port}");
    // Must satisfy the full authority grammar.
    crate::advertised::grammar::validate_authority(&candidate).ok()?;
    Some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_browser_port_and_appends_control_port() {
        assert_eq!(
            host_from_header("localhost:5173", 7443).as_deref(),
            Some("localhost:7443")
        );
        assert_eq!(
            host_from_header("public.example:443", 7443).as_deref(),
            Some("public.example:7443")
        );
        assert_eq!(
            host_from_header("public.example", 7443).as_deref(),
            Some("public.example:7443")
        );
    }

    #[test]
    fn rejects_unusable_headers() {
        for bad in [
            "",
            "http://x",
            "x/y",
            "user@x",
            "[::1]:443",
            "x y",
            "x:",
            "x:bad",
        ] {
            assert_eq!(host_from_header(bad, 7443), None, "should reject {bad:?}");
        }
    }
}
