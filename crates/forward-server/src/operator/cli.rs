//! In-process operator handlers — used by the CLI subcommands and reused
//! by the loopback HTTP API.
//!
//! These functions are intentionally synchronous (file I/O + lock-protected
//! in-memory state) where possible, with `async` only where they reach into
//! tokio-aware structures.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use forward_auth::{AuthError, Authenticator};
use forward_core::{ClientName, ClientNameError, RequestId, RuleId};
use forward_proto::v1::{
    ActivationOutcome, Protocol as ProtoProto, Rule as ProtoRule, RuleAction, RuleUpdate,
    ServerMessage, server_message,
};
use thiserror::Error;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::bundle::CredentialBundle;
use crate::operator::ClientView;
use crate::rules::{Protocol, Rule, RuleStoreError};
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
    #[error("client_not_connected: {0}")]
    ClientNotConnected(ClientName),
    #[error("port_in_use")]
    PortInUse,
    #[error("activation_failed: {0}")]
    ActivationFailed(String),
    #[error("ack_timeout")]
    AckTimeout,
    #[error("rule_not_found")]
    RuleNotFound,
    #[error("invalid_protocol: {0}")]
    InvalidProtocol(String),
    #[error("invalid_target: {0}")]
    InvalidTarget(String),
}

impl OperatorError {
    /// Maps to operator-api.md frozen exit codes.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::ClientAlreadyExists(_) | Self::Auth(AuthError::ClientAlreadyExists(_)) => 2,
            Self::InvalidName(_) | Self::InvalidProtocol(_) | Self::InvalidTarget(_) => 3,
            Self::ClientNotConnected(_) => 4,
            Self::PortInUse => 5,
            Self::ActivationFailed(_) => 6,
            Self::AckTimeout => 7,
            Self::RuleNotFound => 8,
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
            Self::InvalidProtocol(_) => "invalid_protocol",
            Self::InvalidTarget(_) => "invalid_target",
            Self::ClientNotConnected(_) => "client_not_connected",
            Self::PortInUse => "port_in_use",
            Self::ActivationFailed(_) => "activation_failed",
            Self::AckTimeout => "ack_timeout",
            Self::RuleNotFound => "rule_not_found",
            Self::Io(_) => "io_error",
            Self::Auth(_) => "auth_error",
        }
    }
}

impl From<RuleStoreError> for OperatorError {
    fn from(e: RuleStoreError) -> Self {
        match e {
            RuleStoreError::PortInUse => Self::PortInUse,
            RuleStoreError::NotFound => Self::RuleNotFound,
            RuleStoreError::InvalidTransition => Self::ActivationFailed("invalid_state".into()),
        }
    }
}

/// Issue a fresh bearer token for `raw_name` and assemble the credential
/// bundle, without touching the filesystem.
///
/// Used directly by the HTTP `POST /v1/clients` handler, which only needs
/// the bundle in the response body. The CLI path goes through
/// [`provision_client`] which additionally writes the bundle to disk.
pub fn issue_bundle(
    state: &AppState,
    raw_name: &str,
) -> Result<(ClientName, CredentialBundle), OperatorError> {
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
    info!(
        event = "audit.provision",
        outcome = "success",
        client_name = %name,
    );
    Ok((name, bundle))
}

/// `provision-client <name> [--out path]`.
///
/// Returns the `(bundle_path, bundle)` pair on success. The bundle file is
/// written atomically with mode 0600. When `out` is `None`, the file is
/// written to `<cwd>/<name>.bundle.json`.
pub fn provision_client(
    state: &AppState,
    raw_name: &str,
    out: Option<PathBuf>,
) -> Result<(PathBuf, CredentialBundle), OperatorError> {
    let (name, bundle) = issue_bundle(state, raw_name)?;
    let path = out.unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(format!("{name}.bundle.json"))
    });
    bundle.write_to(&path)?;
    info!(
        event = "audit.provision_written",
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

fn parse_protocol(s: &str) -> Result<Protocol, OperatorError> {
    match s.to_ascii_lowercase().as_str() {
        "tcp" => Ok(Protocol::Tcp),
        other => Err(OperatorError::InvalidProtocol(other.to_string())),
    }
}

/// Parse `host:port` (host may be a DNS name or IP literal). The host is kept
/// as a string and resolved on the client side per `data-model.md`.
pub fn parse_target(spec: &str) -> Result<(String, u16), OperatorError> {
    let (host, port) = spec
        .rsplit_once(':')
        .ok_or_else(|| OperatorError::InvalidTarget(spec.to_string()))?;
    let port: u16 = port
        .parse()
        .map_err(|_| OperatorError::InvalidTarget(spec.to_string()))?;
    if host.is_empty() {
        return Err(OperatorError::InvalidTarget(spec.to_string()));
    }
    Ok((host.to_string(), port))
}

/// `push-rule <client> <listen_port> <target_host>:<target_port>` (FR-009..014).
///
/// Records the rule as `Pending`, sends a `RuleUpdate` with `request_id` to the
/// connected client, and waits up to `ack_timeout` for a matching `RuleStatus`.
/// On success transitions to `Active` and returns the assigned `RuleId`.
#[allow(clippy::too_many_lines)]
pub async fn push_rule(
    state: &AppState,
    raw_client: &str,
    listen_port: u16,
    target: &str,
    protocol: &str,
    ack_timeout: Duration,
) -> Result<Rule, OperatorError> {
    let client_name = ClientName::from_str(raw_client)?;
    let proto = parse_protocol(protocol)?;
    let (target_host, target_port) = parse_target(target)?;

    // Reject up-front if the client isn't connected — saves us from leaving a
    // Pending rule behind that would never be acked.
    let Some((outbound, waiters)) = state.clients.handles(&client_name).await else {
        return Err(OperatorError::ClientNotConnected(client_name));
    };

    let rule = state
        .rules
        .push(
            client_name.clone(),
            listen_port,
            target_host.clone(),
            target_port,
            proto,
        )
        .await?;
    let request_id = RequestId::new().to_string();
    let (tx, rx) = oneshot::channel();
    {
        let mut guard = waiters.lock().await;
        guard.insert(request_id.clone(), tx);
    }

    info!(
        event = "audit.rule_push",
        outcome = "sent",
        request_id = %request_id,
        rule_id = %rule.id,
        client_name = %client_name,
        listen_port = listen_port,
        target = %format!("{target_host}:{target_port}"),
    );

    let update = ServerMessage {
        payload: Some(server_message::Payload::RuleUpdate(RuleUpdate {
            request_id: request_id.clone(),
            action: RuleAction::Push as i32,
            rule: Some(ProtoRule {
                rule_id: rule.id.0,
                listen_port: u32::from(listen_port),
                target_host: target_host.clone(),
                target_port: u32::from(target_port),
                protocol: ProtoProto::Tcp as i32,
            }),
        })),
    };
    if outbound.send(Ok(update)).await.is_err() {
        // Stream torn down between handles() and send — treat like
        // client_not_connected. Drop the pending entry so re-push can succeed
        // after a reconnect.
        let _ = state.rules.remove(rule.id).await;
        let mut guard = waiters.lock().await;
        guard.remove(&request_id);
        return Err(OperatorError::ClientNotConnected(client_name));
    }

    match tokio::time::timeout(ack_timeout, rx).await {
        Ok(Ok(status)) => {
            let outcome = ActivationOutcome::try_from(status.outcome)
                .unwrap_or(ActivationOutcome::Unspecified);
            match outcome {
                ActivationOutcome::Activated => {
                    state.rules.mark_active(rule.id).await?;
                    info!(
                        event = "audit.rule_push",
                        outcome = "activated",
                        request_id = %request_id,
                        rule_id = %rule.id,
                        client_name = %client_name,
                    );
                    state
                        .rules
                        .get(rule.id)
                        .await
                        .ok_or(OperatorError::RuleNotFound)
                }
                ActivationOutcome::Failed => {
                    let reason = if status.reason.is_empty() {
                        "unspecified".to_string()
                    } else {
                        status.reason.clone()
                    };
                    state.rules.mark_failed(rule.id, reason.clone()).await.ok();
                    warn!(
                        event = "audit.rule_push",
                        outcome = "failed",
                        request_id = %request_id,
                        rule_id = %rule.id,
                        client_name = %client_name,
                        reason = %reason,
                    );
                    Err(OperatorError::ActivationFailed(reason))
                }
                _ => Err(OperatorError::ActivationFailed(
                    "unexpected_outcome".to_string(),
                )),
            }
        }
        Ok(Err(_recv_err)) => {
            // Sender dropped — client disconnected mid-flight. Leave the rule
            // in Pending so the operator can list-rules and decide.
            warn!(
                event = "audit.rule_push",
                outcome = "ack_lost",
                request_id = %request_id,
                rule_id = %rule.id,
                client_name = %client_name,
            );
            Err(OperatorError::AckTimeout)
        }
        Err(_elapsed) => {
            // Timeout — clear the waiter to avoid leaking; rule stays Pending.
            let mut guard = waiters.lock().await;
            guard.remove(&request_id);
            warn!(
                event = "audit.rule_push",
                outcome = "ack_timeout",
                request_id = %request_id,
                rule_id = %rule.id,
                client_name = %client_name,
            );
            Err(OperatorError::AckTimeout)
        }
    }
}

/// `remove-rule <rule_id>`. Removes the rule from the store and, if the client
/// is connected, fires a `RuleUpdate{REMOVE}`. The Removed echo is informational
/// (logged in the gRPC service) — operator-api.md says success is "rule gone
/// from the store", not "client confirmed teardown".
pub async fn remove_rule(state: &AppState, rule_id: RuleId) -> Result<Rule, OperatorError> {
    let removed = state.rules.remove(rule_id).await?;
    state
        .stats_cache
        .drop_rule(rule_id, &removed.client_name, &state.metrics)
        .await;
    let request_id = RequestId::new().to_string();
    if let Some((outbound, _waiters)) = state.clients.handles(&removed.client_name).await {
        let update = ServerMessage {
            payload: Some(server_message::Payload::RuleUpdate(RuleUpdate {
                request_id: request_id.clone(),
                action: RuleAction::Remove as i32,
                rule: Some(ProtoRule {
                    rule_id: rule_id.0,
                    listen_port: u32::from(removed.listen_port),
                    target_host: removed.target_host.clone(),
                    target_port: u32::from(removed.target_port),
                    protocol: ProtoProto::Tcp as i32,
                }),
            })),
        };
        if outbound.send(Ok(update)).await.is_err() {
            warn!(
                event = "audit.rule_remove",
                outcome = "client_unreachable",
                request_id = %request_id,
                rule_id = %rule_id,
                client_name = %removed.client_name,
            );
        }
    }
    info!(
        event = "audit.rule_remove",
        outcome = "success",
        request_id = %request_id,
        rule_id = %rule_id,
        client_name = %removed.client_name,
    );
    Ok(removed)
}

/// `rule-stats <rule_id>` (FR-024). Returns the latest cached snapshot fed by
/// the client's `StatsReport` stream. Returns `RuleNotFound` if either the rule
/// store has no record of this id OR no `StatsReport` has arrived yet.
pub async fn rule_stats(
    state: &AppState,
    rule_id: RuleId,
) -> Result<crate::metrics::RuleStatsSnapshot, OperatorError> {
    state
        .stats_cache
        .get(rule_id)
        .await
        .ok_or(OperatorError::RuleNotFound)
}

/// `list-rules [--client <name>]`.
pub async fn list_rules(
    state: &AppState,
    raw_client: Option<&str>,
) -> Result<Vec<Rule>, OperatorError> {
    let filter = match raw_client {
        Some(s) => Some(ClientName::from_str(s)?),
        None => None,
    };
    Ok(state.rules.list(filter.as_ref()).await)
}

#[allow(dead_code)]
pub fn render_rules_text(rules: &[Rule]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{:<6} {:<20} {:<6} {:<32} {:<10}",
        "ID", "CLIENT", "PORT", "TARGET", "STATE"
    );
    for r in rules {
        let state = match &r.state {
            crate::rules::RuleState::Pending => "pending".to_string(),
            crate::rules::RuleState::Active => "active".to_string(),
            crate::rules::RuleState::Failed { reason } => format!("failed:{reason}"),
            crate::rules::RuleState::Removed => "removed".to_string(),
        };
        let _ = writeln!(
            s,
            "{:<6} {:<20} {:<6} {:<32} {:<10}",
            r.id.0,
            r.client_name,
            r.listen_port,
            format!("{}:{}", r.target_host, r.target_port),
            state,
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
