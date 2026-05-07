//! 006-management-web-ui T024: `GET /v1/audit` — superadmin-only read
//! of the in-memory audit ring buffer.
//!
//! Contract: `specs/006-management-web-ui/contracts/audit-endpoint.md`.
//!
//! Behaviour:
//! - `?limit=N` (default 100, range 1..=1000). Out of range → 422
//!   `invalid_limit`.
//! - `?outcome=allow|deny` (optional). Other values → 422
//!   `invalid_outcome`.
//! - Caller MUST be superadmin (handler-side `RbacError::RoleRequired`
//!   on miss, mapped to HTTP 403 by `auth_layer::http_status_for`).
//! - Newest-first ordering. `Cache-Control: no-store`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use forward_auth::{OperatorIdentity, OperatorRole, RbacError};

use crate::operator::audit::{AuditEntry, AuditOutcome};
use crate::operator::http::ApiError;
use crate::operator::rbac;
use crate::state::AppState;

const DEFAULT_LIMIT: usize = 100;
const MAX_LIMIT: usize = 1000;

#[allow(clippy::implicit_hasher)]
pub async fn get_audit(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin).map_err(ApiError::from)?;

    // Validate ?limit
    let limit = match params.get("limit") {
        None => DEFAULT_LIMIT,
        Some(raw) => {
            let n = raw.parse::<usize>().map_err(|_| {
                ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_limit",
                    "limit must be a positive integer 1..=1000",
                )
            })?;
            if !(1..=MAX_LIMIT).contains(&n) {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_limit",
                    format!("limit must be 1..={MAX_LIMIT}"),
                ));
            }
            n
        }
    };

    // Validate ?outcome
    let outcome = match params.get("outcome") {
        None => None,
        Some(raw) => match AuditOutcome::parse(raw) {
            Some(o) => Some(o),
            None => {
                return Err(ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_outcome",
                    "outcome must be `allow` or `deny`",
                ));
            }
        },
    };

    let snapshot: Vec<AuditEntry> = state.audit.snapshot(limit, outcome);

    let body = Json(snapshot).into_response();
    let mut response = body;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

/// Convenience used by the legacy superadmin path which doesn't have an
/// `OperatorRole::Superadmin` projection on its identity directly. Not
/// currently called externally — exported only so future handlers can
/// reuse the role-required mapping.
pub fn require_superadmin(identity: &OperatorIdentity) -> Result<(), ApiError> {
    if identity.role == OperatorRole::Superadmin {
        Ok(())
    } else {
        Err(ApiError::from(RbacError::RoleRequired))
    }
}
