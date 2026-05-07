//! Operator-facing surfaces.
//!
//! - [`cli`] holds in-process handlers used both by the `forward-server`
//!   subcommands and (via [`http`]) by the loopback HTTP API.
//! - [`http`] mounts a thin axum wrapper exposing the same operations on
//!   `operator_http_listen` (default `127.0.0.1:7080`).

pub mod cli;
pub mod http;
pub mod per_port_stats;
pub mod rule_cli;

use serde::Serialize;

use forward_core::ClientName;

#[derive(Debug, Serialize)]
pub struct ClientView {
    pub client_name: ClientName,
    pub provisioned_at: chrono::DateTime<chrono::Utc>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub connected: bool,
    pub remote_addr: Option<String>,
    pub connected_at: Option<chrono::DateTime<chrono::Utc>>,
}
