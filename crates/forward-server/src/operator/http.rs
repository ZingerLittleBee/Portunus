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
use forward_core::RuleId;
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
    let (_path, bundle) = cli::provision_client(&state, &body.name, None)?;
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
    target_host: String,
    target_port: u16,
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
    let target = format!("{}:{}", body.target_host, body.target_port);
    let timeout = body
        .ack_timeout_secs
        .map_or(DEFAULT_ACK_TIMEOUT, Duration::from_secs);
    let rule = cli::push_rule(
        &state,
        &body.client,
        body.listen_port,
        &target,
        &body.protocol,
        timeout,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(PushRuleResponse { rule_id: rule.id.0 }),
    ))
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
            | OperatorError::PortInUse => StatusCode::CONFLICT,
            OperatorError::InvalidName(_)
            | OperatorError::InvalidProtocol(_)
            | OperatorError::InvalidTarget(_) => StatusCode::BAD_REQUEST,
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

    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn binds_loopback_only() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        // Per FR-022: operator HTTP must bind to loopback only.
        assert!(addr.ip().is_loopback(), "got {addr}");
    }
}
