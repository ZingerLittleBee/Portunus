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
use forward_core::{
    ClientName, ClientNameError, HostnameError, PortRange, PortRangeError, RequestId, RuleId,
    Target, TargetError,
};
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
    /// `target_host` failed `forward_core::Target::parse` (FR-001 /
    /// 003-domain-name-forward). The `code` field carries the
    /// `operator-api.md`-frozen subcategory string so the HTTP layer
    /// and CLI exit-mapper can route on it without reparsing the
    /// message.
    #[error("{code}: {message}")]
    InvalidTargetHost { code: &'static str, message: String },
    /// Range size > server-configured cap (FR-008, 002-port-range-forward).
    #[error("exceeds_cap: requested={requested} cap={cap}")]
    ExceedsCap { requested: u32, cap: u32 },
    /// Range structurally invalid (inverted, length mismatch, etc.).
    /// HTTP maps this to 400 with code `range_inverted` or
    /// `mismatched_range`; CLI maps to exit `3`.
    #[error("range_invalid: {0}")]
    RangeInvalid(String),
    /// 004-udp-forward T017: target client never declared support for
    /// the requested protocol in its `Hello.supported_protocols`.
    /// Maps to HTTP 422 / exit 3 with code `unsupported_protocol`
    /// (see `contracts/operator-api.md`).
    #[error("unsupported_protocol: client {client_name} does not support protocol {protocol}")]
    UnsupportedProtocol {
        client_name: ClientName,
        protocol: &'static str,
    },
    /// 005-multi-user-rbac: authorisation rejection. Maps to HTTP
    /// 401/403/422/etc. per the variant; CLI exit per the table in
    /// `contracts/operator-api.md` § "CLI Exit Codes".
    #[error("rbac: {0}")]
    Rbac(forward_auth::RbacError),

    // ---- 007-multi-target-failover ----
    //
    /// Operator submitted BOTH `target_host`/`target_port` AND
    /// `targets[]`. Maps to 400 / `rule_shape_conflict` (FR-004).
    #[error(
        "rule_shape_conflict: legacy target_host/target_port and targets[] are mutually exclusive"
    )]
    RuleShapeConflict,
    /// Operator submitted NEITHER shape. Maps to 400 /
    /// `rule_shape_missing` (FR-004).
    #[error("rule_shape_missing: rule must carry target_host/target_port OR targets[]")]
    RuleShapeMissing,
    /// `targets[]` validation failed (V-T1..V-T4 + V-R5). The inner
    /// `RuleTargetError` carries the specific failure; `code()` maps
    /// each variant to its operator-api stable code.
    #[error("{0}")]
    TargetsInvalid(#[from] forward_core::RuleTargetError),
    /// `health_check_interval_secs` outside `1..=3600` (V-R6).
    #[error("health_check_interval_out_of_range: {value} not in 1..=3600")]
    HealthCheckIntervalOutOfRange { value: u32 },
    /// Operator pushed a multi-target rule (`targets.len() >= 2`) at a
    /// client whose last-known `Hello.client_version` is `< 0.7.0`.
    /// That client cannot decode `Rule.targets` and would activate a
    /// broken single-target rule with empty `target_host`. Maps to
    /// 422 / `multi_target_unsupported_by_client` (R-007).
    #[error(
        "multi_target_unsupported_by_client: client {client_name} (version {client_version}) requires >= 0.7.0"
    )]
    MultiTargetUnsupportedByClient {
        client_name: ClientName,
        client_version: String,
    },

    // ---- 009-tls-sni-routing ----
    //
    /// FR-013: a candidate SNI rule has the same `sni_pattern` as an
    /// existing sibling on `(client, listen_port)`. Maps to HTTP 409 /
    /// `conflict.sni_route_duplicate`.
    #[error(
        "conflict.sni_route_duplicate: client {client_name} listen_port {listen_port} sni_pattern {sni_pattern} already in use"
    )]
    SniRouteDuplicate {
        client_name: ClientName,
        listen_port: u16,
        sni_pattern: String,
    },
    /// FR-014: a candidate fallback rule (`sni_pattern = None`) is
    /// being pushed to a listener that already has a fallback slot.
    /// Maps to HTTP 409 / `conflict.sni_fallback_duplicate`.
    #[error(
        "conflict.sni_fallback_duplicate: client {client_name} listen_port {listen_port} already has a fallback rule"
    )]
    SniFallbackDuplicate {
        client_name: ClientName,
        listen_port: u16,
    },
    /// FR-015: a candidate would flip an active listener's mode (legacy
    /// plain-TCP <-> SNI dispatch). Maps to HTTP 409 /
    /// `conflict.legacy_to_sni_unsupported`. Operator must remove the
    /// existing rule first.
    #[error(
        "conflict.legacy_to_sni_unsupported: client {client_name} listen_port {listen_port} has an active rule in {existing_mode} mode; remove it first before pushing in {candidate_mode} mode"
    )]
    LegacyToSniUnsupported {
        client_name: ClientName,
        listen_port: u16,
        existing_mode: &'static str,
        candidate_mode: &'static str,
    },
    /// FR-018 / T028: operator pushed a rule carrying `sni_pattern` at
    /// a client whose last-known `Hello.client_version` is `< 0.9.0`.
    /// Maps to HTTP 422 / `sni_unsupported_by_client`.
    #[error(
        "sni_unsupported_by_client: client {client_name} (version {client_version}) requires >= 0.9.0"
    )]
    SniUnsupportedByClient {
        client_name: ClientName,
        client_version: String,
    },
    /// FR-009 / T029: `sni_pattern` validation failure. The `code`
    /// carries the operator-api stable subcategory:
    /// - `validation.sni_on_unsupported_rule` — UDP or range rule.
    /// - `validation.sni_pattern_malformed` — grammar reject.
    ///
    /// Maps to HTTP 400 / exit 3.
    #[error("{code}: {message}")]
    SniValidation { code: &'static str, message: String },
    #[error(
        "proxy_protocol_unsupported_by_client: client {client_name} (version {client_version}) requires >= 0.10.0"
    )]
    ProxyProtocolUnsupportedByClient {
        client_name: ClientName,
        client_version: String,
    },
    #[error("{code}: {message}")]
    ProxyProtocolValidation { code: &'static str, message: String },
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
            | Self::InvalidTargetHost { .. }
            | Self::ExceedsCap { .. }
            | Self::RangeInvalid(_)
            | Self::UnsupportedProtocol { .. }
            // 007-multi-target-failover: shape and target validation
            // share exit code 3 with the v0.6.0 validation family.
            // Capability mismatch (`MultiTargetUnsupportedByClient`)
            // mirrors `UnsupportedProtocol` semantics — same exit.
            | Self::RuleShapeConflict
            | Self::RuleShapeMissing
            | Self::TargetsInvalid(_)
            | Self::HealthCheckIntervalOutOfRange { .. }
            | Self::MultiTargetUnsupportedByClient { .. }
            // 009-tls-sni-routing: SNI capability gate mirrors the
            // 007 multi-target gate (HTTP 422 / exit 3).
            | Self::SniUnsupportedByClient { .. }
            | Self::SniValidation { .. }
            | Self::ProxyProtocolUnsupportedByClient { .. }
            | Self::ProxyProtocolValidation { .. } => 3,
            Self::ClientNotConnected(_) => 4,
            // 009-tls-sni-routing: SNI conflicts share exit 5 with
            // PortInUse (the closest analogue: rule shape rejected
            // because the listener is already committed).
            Self::PortInUse { .. }
            | Self::SniRouteDuplicate { .. }
            | Self::SniFallbackDuplicate { .. }
            | Self::LegacyToSniUnsupported { .. } => 5,
            Self::ActivationFailed(_) => 6,
            Self::AckTimeout => 7,
            Self::RuleNotFound => 8,
            // 005: RBAC failures use the new operator-api table:
            // 4=auth, 5=rbac denial, 6=bootstrap_required, 2=already_bootstrapped, 3=validation.
            Self::Rbac(e) => match e {
                forward_auth::RbacError::Unauthenticated
                | forward_auth::RbacError::CredentialInvalid
                | forward_auth::RbacError::UserDisabled => 4,
                forward_auth::RbacError::ClientNotGranted
                | forward_auth::RbacError::PortOutsideGrant
                | forward_auth::RbacError::ProtocolNotGranted
                | forward_auth::RbacError::NotOwner
                | forward_auth::RbacError::RoleRequired => 5,
                forward_auth::RbacError::BootstrapRequired => 6,
                forward_auth::RbacError::AlreadyBootstrapped => 2,
                forward_auth::RbacError::InvalidUserId
                | forward_auth::RbacError::InvalidDisplayName
                | forward_auth::RbacError::ReservedUserId
                | forward_auth::RbacError::InvalidPortRange
                | forward_auth::RbacError::EmptyProtocolSet
                | forward_auth::RbacError::InvalidClient => 3,
                _ => 1,
            },
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
            // `InvalidTargetHost` and `SniValidation` (below) both
            // store an operator-api-stable subcode in their `code`
            // field; merging the arms keeps the dispatch trivial.
            Self::InvalidTargetHost { code, .. }
            | Self::SniValidation { code, .. }
            | Self::ProxyProtocolValidation { code, .. } => code,
            Self::ClientNotConnected(_) => "client_not_connected",
            Self::PortInUse { .. } => "port_in_use",
            Self::ActivationFailed(_) => "activation_failed",
            Self::AckTimeout => "ack_timeout",
            Self::RuleNotFound => "rule_not_found",
            Self::ExceedsCap { .. } => "exceeds_cap",
            Self::RangeInvalid(_) => "range_invalid",
            Self::UnsupportedProtocol { .. } => "unsupported_protocol",
            Self::Rbac(e) => e.code(),
            Self::Io(_) => "io_error",
            Self::Auth(_) => "auth_error",
            // 007-multi-target-failover (operator-api.md §1):
            Self::RuleShapeConflict => "rule_shape_conflict",
            Self::RuleShapeMissing => "rule_shape_missing",
            Self::TargetsInvalid(e) => match e {
                forward_core::RuleTargetError::Empty => "targets_empty",
                forward_core::RuleTargetError::TooMany(_) => "targets_too_many",
                forward_core::RuleTargetError::EmptyHost { .. }
                | forward_core::RuleTargetError::InvalidHost { .. } => "target_invalid_host",
                forward_core::RuleTargetError::InvalidPort { .. } => "target_invalid_port",
                forward_core::RuleTargetError::Duplicate { .. } => "targets_duplicate",
            },
            Self::HealthCheckIntervalOutOfRange { .. } => "health_check_interval_out_of_range",
            Self::MultiTargetUnsupportedByClient { .. } => "multi_target_unsupported_by_client",
            // 009-tls-sni-routing (operator-api.md §1):
            Self::SniRouteDuplicate { .. } => "conflict.sni_route_duplicate",
            Self::SniFallbackDuplicate { .. } => "conflict.sni_fallback_duplicate",
            Self::LegacyToSniUnsupported { .. } => "conflict.legacy_to_sni_unsupported",
            Self::SniUnsupportedByClient { .. } => "sni_unsupported_by_client",
            Self::ProxyProtocolUnsupportedByClient { .. } => "proxy_protocol_unsupported_by_client",
        }
    }
}

impl From<TargetError> for OperatorError {
    fn from(e: TargetError) -> Self {
        // Subcategory codes per `contracts/operator-api.md`:
        // we expose the four most-actionable shapes so operators can
        // pattern-match on `error.code` without parsing prose. Every
        // other validator failure folds into the bare
        // `invalid_target_host`.
        let code = match &e {
            TargetError::Hostname(HostnameError::TotalTooLong(_)) => "invalid_target_host_too_long",
            TargetError::Hostname(HostnameError::LabelTooLong { .. }) => {
                "invalid_target_host_label_too_long"
            }
            TargetError::Hostname(HostnameError::HyphenBoundary { .. }) => {
                "invalid_target_host_label_hyphen"
            }
            _ => "invalid_target_host",
        };
        Self::InvalidTargetHost {
            code,
            message: e.to_string(),
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
            RuleStoreError::UnsupportedProtocol {
                client_name,
                protocol,
            } => Self::UnsupportedProtocol {
                client_name,
                protocol,
            },
            RuleStoreError::SniRouteDuplicate {
                client_name,
                listen_port,
                sni_pattern,
            } => Self::SniRouteDuplicate {
                client_name,
                listen_port,
                sni_pattern,
            },
            RuleStoreError::SniFallbackDuplicate {
                client_name,
                listen_port,
            } => Self::SniFallbackDuplicate {
                client_name,
                listen_port,
            },
            RuleStoreError::LegacyToSniUnsupported {
                client_name,
                listen_port,
                existing_mode,
                candidate_mode,
            } => Self::LegacyToSniUnsupported {
                client_name,
                listen_port,
                existing_mode,
                candidate_mode,
            },
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
    let provisioned = state.tokens.list().unwrap_or_default();
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

#[must_use]
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
        // 004-udp-forward T017: accept "udp" on the operator surface.
        // Capability gating against the connected client lives in
        // `push_rule`, not here — `parse_protocol` only knows about
        // protocol strings the server can in principle activate.
        "udp" => Ok(Protocol::Udp),
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
    identity: &forward_auth::OperatorIdentity,
    raw_client: &str,
    listen: PortRange,
    target_host: &str,
    target: PortRange,
    protocol: &str,
    prefer_ipv6: Option<bool>,
    range_cap: u32,
    ack_timeout: Duration,
) -> Result<Rule, OperatorError> {
    let client_name = ClientName::from_str(raw_client)?;
    let proto = parse_protocol(protocol)?;

    // 005-multi-user-rbac T022: authorise BEFORE any state mutation.
    // Superadmin short-circuits inside enforce_push.
    let push_proto = match proto {
        Protocol::Tcp => crate::operator::rbac::PushProtocol::Tcp,
        Protocol::Udp => crate::operator::rbac::PushProtocol::Udp,
    };
    let push_req = crate::operator::rbac::PushRequest {
        client: &client_name,
        listen_port_start: listen.start(),
        listen_port_end: listen.end(),
        protocol: push_proto,
    };
    let grants = state.operator_auth.grants_for(&identity.user_id);
    crate::operator::rbac::enforce_push(identity, &push_req, &grants)
        .map_err(OperatorError::Rbac)?;
    // 003-domain-name-forward T021: validate `target_host` against the
    // shared classifier before we touch any connected-client state.
    // We discard the parsed `Target` because the client side reparses
    // it from the proto wire form — the server stores `target_host`
    // as a verbatim string per `contracts/operator-api.md`.
    let _ = Target::parse(target_host).map_err(OperatorError::from)?;

    // Reject up-front if the client isn't connected — saves us from leaving a
    // Pending rule behind that would never be acked.
    let Some((outbound, waiters)) = state.clients.handles(&client_name).await else {
        return Err(OperatorError::ClientNotConnected(client_name));
    };

    // 004-udp-forward T017: capability gating. UDP rules can only be
    // pushed to a client whose Hello declared UDP support. v0.3 clients
    // (no Hello / TCP-only Hello) get a clean 422 / exit 3 surface
    // instead of a delayed RuleStatus.failed (HIGH-1 review fix).
    if matches!(proto, Protocol::Udp) {
        let proto_wire = forward_proto::v1::Protocol::Udp;
        let supported = state
            .clients
            .supports(&client_name, proto_wire)
            .await
            .unwrap_or(false);
        if !supported {
            return Err(OperatorError::UnsupportedProtocol {
                client_name,
                protocol: "udp",
            });
        }
    }

    let rule = state
        .rules
        .push_range(
            client_name.clone(),
            listen,
            target_host.to_string(),
            target,
            proto,
            prefer_ipv6,
            range_cap,
            identity.user_id.clone(),
        )
        .await?;
    state
        .rule_store
        .upsert_rule(&rule)
        .map_err(|e| OperatorError::ActivationFailed(format!("persist_rule: {e}")))?;
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
                // 004-udp-forward T017: encode the actual rule protocol
                // on the wire so the client routes UDP rules into the
                // UDP forwarder (US1 T026+). v0.3 clients never reach
                // this branch because the capability gate above rejects
                // UDP pushes to TCP-only clients.
                protocol: match proto {
                    Protocol::Tcp => ProtoProto::Tcp as i32,
                    Protocol::Udp => ProtoProto::Udp as i32,
                },
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
                prefer_ipv6,
                // 007-multi-target-failover (Phase 2 stub): the legacy
                // push path always emits a single-target rule on the
                // wire (back-compat encoding R-002 — `targets` empty,
                // legacy fields populated). The new shape lands in
                // Phase 6 (T043).
                targets: Vec::new(),
                health_check_interval_secs: 0,
                // 009-tls-sni-routing T015: legacy (pre-009) push helpers
                // never set sni_pattern. The new SNI-aware push path
                // (added in T026/T043) plumbs this from rule.sni_pattern.
                sni_pattern: None,
            }),
        })),
    };
    if outbound.send(Ok(update)).await.is_err() {
        // Stream torn down between handles() and send — treat like
        // client_not_connected. Drop the pending entry so re-push can succeed
        // after a reconnect.
        let _ = state.rules.remove(rule.id).await;
        let _ = state.rule_store.delete_rule(rule.id);
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
                    if let Some(rule) = state.rules.get(rule.id).await {
                        let _ = state.rule_store.upsert_rule(&rule);
                    }
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
                    if let Some(rule) = state.rules.get(rule.id).await {
                        let _ = state.rule_store.upsert_rule(&rule);
                    }
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

/// 007-multi-target-failover (Phase 3 / T022): wire-pushing a rule whose
/// `targets` list has length >= 1 with real failover semantics. Mirrors
/// `push_rule` but emits the multi-target wire shape (`Rule.targets[]`
/// populated, legacy `target_host`/`target_port` carry the FIRST
/// target's values for back-compat with v0.6.0 readers — those readers
/// drop field 9 silently and run the rule as single-target).
///
/// Validation, RBAC, and version-guard work happens BEFORE this helper
/// in `operator::http::push_multi_target` so callers don't re-validate.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn push_rule_multi_target(
    state: &AppState,
    identity: &forward_auth::OperatorIdentity,
    client_name: ClientName,
    listen: PortRange,
    targets: Vec<forward_core::RuleTarget>,
    health_check_interval_secs: Option<u32>,
    proto: Protocol,
    prefer_ipv6: Option<bool>,
    range_cap: u32,
    ack_timeout: Duration,
    // 009-tls-sni-routing: optional SNI selector. Already validated +
    // lowercased by the HTTP handler (operator::http::post_rules)
    // before reaching this helper.
    sni_pattern: Option<String>,
) -> Result<Rule, OperatorError> {
    debug_assert!(
        !targets.is_empty(),
        "push_rule_multi_target with empty targets"
    );

    // First target carries the legacy mirror — v0.6.0 readers ignore
    // `targets` and use these. Multi-target clients ignore them in
    // favour of `targets`. The target_range is always single-port:
    // multi-target rules don't combine with port ranges in v0.7.0.
    let first = &targets[0];
    let target_host = first.host.clone();
    let target_range = PortRange::single(first.port);

    let Some((outbound, waiters)) = state.clients.handles(&client_name).await else {
        return Err(OperatorError::ClientNotConnected(client_name));
    };

    // Capability gate (mirrors push_rule). UDP multi-target rules need
    // a UDP-capable client just like single-target UDP rules.
    if matches!(proto, Protocol::Udp) {
        let proto_wire = forward_proto::v1::Protocol::Udp;
        let supported = state
            .clients
            .supports(&client_name, proto_wire)
            .await
            .unwrap_or(false);
        if !supported {
            return Err(OperatorError::UnsupportedProtocol {
                client_name,
                protocol: "udp",
            });
        }
    }

    let rule = state
        .rules
        .push_range_with_targets(
            client_name.clone(),
            listen,
            target_host.clone(),
            target_range,
            proto,
            prefer_ipv6,
            range_cap,
            identity.user_id.clone(),
            targets.clone(),
            health_check_interval_secs,
            sni_pattern,
        )
        .await?;
    state
        .rule_store
        .upsert_rule(&rule)
        .map_err(|e| OperatorError::ActivationFailed(format!("persist_rule: {e}")))?;
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
        target_count = targets.len(),
        multi_target = true,
        health_check_interval_secs = ?health_check_interval_secs,
    );

    let proto_targets: Vec<forward_proto::v1::Target> = targets
        .iter()
        .map(|t| forward_proto::v1::Target {
            host: t.host.clone(),
            port: u32::from(t.port),
            priority: t.priority,
            proxy_protocol: t.proxy_protocol.map(|mode| match mode {
                forward_core::ProxyProtocolVersion::V1 => {
                    forward_proto::v1::ProxyProtocolVersion::V1 as i32
                }
                forward_core::ProxyProtocolVersion::V2 => {
                    forward_proto::v1::ProxyProtocolVersion::V2 as i32
                }
            }),
        })
        .collect();
    let update = ServerMessage {
        payload: Some(server_message::Payload::RuleUpdate(RuleUpdate {
            request_id: request_id.clone(),
            action: RuleAction::Push as i32,
            rule: Some(ProtoRule {
                rule_id: rule.id.0,
                listen_port: u32::from(listen.start()),
                target_host,
                target_port: u32::from(first.port),
                protocol: match proto {
                    Protocol::Tcp => ProtoProto::Tcp as i32,
                    Protocol::Udp => ProtoProto::Udp as i32,
                },
                listen_port_end: if listen.len() > 1 {
                    u32::from(listen.end())
                } else {
                    0
                },
                target_port_end: 0,
                prefer_ipv6,
                targets: proto_targets,
                health_check_interval_secs: health_check_interval_secs.unwrap_or(0),
                // 009-tls-sni-routing T026: forward the validated SNI
                // pattern to the data plane. Pre-0.9 clients are
                // refused upstream by the capability gate, so this is
                // safe to send unconditionally.
                sni_pattern: rule.sni_pattern.clone(),
            }),
        })),
    };
    if outbound.send(Ok(update)).await.is_err() {
        let _ = state.rules.remove(rule.id).await;
        let _ = state.rule_store.delete_rule(rule.id);
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
                    if let Some(rule) = state.rules.get(rule.id).await {
                        let _ = state.rule_store.upsert_rule(&rule);
                    }
                    info!(
                        event = "audit.rule_push",
                        outcome = "activated",
                        request_id = %request_id,
                        rule_id = %rule.id,
                        client_name = %client_name,
                        multi_target = true,
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
                    if let Some(rule) = state.rules.get(rule.id).await {
                        let _ = state.rule_store.upsert_rule(&rule);
                    }
                    warn!(
                        event = "audit.rule_push",
                        outcome = "failed",
                        request_id = %request_id,
                        rule_id = %rule.id,
                        client_name = %client_name,
                        reason = %reason,
                        multi_target = true,
                    );
                    Err(OperatorError::ActivationFailed(reason))
                }
                _ => Err(OperatorError::ActivationFailed(
                    "unexpected_outcome".to_string(),
                )),
            }
        }
        Ok(Err(_recv_err)) => {
            warn!(
                event = "audit.rule_push",
                outcome = "ack_lost",
                request_id = %request_id,
                rule_id = %rule.id,
                client_name = %client_name,
                multi_target = true,
            );
            Err(OperatorError::AckTimeout)
        }
        Err(_elapsed) => {
            let mut guard = waiters.lock().await;
            guard.remove(&request_id);
            warn!(
                event = "audit.rule_push",
                outcome = "ack_timeout",
                request_id = %request_id,
                rule_id = %rule.id,
                client_name = %client_name,
                multi_target = true,
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
    let _ = state.rule_store.delete_rule(rule_id);
    let owner = removed.owner_user_id.to_string();
    state
        .stats_cache
        .drop_rule(
            rule_id,
            &removed.client_name,
            owner.as_str(),
            &state.metrics,
        )
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
                    prefer_ipv6: removed.prefer_ipv6,
                    // 007-multi-target-failover (Phase 2 stub): REMOVE
                    // pushes only need rule_id, but we keep the message
                    // shape canonical. Empty/zero on the wire.
                    targets: Vec::new(),
                    health_check_interval_secs: 0,
                    // 009-tls-sni-routing T015: REMOVE only reads rule_id
                    // on the receiving side; SNI is irrelevant here.
                    sni_pattern: None,
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
#[must_use]
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
#[must_use]
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

    // ---- T021 (US1): TargetError → OperatorError::InvalidTargetHost
    // mapping. Codes are part of `contracts/operator-api.md`'s frozen
    // surface, so we pin them down here.

    #[test]
    fn target_error_invalid_char_maps_to_generic_invalid_target_host() {
        let err: OperatorError = Target::parse("foo_bar.example").unwrap_err().into();
        assert_eq!(err.code(), "invalid_target_host");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn target_error_label_too_long_maps_to_label_subcode() {
        let long_label = "a".repeat(64);
        let host = format!("{long_label}.example.com");
        let err: OperatorError = Target::parse(&host).unwrap_err().into();
        assert_eq!(err.code(), "invalid_target_host_label_too_long");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn target_error_total_too_long_maps_to_total_subcode() {
        // 254 chars: build labels of 63 to dodge the per-label limit.
        let label = "a".repeat(63);
        let host = format!("{label}.{label}.{label}.{label}xx");
        assert!(host.len() > 253);
        let err: OperatorError = Target::parse(&host).unwrap_err().into();
        assert_eq!(err.code(), "invalid_target_host_too_long");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn target_error_hyphen_boundary_maps_to_hyphen_subcode() {
        let err: OperatorError = Target::parse("-leading.example").unwrap_err().into();
        assert_eq!(err.code(), "invalid_target_host_label_hyphen");
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn target_error_unbracketed_ipv6_maps_to_generic_subcode() {
        let err: OperatorError = Target::parse("2001:db8::1").unwrap_err().into();
        assert_eq!(err.code(), "invalid_target_host");
        assert_eq!(err.exit_code(), 3);
    }
}
