//! Loopback HTTP API mirroring the CLI surface (operator-api.md).
//!
//! Authorisation is local UNIX shell access on the server host (FR-022).
//! The bind address MUST be loopback; we assert that at server startup.

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    middleware::from_fn_with_state,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use forward_auth::OperatorIdentity;
use forward_core::{PortRange, RuleId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::bundle::CredentialBundle;
use crate::operator::ClientView;
use crate::operator::auth_layer::auth_middleware;
use crate::operator::cli::{self, OperatorError};
use crate::rules::Rule;
use crate::state::AppState;

const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(2);

pub fn router(state: Arc<AppState>) -> Router {
    use crate::operator::{audit_http, credentials, grants, stats_stream, users, users_me};
    Router::new()
        .route("/v1/clients", get(get_clients).post(post_clients))
        .route("/v1/clients/{name}/revoke", post(post_revoke))
        .route("/v1/rules", get(get_rules).post(post_rules))
        .route("/v1/rules/{rule_id}", delete(delete_rule))
        .route("/v1/rules/{rule_id}/stats", get(get_rule_stats))
        // 006-management-web-ui T025: SSE live stats stream.
        .route(
            "/v1/rules/{rule_id}/stats/stream",
            get(stats_stream::get_rule_stats_stream),
        )
        // 006-management-web-ui T024: superadmin-only audit log read.
        .route("/v1/audit", get(audit_http::get_audit))
        // 006-management-web-ui follow-up: same Prometheus payload as the
        // standalone `/metrics` listener, but RBAC-gated so the embedded
        // SPA (loaded same-origin from operator_http_listen) can render
        // it. The standalone loopback listener stays for prometheus
        // scrapers that don't carry a bearer token.
        .route("/v1/metrics", get(get_v1_metrics))
        // 005-multi-user-rbac T038: identity-management routes. All
        // superadmin-only (enforced inside each handler).
        .route("/v1/users", get(users::get_users).post(users::post_users))
        // 006-management-web-ui T012: caller's own identity projection.
        // Mounted BEFORE `/v1/users/{user_id}` so axum's path matcher
        // routes `/v1/users/me` here and never to `get_user("me")`.
        .route("/v1/users/me", get(users_me::get_users_me))
        .route(
            "/v1/users/{user_id}",
            get(users::get_user).delete(users::delete_user),
        )
        .route(
            "/v1/users/{user_id}/credentials",
            get(credentials::get_credentials).post(credentials::post_credential),
        )
        .route(
            "/v1/users/{user_id}/credentials/{cred_id}",
            delete(credentials::delete_credential),
        )
        .route(
            "/v1/users/{user_id}/credentials/{cred_id}/rotate",
            post(credentials::post_credential_rotate),
        )
        .route("/v1/grants", get(grants::get_grants).post(grants::post_grants))
        .route("/v1/grants/{grant_id}", delete(grants::delete_grant))
        // 005-multi-user-rbac T023: every /v1/* request goes through the
        // auth middleware FIRST. Mounted via `route_layer` so it applies
        // to all routes registered above.
        .route_layer(from_fn_with_state(state.clone(), auth_middleware))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct ProvisionBody {
    name: String,
}

async fn post_clients(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ProvisionBody>,
) -> Result<(StatusCode, Json<CredentialBundle>), ApiError> {
    let (_name, bundle) = cli::issue_bundle(&state, &body.name)?;
    Ok((StatusCode::CREATED, Json(bundle)))
}

async fn post_revoke(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    cli::revoke(&state, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_clients(State(state): State<Arc<AppState>>) -> Json<Vec<ClientView>> {
    Json(cli::list_clients(&state).await)
}

/// Superadmin-only mirror of the loopback `/metrics` endpoint.
/// Lets the embedded SPA render Prometheus output without crossing
/// listeners. Same payload as the scraper-facing endpoint.
async fn get_v1_metrics(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
) -> Result<Response, ApiError> {
    crate::operator::rbac::require_role(&identity, forward_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    let body = state.metrics.render();
    Ok((
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response())
}

#[derive(Debug, Deserialize)]
struct PushRuleBody {
    client: String,
    listen_port: u16,
    /// Inclusive listen-range end. Absent (or equal to `listen_port`)
    /// → single-port rule (v0.1.0 shape preserved). Present and
    /// greater than `listen_port` → range rule (002-port-range-forward).
    #[serde(default)]
    listen_port_end: Option<u16>,
    target_host: String,
    target_port: u16,
    /// Inclusive target-range end. MUST be present iff `listen_port_end`
    /// is present (the server enforces co-presence and equal length).
    #[serde(default)]
    target_port_end: Option<u16>,
    #[serde(default = "default_protocol")]
    protocol: String,
    /// 003-domain-name-forward: per-rule address-family preference
    /// for DNS-target rules. Absent → IPv4-first (default).
    /// `true` → prefer IPv6 (AAAA-first). Silently ignored for
    /// IP-literal targets.
    #[serde(default)]
    prefer_ipv6: Option<bool>,
    /// Optional override of the per-request ack timeout in seconds.
    #[serde(default)]
    ack_timeout_secs: Option<u64>,
}

fn default_protocol() -> String {
    "tcp".to_string()
}

/// 003-domain-name-forward T042 / `contracts/operator-api.md`
/// § "Response (additive)": always include `target_host` and
/// `prefer_ipv6` so generic operator tooling can rely on the
/// fields' presence without branching on rule type.
#[derive(Debug, Serialize)]
struct PushRuleResponse {
    rule_id: u64,
    status: String,
    target_host: String,
    prefer_ipv6: bool,
    /// 004-udp-forward T035: echo the activated protocol so generic
    /// operator tooling doesn't need to remember what it pushed. Always
    /// `"tcp"` or `"udp"`; future protocols extend this set without a
    /// wire bump.
    protocol: String,
    /// 005-multi-user-rbac T023: owning user id stamped at creation
    /// (FR-014). Always present; superadmin-pushed rules carry
    /// `_superadmin`.
    owner: String,
}

async fn post_rules(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Json(body): Json<PushRuleBody>,
) -> Result<(StatusCode, Json<PushRuleResponse>), ApiError> {
    // Co-presence check (FR-005 / contracts/operator-api.md):
    // listen_port_end / target_port_end MUST appear together.
    let listen =
        build_range(body.listen_port, body.listen_port_end).map_err(OperatorError::RangeInvalid)?;
    let target =
        build_range(body.target_port, body.target_port_end).map_err(OperatorError::RangeInvalid)?;
    if body.listen_port_end.is_some() != body.target_port_end.is_some() {
        return Err(OperatorError::RangeInvalid(
            "mismatched_range: listen_port_end and target_port_end must be present together".into(),
        )
        .into());
    }

    let timeout = body
        .ack_timeout_secs
        .map_or(DEFAULT_ACK_TIMEOUT, Duration::from_secs);
    let rule = cli::push_rule(
        &state,
        &identity,
        &body.client,
        listen,
        &body.target_host,
        target,
        &body.protocol,
        body.prefer_ipv6,
        state.range_rule_max_ports,
        timeout,
    )
    .await?;
    let status = match &rule.state {
        crate::rules::RuleState::Pending => "Pending".to_string(),
        crate::rules::RuleState::Active => "Active".to_string(),
        crate::rules::RuleState::Failed { reason } => format!("Failed:{reason}"),
        crate::rules::RuleState::Removed => "Removed".to_string(),
    };
    Ok((
        StatusCode::CREATED,
        Json(PushRuleResponse {
            rule_id: rule.id.0,
            status,
            target_host: rule.target_host.clone(),
            prefer_ipv6: rule.prefer_ipv6.unwrap_or(false),
            protocol: rule.protocol.as_str().to_string(),
            owner: rule.owner_user_id.to_string(),
        }),
    ))
}

/// Build a `PortRange` from a `(start, optional end)` pair. Returns
/// the range or a human-readable error string used in the
/// `range_invalid` HTTP response message.
fn build_range(start: u16, end: Option<u16>) -> Result<PortRange, String> {
    let end = end.unwrap_or(start);
    PortRange::new(start, end).map_err(|e| e.to_string())
}

async fn delete_rule(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(rule_id): Path<u64>,
) -> Result<StatusCode, ApiError> {
    // 005-multi-user-rbac T043: enforce read-side ownership before
    // any mutation. Superadmin always bypasses; everyone else must
    // own the rule.
    if let Some(rule) = state.rules.get(RuleId(rule_id)).await {
        crate::operator::rbac::enforce_read(&identity, &rule.owner_user_id)?;
    }
    cli::remove_rule(&state, RuleId(rule_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_rules(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<Rule>>, ApiError> {
    let client = params.get("client").map(String::as_str);
    let mut rules = cli::list_rules(&state, client).await?;
    // T043: superadmin sees all (with optional `?owner=` filter);
    // everyone else only sees their own.
    if identity.role != forward_auth::OperatorRole::Superadmin {
        rules.retain(|r| r.owner_user_id == identity.user_id);
    } else if let Some(o) = params.get("owner") {
        rules.retain(|r| r.owner_user_id.as_str() == o);
    }
    Ok(Json(rules))
}

async fn get_rule_stats(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(rule_id): Path<u64>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if let Some(rule) = state.rules.get(RuleId(rule_id)).await {
        crate::operator::rbac::enforce_read(&identity, &rule.owner_user_id)?;
    }
    let snap = cli::rule_stats(&state, RuleId(rule_id)).await?;
    let mut body = serde_json::to_value(&snap).map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal".into(),
        message: e.to_string(),
    })?;
    // 004-udp-forward T040: inject the rule's protocol so operators can
    // render TCP rules with `active_connections` and UDP rules with
    // `active_flows / datagrams_*`. The store lookup is cheap; we fall
    // back to "tcp" if the rule has been removed between the
    // stats_cache hit and this lookup (race window is microseconds and
    // the stale snapshot is already TCP-shaped).
    if let serde_json::Value::Object(ref mut map) = body {
        let proto = state
            .rules
            .get(RuleId(rule_id))
            .await
            .map_or_else(|| "tcp".to_string(), |r| r.protocol.as_str().to_string());
        map.insert("protocol".to_string(), serde_json::Value::String(proto));
    }
    // T046 (002-port-range-forward): when `?per_port=true`, append a
    // `per_port` array sourced from the per-port cache. Default
    // behavior (no query param) is unchanged so v0.1.0 callers see the
    // identical body shape.
    let per_port_requested = params
        .get("per_port")
        .is_some_and(|v| matches!(v.as_str(), "true" | "1" | "yes"));
    if per_port_requested
        && let Some(per_port) = state.per_port_stats.get(RuleId(rule_id)).await
        && let serde_json::Value::Object(ref mut map) = body
    {
        map.insert(
            "per_port".to_string(),
            serde_json::to_value(&per_port).map_err(|e| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "internal".into(),
                message: e.to_string(),
            })?,
        );
    }
    Ok(Json(body))
}

#[derive(Debug, Serialize)]
struct ApiErrorBody {
    error: ApiErrorInner,
}

#[derive(Debug, Serialize)]
struct ApiErrorInner {
    code: String,
    message: String,
}

pub struct ApiError {
    status: StatusCode,
    code: String,
    message: String,
}

impl ApiError {
    /// Public constructor used by the v0.5 user/credential/grant
    /// handlers when they need to surface a non-`OperatorError` failure
    /// (e.g., `IdentityStoreError::WriteFailed`).
    #[must_use]
    pub fn new(status: StatusCode, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }
}

impl From<forward_auth::RbacError> for ApiError {
    fn from(e: forward_auth::RbacError) -> Self {
        Self::from(OperatorError::Rbac(e))
    }
}

impl From<OperatorError> for ApiError {
    fn from(e: OperatorError) -> Self {
        let status = match &e {
            OperatorError::ClientAlreadyExists(_)
            | OperatorError::Auth(forward_auth::AuthError::ClientAlreadyExists(_))
            | OperatorError::PortInUse { .. } => StatusCode::CONFLICT,
            OperatorError::InvalidName(_)
            | OperatorError::InvalidProtocol(_)
            | OperatorError::InvalidTarget(_)
            | OperatorError::InvalidTargetHost { .. }
            | OperatorError::ExceedsCap { .. }
            | OperatorError::RangeInvalid(_) => StatusCode::BAD_REQUEST,
            OperatorError::ClientNotConnected(_)
            | OperatorError::ActivationFailed(_)
            // 004-udp-forward T019: capability mismatch surfaces as 422
            // (client connected but cannot fulfil the rule) — distinct
            // from 400 (operator's input was syntactically wrong).
            | OperatorError::UnsupportedProtocol { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            OperatorError::AckTimeout => StatusCode::GATEWAY_TIMEOUT,
            OperatorError::RuleNotFound => StatusCode::NOT_FOUND,
            // 005-multi-user-rbac: RBAC failures use the auth_layer's
            // shared status table (single source of truth).
            OperatorError::Rbac(rb) => crate::operator::auth_layer::rbac_status(rb),
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            code: e.code().to_string(),
            message: e.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (
            self.status,
            Json(ApiErrorBody {
                error: ApiErrorInner {
                    code: self.code,
                    message: self.message,
                },
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn binds_loopback_only() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        // Per FR-022: operator HTTP must bind to loopback only.
        assert!(addr.ip().is_loopback(), "got {addr}");
    }

    // ---- T017 (US1): structural body validation in `post_rules` ----
    //
    // We don't spin up a real client here — the structural checks
    // (range_inverted, mismatched_range, build_range failures) live in
    // `post_rules` BEFORE the ClientNotConnected gate, so they can be
    // exercised against a synthetic `AppState`.

    #[test]
    fn build_range_accepts_single_port() {
        let r = build_range(18080, None).unwrap();
        assert_eq!(r.start(), 18080);
        assert_eq!(r.end(), 18080);
    }

    #[test]
    fn build_range_accepts_explicit_range() {
        let r = build_range(30000, Some(30050)).unwrap();
        assert_eq!(r.start(), 30000);
        assert_eq!(r.end(), 30050);
    }

    #[test]
    fn build_range_rejects_inverted() {
        let err = build_range(30050, Some(30000)).unwrap_err();
        assert!(err.contains("inverted"), "got: {err}");
    }

    #[test]
    fn build_range_rejects_zero_port() {
        // OutOfBounds — port 0 is not a real listening port.
        let err = build_range(0, Some(10)).unwrap_err();
        assert!(err.contains("out_of_bounds"), "got: {err}");
    }
}
