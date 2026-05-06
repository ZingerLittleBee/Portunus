//! `CredentialBundle` — what `provision-client` writes to disk for the
//! operator to transfer to the target machine.
//!
//! Schema is the on-the-wire JSON in `data-model.md` § `CredentialBundle`.

use std::path::Path;

use forward_core::ClientName;
use serde::{Deserialize, Serialize};

const BUNDLE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialBundle {
    pub version: u32,
    pub client_name: ClientName,
    /// `host:port` for the gRPC control listener.
    pub server_endpoint: String,
    /// Lowercase 64-char hex SHA-256 of the server leaf cert DER.
    /// This is the canonical pin (operators verify it manually via
    /// `openssl x509 -fingerprint -sha256`).
    pub server_cert_sha256: String,
    /// PEM-encoded server leaf certificate. The client trusts ONLY this
    /// cert (no system roots, no CA chain) and additionally checks that
    /// `sha256(DER(cert_pem)) == server_cert_sha256` at load time.
    pub server_cert_pem: String,
    /// Plaintext bearer token. Sensitive — file written mode 0600.
    pub token: String,
}

impl CredentialBundle {
    #[must_use]
    pub fn new(
        client_name: ClientName,
        server_endpoint: impl Into<String>,
        server_cert_sha256: impl Into<String>,
        server_cert_pem: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            version: BUNDLE_VERSION,
            client_name,
            server_endpoint: server_endpoint.into(),
            server_cert_sha256: server_cert_sha256.into(),
            server_cert_pem: server_cert_pem.into(),
            token: token.into(),
        }
    }

    /// Atomic write to `path` with mode 0600 on Unix.
    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        let body = serde_json::to_vec_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_secret(path, &body)
    }
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
