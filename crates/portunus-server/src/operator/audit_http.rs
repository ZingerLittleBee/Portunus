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
use chrono::{DateTime, Utc};
use portunus_auth::{OperatorIdentity, OperatorRole, RbacError};
use serde::Serialize;

use crate::operator::audit::{AuditEntry, AuditOutcome};
use crate::operator::http::ApiError;
use crate::operator::rbac;
use crate::state::AppState;
use crate::store::audit_query::{AuditQuery, decode_cursor};

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

    // 008-sqlite-storage T073: switch shapes based on whether ANY of
    // the new envelope-mode params are present. v0.7 callers (no
    // since/until/cursor) keep getting the array-root response.
    let since = parse_optional_ts(&params, "since")?;
    let until = parse_optional_ts(&params, "until")?;
    let before_seq = match params.get("cursor") {
        Some(s) => match decode_cursor(s) {
            Some(seq) => Some(seq),
            None => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_cursor",
                    "cursor is malformed",
                ));
            }
        },
        None => None,
    };
    if let (Some(s), Some(u)) = (since, until)
        && s > u
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_time_range",
            "since must be <= until",
        ));
    }
    let envelope_mode = since.is_some() || until.is_some() || before_seq.is_some();

    if !envelope_mode {
        // v0.7 array-root response. Byte-stable with v0.6 / v0.7.
        let snapshot: Vec<AuditEntry> =
            state
                .store
                .query_audit_recent(limit, outcome)
                .map_err(|e| {
                    ApiError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal",
                        format!("audit read: {e}"),
                    )
                })?;
        let body = Json(snapshot).into_response();
        let mut response = body;
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return Ok(response);
    }

    // 008-sqlite-storage T073..T075: envelope mode. Returns
    // `{ entries: [...], next_cursor: "...", count: N }`.
    let q = AuditQuery {
        limit,
        outcome,
        since,
        until,
        before_seq,
    };
    let page = state.store.query_audit_envelope(&q).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            format!("audit read: {e}"),
        )
    })?;
    let count = page.rows.len();
    let body = Json(AuditEnvelope {
        entries: page.rows,
        next_cursor: page.next_cursor,
        count,
    })
    .into_response();
    let mut response = body;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

#[derive(Debug, Serialize)]
struct AuditEnvelope {
    entries: Vec<AuditEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
    count: usize,
}

fn parse_optional_ts(
    params: &HashMap<String, String>,
    key: &str,
) -> Result<Option<DateTime<Utc>>, ApiError> {
    match params.get(key) {
        None => Ok(None),
        Some(raw) => DateTime::parse_from_rfc3339(raw)
            .map(|dt| Some(dt.with_timezone(&Utc)))
            .map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_timestamp",
                    format!("{key} must be RFC3339"),
                )
            }),
    }
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
