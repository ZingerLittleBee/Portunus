//! Operator-facing surfaces.
//!
//! - [`cli`] holds in-process handlers used both by the `portunus-server`
//!   subcommands and (via [`http`]) by the loopback HTTP API.
//! - [`http`] mounts a thin axum wrapper exposing the same operations on
//!   `operator_http_listen` (default `127.0.0.1:7080`).

pub mod audit;
pub mod audit_http;
pub mod auth_layer;
pub mod bootstrap;
pub mod cli;
pub mod credentials;
pub(crate) mod csrf;
pub mod grants;
pub mod http;
pub mod identity_cli;
pub mod owner_cap;
pub mod owner_cap_cli;
pub mod password_cli;
pub(crate) mod passwords;
pub mod per_port_stats;
pub mod quota_http;
pub mod rbac;
pub mod rule_cli;
pub(crate) mod sessions;
pub(crate) mod setup_token;
pub mod stats_stream;
pub(crate) mod throttle;
pub(crate) mod user_ids;
pub mod users;
pub mod users_me;
pub(crate) mod web_auth;
pub mod webui;

use serde::Serialize;

use portunus_core::ClientName;

pub(crate) fn operator_token_from_env() -> Option<String> {
    ["PORTUNUS_OPERATOR_TOKEN"]
        .into_iter()
        .find_map(|key| std::env::var(key).ok().filter(|s| !s.is_empty()))
}

pub(crate) fn operator_token_missing_message() -> &'static str {
    "error: unauthenticated (set PORTUNUS_OPERATOR_TOKEN env var with the operator bearer token)"
}

#[derive(Debug, Serialize)]
pub struct ClientView {
    pub client_name: ClientName,
    pub provisioned_at: chrono::DateTime<chrono::Utc>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub connected: bool,
    pub client_address: Option<String>,
    pub remote_addr: Option<String>,
    pub connected_at: Option<chrono::DateTime<chrono::Utc>>,
}
