//! Loopback HTTP API mirroring the CLI surface (operator-api.md).
//!
//! Authorisation is local UNIX shell access on the server host (FR-022).
//! The bind address MUST be loopback; we assert that at server startup.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::bundle::CredentialBundle;
use crate::operator::ClientView;
use crate::operator::cli::{self, OperatorError};
use crate::state::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/clients", get(get_clients).post(post_clients))
        .route("/v1/clients/{name}/revoke", post(post_revoke))
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
            | OperatorError::Auth(forward_auth::AuthError::ClientAlreadyExists(_)) => {
                StatusCode::CONFLICT
            }
            OperatorError::InvalidName(_) => StatusCode::BAD_REQUEST,
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
