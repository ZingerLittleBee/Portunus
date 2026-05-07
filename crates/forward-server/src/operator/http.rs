//! Loopback HTTP API mirroring the CLI surface (operator-api.md).
//!
//! Authorisation is local UNIX shell access on the server host (FR-022).
//! The bind address MUST be loopback; we assert that at server startup.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
};
use forward_core::{PortRange, RuleId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::bundle::CredentialBundle;
use crate::operator::ClientView;
use crate::operator::cli::{self, OperatorError};
use crate::rules::Rule;
use crate::state::AppState;

const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(2);

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/clients", get(get_clients).post(post_clients))
        .route("/v1/clients/{name}/revoke", post(post_revoke))
        .route("/v1/rules", get(get_rules).post(post_rules))
        .route("/v1/rules/{rule_id}", delete(delete_rule))
        .route("/v1/rules/{rule_id}/stats", get(get_rule_stats))
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
    /// Optional override of the per-request ack timeout in seconds.
    #[serde(default)]
    ack_timeout_secs: Option<u64>,
}

fn default_protocol() -> String {
    "tcp".to_string()
}

#[derive(Debug, Serialize)]
struct PushRuleResponse {
    rule_id: u64,
}

async fn post_rules(
    State(state): State<Arc<AppState>>,
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
        &body.client,
        listen,
        &body.target_host,
        target,
        &body.protocol,
        state.range_rule_max_ports,
        timeout,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(PushRuleResponse { rule_id: rule.id.0 }),
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
    Path(rule_id): Path<u64>,
) -> Result<StatusCode, ApiError> {
    cli::remove_rule(&state, RuleId(rule_id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_rules(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<Rule>>, ApiError> {
    let client = params.get("client").map(String::as_str);
    let rules = cli::list_rules(&state, client).await?;
    Ok(Json(rules))
}

async fn get_rule_stats(
    State(state): State<Arc<AppState>>,
    Path(rule_id): Path<u64>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let snap = cli::rule_stats(&state, RuleId(rule_id)).await?;
    let mut body = serde_json::to_value(&snap).map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "internal".into(),
        message: e.to_string(),
    })?;
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

impl From<OperatorError> for ApiError {
    fn from(e: OperatorError) -> Self {
        let status = match &e {
            OperatorError::ClientAlreadyExists(_)
            | OperatorError::Auth(forward_auth::AuthError::ClientAlreadyExists(_))
            | OperatorError::PortInUse { .. } => StatusCode::CONFLICT,
            OperatorError::InvalidName(_)
            | OperatorError::InvalidProtocol(_)
            | OperatorError::InvalidTarget(_)
            | OperatorError::ExceedsCap { .. }
            | OperatorError::RangeInvalid(_) => StatusCode::BAD_REQUEST,
            OperatorError::ClientNotConnected(_) | OperatorError::ActivationFailed(_) => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            OperatorError::AckTimeout => StatusCode::GATEWAY_TIMEOUT,
            OperatorError::RuleNotFound => StatusCode::NOT_FOUND,
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
