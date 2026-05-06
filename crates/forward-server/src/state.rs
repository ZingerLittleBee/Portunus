//! Shared state injected into operator handlers and the gRPC service.

use std::sync::Arc;

use forward_auth::file_store::FileTokenStore;

use crate::clients::ConnectedClients;

#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<FileTokenStore>,
    pub clients: ConnectedClients,
    /// `host:port` advertised in newly-issued credential bundles.
    pub server_endpoint: String,
    /// Lowercase 64-char hex SHA-256 of the server leaf cert DER.
    pub server_cert_sha256: String,
    /// PEM-encoded server leaf certificate (carried in bundles so the
    /// client can trust exactly this cert without a CA chain).
    pub server_cert_pem: String,
}

impl AppState {
    #[must_use]
    pub fn new(
        tokens: Arc<FileTokenStore>,
        clients: ConnectedClients,
        server_endpoint: impl Into<String>,
        server_cert_sha256: impl Into<String>,
        server_cert_pem: impl Into<String>,
    ) -> Self {
        Self {
            tokens,
            clients,
            server_endpoint: server_endpoint.into(),
            server_cert_sha256: server_cert_sha256.into(),
            server_cert_pem: server_cert_pem.into(),
        }
    }
}
