//! 006-management-web-ui T012: `GET /v1/users/me` — return the
//! caller's identity projection so the SPA's `<AuthGate>` can probe
//! the bearer once on first mount and cache role/display_name.
//!
//! Returns 401 if the bearer is missing/invalid (handled upstream by
//! `auth_layer`); never 403 — every authenticated identity may read
//! its own projection.

use std::sync::Arc;

use axum::{Extension, Json, extract::State};
use portunus_auth::{OperatorIdentity, OperatorRole};
use serde::Serialize;

use crate::operator::http::ApiError;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct OperatorIdentitySelf {
    pub user_id: String,
    pub role: OperatorRole,
    /// `display_name` from the identity store. Falls back to `user_id`
    /// for the synthetic `_legacy` superadmin (which has no row in the
    /// store).
    pub display_name: String,
}

pub async fn get_users_me(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
) -> Result<Json<OperatorIdentitySelf>, ApiError> {
    let display_name = state
        .operator_store
        .get_user(&identity.user_id)
        .map_or_else(|| identity.user_id.to_string(), |u| u.display_name.clone());
    Ok(Json(OperatorIdentitySelf {
        user_id: identity.user_id.to_string(),
        role: identity.role,
        display_name,
    }))
}
