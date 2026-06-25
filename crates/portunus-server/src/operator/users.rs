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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::ConnectedClients;
    use crate::store::Store;
    use crate::store::operator_store::SqliteOperatorStore;
    use crate::store::token_store::SqliteTokenStore;
    use axum::response::IntoResponse;
    use tempfile::tempdir;

    /// Build a full `AppState` backed by a temp SQLite store. Mirrors the
    /// helper in `grpc/enrollment.rs` / `traffic_quotas/rollover.rs`. The
    /// store starts empty (no bootstrap) so each test controls the exact
    /// superadmin population it needs.
    fn test_state() -> (tempfile::TempDir, Arc<AppState>) {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&store)));
        let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&store)));
        let state = AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            None,
            7443,
            "deadbeef",
            include_str!("../advertised/testdata/san_fixture.pem"),
            16,
            store,
        )
        .unwrap();
        (dir, Arc::new(state))
    }

    fn superadmin_identity(uid: &str) -> OperatorIdentity {
        OperatorIdentity {
            user_id: UserId::from_str(uid).unwrap(),
            role: OperatorRole::Superadmin,
        }
    }

    fn user_identity(uid: &str) -> OperatorIdentity {
        OperatorIdentity {
            user_id: UserId::from_str(uid).unwrap(),
            role: OperatorRole::User,
        }
    }

    /// Seed a user directly into the operator store, bypassing the handler.
    fn seed_user(state: &AppState, uid: &str, role: OperatorRole) {
        let user = User {
            id: UserId::from_str(uid).unwrap(),
            display_name: uid.to_string(),
            role,
            created_at: Utc::now(),
            disabled: false,
        };
        state.operator_store.add_user(user).unwrap();
    }

    fn create_body(uid: &str, role: &str, password: Option<&str>) -> CreateUserBody {
        CreateUserBody {
            user_id: uid.to_string(),
            display_name: format!("{uid} display"),
            role: role.to_string(),
            initial_password: password.map(str::to_string),
            password_change_required: false,
        }
    }

    fn status_of(err: ApiError) -> StatusCode {
        err.into_response().status()
    }

    // --- api_store ---------------------------------------------------------

    #[test]
    fn api_store_short_circuits_to_rbac_for_user_not_found() {
        // `UserNotFound` maps through `as_rbac()` to RbacError::UserNotFound
        // -> 404 NOT_FOUND.
        let err = api_store(IdentityStoreError::UserNotFound(
            UserId::from_str("ghost").unwrap(),
        ));
        assert_eq!(status_of(err), StatusCode::NOT_FOUND);
    }

    #[test]
    fn api_store_invalid_port_range_is_422() {
        let err = api_store(IdentityStoreError::InvalidPortRange { start: 9, end: 1 });
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn api_store_hash_collision_is_409() {
        let err = api_store(IdentityStoreError::HashCollision);
        assert_eq!(status_of(err), StatusCode::CONFLICT);
    }

    #[test]
    fn api_store_unmapped_variant_is_500() {
        let err = api_store(IdentityStoreError::WriteFailed("boom".into()));
        assert_eq!(status_of(err), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // --- password_error ----------------------------------------------------

    #[test]
    fn password_error_validation_failures_are_422() {
        for e in [
            PasswordError::TooShort,
            PasswordError::TooLong,
            PasswordError::Invalid,
        ] {
            assert_eq!(
                status_of(password_error(e)),
                StatusCode::UNPROCESSABLE_ENTITY
            );
        }
    }

    #[test]
    fn password_error_hash_failed_is_500() {
        assert_eq!(
            status_of(password_error(PasswordError::HashFailed)),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    // --- post_users --------------------------------------------------------

    #[tokio::test]
    async fn post_users_creates_superadmin_role() {
        let (_dir, state) = test_state();
        let identity = superadmin_identity("boss");
        let body = create_body("alice", "superadmin", Some("correct horse staple"));
        let (status, resp) = post_users(State(state.clone()), Extension(identity), Json(body))
            .await
            .expect("create ok");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(resp.0.role, "superadmin");
        let stored = state
            .operator_store
            .get_user(&UserId::from_str("alice").unwrap())
            .unwrap();
        assert_eq!(stored.role, OperatorRole::Superadmin);
    }

    #[tokio::test]
    async fn post_users_rejects_empty_display_name() {
        let (_dir, state) = test_state();
        let mut body = create_body("alice", "user", Some("correct horse staple"));
        body.display_name = "   ".to_string();
        let err = post_users(
            State(state),
            Extension(superadmin_identity("boss")),
            Json(body),
        )
        .await
        .expect_err("blank display name rejected");
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_users_rejects_overlong_display_name() {
        let (_dir, state) = test_state();
        let mut body = create_body("alice", "user", Some("correct horse staple"));
        body.display_name = "x".repeat(65);
        let err = post_users(
            State(state),
            Extension(superadmin_identity("boss")),
            Json(body),
        )
        .await
        .expect_err("overlong display name rejected");
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_users_rejects_unknown_role() {
        let (_dir, state) = test_state();
        let body = create_body("alice", "wizard", Some("correct horse staple"));
        let err = post_users(
            State(state),
            Extension(superadmin_identity("boss")),
            Json(body),
        )
        .await
        .expect_err("unknown role rejected");
        // RbacError::RoleRequired -> 403 FORBIDDEN.
        assert_eq!(status_of(err), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn post_users_requires_initial_password() {
        let (_dir, state) = test_state();
        let body = create_body("alice", "user", None);
        let err = post_users(
            State(state),
            Extension(superadmin_identity("boss")),
            Json(body),
        )
        .await
        .expect_err("missing password rejected");
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_users_rejects_non_superadmin_caller() {
        let (_dir, state) = test_state();
        let body = create_body("alice", "user", Some("correct horse staple"));
        let err = post_users(State(state), Extension(user_identity("bob")), Json(body))
            .await
            .expect_err("non-superadmin rejected");
        assert_eq!(status_of(err), StatusCode::FORBIDDEN);
    }

    // --- get_users / get_user ----------------------------------------------

    #[tokio::test]
    async fn get_users_lists_with_grant_counts() {
        let (_dir, state) = test_state();
        seed_user(&state, "alice", OperatorRole::User);
        seed_user(&state, "bob", OperatorRole::Superadmin);
        let resp = get_users(State(state), Extension(superadmin_identity("boss")))
            .await
            .expect("list ok");
        let names: Vec<&str> = resp.0.iter().map(|u| u.user_id.as_str()).collect();
        assert!(names.contains(&"alice"));
        assert!(names.contains(&"bob"));
        for v in &resp.0 {
            assert_eq!(v.grant_count, 0);
        }
    }

    #[tokio::test]
    async fn get_user_returns_view_for_existing_user() {
        let (_dir, state) = test_state();
        seed_user(&state, "alice", OperatorRole::User);
        let resp = get_user(
            State(state),
            Extension(superadmin_identity("boss")),
            Path("alice".to_string()),
        )
        .await
        .expect("get ok");
        assert_eq!(resp.0.user_id, "alice");
        assert_eq!(resp.0.role, "user");
        assert_eq!(resp.0.grant_count, 0);
    }

    #[tokio::test]
    async fn get_user_missing_is_404() {
        let (_dir, state) = test_state();
        let err = get_user(
            State(state),
            Extension(superadmin_identity("boss")),
            Path("ghost".to_string()),
        )
        .await
        .expect_err("missing user 404");
        assert_eq!(status_of(err), StatusCode::NOT_FOUND);
    }

    // --- delete_user -------------------------------------------------------

    #[tokio::test]
    async fn delete_user_removes_regular_user_and_cascades() {
        let (_dir, state) = test_state();
        seed_user(&state, "alice", OperatorRole::User);
        let resp = delete_user(
            State(state.clone()),
            Extension(superadmin_identity("boss")),
            Path("alice".to_string()),
        )
        .await
        .expect("delete ok");
        assert_eq!(resp.0.user_id, "alice");
        assert!(resp.0.removed_rule_ids.is_empty());
        assert!(
            state
                .operator_store
                .get_user(&UserId::from_str("alice").unwrap())
                .is_none()
        );
    }

    #[tokio::test]
    async fn delete_user_rejects_self_removal() {
        let (_dir, state) = test_state();
        seed_user(&state, "boss", OperatorRole::Superadmin);
        let err = delete_user(
            State(state),
            Extension(superadmin_identity("boss")),
            Path("boss".to_string()),
        )
        .await
        .expect_err("self removal rejected");
        // RbacError::CannotRemoveSelf -> 409 CONFLICT.
        assert_eq!(status_of(err), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_user_protects_last_superadmin() {
        let (_dir, state) = test_state();
        // Exactly one superadmin in the store; the acting identity is a
        // distinct (non-persisted) superadmin so the self-removal guard is
        // not what trips.
        seed_user(&state, "onlyboss", OperatorRole::Superadmin);
        let err = delete_user(
            State(state),
            Extension(superadmin_identity("other")),
            Path("onlyboss".to_string()),
        )
        .await
        .expect_err("last superadmin protected");
        // RbacError::LastSuperadmin -> 409 CONFLICT.
        assert_eq!(status_of(err), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_user_allows_superadmin_when_not_last() {
        let (_dir, state) = test_state();
        // Two superadmins -> removing one leaves a survivor, so the
        // last-superadmin guard is skipped and the delete proceeds.
        seed_user(&state, "boss-a", OperatorRole::Superadmin);
        seed_user(&state, "boss-b", OperatorRole::Superadmin);
        let resp = delete_user(
            State(state.clone()),
            Extension(superadmin_identity("other")),
            Path("boss-a".to_string()),
        )
        .await
        .expect("delete ok");
        assert_eq!(resp.0.user_id, "boss-a");
        assert_eq!(state.operator_store.count_superadmins(), 1);
    }

    #[tokio::test]
    async fn delete_user_rejects_non_superadmin_caller() {
        let (_dir, state) = test_state();
        seed_user(&state, "alice", OperatorRole::User);
        let err = delete_user(
            State(state),
            Extension(user_identity("bob")),
            Path("alice".to_string()),
        )
        .await
        .expect_err("non-superadmin rejected");
        assert_eq!(status_of(err), StatusCode::FORBIDDEN);
    }
}
