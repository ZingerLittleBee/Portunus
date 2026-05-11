//! Operator HTTP auth middleware (005-multi-user-rbac, US1 / T019, T020).
//!
//! Wraps every `/v1/*` route. Extracts the bearer token from the
//! `Authorization` header, calls
//! [`portunus_auth::OperatorAuthenticator::verify`], and either:
//!
//! - On success: inserts the recovered [`portunus_auth::OperatorIdentity`]
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
    http::{Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use portunus_auth::{OperatorIdentity, OperatorRole, RbacError};
use serde::Serialize;
use tracing::{info, warn};

use crate::operator::audit::{AuditEntry, AuditOutcome};
use crate::operator::{csrf, sessions};
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
        record_deny(
            &state,
            "_anonymous",
            None,
            &method,
            &path,
            "bootstrap_required",
        );
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            RbacError::BootstrapRequired,
            "no superadmin exists; run `portunus-server bootstrap-superadmin` or set `operator_token` in server.toml",
        );
    }

    let (identity, auth_method) = match authenticate_request(&state, &req) {
        Ok(auth) => auth,
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
            record_deny(&state, "_anonymous", None, &method, &path, e.code());
            return error_response(status, e, "missing or invalid operator authentication");
        }
    };

    if auth_method == AuthMethod::Cookie
        && let Err(e) = csrf::verify(&req, &state.operator_http_public_origin)
    {
        bump_request(&state, "deny", e.code());
        warn!(
            event = "operator.deny",
            actor = %identity.user_id,
            method = %method,
            path = %path,
            outcome = "deny",
            reason = e.code(),
            auth_method = auth_method.as_str(),
        );
        record_deny(
            &state,
            identity.user_id.as_str(),
            Some(identity.role),
            &method,
            &path,
            e.code(),
        );
        return csrf_error_response(e);
    }

    if auth_method == AuthMethod::Cookie && !password_change_route_allowed(&method, &path) {
        match password_change_required(&state, &identity) {
            Ok(true) => {
                bump_request(&state, "deny", "password_change_required");
                warn!(
                    event = "operator.deny",
                    actor = %identity.user_id,
                    method = %method,
                    path = %path,
                    outcome = "deny",
                    reason = "password_change_required",
                    auth_method = auth_method.as_str(),
                );
                record_deny(
                    &state,
                    identity.user_id.as_str(),
                    Some(identity.role),
                    &method,
                    &path,
                    "password_change_required",
                );
                return json_error_response(
                    StatusCode::FORBIDDEN,
                    "password_change_required",
                    "password change required",
                );
            }
            Ok(false) => {}
            Err(()) => {
                bump_request(&state, "deny", "password_state_unavailable");
                warn!(
                    event = "operator.deny",
                    actor = %identity.user_id,
                    method = %method,
                    path = %path,
                    outcome = "deny",
                    reason = "password_state_unavailable",
                    auth_method = auth_method.as_str(),
                );
                record_deny(
                    &state,
                    identity.user_id.as_str(),
                    Some(identity.role),
                    &method,
                    &path,
                    "password_state_unavailable",
                );
                return json_error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "password state unavailable",
                );
            }
        }
    }

    // Audit-log success and inject identity into request extensions.
    bump_request(&state, "allow", "ok");
    info!(
        event = "operator.allow",
        actor = %identity.user_id,
        role = ?identity.role,
        method = %method,
        path = %path,
        outcome = "allow",
        auth_method = auth_method.as_str(),
    );
    record_allow(
        &state,
        identity.user_id.as_str(),
        identity.role,
        &method,
        &path,
    );
    req.extensions_mut().insert(identity);

    next.run(req).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthMethod {
    Cookie,
    Bearer,
}

impl AuthMethod {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Cookie => "cookie",
            Self::Bearer => "bearer",
        }
    }
}

fn authenticate_request(
    state: &AppState,
    req: &Request<Body>,
) -> Result<(OperatorIdentity, AuthMethod), RbacError> {
    if let Some(secret) = sessions::cookie_value(req.headers(), sessions::SESSION_COOKIE) {
        let identity = verify_session_secret(state, secret)?;
        return Ok((identity, AuthMethod::Cookie));
    }

    let token = extract_bearer(req)?;
    let identity = state.operator_auth.verify(&token)?;
    Ok((identity, AuthMethod::Bearer))
}

pub(crate) fn verify_session_secret(
    state: &AppState,
    secret: &str,
) -> Result<OperatorIdentity, RbacError> {
    let session_hash = sessions::hash_session_secret(secret);
    let session = state
        .operator_store
        .verify_web_session(&session_hash, Utc::now())
        .map_err(|_| RbacError::CredentialInvalid)?
        .ok_or(RbacError::CredentialInvalid)?;
    let user = state
        .operator_store
        .get_user(&session.user_id)
        .ok_or(RbacError::CredentialInvalid)?;
    if user.disabled {
        return Err(RbacError::UserDisabled);
    }
    Ok(OperatorIdentity {
        user_id: user.id,
        role: user.role,
    })
}

/// 006-management-web-ui T010: push an `allow` row into the ring.
/// Tokens NEVER appear here — `actor` / `role` are post-verify.
fn record_allow(state: &AppState, actor: &str, role: OperatorRole, method: &Method, path: &str) {
    state.audit.push(AuditEntry {
        timestamp: Utc::now(),
        actor: actor.to_string(),
        role: Some(role),
        method: method.to_string(),
        path: path.to_string(),
        outcome: AuditOutcome::Allow,
        reason: None,
        action: None,
        resource_kind: None,
        resource_value: None,
        details: None,
    });
}

/// 006-management-web-ui T010: push a `deny` row. `role` is `None`
/// for pre-verify denies (`_anonymous`), `Some` once we know who
/// the caller was meant to be (the v0.5 auth_layer never reaches
/// that branch — it always denies anonymously).
fn record_deny(
    state: &AppState,
    actor: &str,
    role: Option<OperatorRole>,
    method: &Method,
    path: &str,
    reason: &str,
) {
    state.audit.push(AuditEntry {
        timestamp: Utc::now(),
        actor: actor.to_string(),
        role,
        method: method.to_string(),
        path: path.to_string(),
        outcome: AuditOutcome::Deny,
        reason: Some(reason.to_string()),
        action: None,
        resource_kind: None,
        resource_value: None,
        details: None,
    });
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

fn csrf_error_response(error: csrf::CsrfError) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ErrorBody {
            error: ErrorBodyInner {
                code: error.code().to_string(),
                message: "csrf verification failed".to_string(),
            },
        }),
    )
        .into_response()
}

fn json_error_response(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorBody {
            error: ErrorBodyInner {
                code: code.to_string(),
                message: message.to_string(),
            },
        }),
    )
        .into_response()
}

fn password_change_route_allowed(method: &Method, path: &str) -> bool {
    (method == Method::GET && path == "/v1/users/me")
        || (method == Method::POST && path == "/v1/users/me/password")
}

fn password_change_required(state: &AppState, identity: &OperatorIdentity) -> Result<bool, ()> {
    state
        .operator_store
        .password_state(&identity.user_id)
        .map(|password| password.is_some_and(|password| password.password_change_required))
        .map_err(|_| ())
}

/// Public so handlers can map `RbacError` from `enforce_push` /
/// `enforce_read` to the same status table.
#[must_use]
pub fn rbac_status(e: &RbacError) -> StatusCode {
    http_status_for(e)
}
