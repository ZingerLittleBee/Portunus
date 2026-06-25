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
        if pem_block.label != "CERTIFICATE" {
            return Err(format!(
                "expected CERTIFICATE PEM block, found {:?}",
                pem_block.label
            ));
        }
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
        // webpki (is_valid_dns_id) requires >=3 labels total, i.e. the
        // wildcard suffix must itself contain a dot. "*.com"/"*.example"
        // are MalformedDnsIdentifier in webpki and never match — mirror that.
        if !suffix.contains('.') {
            return false;
        }
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

    #[test]
    fn wildcard_single_label_suffix_rejected() {
        // webpki MalformedDnsIdentifier: wildcard suffix with no dot.
        assert!(!dns_matches("*.com", "foo.com"));
        assert!(!dns_matches("*.example", "foo.example"));
        // A properly multi-label wildcard still matches.
        assert!(dns_matches("*.wild.example", "a.wild.example"));
    }

    #[test]
    fn absent_san_is_uncovered() {
        // Cert generated with no SAN extension at all.
        const NO_SAN_PEM: &str = include_str!("testdata/san_fixture_no_san.pem");
        let s = CertSanSet::from_pem(NO_SAN_PEM).expect("parse no-san fixture");
        assert!(!s.covers("no-san.example"));
        assert!(!s.covers("127.0.0.1"));
    }

    #[test]
    fn san_case_insensitive_from_cert() {
        // Cert SAN entry is uppercase: DNS:Public.Example.
        const UPPER_PEM: &str = include_str!("testdata/san_fixture_upper.pem");
        let s = CertSanSet::from_pem(UPPER_PEM).expect("parse uppercase fixture");
        assert!(s.covers("public.example"));
        assert!(s.covers("PUBLIC.EXAMPLE"));
    }

    #[test]
    fn from_pem_no_pem_block_errors() {
        // Input has no PEM armor at all -> parse_x509_pem fails.
        let err = CertSanSet::from_pem("not a pem document").expect_err("must fail");
        assert!(err.starts_with("pem parse:"), "unexpected error: {err}");
    }

    #[test]
    fn from_pem_wrong_label_errors() {
        // Valid PEM, but the block label is not CERTIFICATE.
        const KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n";
        let err = CertSanSet::from_pem(KEY_PEM).expect_err("must fail");
        assert!(
            err.contains("expected CERTIFICATE PEM block"),
            "unexpected error: {err}"
        );
        assert!(err.contains("PRIVATE KEY"), "unexpected error: {err}");
    }

    #[test]
    fn from_pem_invalid_der_errors() {
        // Valid CERTIFICATE PEM armor, but the contents are not valid DER.
        const BAD_DER_PEM: &str = "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
        let err = CertSanSet::from_pem(BAD_DER_PEM).expect_err("must fail");
        assert!(err.starts_with("der parse:"), "unexpected error: {err}");
    }

    #[test]
    fn non_dns_non_ip_san_entries_ignored() {
        // Self-signed leaf whose only SAN entries are URI + email, exercising
        // the `_ => {}` arm; it covers no DNS host and no IP.
        const URI_EMAIL_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIDMDCCAhigAwIBAgIUbx4wBLOhz6399aZRd4zKjzGgqSYwDQYJKoZIhvcNAQEL\n\
BQAwDzENMAsGA1UEAwwEdGVzdDAeFw0yNjA2MjUwNTE5MjBaFw0zNjA2MjIwNTE5\n\
MjBaMA8xDTALBgNVBAMMBHRlc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEK\n\
AoIBAQDVzjnI81KAqxK6ReyZhj7EPW4+I35AwNZZ86R7/JhwV+N62teJQFqyD3Sd\n\
+sJREJ+wUDdy55F5IYg8t4fDzMRrCE4EYx/sm4Y5RTU9cmZHVYAzqKiKiuFng4+a\n\
1DPLV/WJPQzhAxH/+MDyc86AW5O5xjvTgW/+MBh6O8KAujbO8Tv8wsM3HWxcZhtC\n\
wupmelI8ekIw9xiNZ3dGAmZwi47AQK6UZ/kmwTqL3Hzkzzaf25ItaK7vka3EUEqJ\n\
kkOZWHVCpNhpS/IL1dHtbbehQjoNIAMQKhBV3p0YF6rR82QEzNCHmXjjKdLm/qgz\n\
9xGTRBiAUG2+90eguEP3IxjskOahAgMBAAGjgYMwgYAwHQYDVR0OBBYEFK0c+LVE\n\
Hbd2HkazauwyoAugBYANMB8GA1UdIwQYMBaAFK0c+LVEHbd2HkazauwyoAugBYAN\n\
MA8GA1UdEwEB/wQFMAMBAf8wLQYDVR0RBCYwJIYTaHR0cHM6Ly9leGFtcGxlLmNv\n\
bYENYUBleGFtcGxlLmNvbTANBgkqhkiG9w0BAQsFAAOCAQEAGFPwmhgKgf5sFI5r\n\
mmv44As96gO3Qa4ALtViu8tnabW5PUd01AnangPGUfoZMbeggMcMQngTtPxwy7vm\n\
a5UjmXipJJXU58cmPCUrqisebaK+zeooGPrNs8ZgWVSQ/35R9mpZe6oMZyi6HhJc\n\
QT4cfAa6YBwttvxPtKEGQxvM3UG7pCRHwgsmQmCvGLLoZ27DpbN2b6neGLex/sdO\n\
XjwU4bXwpYaFyNBSQpY1/0DRdmVxibxvegjoEBtdOQnuGwMu3SnoXvNtK+arlPVm\n\
eLDAUqzTmTKl5lvMJRUVzE22NSL2LuYWJC5bNaVmGoNKxSX4fFFjSu7U0qcRkwLL\n\
dsdg3w==\n\
-----END CERTIFICATE-----\n";
        let s = CertSanSet::from_pem(URI_EMAIL_PEM).expect("parse uri/email fixture");
        assert!(!s.covers("example.com"));
        assert!(!s.covers("a@example.com"));
    }

    #[test]
    fn ipv6_ip_san_is_collected() {
        // Self-signed leaf with a single IP SAN: IP:::1 (16-byte address).
        const IPV6_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
MIIBoTCCAUagAwIBAgIUX76EUIaVgPRAJ0qb71Sk/NzqTvQwCgYIKoZIzj0EAwIw\n\
FzEVMBMGA1UEAwwMaXB2Ni5leGFtcGxlMB4XDTI2MDYyNTA1MTkzNFoXDTM2MDYy\n\
MjA1MTkzNFowFzEVMBMGA1UEAwwMaXB2Ni5leGFtcGxlMFkwEwYHKoZIzj0CAQYI\n\
KoZIzj0DAQcDQgAEekRTUkxJUXGXYtuoawLXAKK+BgO4Vuj3o+7gBbym5pZk/rsN\n\
z4uSuVFNLXMMzmmcFg6nPyTngV8qxnDhDWFc46NwMG4wHQYDVR0OBBYEFG4N6bAU\n\
4baDITWTaEnBeJj2fsUwMB8GA1UdIwQYMBaAFG4N6bAU4baDITWTaEnBeJj2fsUw\n\
MA8GA1UdEwEB/wQFMAMBAf8wGwYDVR0RBBQwEocQAAAAAAAAAAAAAAAAAAAAATAK\n\
BggqhkjOPQQDAgNJADBGAiEAxtUoAIRx4Ure5R31EIfACMHnzoofXCUKB8E4e3va\n\
9ysCIQClX636Nq4/zM1FNNZiAkBC37+O5n/7ra0GEEZZu67o2A==\n\
-----END CERTIFICATE-----\n";
        let s = CertSanSet::from_pem(IPV6_PEM).expect("parse ipv6 fixture");
        assert!(s.covers("::1"));
        assert!(!s.covers("::2"));
        // IPv6 lives in the IP SAN set only, never the DNS set.
        assert!(!s.covers("ipv6.example"));
    }

    #[test]
    fn bytes_to_ip_length_branches() {
        // 4 bytes -> IPv4.
        assert_eq!(
            bytes_to_ip(&[127, 0, 0, 1]),
            Some(IpAddr::from([127, 0, 0, 1]))
        );
        // 16 bytes -> IPv6.
        let v6 = [0u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        assert_eq!(bytes_to_ip(&v6), Some(IpAddr::from(v6)));
        // Any other length -> None.
        assert_eq!(bytes_to_ip(&[1, 2, 3]), None);
        assert_eq!(bytes_to_ip(&[]), None);
    }
}
