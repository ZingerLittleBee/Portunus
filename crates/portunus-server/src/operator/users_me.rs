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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::ConnectedClients;
    use crate::store::Store;
    use crate::store::operator_store::SqliteOperatorStore;
    use crate::store::token_store::SqliteTokenStore;
    use chrono::Utc;
    use portunus_auth::{User, UserId};
    use std::str::FromStr;
    use tempfile::tempdir;

    /// Build a full `AppState` backed by a temp SQLite store. Mirrors the
    /// helper in `operator/users.rs`. The store starts empty (no bootstrap)
    /// so each test controls the exact user population it needs.
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

    /// Seed a user directly into the operator store, bypassing any handler.
    fn seed_user(state: &AppState, uid: &str, display_name: &str, role: OperatorRole) {
        let user = User {
            id: UserId::from_str(uid).unwrap(),
            display_name: display_name.to_string(),
            role,
            created_at: Utc::now(),
            disabled: false,
        };
        state.operator_store.add_user(user).unwrap();
    }

    fn identity(user_id: UserId, role: OperatorRole) -> OperatorIdentity {
        OperatorIdentity { user_id, role }
    }

    #[tokio::test]
    async fn returns_store_display_name_for_existing_user() {
        // A user present in the store yields the projection's `display_name`
        // from its row — the `Some(u)` closure arm of `map_or_else`.
        let (_dir, state) = test_state();
        seed_user(&state, "alice", "Alice Liddell", OperatorRole::User);

        let Json(me) = get_users_me(
            State(state),
            Extension(identity(
                UserId::from_str("alice").unwrap(),
                OperatorRole::User,
            )),
        )
        .await
        .expect("authenticated identity always projects");

        assert_eq!(me.user_id, "alice");
        assert_eq!(me.display_name, "Alice Liddell");
        assert_eq!(me.role, OperatorRole::User);
    }

    #[tokio::test]
    async fn falls_back_to_user_id_when_no_store_row() {
        // An identity with no matching store row (the synthetic `_legacy`
        // superadmin has no `users` row) falls back to `user_id` for
        // `display_name` — the `None` arm of `map_or_else`.
        let (_dir, state) = test_state();

        let Json(me) = get_users_me(
            State(state),
            Extension(identity(
                UserId::reserved("_legacy"),
                OperatorRole::Superadmin,
            )),
        )
        .await
        .expect("authenticated identity always projects");

        assert_eq!(me.user_id, "_legacy");
        assert_eq!(me.display_name, "_legacy");
        assert_eq!(me.role, OperatorRole::Superadmin);
    }

    #[test]
    fn projection_serializes_role_lowercase() {
        // The `Serialize` derive must round-trip the role as a lowercase
        // string and surface `display_name` / `user_id` verbatim.
        let me = OperatorIdentitySelf {
            user_id: "bob".to_string(),
            role: OperatorRole::Superadmin,
            display_name: "Bob".to_string(),
        };
        let json = serde_json::to_value(&me).unwrap();
        assert_eq!(json["user_id"], "bob");
        assert_eq!(json["display_name"], "Bob");
        assert_eq!(json["role"], "superadmin");
    }
}
