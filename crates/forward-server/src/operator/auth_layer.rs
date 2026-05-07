//! Operator HTTP auth middleware (005-multi-user-rbac, US1 / T019, T020).
//!
//! Wraps every `/v1/*` route. Extracts the bearer token from the
//! `Authorization` header, calls
//! [`forward_auth::OperatorAuthenticator::verify`], and either:
//!
//! - On success: inserts the recovered [`forward_auth::OperatorIdentity`]
//!   into request extensions so handlers can read it via
//!   `req.extensions().get::<OperatorIdentity>()`. Emits one structured
//!   INFO log (`event = "operator.allow"`).
//! - On failure: returns a JSON error body matching
//!   `contracts/operator-api.md` § "Authentication". Emits one
//!   structured WARN log (`event = "operator.deny"`).
//!
//! Constitution Principle I "single seam": every operator request flows
//! through one place. Constitution Principle IV "no raw credentials in
//! logs": the audit log emitter takes only the post-verify identity and
//! the URI; the raw token never traverses this code path.

use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use forward_auth::RbacError;
use serde::Serialize;
use tracing::{info, warn};

use crate::state::AppState;

/// Axum middleware fn: authenticate + inject identity + audit-log.
///
/// Mount via
/// `Router::layer(axum::middleware::from_fn_with_state(state, auth_middleware))`.
pub async fn auth_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();

    // 1. Bootstrap-required check (T020). If no superadmin exists yet,
    //    every operator request 503's. Data plane (gRPC) is unaffected.
    if !state.operator_auth.has_any_superadmin() {
        bump_request(&state, "deny", "bootstrap_required");
        warn!(
            event = "operator.deny",
            actor = "_anonymous",
            method = %method,
            path = %path,
            outcome = "deny",
            reason = "bootstrap_required",
        );
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            RbacError::BootstrapRequired,
            "no superadmin exists; run `forward-server bootstrap-superadmin` or set `operator_token` in server.toml",
        );
    }

    // 2. Extract the bearer token.
    let token = match extract_bearer(&req) {
        Ok(t) => t,
        Err(reason) => {
            bump_request(&state, "deny", reason.code());
            warn!(
                event = "operator.deny",
                actor = "_anonymous",
                method = %method,
                path = %path,
                outcome = "deny",
                reason = reason.code(),
            );
            return error_response(
                StatusCode::UNAUTHORIZED,
                reason,
                "missing or malformed Authorization header",
            );
        }
    };

    // 3. Verify against the store.
    let identity = match state.operator_auth.verify(&token) {
        Ok(id) => id,
        Err(e) => {
            let status = http_status_for(&e);
            bump_request(&state, "deny", e.code());
            warn!(
                event = "operator.deny",
                actor = "_anonymous",
                method = %method,
                path = %path,
                outcome = "deny",
                reason = e.code(),
            );
            return error_response(status, e, "invalid or revoked credential");
        }
    };

    // 4. Audit-log success and inject identity into request extensions.
    bump_request(&state, "allow", "ok");
    info!(
        event = "operator.allow",
        actor = %identity.user_id,
        role = ?identity.role,
        method = %method,
        path = %path,
        outcome = "allow",
    );
    req.extensions_mut().insert(identity);

    next.run(req).await
}

/// 005-multi-user-rbac T045: bump the auth-layer's outcome/reason
/// counter. Bounded label set: `outcome` ∈ {allow, deny}; `reason` is
/// either `"ok"` or the static `RbacError::code()` string. Cardinality
/// stays predictable regardless of traffic shape (R-009).
fn bump_request(state: &AppState, outcome: &str, reason: &str) {
    state
        .metrics
        .operator_requests_total
        .with_label_values(&[outcome, reason])
        .inc();
}

fn extract_bearer(req: &Request<Body>) -> Result<String, RbacError> {
    let value = req
        .headers()
        .get(header::AUTHORIZATION)
        .ok_or(RbacError::Unauthenticated)?
        .to_str()
        .map_err(|_| RbacError::Unauthenticated)?;
    let prefix = "Bearer ";
    if !value.starts_with(prefix) {
        return Err(RbacError::Unauthenticated);
    }
    let token = value[prefix.len()..].trim();
    if token.is_empty() {
        return Err(RbacError::Unauthenticated);
    }
    Ok(token.to_string())
}

fn http_status_for(e: &RbacError) -> StatusCode {
    match e {
        RbacError::Unauthenticated | RbacError::CredentialInvalid | RbacError::UserDisabled => {
            StatusCode::UNAUTHORIZED
        }
        RbacError::ClientNotGranted
        | RbacError::PortOutsideGrant
        | RbacError::ProtocolNotGranted
        | RbacError::NotOwner
        | RbacError::RoleRequired => StatusCode::FORBIDDEN,
        RbacError::BootstrapRequired => StatusCode::SERVICE_UNAVAILABLE,
        RbacError::AlreadyBootstrapped
        | RbacError::UserAlreadyExists
        | RbacError::CannotRemoveSelf
        | RbacError::LastSuperadmin => StatusCode::CONFLICT,
        RbacError::UserNotFound | RbacError::CredentialNotFound | RbacError::GrantNotFound => {
            StatusCode::NOT_FOUND
        }
        RbacError::InvalidUserId
        | RbacError::InvalidDisplayName
        | RbacError::ReservedUserId
        | RbacError::InvalidPortRange
        | RbacError::EmptyProtocolSet
        | RbacError::InvalidClient => StatusCode::UNPROCESSABLE_ENTITY,
    }
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorBodyInner,
}

#[derive(Debug, Serialize)]
struct ErrorBodyInner {
    code: String,
    message: String,
}

fn error_response(status: StatusCode, code: RbacError, message: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorBodyInner {
                code: code.code().to_string(),
                message: message.to_string(),
            },
        }),
    )
        .into_response()
}

/// Public so handlers can map `RbacError` from `enforce_push` /
/// `enforce_read` to the same status table.
#[must_use]
pub fn rbac_status(e: &RbacError) -> StatusCode {
    http_status_for(e)
}
