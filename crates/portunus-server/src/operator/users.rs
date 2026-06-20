//! T033 (005-multi-user-rbac, US2) — user-management HTTP handlers.
//!
//! Each handler runs AFTER the auth middleware has injected an
//! `OperatorIdentity` into request extensions. They additionally call
//! [`crate::operator::rbac::require_role`] to enforce that the caller is
//! a superadmin (FR-007: "user management is superadmin-only").
//!
//! Cascade in `delete_user`: the operator-side flush commits BEFORE
//! rule removals (R-006: identity-side state must always reflect the
//! latest authoritative truth, even if the rule cascade crashes mid-way).

use std::str::FromStr;
use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use chrono::Utc;
use portunus_auth::{OperatorIdentity, OperatorRole, RbacError, User, UserId};
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::operator::cli::OperatorError;
use crate::operator::http::ApiError;
use crate::operator::passwords::{PasswordError, hash_password};
use crate::operator::rbac;
use crate::operator::user_ids::parse_stored_user_id;
use crate::state::AppState;
use portunus_auth::IdentityStoreError;

fn api_rbac(e: RbacError) -> ApiError {
    ApiError::from(OperatorError::Rbac(e))
}

fn api_store(e: IdentityStoreError) -> ApiError {
    if let Some(rbac) = e.as_rbac() {
        return api_rbac(rbac);
    }
    let (status, code) = match &e {
        IdentityStoreError::InvalidPortRange { .. } => (
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_port_range",
        ),
        IdentityStoreError::HashCollision => (axum::http::StatusCode::CONFLICT, "hash_collision"),
        _ => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    };
    ApiError::new(status, code, e.to_string())
}

fn password_error(error: PasswordError) -> ApiError {
    match error {
        PasswordError::TooShort | PasswordError::TooLong | PasswordError::Invalid => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            error.to_string(),
            error.to_string(),
        ),
        PasswordError::HashFailed => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "password_hash_failed",
            "password hashing failed",
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateUserBody {
    pub user_id: String,
    pub display_name: String,
    /// "superadmin" or "user". Defaults to "user" if absent.
    #[serde(default = "default_role")]
    pub role: String,
    #[serde(default)]
    pub initial_password: Option<String>,
    #[serde(default)]
    pub password_change_required: bool,
}

fn default_role() -> String {
    "user".to_string()
}

#[derive(Debug, Serialize)]
pub struct UserView {
    pub user_id: String,
    pub display_name: String,
    pub role: String,
    pub disabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub grant_count: usize,
}

impl UserView {
    fn from_user(u: &User, grant_count: usize) -> Self {
        Self {
            user_id: u.id.as_str().to_string(),
            display_name: u.display_name.clone(),
            role: match u.role {
                OperatorRole::Superadmin => "superadmin".to_string(),
                OperatorRole::User => "user".to_string(),
            },
            disabled: u.disabled,
            created_at: u.created_at,
            grant_count,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CreateUserResponse {
    pub user_id: String,
    pub display_name: String,
    pub role: String,
}

pub async fn post_users(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Json(body): Json<CreateUserBody>,
) -> Result<(StatusCode, Json<CreateUserResponse>), ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;

    let id = UserId::from_str(&body.user_id).map_err(api_rbac)?;
    let display_name = body.display_name.trim();
    if display_name.is_empty() || display_name.len() > 64 {
        return Err(api_rbac(RbacError::InvalidDisplayName));
    }
    let role = match body.role.as_str() {
        "superadmin" => OperatorRole::Superadmin,
        "user" => OperatorRole::User,
        _ => return Err(api_rbac(RbacError::RoleRequired)),
    };

    let user = User {
        id: id.clone(),
        display_name: display_name.to_string(),
        role,
        created_at: Utc::now(),
        disabled: false,
    };
    let initial_password = match body.initial_password.as_deref() {
        Some(p) if !p.is_empty() => p,
        _ => {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "initial_password_required",
                "a user must be created with an initial password",
            ));
        }
    };
    let password_hash = hash_password(initial_password).map_err(password_error)?;
    state
        .operator_store
        .add_user_with_password(
            user.clone(),
            Some(password_hash.as_str()),
            body.password_change_required,
        )
        .map_err(api_store)?;

    info!(
        event = "operator.user_added",
        actor = %identity.user_id,
        new_user = %user.id,
        outcome = "ok",
    );

    Ok((
        StatusCode::CREATED,
        Json(CreateUserResponse {
            user_id: user.id.as_str().to_string(),
            display_name: user.display_name,
            role: body.role,
        }),
    ))
}

pub async fn get_users(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
) -> Result<Json<Vec<UserView>>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let users = state.operator_store.list_users();
    let mut out = Vec::with_capacity(users.len());
    for u in users {
        let grant_count = state.operator_store.list_grants(Some(&u.id)).len();
        out.push(UserView::from_user(&u, grant_count));
    }
    Ok(Json(out))
}

pub async fn get_user(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(user_id): Path<String>,
) -> Result<Json<UserView>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let id = parse_stored_user_id(&user_id).map_err(api_rbac)?;
    let user = state
        .operator_store
        .get_user(&id)
        .ok_or(api_rbac(RbacError::UserNotFound))?;
    let grant_count = state.operator_store.list_grants(Some(&id)).len();
    Ok(Json(UserView::from_user(&user, grant_count)))
}

#[derive(Debug, Serialize)]
pub struct DeleteUserResponse {
    pub user_id: String,
    pub removed_credential_ids: Vec<String>,
    pub revoked_grant_ids: Vec<String>,
    pub removed_rule_ids: Vec<u64>,
}

pub async fn delete_user(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(user_id): Path<String>,
) -> Result<Json<DeleteUserResponse>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let id = parse_stored_user_id(&user_id).map_err(api_rbac)?;

    if id == identity.user_id {
        return Err(api_rbac(RbacError::CannotRemoveSelf));
    }

    // Last-superadmin protection: if this would remove the last
    // remaining Superadmin, refuse.
    if let Some(target) = state.operator_store.get_user(&id)
        && target.role == OperatorRole::Superadmin
        && state.operator_store.count_superadmins() <= 1
    {
        return Err(api_rbac(RbacError::LastSuperadmin));
    }

    // R-006 cascade ordering: flush identity FIRST, then drop owned rules.
    let summary = state.operator_store.remove_user(&id).map_err(api_store)?;

    let removed_rule_ids = state.rules.remove_owned_by(&id).await;
    let _ = state.rule_store.delete_rules_owned_by(&id);

    info!(
        event = "operator.user_removed",
        actor = %identity.user_id,
        removed_user = %id,
        revoked_credentials = summary.removed_credential_ids.len(),
        revoked_grants = summary.revoked_grant_ids.len(),
        removed_rules = removed_rule_ids.len(),
        outcome = "ok",
    );

    Ok(Json(DeleteUserResponse {
        user_id,
        removed_credential_ids: summary
            .removed_credential_ids
            .into_iter()
            .map(|c| c.to_string())
            .collect(),
        revoked_grant_ids: summary
            .revoked_grant_ids
            .into_iter()
            .map(|g| g.to_string())
            .collect(),
        removed_rule_ids,
    }))
}
