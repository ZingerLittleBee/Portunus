//! In-process operator handlers — used by the CLI subcommands and reused
//! by the loopback HTTP API.
//!
//! These functions are intentionally synchronous (file I/O + lock-protected
//! in-memory state) where possible, with `async` only where they reach into
//! tokio-aware structures.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use forward_auth::{AuthError, Authenticator};
use forward_core::{ClientName, ClientNameError};
use thiserror::Error;
use tracing::info;

use crate::bundle::CredentialBundle;
use crate::operator::ClientView;
use crate::state::AppState;

#[derive(Debug, Error)]
pub enum OperatorError {
    #[error("invalid_name: {0}")]
    InvalidName(#[from] ClientNameError),
    #[error("client_already_exists: {0}")]
    ClientAlreadyExists(ClientName),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("auth: {0}")]
    Auth(#[from] AuthError),
}

impl OperatorError {
    /// Maps to operator-api.md frozen exit codes.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::ClientAlreadyExists(_) | Self::Auth(AuthError::ClientAlreadyExists(_)) => 2,
            Self::InvalidName(_) => 3,
            _ => 1,
        }
    }

    /// Stable machine-readable error code for HTTP responses.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::ClientAlreadyExists(_) | Self::Auth(AuthError::ClientAlreadyExists(_)) => {
                "client_already_exists"
            }
            Self::InvalidName(_) => "invalid_name",
            Self::Io(_) => "io_error",
            Self::Auth(_) => "auth_error",
        }
    }
}

/// `provision-client <name> [--out path]`.
///
/// Returns the `(bundle_path, bundle)` pair on success. The bundle file is
/// written atomically with mode 0600.
pub fn provision_client(
    state: &AppState,
    raw_name: &str,
    out: Option<PathBuf>,
) -> Result<(PathBuf, CredentialBundle), OperatorError> {
    let name = ClientName::from_str(raw_name)?;
    let token = match state.tokens.issue(name.clone()) {
        Ok(t) => t,
        Err(AuthError::ClientAlreadyExists(n)) => {
            return Err(OperatorError::ClientAlreadyExists(n));
        }
        Err(e) => return Err(OperatorError::Auth(e)),
    };
    let bundle = CredentialBundle::new(
        name.clone(),
        state.server_endpoint.clone(),
        state.server_cert_sha256.clone(),
        state.server_cert_pem.clone(),
        token,
    );
    let path = out.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(format!("{name}.bundle.json"))
    });
    bundle.write_to(&path)?;
    info!(
        event = "audit.provision",
        outcome = "success",
        client_name = %name,
        bundle_path = %path.display(),
    );
    Ok((path, bundle))
}

/// `revoke <name>`. Idempotent.
pub async fn revoke(state: &AppState, raw_name: &str) -> Result<(), OperatorError> {
    let name = ClientName::from_str(raw_name)?;
    state.tokens.revoke(&name)?;
    let disconnected = state.clients.disconnect(&name).await;
    info!(
        event = "audit.revoke",
        outcome = "success",
        client_name = %name,
        was_connected = disconnected,
    );
    Ok(())
}

/// `list-clients`. Joins the union of provisioned + currently-connected.
pub async fn list_clients(state: &AppState) -> Vec<ClientView> {
    let provisioned = state.tokens.list();
    let connected = state.clients.snapshot().await;

    let mut views = Vec::with_capacity(provisioned.len());
    for p in provisioned {
        let conn = connected.get(&p.client_name);
        views.push(ClientView {
            client_name: p.client_name.clone(),
            provisioned_at: p.issued_at,
            revoked_at: p.revoked_at,
            connected: conn.is_some(),
            remote_addr: conn.and_then(|c| c.remote_addr.map(|a| a.to_string())),
            connected_at: conn.map(|c| c.connected_at),
        });
    }
    views
}

pub fn render_client_view_text(views: &[ClientView]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{:<32} {:<10} {:<25} REMOTE",
        "CLIENT", "STATE", "PROVISIONED_AT"
    );
    for v in views {
        let state = if v.revoked_at.is_some() {
            "revoked"
        } else if v.connected {
            "connected"
        } else {
            "offline"
        };
        let _ = writeln!(
            s,
            "{:<32} {:<10} {:<25} {}",
            v.client_name,
            state,
            v.provisioned_at.format("%Y-%m-%dT%H:%M:%SZ"),
            v.remote_addr.as_deref().unwrap_or("-"),
        );
    }
    s
}

/// Used by the CLI when no config file exists — synthesises a `ServerConfig`
/// with sensible defaults rooted at `<config_dir>`.
pub fn default_paths(config_dir: &Path) -> DefaultPaths {
    DefaultPaths {
        cert: config_dir.join("server.crt"),
        key: config_dir.join("server.key"),
        tokens: config_dir.join("tokens.json"),
    }
}

#[derive(Debug, Clone)]
pub struct DefaultPaths {
    pub cert: PathBuf,
    pub key: PathBuf,
    pub tokens: PathBuf,
}
