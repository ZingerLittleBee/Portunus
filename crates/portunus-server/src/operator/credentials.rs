//! T034 / T035 (005-multi-user-rbac, US2 + US4) — credential management.
//!
//! Cross-user gating: superadmin can issue/revoke/rotate any credential;
//! a non-superadmin user can only operate on their own credentials. The
//! handler returns 403 `not_owner` otherwise.
//!
//! `post_credential` is the ONLY operator HTTP path that ever sends a
//! raw bearer token over the wire (in the response body). The token MUST
//! NOT appear in any `tracing` log (Constitution Principle IV).

use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use portunus_auth::IdentityStoreError;
use portunus_auth::{
    Credential, CredentialId, CredentialStatus, OperatorIdentity, OperatorRole, RbacError, UserId,
};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::operator::cli::OperatorError;
use crate::operator::http::ApiError;
use crate::operator::user_ids::parse_stored_user_id;
use crate::state::AppState;

fn api_rbac(e: RbacError) -> ApiError {
    ApiError::from(OperatorError::Rbac(e))
}

fn api_store(e: IdentityStoreError) -> ApiError {
    if let Some(rbac) = e.as_rbac() {
        return api_rbac(rbac);
    }
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal", e.to_string())
}

fn parse_user_id(raw: &str) -> Result<UserId, ApiError> {
    parse_stored_user_id(raw).map_err(api_rbac)
}

fn parse_cred_id(raw: &str) -> Result<CredentialId, ApiError> {
    let ulid = ulid::Ulid::from_string(raw).map_err(|_| api_rbac(RbacError::CredentialNotFound))?;
    Ok(CredentialId(ulid))
}

fn check_owner_or_super(identity: &OperatorIdentity, target: &UserId) -> Result<(), ApiError> {
    if identity.role == OperatorRole::Superadmin || &identity.user_id == target {
        Ok(())
    } else {
        Err(api_rbac(RbacError::NotOwner))
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct IssueCredentialBody {
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IssueCredentialResponse {
    pub credential_id: String,
    pub user_id: String,
    /// Raw bearer token, returned EXACTLY ONCE at issuance. Never logged.
    pub token: String,
    pub label: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

pub async fn post_credential(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(user_id): Path<String>,
    Json(body): Json<IssueCredentialBody>,
) -> Result<(StatusCode, Json<IssueCredentialResponse>), ApiError> {
    let target = parse_user_id(&user_id)?;
    check_owner_or_super(&identity, &target)?;

    let (cred, raw) = state
        .operator_store
        .issue_credential(&target, body.label.clone())
        .map_err(api_store)?;

    info!(
        event = "operator.credential_issued",
        actor = %identity.user_id,
        target = %target,
        credential_id = %cred.id,
        outcome = "ok",
    );

    Ok((
        StatusCode::CREATED,
        Json(IssueCredentialResponse {
            credential_id: cred.id.to_string(),
            user_id: cred.user_id.as_str().to_string(),
            token: raw,
            label: cred.label,
            created_at: cred.created_at,
        }),
    ))
}

#[derive(Debug, Serialize)]
pub struct CredentialView {
    pub credential_id: String,
    pub user_id: String,
    pub label: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
    pub status: String,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<Credential> for CredentialView {
    fn from(c: Credential) -> Self {
        let (status, revoked_at) = match &c.status {
            CredentialStatus::Active(_) => ("active".to_string(), None),
            CredentialStatus::Revoked { revoked } => {
                ("revoked".to_string(), Some(revoked.revoked_at))
            }
        };
        Self {
            credential_id: c.id.to_string(),
            user_id: c.user_id.as_str().to_string(),
            label: c.label,
            created_at: c.created_at,
            last_used_at: c.last_used_at,
            status,
            revoked_at,
        }
    }
}

pub async fn get_credentials(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(user_id): Path<String>,
) -> Result<Json<Vec<CredentialView>>, ApiError> {
    let target = parse_user_id(&user_id)?;
    check_owner_or_super(&identity, &target)?;
    let creds = state.operator_store.list_credentials(&target);
    Ok(Json(creds.into_iter().map(CredentialView::from).collect()))
}

pub async fn delete_credential(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path((user_id, cred_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let target = parse_user_id(&user_id)?;
    check_owner_or_super(&identity, &target)?;
    let cid = parse_cred_id(&cred_id)?;
    state
        .operator_store
        .revoke_credential(&target, &cid)
        .map_err(api_store)?;
    info!(
        event = "operator.credential_revoked",
        actor = %identity.user_id,
        target = %target,
        credential_id = %cid,
        outcome = "ok",
    );
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, Default)]
pub struct RotateCredentialBody {
    #[serde(default)]
    pub label: Option<String>,
}

pub async fn post_credential_rotate(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path((user_id, cred_id)): Path<(String, String)>,
    // Rotation needs no input; the optional `label` override may be omitted.
    // Accept a missing / empty body (no `Content-Type` required) instead of
    // forcing callers to send an explicit `{}` (was a 400 `EOF while parsing`).
    body: Option<Json<RotateCredentialBody>>,
) -> Result<Json<IssueCredentialResponse>, ApiError> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let target = parse_user_id(&user_id)?;
    check_owner_or_super(&identity, &target)?;
    let cid = parse_cred_id(&cred_id)?;
    let (new_cred, raw) = state
        .operator_store
        .rotate_credential(&target, &cid, body.label.clone())
        .map_err(api_store)?;
    info!(
        event = "operator.credential_rotated",
        actor = %identity.user_id,
        target = %target,
        old_credential = %cid,
        new_credential = %new_cred.id,
        outcome = "ok",
    );
    Ok(Json(IssueCredentialResponse {
        credential_id: new_cred.id.to_string(),
        user_id: new_cred.user_id.as_str().to_string(),
        token: raw,
        label: new_cred.label,
        created_at: new_cred.created_at,
    }))
}
