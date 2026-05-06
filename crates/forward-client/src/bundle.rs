//! `CredentialBundle` reader (mirror of `forward-server/src/bundle.rs`).
//!
//! The same JSON schema is consumed here. We do NOT pull `forward-server`
//! into the client's compile graph — duplicating the small struct keeps the
//! two binaries decoupled.

use std::path::Path;

use forward_core::ClientName;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct CredentialBundle {
    #[serde(default = "default_version")]
    pub version: u32,
    pub client_name: ClientName,
    pub server_endpoint: String,
    pub server_cert_sha256: String,
    pub server_cert_pem: String,
    pub token: String,
}

fn default_version() -> u32 {
    1
}

impl CredentialBundle {
    pub fn read_from(path: &Path) -> std::io::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let bundle: Self = serde_json::from_str(&raw).map_err(std::io::Error::other)?;
        if bundle.version != 1 {
            return Err(std::io::Error::other(format!(
                "unsupported bundle version: {}",
                bundle.version
            )));
        }
        bundle.verify_pin_consistency()?;
        Ok(bundle)
    }

    /// Confirm `sha256(DER(server_cert_pem)) == server_cert_sha256`. A bundle
    /// that fails this check is corrupt or maliciously assembled — fail
    /// loudly rather than dialling out under a forged pin.
    fn verify_pin_consistency(&self) -> std::io::Result<()> {
        let der = leaf_der_from_pem(&self.server_cert_pem)?;
        let computed = forward_core::fingerprint::sha256_hex(&der);
        if !computed.eq_ignore_ascii_case(&self.server_cert_sha256) {
            return Err(std::io::Error::other(format!(
                "bundle pin mismatch: cert_pem hashes to {computed}, bundle says {}",
                self.server_cert_sha256
            )));
        }
        Ok(())
    }
}

fn leaf_der_from_pem(pem: &str) -> std::io::Result<Vec<u8>> {
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
                .map_err(std::io::Error::other);
        }
        if in_block {
            buf.push_str(line);
        }
    }
    Err(std::io::Error::other("no CERTIFICATE block in PEM"))
}
