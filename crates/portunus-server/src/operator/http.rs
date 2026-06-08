//! Operator HTTP API mirroring the CLI surface (operator-api.md).
//!
//! Every protected `/v1/*` route is bearer-token authenticated and RBAC-gated.
//! `/v1/auth/status` and `/v1/auth/onboarding` stay outside that layer for
//! first-run setup. The bind address defaults to loopback, but operators may
//! expose it explicitly.

use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    middleware::from_fn_with_state,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post, put},
};
use portunus_auth::OperatorIdentity;
use portunus_core::{PortRange, RuleId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::operator::ClientView;
use crate::operator::auth_layer::auth_middleware;
use crate::operator::cli::{self, OperatorError};
use crate::rules::Rule;
use crate::state::AppState;

const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(2);

pub fn router(state: Arc<AppState>) -> Router {
    use crate::operator::{
        audit_http, credentials, grants, stats_stream, users, users_me, web_auth,
    };

    let protected = Router::new()
        .route("/v1/clients", get(get_clients))
        // 015-client-stable-id (US3): client-scoped routes address the
        // client by its stable, opaque client_id (ULID), not its mutable
        // display name. Unknown / malformed id -> 404.
        .route("/v1/clients/{client_id}", put(put_client).delete(delete_client))
        .route("/v1/clients/{client_id}/name", patch(patch_client_name))
        .route("/v1/clients/{client_id}/revoke", post(post_revoke))
        .route("/v1/clients/{client_id}/enrollment", post(post_client_reenrollment))
        .route("/v1/client-enrollments", post(post_client_enrollments))
        .route("/v1/rules", get(get_rules).post(post_rules))
        .route(
            "/v1/rules/{rule_id}",
            delete(delete_rule).put(put_rule),
        )
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
        .route("/v1/users/me/password", post(web_auth::post_self_password))
        .route(
            "/v1/users/{user_id}",
            get(users::get_user).delete(users::delete_user),
        )
        .route(
            "/v1/users/{user_id}/password",
            post(web_auth::post_user_password),
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
        // 011-rate-limiting-qos T028: per-owner cap endpoints. Nested
        // under client (Q5) so /v1/clients/{id}/owners is the listing
        // surface and /v1/clients/{id}/owners/{owner_id}/rate-limit
        // carries the envelope GET / PUT / DELETE.
        .route(
            "/v1/clients/{client_id}/owners",
            get(crate::operator::owner_cap::get_owners_under_client),
        )
        .route(
            "/v1/clients/{client_id}/owners/{owner_id}/rate-limit",
            get(crate::operator::owner_cap::get_owner_rate_limit)
                .put(crate::operator::owner_cap::put_owner_rate_limit)
                .delete(crate::operator::owner_cap::delete_owner_rate_limit),
        )
        // 013-traffic-quotas C1: per-(user, client) monthly traffic
        // quota CRUD + historical traffic queries. CRUD pushes
        // TrafficQuotaUpdate to the connected client; reconnect replay
        // (C5) handles the offline-client path.
        .route(
            "/v1/users/{user_id}/quotas",
            get(crate::operator::quota_http::list_user_quotas),
        )
        .route(
            "/v1/users/{user_id}/quotas/{client_id}",
            put(crate::operator::quota_http::put_quota)
                .patch(crate::operator::quota_http::patch_quota)
                .delete(crate::operator::quota_http::delete_quota),
        )
        .route(
            "/v1/users/{user_id}/quotas/{client_id}/status",
            get(crate::operator::quota_http::get_quota_status),
        )
        .route(
            "/v1/users/{user_id}/traffic",
            get(crate::operator::quota_http::get_user_traffic),
        )
        .route(
            "/v1/clients/{client_id}/quotas",
            get(crate::operator::quota_http::list_client_quotas),
        )
        .route(
            "/v1/clients/{client_id}/traffic",
            get(crate::operator::quota_http::get_client_traffic),
        )
        .route(
            "/v1/traffic/global",
            get(crate::operator::quota_http::get_global_traffic),
        )
        // 014-advertised-endpoint: operator GET/PUT for the runtime
        // advertised-endpoint override. Superadmin-only.
        .route(
            "/v1/settings/advertised-endpoint",
            get(get_advertised_endpoint).put(put_advertised_endpoint),
        )
        // 005-multi-user-rbac T023: every /v1/* request goes through the
        // auth middleware FIRST. Mounted via `route_layer` so it applies
        // to all routes registered above.
        .route_layer(from_fn_with_state(state.clone(), auth_middleware));

    Router::new()
        .route("/v1/auth/status", get(web_auth::get_auth_status))
        .route("/v1/auth/onboarding", post(web_auth::post_auth_onboarding))
        .route("/v1/auth/login", post(web_auth::post_auth_login))
        .route("/v1/auth/logout", post(web_auth::post_auth_logout))
        .merge(protected)
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct EnrollmentBody {
    name: String,
    address: String,
    ttl_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ReEnrollmentBody {
    ttl_secs: Option<u64>,
}

#[derive(Debug, serde::Serialize)]
struct EnrollmentResponse {
    client_name: String,
    expires_at: String,
    command: String,
    uri: String,
}

#[derive(Debug, Deserialize)]
struct UpdateClientBody {
    address: String,
}

async fn post_client_enrollments(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    headers: axum::http::HeaderMap,
    Json(body): Json<EnrollmentBody>,
) -> Result<(StatusCode, Json<EnrollmentResponse>), ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    let req_host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok());
    let enrollment = cli::enroll_client(
        &state,
        &body.name,
        Some(&body.address),
        body.ttl_secs.unwrap_or(600),
        req_host,
    )?;
    Ok((
        StatusCode::CREATED,
        Json(EnrollmentResponse {
            client_name: enrollment.client_name.to_string(),
            expires_at: enrollment.expires_at.to_rfc3339(),
            command: enrollment.command,
            uri: enrollment.uri,
        }),
    ))
}

async fn post_client_reenrollment(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    headers: axum::http::HeaderMap,
    Path(client_id): Path<String>,
    Json(body): Json<ReEnrollmentBody>,
) -> Result<(StatusCode, Json<EnrollmentResponse>), ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    let req_host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok());
    let enrollment = cli::enroll_existing_client_by_id(
        &state,
        &client_id,
        body.ttl_secs.unwrap_or(600),
        req_host,
    )?;
    Ok((
        StatusCode::CREATED,
        Json(EnrollmentResponse {
            client_name: enrollment.client_name.to_string(),
            expires_at: enrollment.expires_at.to_rfc3339(),
            command: enrollment.command,
            uri: enrollment.uri,
        }),
    ))
}

async fn post_revoke(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(client_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    cli::revoke_by_id(&state, &client_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_client(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(client_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    cli::delete_client_by_id(&state, &client_id)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn put_client(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(client_id): Path<String>,
    Json(body): Json<UpdateClientBody>,
) -> Result<Json<ClientView>, ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    let updated = cli::update_client_by_id(&state, &client_id, Some(&body.address))?;
    let connected = state.clients.snapshot().await;
    let conn = connected.get(&updated.client_id);
    Ok(Json(ClientView {
        client_id: updated.client_id,
        client_name: updated.client_name,
        provisioned_at: updated.issued_at,
        revoked_at: updated.revoked_at,
        connected: conn.is_some(),
        client_address: updated.client_address,
        remote_addr: conn.and_then(|c| c.remote_addr.map(|a| a.to_string())),
        connected_at: conn.map(|c| c.connected_at),
    }))
}

async fn get_clients(State(state): State<Arc<AppState>>) -> Json<Vec<ClientView>> {
    Json(cli::list_clients(&state).await)
}

#[derive(Debug, Deserialize)]
struct RenameClientBody {
    client_name: String,
}

/// `PATCH /v1/clients/{client_id}/name` — 015-client-stable-id (US2).
/// Identity-safe rename: the client is addressed by its stable id, so
/// the display name change leaves rules / tokens / quotas / history and
/// any live session intact. An unknown or malformed id is a 404.
async fn patch_client_name(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(client_id): Path<String>,
    Json(body): Json<RenameClientBody>,
) -> Result<Json<ClientView>, ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    let updated = cli::rename_client(&state, &client_id, &body.client_name)?;
    // 015-client-stable-id: the store write already re-synced the persisted
    // `rules.client_name`; refresh the live in-memory rule snapshot too so
    // `/v1/rules` and the Web UI Rules page reflect the new name immediately
    // (without waiting for a restart/hydration).
    state
        .rules
        .rename_client(&updated.client_id, &updated.client_name)
        .await;
    let connected = state.clients.snapshot().await;
    let conn = connected.get(&updated.client_id);
    Ok(Json(ClientView {
        client_id: updated.client_id,
        client_name: updated.client_name,
        provisioned_at: updated.issued_at,
        revoked_at: updated.revoked_at,
        connected: conn.is_some(),
        client_address: updated.client_address,
        remote_addr: conn.and_then(|c| c.remote_addr.map(|a| a.to_string())),
        connected_at: conn.map(|c| c.connected_at),
    }))
}

/// Superadmin-only mirror of the loopback `/metrics` endpoint.
/// Lets the embedded SPA render Prometheus output without crossing
/// listeners. Same payload as the scraper-facing endpoint.
async fn get_v1_metrics(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
) -> Result<Response, ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
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

/// `POST /v1/rules` request body. Accepts BOTH the v0.6.0 legacy
/// shape (`target_host` + `target_port`) AND the new v0.7.0 shape
/// (`targets[]` + optional `health_check_interval_secs`). Per
/// `contracts/operator-api.md` §1, supplying both shapes or neither
/// is a 400 (`rule_shape_conflict` / `rule_shape_missing` —
/// 007-multi-target-failover, FR-004).
#[derive(Debug, Deserialize)]
struct PushRuleBody {
    client: String,
    listen_port: u16,
    /// Inclusive listen-range end. Absent (or equal to `listen_port`)
    /// → single-port rule (v0.1.0 shape preserved). Present and
    /// greater than `listen_port` → range rule (002-port-range-forward).
    #[serde(default)]
    listen_port_end: Option<u16>,
    /// Legacy single-target host. Present iff legacy shape (mutually
    /// exclusive with `targets[]`).
    #[serde(default)]
    target_host: Option<String>,
    /// Legacy single-target port. Present iff legacy shape.
    #[serde(default)]
    target_port: Option<u16>,
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
    /// 007-multi-target-failover (FR-001): ordered list of upstream
    /// targets. Present iff new shape (mutually exclusive with
    /// `target_host` / `target_port`).
    #[serde(default)]
    targets: Option<Vec<TargetBody>>,
    /// 007-multi-target-failover (FR-013): optional active TCP-connect
    /// probe interval in seconds (1..=3600). Only valid alongside
    /// `targets[]`.
    #[serde(default)]
    health_check_interval_secs: Option<u32>,
    /// 009-tls-sni-routing (FR-009..FR-012): optional SNI selector for
    /// TCP single-port rules. Absent → fallback / legacy behaviour
    /// preserved. Present → exact host (`api.example.com`) or
    /// single-label wildcard (`*.example.com`). Lowercased and
    /// grammar-validated by `post_rules`. UDP and range rules carrying
    /// `sni_pattern` are rejected with 400 / `validation.sni_on_unsupported_rule`.
    #[serde(default)]
    sni_pattern: Option<String>,
    /// 011-rate-limiting-qos (FR-001..FR-004, FR-018, FR-020): optional
    /// per-rule cap envelope. Absent → uncapped on every dimension
    /// (legacy v0.10 behaviour, byte-identical wire). Present →
    /// validated by `portunus_core::rate_limit::validate` before
    /// persistence; capability-gated against pre-0.11 clients
    /// (HTTP 422 / `rate_limit_unsupported_by_client`). All four cap
    /// dimensions and their burst overrides are independently
    /// optional; `concurrent_connections_burst` is intentionally
    /// absent (concurrent caps are hard ceilings, not buckets — the
    /// reserved-rejection check happens at the operator-API
    /// boundary, see `RateLimitBody`).
    #[serde(default)]
    rate_limit: Option<RateLimitBody>,
}

/// 011-rate-limiting-qos T016: operator-API request shape for the
/// per-rule cap envelope. Distinct from `portunus_core::RateLimit`
/// because we accept the reserved
/// `concurrent_connections_burst` field at the wire level so we
/// can reject it with the stable subcategory
/// `validation.rate_limit_burst_unsupported`. The reserved field is
/// not stored or otherwise plumbed.
#[derive(Debug, Deserialize)]
struct RateLimitBody {
    #[serde(default)]
    bandwidth_in_bps: Option<u64>,
    #[serde(default)]
    bandwidth_out_bps: Option<u64>,
    #[serde(default)]
    new_connections_per_sec: Option<u32>,
    #[serde(default)]
    concurrent_connections: Option<u32>,
    #[serde(default)]
    bandwidth_in_burst: Option<u64>,
    #[serde(default)]
    bandwidth_out_burst: Option<u64>,
    #[serde(default)]
    new_connections_burst: Option<u32>,
    /// Reserved for future use (concurrent caps are hard ceilings,
    /// not token buckets). Any non-null value is rejected with
    /// `400 validation.rate_limit_burst_unsupported`.
    #[serde(default)]
    concurrent_connections_burst: Option<u32>,
}

/// One entry in the new `targets[]` shape. `priority` defaults to the
/// row index when omitted.
#[derive(Debug, Deserialize)]
struct TargetBody {
    host: String,
    port: u16,
    #[serde(default)]
    priority: Option<u32>,
    #[serde(default)]
    proxy_protocol: Option<String>,
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
    /// 009-tls-sni-routing T045: echo the SNI selector when the rule
    /// carries one. Omitted when absent so v0.8 callers see byte-
    /// identical bodies (`#[serde(skip_serializing_if = "Option::is_none")]`).
    #[serde(skip_serializing_if = "Option::is_none")]
    sni_pattern: Option<String>,
    /// 011-rate-limiting-qos T016: echo the cap envelope when the
    /// rule carries one. Omitted when absent so pre-0.11 callers see
    /// byte-identical bodies. The shape mirrors `portunus_core::RateLimit`
    /// 1:1 — every cap dimension is independently optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    rate_limit: Option<portunus_core::RateLimit>,
}

async fn post_rules(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Json(body): Json<PushRuleBody>,
) -> Result<(StatusCode, Json<PushRuleResponse>), ApiError> {
    // 007-multi-target-failover (FR-004): shape dispatch.
    let has_legacy = body.target_host.is_some() || body.target_port.is_some();
    let has_new = body.targets.is_some();
    if has_legacy && has_new {
        return Err(OperatorError::RuleShapeConflict.into());
    }
    if !has_legacy && !has_new {
        return Err(OperatorError::RuleShapeMissing.into());
    }

    // Co-presence check (FR-005 / contracts/operator-api.md):
    // listen_port_end / target_port_end MUST appear together.
    if body.listen_port_end.is_some() != body.target_port_end.is_some() {
        return Err(OperatorError::RangeInvalid(
            "mismatched_range: listen_port_end and target_port_end must be present together".into(),
        )
        .into());
    }
    let listen =
        build_range(body.listen_port, body.listen_port_end).map_err(OperatorError::RangeInvalid)?;
    let timeout = body
        .ack_timeout_secs
        .map_or(DEFAULT_ACK_TIMEOUT, Duration::from_secs);

    // 009-tls-sni-routing T029: SNI grammar + applicability gate.
    // Applies to BOTH legacy and new shapes — `sni_pattern` is only
    // valid on TCP single-port rules. UDP and range rules carrying
    // a non-null `sni_pattern` are rejected with a 400.
    let sni_pattern: Option<String> = if let Some(raw) = body.sni_pattern.as_deref() {
        if !body.protocol.eq_ignore_ascii_case("tcp") {
            return Err(OperatorError::SniValidation {
                code: "validation.sni_on_unsupported_rule",
                message: format!(
                    "sni_pattern is only valid on tcp single-port rules; got protocol `{}`",
                    body.protocol
                ),
            }
            .into());
        }
        if listen.len() > 1 {
            return Err(OperatorError::SniValidation {
                code: "validation.sni_on_unsupported_rule",
                message: format!(
                    "sni_pattern is only valid on single-port rules, not ranges (listen {}..={})",
                    listen.start(),
                    listen.end()
                ),
            }
            .into());
        }
        Some(validate_sni_pattern(raw)?)
    } else {
        None
    };

    // 011-rate-limiting-qos T016: parse + validate the optional cap
    // envelope before any rule mutation. Returns Err with a stable
    // `validation.rate_limit_*` subcategory on the four documented
    // failure modes (cap_zero, burst_without_rate, burst_range,
    // burst_unsupported).
    let rate_limit = parse_rate_limit_body(body.rate_limit.as_ref())?;

    if has_new {
        return push_multi_target(
            &state,
            &identity,
            &body,
            listen,
            timeout,
            sni_pattern,
            rate_limit,
        )
        .await;
    }

    // 011-rate-limiting-qos T016: legacy `target_host` shape does not
    // currently accept `rate_limit`. Operators who need caps must
    // migrate to the `targets[]` shape, which is the recommended
    // form since v0.7. Error early so operators get a clear message
    // instead of silently dropping the cap.
    if rate_limit.is_some() {
        return Err(OperatorError::RateLimitValidation {
            code: "validation.rate_limit_on_legacy_shape",
            message:
                "rate_limit requires the `targets[]` request shape; legacy `target_host`/`target_port` is not supported"
                    .into(),
        }
        .into());
    }

    // Legacy single-target shape (v0.6.0).
    let target_host = body
        .target_host
        .as_deref()
        .expect("has_legacy true → target_host present");
    let target_port = body
        .target_port
        .expect("has_legacy true → target_port present");
    let target =
        build_range(target_port, body.target_port_end).map_err(OperatorError::RangeInvalid)?;

    // 009-tls-sni-routing: legacy single-target shape carrying
    // `sni_pattern` is reshaped into a 1-element `targets[]` and routed
    // through the multi-target path so the SNI selector reaches
    // `Rule.sni_pattern` and the wire emitter. The capability gate
    // (T028) lives inside `push_multi_target_with_sni`.
    if let Some(pat) = sni_pattern.clone() {
        return push_legacy_with_sni(
            &state,
            &identity,
            &body.client,
            &body.protocol,
            listen,
            target_host,
            target,
            body.prefer_ipv6,
            timeout,
            pat,
        )
        .await;
    }

    let rule = cli::push_rule(
        &state,
        &identity,
        &body.client,
        listen,
        target_host,
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
            sni_pattern: rule.sni_pattern.clone(),
            // 011-rate-limiting-qos T016: echo the persisted cap
            // envelope so operators can confirm what landed.
            rate_limit: rule.rate_limit.clone(),
        }),
    ))
}

/// 007-multi-target-failover handler for the new `targets[]` shape.
/// Validates + RBAC + version-guards BEFORE any rule mutation, then
/// hands off to `cli::push_rule_multi_target` which emits the
/// multi-target `RuleUpdate` and waits for activation.
async fn push_multi_target(
    state: &Arc<AppState>,
    identity: &OperatorIdentity,
    body: &PushRuleBody,
    listen: PortRange,
    timeout: Duration,
    sni_pattern: Option<String>,
    // 011-rate-limiting-qos T016: parsed + validated cap envelope
    // already vetted for cap-zero / burst range / reserved-burst
    // rejection. Capability gate runs inside this helper alongside
    // the v0.9 / v0.10 gates.
    rate_limit: Option<portunus_core::RateLimit>,
) -> Result<(StatusCode, Json<PushRuleResponse>), ApiError> {
    use crate::operator::rbac;
    use portunus_core::rule_target;
    use std::str::FromStr;

    let raw_targets = body.targets.as_deref().expect("has_new true");

    // V-R6: health_check_interval_secs in 1..=3600 if Some.
    if let Some(hci) = body.health_check_interval_secs
        && (hci == 0 || hci > 3600)
    {
        return Err(OperatorError::HealthCheckIntervalOutOfRange { value: hci }.into());
    }

    // Empty list special-case: surface as `targets_empty` (V-R1).
    if raw_targets.is_empty() {
        return Err(OperatorError::TargetsInvalid(portunus_core::RuleTargetError::Empty).into());
    }

    // Build typed targets, defaulting `priority` to the row index.
    let typed: Vec<portunus_core::RuleTarget> = raw_targets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            Ok(portunus_core::RuleTarget {
                host: t.host.clone(),
                port: t.port,
                priority: t.priority.unwrap_or(u32::try_from(i).unwrap_or(u32::MAX)),
                proxy_protocol: parse_proxy_protocol_version(t.proxy_protocol.as_deref())?,
            })
        })
        .collect::<Result<_, OperatorError>>()?;

    // V-T1..V-T4 + V-R5 (FR-001, FR-005).
    rule_target::validate(&typed).map_err(OperatorError::TargetsInvalid)?;

    // Resolve protocol + client name.
    let client_name =
        portunus_core::ClientName::from_str(&body.client).map_err(OperatorError::InvalidName)?;
    let proto = parse_protocol_str(&body.protocol)?;

    // RBAC: same envelope as legacy push (FR-021 — targets are NOT
    // gated). enforce_push only inspects (client, listen-port range,
    // protocol).
    let push_proto = match proto {
        ProtocolWire::Tcp => rbac::PushProtocol::Tcp,
        ProtocolWire::Udp => rbac::PushProtocol::Udp,
    };
    let push_req = rbac::PushRequest {
        client: &client_name,
        listen_port_start: listen.start(),
        listen_port_end: listen.end(),
        protocol: push_proto,
    };
    let grants = state.operator_auth.grants_for(&identity.user_id);
    rbac::enforce_push(identity, &push_req, &grants).map_err(OperatorError::Rbac)?;

    if matches!(proto, ProtocolWire::Udp) && typed.iter().any(|t| t.proxy_protocol.is_some()) {
        return Err(OperatorError::ProxyProtocolValidation {
            code: "validation.proxy_protocol_on_unsupported_rule",
            message: "proxy_protocol is only valid on tcp rules".into(),
        }
        .into());
    }

    // R-007: client-version guard. Multi-target push (length >= 2) to
    // a client whose last-known Hello.client_version is < 0.7.0 is
    // refused before any rule mutation, since the v0.6.0 client can't
    // decode `Rule.targets` and would activate a broken single-target
    // rule with empty `target_host`.
    if typed.len() >= 2
        && let Some(v) = state.clients.client_version_by_name(&client_name).await
        && !version_at_least_0_7(&v)
    {
        return Err(OperatorError::MultiTargetUnsupportedByClient {
            client_name: client_name.clone(),
            client_version: v,
        }
        .into());
    }

    // 009-tls-sni-routing T028: SNI capability gate. A rule carrying
    // `sni_pattern` requires a v0.9+ client; older clients cannot
    // decode the field and would silently fall through to the
    // pre-009 plain-TCP forwarding plane.
    if sni_pattern.is_some()
        && let Some(v) = state.clients.client_version_by_name(&client_name).await
        && !version_at_least_0_9(&v)
    {
        return Err(OperatorError::SniUnsupportedByClient {
            client_name: client_name.clone(),
            client_version: v,
        }
        .into());
    }
    if typed.iter().any(|t| t.proxy_protocol.is_some()) {
        let Some(v) = state.clients.client_version_by_name(&client_name).await else {
            return Err(OperatorError::ProxyProtocolUnsupportedByClient {
                client_name: client_name.clone(),
                client_version: "unknown".into(),
            }
            .into());
        };
        if !version_at_least_0_10(&v) {
            return Err(OperatorError::ProxyProtocolUnsupportedByClient {
                client_name: client_name.clone(),
                client_version: v,
            }
            .into());
        }
    }

    // 011-rate-limiting-qos T008/T016: rate-limit capability gate. A
    // rule carrying any `rate_limit` field requires a v0.11+ client;
    // older clients silently drop `Rule.rate_limit = 12` on decode and
    // would activate uncapped, violating the operator-visible
    // contract. An unknown / missing client_version gates conservatively.
    if rate_limit.is_some() {
        let Some(v) = state.clients.client_version_by_name(&client_name).await else {
            return Err(OperatorError::RateLimitUnsupportedByClient {
                client_name: client_name.clone(),
                client_version: "unknown".into(),
            }
            .into());
        };
        if !version_at_least_0_11(&v) {
            return Err(OperatorError::RateLimitUnsupportedByClient {
                client_name: client_name.clone(),
                client_version: v,
            }
            .into());
        }
    }

    // Phase 3 (T022): hand off to the multi-target push helper which
    // emits the new wire shape and waits for activation.
    let proto_internal = match proto {
        ProtocolWire::Tcp => crate::rules::Protocol::Tcp,
        ProtocolWire::Udp => crate::rules::Protocol::Udp,
    };
    let rule = cli::push_rule_multi_target(
        state,
        identity,
        client_name,
        listen,
        typed,
        body.health_check_interval_secs,
        proto_internal,
        body.prefer_ipv6,
        state.range_rule_max_ports,
        timeout,
        sni_pattern,
        rate_limit,
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
            sni_pattern: rule.sni_pattern.clone(),
            // 011-rate-limiting-qos T016: echo the persisted cap
            // envelope so operators can confirm what landed.
            rate_limit: rule.rate_limit.clone(),
        }),
    ))
}

/// 009-tls-sni-routing: legacy single-target shape carrying
/// `sni_pattern` is internally upgraded to the 1-element `targets[]`
/// form so the SNI selector reaches `Rule.sni_pattern` and the wire
/// emitter via `cli::push_rule_multi_target`. Validation, RBAC and
/// the v0.9 capability gate run here just like in `push_multi_target`.
#[allow(clippy::too_many_arguments)]
async fn push_legacy_with_sni(
    state: &Arc<AppState>,
    identity: &OperatorIdentity,
    raw_client: &str,
    protocol: &str,
    listen: PortRange,
    target_host: &str,
    target: PortRange,
    prefer_ipv6: Option<bool>,
    timeout: Duration,
    sni_pattern: String,
) -> Result<(StatusCode, Json<PushRuleResponse>), ApiError> {
    use crate::operator::rbac;
    use std::str::FromStr;

    let client_name =
        portunus_core::ClientName::from_str(raw_client).map_err(OperatorError::InvalidName)?;
    let proto_wire = parse_protocol_str(protocol)?;
    let push_proto = match proto_wire {
        ProtocolWire::Tcp => rbac::PushProtocol::Tcp,
        ProtocolWire::Udp => rbac::PushProtocol::Udp,
    };
    let push_req = rbac::PushRequest {
        client: &client_name,
        listen_port_start: listen.start(),
        listen_port_end: listen.end(),
        protocol: push_proto,
    };
    let grants = state.operator_auth.grants_for(&identity.user_id);
    rbac::enforce_push(identity, &push_req, &grants).map_err(OperatorError::Rbac)?;

    // SNI rules are TCP-only — the grammar gate already rejects UDP
    // before we get here, but be defensive.
    debug_assert!(matches!(proto_wire, ProtocolWire::Tcp));

    // Capability gate (T028): client must be >= 0.9.0.
    if let Some(v) = state.clients.client_version_by_name(&client_name).await
        && !version_at_least_0_9(&v)
    {
        return Err(OperatorError::SniUnsupportedByClient {
            client_name: client_name.clone(),
            client_version: v,
        }
        .into());
    }

    let synth = vec![portunus_core::RuleTarget {
        host: target_host.to_string(),
        port: target.start(),
        priority: 0,
        proxy_protocol: None,
    }];
    portunus_core::rule_target::validate(&synth).map_err(OperatorError::TargetsInvalid)?;

    let proto_internal = match proto_wire {
        ProtocolWire::Tcp => crate::rules::Protocol::Tcp,
        ProtocolWire::Udp => crate::rules::Protocol::Udp,
    };
    let rule = cli::push_rule_multi_target(
        state,
        identity,
        client_name,
        listen,
        synth,
        None,
        proto_internal,
        prefer_ipv6,
        state.range_rule_max_ports,
        timeout,
        Some(sni_pattern),
        // 011-rate-limiting-qos: legacy `target_host`/`target_port` shape
        // does not currently carry a `rate_limit` field; capped rules
        // must use the `targets[]` shape (push_multi_target).
        None,
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
            sni_pattern: rule.sni_pattern.clone(),
            // 011-rate-limiting-qos T016: echo the persisted cap
            // envelope so operators can confirm what landed.
            rate_limit: rule.rate_limit.clone(),
        }),
    ))
}

/// Internal protocol enum used by the multi-target helper. Mirrors
/// `portunus_proto::v1::Protocol` but kept private to avoid leaking
/// proto types into the HTTP module.
enum ProtocolWire {
    Tcp,
    Udp,
}

fn parse_protocol_str(s: &str) -> Result<ProtocolWire, OperatorError> {
    match s.to_ascii_lowercase().as_str() {
        "tcp" => Ok(ProtocolWire::Tcp),
        "udp" => Ok(ProtocolWire::Udp),
        _ => Err(OperatorError::InvalidProtocol(s.to_string())),
    }
}

/// Semver-prefix comparison: `version >= 0.7.0` for the multi-target
/// version guard (R-007). Non-strict semver — strips any `-suffix` /
/// `+meta` and parses `MAJOR.MINOR` as `(u32, u32)`. Returns `false`
/// for malformed input (treats unknown / unparseable versions as
/// "not new enough" so the safe default is to gate).
fn version_at_least_0_7(version: &str) -> bool {
    let trimmed = version.split(['-', '+']).next().unwrap_or("");
    let mut parts = trimmed.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor) >= (0, 7)
}

/// 009-tls-sni-routing T027: semver-prefix comparison `version >= 0.9.0`
/// for the SNI capability guard (FR-018). Same parsing semantics as
/// `version_at_least_0_7` — malformed input gates conservatively.
fn version_at_least_0_9(version: &str) -> bool {
    let trimmed = version.split(['-', '+']).next().unwrap_or("");
    let mut parts = trimmed.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor) >= (0, 9)
}

fn version_at_least_0_10(version: &str) -> bool {
    let trimmed = version.split(['-', '+']).next().unwrap_or("");
    let mut parts = trimmed.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor) >= (0, 10)
}

/// 011-rate-limiting-qos T008: semver-prefix comparison
/// `version >= 0.11.0` for the rate-limit capability guard
/// (FR-006). Same parsing semantics as `version_at_least_0_7` —
/// malformed input gates conservatively. Used by both the HTTP push
/// handler (T016) and the per-owner cap PUT/DELETE handler (T028).
pub(crate) fn version_at_least_0_11(version: &str) -> bool {
    let trimmed = version.split(['-', '+']).next().unwrap_or("");
    let mut parts = trimmed.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor) >= (0, 11)
}

/// 011-rate-limiting-qos T016: convert the operator-API rate-limit
/// request body into a `portunus_core::RateLimit` envelope, performing
/// the four documented validation checks against the operator-visible
/// subcategory codes:
/// - `validation.rate_limit_burst_unsupported` — `concurrent_connections_burst`
///   is reserved (concurrent caps are hard ceilings, not buckets).
/// - `validation.rate_limit_cap_zero` — any cap value is 0.
/// - `validation.rate_limit_burst_without_rate` — a `*_burst` field
///   was supplied without its companion rate.
/// - `validation.rate_limit_burst_range` — a `*_burst` value falls
///   outside `[rate / 100, rate * 60]`.
///
/// Returns `Ok(None)` when the operator omitted `rate_limit` from the
/// body (= uncapped, byte-stable v0.10 path). Returns
/// `Ok(Some(envelope))` when the body parsed and validated cleanly.
fn parse_rate_limit_body(
    body: Option<&RateLimitBody>,
) -> Result<Option<portunus_core::RateLimit>, OperatorError> {
    let Some(body) = body else {
        return Ok(None);
    };
    // 1. Reserved burst slot — concurrent is a hard ceiling, not a
    //    bucket; surface a stable subcategory the operator can
    //    pattern-match on.
    if body.concurrent_connections_burst.is_some() {
        return Err(OperatorError::RateLimitValidation {
            code: "validation.rate_limit_burst_unsupported",
            message: "concurrent_connections_burst is reserved; concurrent caps are a hard ceiling and cannot be bursted".into(),
        });
    }
    // 2. Project onto the core envelope.
    let envelope = portunus_core::RateLimit {
        bandwidth_in_bps: body.bandwidth_in_bps,
        bandwidth_out_bps: body.bandwidth_out_bps,
        new_connections_per_sec: body.new_connections_per_sec,
        concurrent_connections: body.concurrent_connections,
        bandwidth_in_burst: body.bandwidth_in_burst,
        bandwidth_out_burst: body.bandwidth_out_burst,
        new_connections_burst: body.new_connections_burst,
    };
    // 3. Validate (cap_zero / burst_without_rate / burst_range).
    portunus_core::rate_limit::validate(&envelope).map_err(|e| match e {
        portunus_core::rate_limit::RateLimitError::CapZero { .. } => {
            OperatorError::RateLimitValidation {
                code: "validation.rate_limit_cap_zero",
                message: e.to_string(),
            }
        }
        portunus_core::rate_limit::RateLimitError::BurstWithoutRate { .. } => {
            OperatorError::RateLimitValidation {
                code: "validation.rate_limit_burst_without_rate",
                message: e.to_string(),
            }
        }
        portunus_core::rate_limit::RateLimitError::BurstRange { .. } => {
            OperatorError::RateLimitValidation {
                code: "validation.rate_limit_burst_range",
                message: e.to_string(),
            }
        }
    })?;
    Ok(Some(envelope))
}

fn parse_proxy_protocol_version(
    raw: Option<&str>,
) -> Result<Option<portunus_core::ProxyProtocolVersion>, OperatorError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "v1" => Ok(Some(portunus_core::ProxyProtocolVersion::V1)),
        "v2" => Ok(Some(portunus_core::ProxyProtocolVersion::V2)),
        other => Err(OperatorError::ProxyProtocolValidation {
            code: "validation.proxy_protocol_invalid",
            message: format!("proxy_protocol must be `v1` or `v2`, got `{other}`"),
        }),
    }
}

/// 009-tls-sni-routing T029: validate + lowercase a candidate
/// `sni_pattern`. Accepts:
/// - exact host: `example.com`, `api.svc.example.com`
/// - single-label wildcard: `*.example.com`, `*.svc.example.com`
///
/// Rejects: empty / whitespace; total length > 253; any label > 63;
/// labels with leading/trailing hyphens; characters outside
/// `[a-z0-9-]` (post-lowercase); `*.x` where `x` has no dot
/// (single-label wildcard requires at least one inner label per
/// FR-011); `*` anywhere except as the leftmost full label;
/// IDN/punycode is accepted as raw ASCII (operator pre-encodes per
/// research.md R-001).
///
/// Returns the normalised lowercased pattern on success.
fn validate_sni_pattern(raw: &str) -> Result<String, OperatorError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(OperatorError::SniValidation {
            code: "validation.sni_pattern_malformed",
            message: "sni_pattern is empty".into(),
        });
    }
    if trimmed.len() > 253 {
        return Err(OperatorError::SniValidation {
            code: "validation.sni_pattern_malformed",
            message: format!("sni_pattern total length {} exceeds 253", trimmed.len()),
        });
    }
    let lower = trimmed.to_ascii_lowercase();
    let (is_wildcard, body) = if let Some(rest) = lower.strip_prefix("*.") {
        (true, rest)
    } else {
        (false, lower.as_str())
    };
    if body.is_empty() {
        return Err(OperatorError::SniValidation {
            code: "validation.sni_pattern_malformed",
            message: "sni_pattern body is empty after wildcard prefix".into(),
        });
    }
    // Wildcard requires at least one inner dot (single-label wildcard
    // must have a multi-label parent: `*.example.com` ok, `*.com` NOT ok
    // per design §wildcards / FR-011 — top-level domain wildcards are
    // refused as too broad).
    if is_wildcard && !body.contains('.') {
        return Err(OperatorError::SniValidation {
            code: "validation.sni_pattern_malformed",
            message: format!("wildcard sni_pattern `{trimmed}` requires a multi-label parent"),
        });
    }
    // Validate every label.
    for label in body.split('.') {
        if label.is_empty() {
            return Err(OperatorError::SniValidation {
                code: "validation.sni_pattern_malformed",
                message: format!("sni_pattern `{trimmed}` has an empty label"),
            });
        }
        if label.len() > 63 {
            return Err(OperatorError::SniValidation {
                code: "validation.sni_pattern_malformed",
                message: format!("sni_pattern label `{label}` exceeds 63 chars"),
            });
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(OperatorError::SniValidation {
                code: "validation.sni_pattern_malformed",
                message: format!("sni_pattern label `{label}` has leading/trailing hyphen"),
            });
        }
        for ch in label.chars() {
            if !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-') {
                return Err(OperatorError::SniValidation {
                    code: "validation.sni_pattern_malformed",
                    message: format!("sni_pattern label `{label}` has illegal character `{ch}`"),
                });
            }
        }
    }
    Ok(if is_wildcard {
        format!("*.{body}")
    } else {
        body.to_string()
    })
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

async fn put_rule(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(rule_id): Path<u64>,
    Json(body): Json<PushRuleBody>,
) -> Result<Json<PushRuleResponse>, ApiError> {
    let existing = state.rules.get(RuleId(rule_id)).await.ok_or(ApiError::new(
        StatusCode::NOT_FOUND,
        "rule_not_found",
        "rule_not_found",
    ))?;
    crate::operator::rbac::enforce_read(&identity, &existing.owner_user_id)?;

    let has_legacy = body.target_host.is_some() || body.target_port.is_some();
    let has_new = body.targets.is_some();
    if has_legacy && has_new {
        return Err(OperatorError::RuleShapeConflict.into());
    }
    if !has_legacy && !has_new {
        return Err(OperatorError::RuleShapeMissing.into());
    }
    if body.listen_port_end.is_some() != body.target_port_end.is_some() {
        return Err(OperatorError::RangeInvalid(
            "mismatched_range: listen_port_end and target_port_end must be present together".into(),
        )
        .into());
    }
    let listen =
        build_range(body.listen_port, body.listen_port_end).map_err(OperatorError::RangeInvalid)?;
    let rate_limit = parse_rate_limit_body(body.rate_limit.as_ref())?;
    let existing_listen = existing.listen_range();
    if body.client != existing.client_name.as_str()
        || !body
            .protocol
            .eq_ignore_ascii_case(existing.protocol.as_str())
        || listen != existing_listen
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "validation.rule_update_shape_mismatch",
            "PUT /v1/rules/{id} only supports rate_limit hot-updates; client/listen/protocol must stay unchanged",
        ));
    }
    if has_legacy {
        let target_host = body.target_host.as_deref().unwrap_or_default();
        let target_port = body.target_port.unwrap_or_default();
        if target_host != existing.target_host || target_port != existing.target_port {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "validation.rule_update_shape_mismatch",
                "PUT /v1/rules/{id} only supports rate_limit hot-updates; target must stay unchanged",
            ));
        }
        if rate_limit.is_some() {
            return Err(OperatorError::RateLimitValidation {
                code: "validation.rate_limit_on_legacy_shape",
                message:
                    "rate_limit requires the `targets[]` request shape; legacy `target_host`/`target_port` is not supported"
                        .into(),
            }
            .into());
        }
    } else {
        let typed = body.targets.as_deref().expect("has_new true");
        let same_targets = typed.len() == existing.targets_view().len()
            && typed
                .iter()
                .zip(existing.targets_view())
                .all(|(incoming, current)| {
                    incoming.host == current.host
                        && incoming.port == current.port
                        && incoming.priority.unwrap_or(0) == current.priority
                });
        if !same_targets {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "validation.rule_update_shape_mismatch",
                "PUT /v1/rules/{id} only supports rate_limit hot-updates; targets must stay unchanged",
            ));
        }
    }

    let updated =
        cli::update_rule_rate_limit(&state, RuleId(rule_id), rate_limit, DEFAULT_ACK_TIMEOUT)
            .await?;
    Ok(Json(PushRuleResponse {
        rule_id: updated.id.0,
        status: match &updated.state {
            crate::rules::RuleState::Pending => "Pending".to_string(),
            crate::rules::RuleState::Active => "Active".to_string(),
            crate::rules::RuleState::Failed { reason } => format!("Failed:{reason}"),
            crate::rules::RuleState::Removed => "Removed".to_string(),
        },
        target_host: updated.target_host.clone(),
        prefer_ipv6: updated.prefer_ipv6.unwrap_or(false),
        protocol: updated.protocol.as_str().to_string(),
        owner: updated.owner_user_id.to_string(),
        sni_pattern: updated.sni_pattern.clone(),
        rate_limit: updated.rate_limit.clone(),
    }))
}

async fn get_rules(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let client = params.get("client").map(String::as_str);
    let mut rules = cli::list_rules(&state, client).await?;
    // T043: superadmin sees all (with optional `?owner=` filter);
    // everyone else only sees their own.
    if identity.role != portunus_auth::OperatorRole::Superadmin {
        rules.retain(|r| r.owner_user_id == identity.user_id);
    } else if let Some(o) = params.get("owner") {
        rules.retain(|r| r.owner_user_id.as_str() == o);
    }
    // 007-multi-target-failover T038: augment each rule's targets[]
    // with per-target health from the stats cache when available.
    // Single-target rules emit one element with `health: null` so
    // generic operator tooling can read targets[0] uniformly.
    let mut out = Vec::with_capacity(rules.len());
    for r in rules {
        out.push(rule_with_health(&state, r).await);
    }
    Ok(Json(serde_json::Value::Array(out)))
}

/// 007-multi-target-failover T038: serialize a rule and augment each
/// entry in `targets[]` with the live `health` snapshot from the
/// stats cache. Single-target rules synthesize a one-element
/// `targets[]` with `health: null` so the wire shape is uniform.
async fn rule_with_health(state: &Arc<AppState>, rule: Rule) -> serde_json::Value {
    let snap = state.stats_cache.get(rule.id).await;
    let mut value = serde_json::to_value(&rule).unwrap_or(serde_json::Value::Null);
    let serde_json::Value::Object(ref mut map) = value else {
        return value;
    };

    // Build canonical targets[] view (legacy single-target rules
    // synthesise a one-element list mirroring `targets_view()`).
    let view = rule.targets_view();
    let mut targets_json = Vec::with_capacity(view.len());
    for (idx, t) in view.iter().enumerate() {
        let health = snap
            .as_ref()
            .and_then(|s| s.per_target.iter().find(|p| p.index as usize == idx))
            .map(|p| {
                serde_json::json!({
                    "healthy": p.health == 0,
                    "consecutive_failures": p.consecutive_failures,
                    "last_failure_at_unix_ms": p.last_failure_at_unix_ms,
                    "last_success_at_unix_ms": p.last_success_at_unix_ms,
                })
            });
        targets_json.push(serde_json::json!({
            "host": t.host,
            "port": t.port,
            "priority": t.priority,
            "proxy_protocol": t.proxy_protocol,
            "health": health,
        }));
    }
    map.insert(
        "targets".to_string(),
        serde_json::Value::Array(targets_json),
    );
    value
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
    // 007-multi-target-failover T036: when `?per_target=true`, surface
    // the per-target snapshot stamped onto the cache entry by
    // `RuleStatsCache::observe`. Default behavior (no query param)
    // strips `per_target` from the JSON via `skip_serializing_if`. I-3
    // (single-target rules emit empty per_target) means the array is
    // present-but-empty for legacy rules.
    let per_target_requested = params
        .get("per_target")
        .is_some_and(|v| matches!(v.as_str(), "true" | "1" | "yes"));
    if per_target_requested && let serde_json::Value::Object(ref mut map) = body {
        let per_target = snap.per_target.clone();
        map.insert(
            "per_target".to_string(),
            serde_json::to_value(&per_target).map_err(|e| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "internal".into(),
                message: e.to_string(),
            })?,
        );
    }
    Ok(Json(body))
}

// ---------------------------------------------------------------------------
// Advertised-endpoint settings (014-advertised-endpoint)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize)]
struct AdvertisedEndpointBody {
    /// `null`/empty clears the override.
    advertised_endpoint: Option<String>,
}

#[derive(serde::Serialize)]
struct AdvertisedEndpointView {
    r#override: Option<String>,
    effective: Option<String>,
    source: Option<crate::advertised::EndpointSource>,
    diagnostic: Option<String>,
}

async fn get_advertised_endpoint(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
) -> Result<Json<AdvertisedEndpointView>, ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    let override_value = state
        .settings
        .get_advertised_endpoint()
        .map_err(map_store_err)?;
    let view =
        match crate::advertised::resolve_advertised_endpoint(&crate::advertised::ResolveInputs {
            override_value: override_value.clone(),
            seed: state.advertised_seed.clone(),
            req_host: None,
            control_port: state.control_port,
            san: &state.cert_san,
        }) {
            Ok(r) => AdvertisedEndpointView {
                r#override: override_value,
                effective: Some(r.endpoint),
                source: Some(r.source),
                diagnostic: None,
            },
            Err(e) => AdvertisedEndpointView {
                r#override: override_value,
                effective: None,
                source: None,
                diagnostic: Some(e.to_string()),
            },
        };
    Ok(Json(view))
}

async fn put_advertised_endpoint(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Json(body): Json<AdvertisedEndpointBody>,
) -> Result<Json<AdvertisedEndpointView>, ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    let value = body.advertised_endpoint.filter(|s| !s.is_empty());
    if let Some(v) = &value {
        let (host, _) = crate::advertised::grammar::validate_authority(v).map_err(|reason| {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "endpoint_invalid", reason)
        })?;
        if !state.cert_san.covers(host) {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "endpoint_not_in_cert_san",
                format!(
                    "host {host} is not covered by the server certificate SAN; \
                     reissue/redeploy the cert to cover it"
                ),
            ));
        }
    }
    state
        .settings
        .set_advertised_endpoint(value)
        .map_err(map_store_err)?;
    get_advertised_endpoint(State(state), Extension(identity)).await
}

fn map_store_err(e: crate::store::StoreError) -> ApiError {
    tracing::warn!(event = "operator.store_error", error = %e);
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store_error",
        "store_error",
    )
}

// ---------------------------------------------------------------------------

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

impl From<portunus_auth::RbacError> for ApiError {
    fn from(e: portunus_auth::RbacError) -> Self {
        Self::from(OperatorError::Rbac(e))
    }
}

impl From<OperatorError> for ApiError {
    fn from(e: OperatorError) -> Self {
        let status = match &e {
            OperatorError::ClientAlreadyExists(_)
            | OperatorError::Auth(portunus_auth::AuthError::ClientAlreadyExists(_))
            | OperatorError::PortInUse { .. }
            // 009-tls-sni-routing (operator-api.md §1): SNI overlap
            // failures share the 409 family with PortInUse — the
            // listener committed to a shape that the candidate would
            // violate.
            | OperatorError::SniRouteDuplicate { .. }
            | OperatorError::SniFallbackDuplicate { .. }
            | OperatorError::LegacyToSniUnsupported { .. }
            | OperatorError::ClientNotRevoked(_) => StatusCode::CONFLICT,
            OperatorError::InvalidName(_)
            | OperatorError::InvalidProtocol(_)
            | OperatorError::InvalidTarget(_)
            | OperatorError::InvalidTargetHost { .. }
            | OperatorError::InvalidClientAddress(_)
            | OperatorError::ExceedsCap { .. }
            | OperatorError::RangeInvalid(_)
            // 007-multi-target-failover: shape + targets validation
            // (operator-api.md §1) — all 400 with stable codes.
            | OperatorError::RuleShapeConflict
            | OperatorError::RuleShapeMissing
            | OperatorError::TargetsInvalid(_)
            | OperatorError::HealthCheckIntervalOutOfRange { .. }
            // 009-tls-sni-routing T029: sni_pattern grammar / applicability
            // failures share the 400 family.
            | OperatorError::SniValidation { .. }
            | OperatorError::ProxyProtocolValidation { .. }
            // 011-rate-limiting-qos T016: cap envelope validation
            // failures (cap_zero, burst_without_rate, burst_range,
            // burst_unsupported, on_legacy_shape) are 400 with stable
            // `validation.rate_limit_*` codes.
            | OperatorError::RateLimitValidation { .. } => StatusCode::BAD_REQUEST,
            OperatorError::ClientNotConnected(_)
            | OperatorError::ActivationFailed(_)
            // 004-udp-forward T019: capability mismatch surfaces as 422
            // (client connected but cannot fulfil the rule) — distinct
            // from 400 (operator's input was syntactically wrong).
            | OperatorError::UnsupportedProtocol { .. }
            // 007-multi-target-failover (R-007): client connected but
            // its version cannot decode the new wire shape. Same
            // semantic class as UnsupportedProtocol.
            | OperatorError::MultiTargetUnsupportedByClient { .. }
            // 009-tls-sni-routing (T028): SNI capability gate mirrors
            // the v0.7 multi-target gate — same semantic class.
            | OperatorError::SniUnsupportedByClient { .. }
            | OperatorError::ProxyProtocolUnsupportedByClient { .. }
            // 011-rate-limiting-qos (T008): rate-limit capability gate
            // is the same semantic class as the surrounding gates —
            // client connected but its version cannot honour the new
            // field. 422 mirrors v0.9 / v0.10.
            | OperatorError::RateLimitUnsupportedByClient { .. }
            // 010-advertised-endpoint: malformed/uncovered config or no
            // SAN-covered candidate — operator-config-level failure that
            // cannot be satisfied as-is. 422 mirrors the capability gates.
            | OperatorError::AdvertisedEndpoint(_) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            OperatorError::AckTimeout => StatusCode::GATEWAY_TIMEOUT,
            OperatorError::RuleNotFound
            | OperatorError::ClientNotFound(_)
            | OperatorError::ClientIdNotFound(_)
            | OperatorError::ClientIdInvalid => StatusCode::NOT_FOUND,
            // 005-multi-user-rbac: RBAC failures use the auth_layer's
            // shared status table (single source of truth).
            OperatorError::Rbac(rb) => crate::operator::auth_layer::rbac_status(rb),
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = match &e {
            OperatorError::Store(inner) => {
                tracing::warn!(event = "operator.store_error", error = %inner);
                "store_error".to_string()
            }
            _ => e.to_string(),
        };
        Self {
            status,
            code: e.code().to_string(),
            message,
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
    async fn loopback_bind_is_valid() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
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

    // ---- M1 security-hygiene: store-error redaction in HTTP responses ----
    //
    // `StoreError::Internal { message }` Display is `"internal: {message}"`.
    // Both `map_store_err` (Site B) and `ApiError::from(OperatorError::Store(…))`
    // (Site A) must NOT propagate that raw text into the response body.

    const SENTINEL: &str = "SENSITIVE_SQL_abc123";

    #[test]
    fn map_store_err_redacts_internal_message() {
        let store_err = crate::store::StoreError::Internal {
            message: SENTINEL.into(),
        };
        let api_err = map_store_err(store_err);
        assert_eq!(
            api_err.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "status must remain INTERNAL_SERVER_ERROR"
        );
        assert_eq!(api_err.code, "store_error", "code must be store_error");
        assert_eq!(
            api_err.message, "store_error",
            "message must be generic, not raw store detail"
        );
        assert!(
            !api_err.message.contains(SENTINEL),
            "sentinel must not appear in message; got: {:?}",
            api_err.message
        );
    }

    #[test]
    fn operator_error_store_from_redacts_message() {
        let store_err = crate::store::StoreError::Internal {
            message: SENTINEL.into(),
        };
        let api_err = ApiError::from(OperatorError::Store(store_err));
        assert_eq!(
            api_err.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "status must remain INTERNAL_SERVER_ERROR"
        );
        assert_eq!(api_err.code, "store_error", "code must be store_error");
        assert_eq!(
            api_err.message, "store_error",
            "message must be generic, not raw store detail"
        );
        assert!(
            !api_err.message.contains(SENTINEL),
            "sentinel must not appear in message; got: {:?}",
            api_err.message
        );
    }

    // Positive control: a non-Store OperatorError variant keeps its informative message.
    // `ResolveEndpointError::NoSanCoveredCandidate` is the cheapest zero-field variant.
    #[test]
    fn operator_error_non_store_message_preserved() {
        let err = OperatorError::AdvertisedEndpoint(
            crate::advertised::ResolveEndpointError::NoSanCoveredCandidate,
        );
        let api_err = ApiError::from(err);
        assert_eq!(
            api_err.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "AdvertisedEndpoint → 422"
        );
        // The message must be informative (non-empty, not the generic "store_error").
        assert_ne!(
            api_err.message, "store_error",
            "non-Store variant must keep its own message"
        );
        assert!(
            !api_err.message.is_empty(),
            "non-Store variant must have an informative message"
        );
    }
}
