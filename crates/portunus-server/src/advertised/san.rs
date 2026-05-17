//! Certificate SAN extraction + webpki-equivalent name matching.
//!
//! Matching mirrors the client's rustls/webpki verifier:
//! - DNS SAN: ASCII-lowercase exact match.
//! - Wildcard `*.example.com`: matches exactly one leftmost label
//!   (`a.example.com`), NOT `a.b.example.com` and NOT bare `example.com`.
//! - IPv4 host: matched only against IP SAN entries (never DNS SAN).
//!   IPv6 is impossible here — grammar rejects IPv6 endpoint hosts.

use std::net::IpAddr;

use x509_parser::prelude::*;

#[derive(Debug, Clone, Default)]
pub struct CertSanSet {
    dns: Vec<String>,
    ips: Vec<IpAddr>,
}

impl CertSanSet {
    /// Parse the leaf (first) certificate from a PEM bundle and collect
    /// its SAN DNS names + IP addresses.
    ///
    /// # Errors
    /// Returns a reason string if no PEM cert is found or parsing fails.
    pub fn from_pem(pem: &str) -> Result<Self, String> {
        let (_, pem_block) =
            parse_x509_pem(pem.as_bytes()).map_err(|e| format!("pem parse: {e}"))?;
        let (_, cert) = X509Certificate::from_der(&pem_block.contents)
            .map_err(|e| format!("der parse: {e}"))?;
        let mut dns = Vec::new();
        let mut ips = Vec::new();
        if let Ok(Some(san)) = cert.subject_alternative_name() {
            for name in &san.value.general_names {
                match name {
                    GeneralName::DNSName(d) => dns.push(d.to_ascii_lowercase()),
                    GeneralName::IPAddress(bytes) => {
                        if let Some(ip) = bytes_to_ip(bytes) {
                            ips.push(ip);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(Self { dns, ips })
    }

    /// webpki-equivalent coverage check for a bare host (DNS or IPv4).
    #[must_use]
    pub fn covers(&self, host: &str) -> bool {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return self.ips.contains(&ip);
        }
        let host = host.to_ascii_lowercase();
        self.dns.iter().any(|san| dns_matches(san, &host))
    }
}

fn bytes_to_ip(bytes: &[u8]) -> Option<IpAddr> {
    match bytes.len() {
        4 => {
            let a: [u8; 4] = bytes.try_into().ok()?;
            Some(IpAddr::from(a))
        }
        16 => {
            let a: [u8; 16] = bytes.try_into().ok()?;
            Some(IpAddr::from(a))
        }
        _ => None,
    }
}

/// `san` is already ASCII-lowercased; `host` too.
fn dns_matches(san: &str, host: &str) -> bool {
    if let Some(suffix) = san.strip_prefix("*.") {
        // Wildcard matches exactly one leftmost label.
        match host.split_once('.') {
            Some((label, rest)) => !label.is_empty() && rest == suffix,
            None => false,
        }
    } else {
        san == host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Self-signed leaf with SAN: DNS:public.example, DNS:*.wild.example,
    // DNS:localhost, IP:127.0.0.1. Generated once with openssl; pasted as a
    // fixture so the test has no runtime cert-gen dependency.
    const FIXTURE_PEM: &str = include_str!("testdata/san_fixture.pem");

    fn set() -> CertSanSet {
        CertSanSet::from_pem(FIXTURE_PEM).expect("parse fixture")
    }

    #[test]
    fn exact_dns_case_insensitive() {
        assert!(set().covers("public.example"));
        assert!(set().covers("PUBLIC.EXAMPLE"));
        assert!(set().covers("localhost"));
    }

    #[test]
    fn wildcard_single_label_only() {
        let s = set();
        assert!(s.covers("a.wild.example"));
        assert!(!s.covers("a.b.wild.example"));
        assert!(!s.covers("wild.example"));
    }

    #[test]
    fn ipv4_matches_ip_san_only() {
        let s = set();
        assert!(s.covers("127.0.0.1"));
        assert!(!s.covers("127.0.0.2"));
    }

    #[test]
    fn miss_is_uncovered() {
        assert!(!set().covers("not.in.cert"));
    }
}
