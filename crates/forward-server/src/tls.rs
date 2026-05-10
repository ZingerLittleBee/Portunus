//! Server-side TLS material: self-signed cert generation, PEM I/O, and
//! key-permission enforcement. The leaf cert's SHA-256 fingerprint is what
//! clients pin (see `forward-client/src/pinned_verifier.rs`).
//!
//! Wired into `serve` in T035; until then `dead_code` lints would fire.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use forward_core::ForwardError;
use forward_core::fingerprint;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256, SanType,
};

/// Material loaded (or freshly generated) for the TLS listener.
#[derive(Debug, Clone)]
pub struct ServerTlsMaterial {
    pub cert_pem: String,
    pub key_pem: String,
    /// SHA-256 of the leaf cert DER, lowercase hex (64 chars). What clients pin.
    pub leaf_fingerprint_hex: String,
}

impl ServerTlsMaterial {
    /// Load from disk, or generate fresh material if either file is missing.
    /// Refuses to start if `key_path` exists with permissions broader than 0600.
    pub fn load_or_generate(cert_path: &Path, key_path: &Path) -> Result<Self, ForwardError> {
        if cert_path.exists() && key_path.exists() {
            enforce_key_perms(key_path)?;
            let cert_pem = std::fs::read_to_string(cert_path)?;
            let key_pem = std::fs::read_to_string(key_path)?;
            let der = leaf_der_from_pem(&cert_pem)?;
            Ok(Self {
                leaf_fingerprint_hex: fingerprint::sha256_hex(&der),
                cert_pem,
                key_pem,
            })
        } else {
            let material = generate_self_signed()?;
            ensure_parent(cert_path)?;
            ensure_parent(key_path)?;
            std::fs::write(cert_path, &material.cert_pem)?;
            write_secret(key_path, material.key_pem.as_bytes())?;
            Ok(material)
        }
    }
}

fn ensure_parent(p: &Path) -> std::io::Result<()> {
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn write_secret(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(body)?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_secret(path: &Path, body: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, body)
}

#[cfg(unix)]
fn enforce_key_perms(path: &Path) -> Result<(), ForwardError> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path)?;
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(ForwardError::Tls(format!(
            "{} has mode {mode:o}, refuse to start (must be ≤ 0600)",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn enforce_key_perms(_: &Path) -> Result<(), ForwardError> {
    Ok(())
}

fn generate_self_signed() -> Result<ServerTlsMaterial, ForwardError> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    let hostname = hostname().unwrap_or_else(|| "portunus-server".to_string());
    let mut params = CertificateParams::default();
    params.not_before = time::OffsetDateTime::now_utc();
    params.not_after = params.not_before + time::Duration::days(365 * 10);
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, hostname.clone());
    // SANs: webpki refuses certs with no SAN, so we ship a minimal set covering
    // the hostname and loopback addresses (devs and tests dial `127.0.0.1`).
    // Operators with a routable hostname can override the cert in
    // `<config_dir>/server.crt`.
    params.subject_alt_names = vec![
        SanType::DnsName(
            hostname
                .clone()
                .try_into()
                .map_err(|e| ForwardError::Tls(format!("hostname not valid for SAN: {e}")))?,
        ),
        SanType::DnsName("localhost".try_into().unwrap()),
        SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        SanType::IpAddress(IpAddr::V6(Ipv6Addr::LOCALHOST)),
    ];

    let key_pair = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .map_err(|e| ForwardError::Tls(e.to_string()))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| ForwardError::Tls(e.to_string()))?;
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();
    let der = cert.der().to_vec();
    Ok(ServerTlsMaterial {
        leaf_fingerprint_hex: fingerprint::sha256_hex(&der),
        cert_pem,
        key_pem,
    })
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME").ok().or_else(|| {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| {
                let s = String::from_utf8(o.stdout).ok()?;
                let trimmed = s.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            })
    })
}

fn leaf_der_from_pem(pem: &str) -> Result<Vec<u8>, ForwardError> {
    use base64::Engine as _;
    let mut in_block = false;
    let mut buf = String::new();
    for line in pem.lines() {
        let line = line.trim();
        if line == "-----BEGIN CERTIFICATE-----" {
            in_block = true;
            buf.clear();
            continue;
        }
        if line == "-----END CERTIFICATE-----" {
            return base64::engine::general_purpose::STANDARD
                .decode(buf.trim())
                .map_err(|e| ForwardError::Tls(format!("base64 decode: {e}")));
        }
        if in_block {
            buf.push_str(line);
        }
    }
    Err(ForwardError::Tls("no CERTIFICATE block in PEM".into()))
}

#[allow(dead_code)]
pub fn fingerprint_paths(cert_path: &Path) -> Result<(PathBuf, String), ForwardError> {
    let pem = std::fs::read_to_string(cert_path)?;
    let der = leaf_der_from_pem(&pem)?;
    Ok((cert_path.to_path_buf(), fingerprint::sha256_hex(&der)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_generate_then_load_matches_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("server.crt");
        let key = dir.path().join("server.key");

        let m1 = ServerTlsMaterial::load_or_generate(&cert, &key).unwrap();
        // Both files must now exist.
        assert!(cert.exists());
        assert!(key.exists());

        let m2 = ServerTlsMaterial::load_or_generate(&cert, &key).unwrap();
        assert_eq!(m1.leaf_fingerprint_hex, m2.leaf_fingerprint_hex);
        assert_eq!(m1.cert_pem, m2.cert_pem);
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("server.crt");
        let key = dir.path().join("server.key");
        ServerTlsMaterial::load_or_generate(&cert, &key).unwrap();
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got mode {mode:o}");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_load_with_loose_key_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("server.crt");
        let key = dir.path().join("server.key");
        ServerTlsMaterial::load_or_generate(&cert, &key).unwrap();
        // Loosen the permissions.
        let mut perms = std::fs::metadata(&key).unwrap().permissions();
        perms.set_mode(0o640);
        std::fs::set_permissions(&key, perms).unwrap();
        let err = ServerTlsMaterial::load_or_generate(&cert, &key).unwrap_err();
        match err {
            ForwardError::Tls(msg) => assert!(msg.contains("mode")),
            other => panic!("expected Tls error, got {other:?}"),
        }
    }

    #[test]
    fn fingerprint_is_64_hex_chars() {
        let dir = tempfile::tempdir().unwrap();
        let m = ServerTlsMaterial::load_or_generate(
            &dir.path().join("c.crt"),
            &dir.path().join("c.key"),
        )
        .unwrap();
        assert_eq!(m.leaf_fingerprint_hex.len(), 64);
        assert!(
            m.leaf_fingerprint_hex
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        );
    }
}
