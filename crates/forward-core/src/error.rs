//! Error taxonomy used across the workspace.
//!
//! These variants map 1:1 to the operator-API exit codes documented in
//! `contracts/operator-api.md`. Adding a variant is a contract change.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ForwardError {
    #[error("client_already_exists: {0}")]
    ClientAlreadyExists(String),

    #[error("client_not_connected: {0}")]
    ClientNotConnected(String),

    #[error("rule_not_found: {0}")]
    RuleNotFound(u64),

    #[error("port_in_use: client={client} port={port}")]
    PortInUse { client: String, port: u16 },

    #[error("activation_failed: {reason}")]
    ActivationFailed { reason: String },

    #[error("auth_failed: {reason}")]
    AuthFailed { reason: String },

    #[error("invalid_client_name: {0}")]
    InvalidClientName(String),

    #[error("config_invalid: {0}")]
    ConfigInvalid(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("tls: {0}")]
    Tls(String),
}
