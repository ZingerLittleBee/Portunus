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
use forward_core::{ClientName, ClientNameError, PortRange, PortRangeError, RequestId, RuleId};
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
    /// Display includes the offending port when known so operator
    /// tooling (and the HTTP `error.message` body) can pinpoint the
    /// collision (US4 / 002-port-range-forward T053). The bare
    /// `port_in_use` form is preserved for v0.1.0 callers that didn't
    /// surface a port.
    #[error("{}", format_port_in_use(*offending_port))]
    PortInUse { offending_port: Option<u16> },
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
    /// Range size > server-configured cap (FR-008, 002-port-range-forward).
    #[error("exceeds_cap: requested={requested} cap={cap}")]
    ExceedsCap { requested: u32, cap: u32 },
    /// Range structurally invalid (inverted, length mismatch, etc.).
    /// HTTP maps this to 400 with code `range_inverted` or
    /// `mismatched_range`; CLI maps to exit `3`.
    #[error("range_invalid: {0}")]
    RangeInvalid(String),
}

fn format_port_in_use(offending_port: Option<u16>) -> String {
    match offending_port {
        Some(p) => format!("port_in_use: port {p} already in use"),
        None => "port_in_use".to_string(),
    }
}

impl OperatorError {
    /// Maps to operator-api.md frozen exit codes. New v1.1 error
    /// codes (`exceeds_cap`, `range_invalid`) reuse exit `3` per the
    /// stability guarantee in `operator-api.md`.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::ClientAlreadyExists(_) | Self::Auth(AuthError::ClientAlreadyExists(_)) => 2,
            Self::InvalidName(_)
            | Self::InvalidProtocol(_)
            | Self::InvalidTarget(_)
            | Self::ExceedsCap { .. }
            | Self::RangeInvalid(_) => 3,
            Self::ClientNotConnected(_) => 4,
            Self::PortInUse { .. } => 5,
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
            Self::PortInUse { .. } => "port_in_use",
            Self::ActivationFailed(_) => "activation_failed",
            Self::AckTimeout => "ack_timeout",
            Self::RuleNotFound => "rule_not_found",
            Self::ExceedsCap { .. } => "exceeds_cap",
            Self::RangeInvalid(_) => "range_invalid",
            Self::Io(_) => "io_error",
            Self::Auth(_) => "auth_error",
        }
    }
}

impl From<RuleStoreError> for OperatorError {
    fn from(e: RuleStoreError) -> Self {
        match e {
            RuleStoreError::PortInUse { offending_port } => Self::PortInUse {
                offending_port: Some(offending_port),
            },
            RuleStoreError::NotFound => Self::RuleNotFound,
            RuleStoreError::InvalidTransition => Self::ActivationFailed("invalid_state".into()),
            RuleStoreError::ExceedsCap { requested, cap } => Self::ExceedsCap { requested, cap },
            RuleStoreError::RangeInvalid(e) => Self::RangeInvalid(e.to_string()),
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

/// Helper for the `audit.rule_push` log: emit `listen_port_end` only
/// when the rule is actually a range (size > 1). Single-port rules
/// keep the v0.1.0 log shape (no end field).
fn listen_end_for_log(r: PortRange) -> Option<u16> {
    if r.len() > 1 { Some(r.end()) } else { None }
}

fn parse_protocol(s: &str) -> Result<Protocol, OperatorError> {
    match s.to_ascii_lowercase().as_str() {
        "tcp" => Ok(Protocol::Tcp),
        other => Err(OperatorError::InvalidProtocol(other.to_string())),
    }
}

/// Parse a listen-port arg of either form:
///   * `"18080"` — a single port (returned as `PortRange::single(18080)`)
///   * `"30000-30050"` — a contiguous range (returned as `PortRange::new`)
///
/// Errors map to `OperatorError::InvalidTarget` for the CLI exit-3
/// family.
pub fn parse_listen(spec: &str) -> Result<PortRange, OperatorError> {
    parse_port_range(spec).map_err(|e| match e {
        PortRangeError::Inverted { .. } => OperatorError::RangeInvalid(e.to_string()),
        _ => OperatorError::InvalidTarget(spec.to_string()),
    })
}

fn parse_port_range(spec: &str) -> Result<PortRange, PortRangeError> {
    if let Some((start_s, end_s)) = spec.split_once('-') {
        let start: u16 = start_s.parse().map_err(|_| PortRangeError::OutOfBounds)?;
        let end: u16 = end_s.parse().map_err(|_| PortRangeError::OutOfBounds)?;
        PortRange::new(start, end)
    } else {
        let p: u16 = spec.parse().map_err(|_| PortRangeError::OutOfBounds)?;
        PortRange::new(p, p)
    }
}

/// Parse `host:port` OR `host:start-end` (host may be a DNS name or IP literal).
/// The host is kept as a string and resolved on the client side per
/// `data-model.md`. Returns `(host, PortRange)` — for the legacy single-port
/// form the range is a `PortRange::single`.
pub fn parse_target(spec: &str) -> Result<(String, PortRange), OperatorError> {
    let (host, port_spec) = spec
        .rsplit_once(':')
        .ok_or_else(|| OperatorError::InvalidTarget(spec.to_string()))?;
    if host.is_empty() {
        return Err(OperatorError::InvalidTarget(spec.to_string()));
    }
    let range = parse_port_range(port_spec).map_err(|e| match e {
        PortRangeError::Inverted { .. } => OperatorError::RangeInvalid(e.to_string()),
        _ => OperatorError::InvalidTarget(spec.to_string()),
    })?;
    Ok((host.to_string(), range))
}

/// `push-rule <client> <listen> <target_host>:<target_port>` where
/// `<listen>` is either a single port (e.g. `18080`) or a contiguous
/// range (e.g. `30000-30050`). Same forms apply to the target side.
/// Mirrors the v0.1.0 single-port behavior when the range size is 1.
///
/// Records the rule as `Pending`, sends a `RuleUpdate` with `request_id` to the
/// connected client, and waits up to `ack_timeout` for a matching `RuleStatus`.
/// On success transitions to `Active` and returns the assigned `RuleId`.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn push_rule(
    state: &AppState,
    raw_client: &str,
    listen: PortRange,
    target_host: &str,
    target: PortRange,
    protocol: &str,
    range_cap: u32,
    ack_timeout: Duration,
) -> Result<Rule, OperatorError> {
    let client_name = ClientName::from_str(raw_client)?;
    let proto = parse_protocol(protocol)?;

    // Reject up-front if the client isn't connected — saves us from leaving a
    // Pending rule behind that would never be acked.
    let Some((outbound, waiters)) = state.clients.handles(&client_name).await else {
        return Err(OperatorError::ClientNotConnected(client_name));
    };

    let rule = state
        .rules
        .push_range(
            client_name.clone(),
            listen,
            target_host.to_string(),
            target,
            proto,
            range_cap,
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
        listen_port = listen.start(),
        listen_port_end = ?listen_end_for_log(listen),
        range_size = listen.len(),
        target = %format!("{}:{}-{}", target_host, target.start(), target.end()),
    );

    let update = ServerMessage {
        payload: Some(server_message::Payload::RuleUpdate(RuleUpdate {
            request_id: request_id.clone(),
            action: RuleAction::Push as i32,
            rule: Some(ProtoRule {
                rule_id: rule.id.0,
                listen_port: u32::from(listen.start()),
                target_host: target_host.to_string(),
                target_port: u32::from(target.start()),
                protocol: ProtoProto::Tcp as i32,
                listen_port_end: if listen.len() > 1 {
                    u32::from(listen.end())
                } else {
                    0
                },
                target_port_end: if target.len() > 1 {
                    u32::from(target.end())
                } else {
                    0
                },
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
    // T046 (002-port-range-forward): a removed rule's per-port detail
    // is no longer meaningful — clear it so a subsequent `rule-stats
    // <id> --per-port` returns 404 (RuleNotFound) instead of stale data.
    state.per_port_stats.drop_rule(rule_id).await;
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
                    listen_port_end: removed.listen_port_end.map_or(0, u32::from),
                    target_port_end: removed.target_port_end.map_or(0, u32::from),
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- T016 (US1): listen + target argument parser ----

    #[test]
    fn parse_listen_accepts_single_port() {
        let r = parse_listen("18080").unwrap();
        assert_eq!(r.start(), 18080);
        assert_eq!(r.end(), 18080);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn parse_listen_accepts_range() {
        let r = parse_listen("30000-30050").unwrap();
        assert_eq!(r.start(), 30000);
        assert_eq!(r.end(), 30050);
        assert_eq!(r.len(), 51);
    }

    #[test]
    fn parse_listen_rejects_inverted_range_with_range_invalid() {
        let err = parse_listen("30050-30000").unwrap_err();
        assert!(matches!(err, OperatorError::RangeInvalid(_)));
        assert_eq!(err.code(), "range_invalid");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn parse_listen_rejects_non_numeric() {
        // Non-numeric "abc" → InvalidTarget (not RangeInvalid — there's
        // no structural sense of "inverted" for a non-port string).
        let err = parse_listen("abc-def").unwrap_err();
        assert!(matches!(err, OperatorError::InvalidTarget(_)));
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn parse_listen_rejects_zero_port() {
        let err = parse_listen("0").unwrap_err();
        // Port 0 is OutOfBounds → CLI exit-3 family via InvalidTarget.
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn parse_target_accepts_single_port() {
        let (host, range) = parse_target("10.0.0.5:8080").unwrap();
        assert_eq!(host, "10.0.0.5");
        assert_eq!(range.start(), 8080);
        assert_eq!(range.end(), 8080);
    }

    #[test]
    fn parse_target_accepts_range() {
        let (host, range) = parse_target("10.0.0.5:8080-8090").unwrap();
        assert_eq!(host, "10.0.0.5");
        assert_eq!(range.start(), 8080);
        assert_eq!(range.end(), 8090);
    }

    #[test]
    fn parse_target_accepts_dns_name() {
        let (host, range) = parse_target("upstream.internal:443").unwrap();
        assert_eq!(host, "upstream.internal");
        assert_eq!(range.start(), 443);
    }

    #[test]
    fn parse_target_rejects_missing_port() {
        let err = parse_target("just-a-host").unwrap_err();
        assert!(matches!(err, OperatorError::InvalidTarget(_)));
    }

    #[test]
    fn parse_target_rejects_empty_host() {
        let err = parse_target(":8080").unwrap_err();
        assert!(matches!(err, OperatorError::InvalidTarget(_)));
    }

    #[test]
    fn parse_target_inverted_range_returns_range_invalid() {
        let err = parse_target("h:8090-8080").unwrap_err();
        assert!(matches!(err, OperatorError::RangeInvalid(_)));
    }
}
