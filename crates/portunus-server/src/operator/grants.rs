//! T036 (005-multi-user-rbac, US2) — grant management HTTP handlers.
//!
//! Superadmin-only (FR-007). The cascade in `delete_grant` re-evaluates
//! every rule owned by the grant's user against the remaining grants;
//! rules that no longer pass `enforce_push` are removed before the HTTP
//! response returns (synchronous per R-006 ordering: identity flush
//! commits FIRST, then rule cascade).

use std::str::FromStr;
use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::Utc;
use portunus_auth::IdentityStoreError;
use portunus_auth::{
    ClientScope, Grant, GrantId, OperatorAuthenticator, OperatorIdentity, OperatorRole,
    ProtocolSet, RbacError, UserId,
};
use portunus_core::ClientName;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::operator::cli::OperatorError;
use crate::operator::http::ApiError;
use crate::operator::rbac;
use crate::operator::user_ids::parse_stored_user_id;
use crate::state::AppState;

fn api_rbac(e: RbacError) -> ApiError {
    ApiError::from(OperatorError::Rbac(e))
}

fn api_store(e: IdentityStoreError) -> ApiError {
    if let Some(rbac) = e.as_rbac() {
        return api_rbac(rbac);
    }
    let (status, code) = match &e {
        IdentityStoreError::InvalidPortRange { .. } => {
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_port_range")
        }
        IdentityStoreError::HashCollision => (StatusCode::CONFLICT, "hash_collision"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    };
    ApiError::new(status, code, e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct CreateGrantBody {
    pub user_id: String,
    /// `"client-a"` or `"*"` (wildcard).
    pub client: String,
    pub listen_port_start: u16,
    pub listen_port_end: u16,
    pub protocols: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GrantView {
    pub grant_id: String,
    pub user_id: String,
    pub client: String,
    pub listen_port_start: u16,
    pub listen_port_end: u16,
    pub protocols: Vec<String>,
    pub note: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<Grant> for GrantView {
    fn from(g: Grant) -> Self {
        let client = match &g.client {
            ClientScope::Any => "*".to_string(),
            ClientScope::Named(n) => n.as_str().to_string(),
        };
        let mut protocols = Vec::with_capacity(2);
        if g.protocols.contains(ProtocolSet::TCP) {
            protocols.push("tcp".to_string());
        }
        if g.protocols.contains(ProtocolSet::UDP) {
            protocols.push("udp".to_string());
        }
        Self {
            grant_id: g.id.to_string(),
            user_id: g.user_id.as_str().to_string(),
            client,
            listen_port_start: g.listen_port_start,
            listen_port_end: g.listen_port_end,
            protocols,
            note: g.note,
            created_at: g.created_at,
        }
    }
}

pub async fn post_grants(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Json(body): Json<CreateGrantBody>,
) -> Result<(StatusCode, Json<GrantView>), ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let user_id = UserId::from_str(&body.user_id).map_err(api_rbac)?;

    let client = if body.client == "*" {
        ClientScope::Any
    } else {
        ClientScope::Named(
            ClientName::new(body.client).map_err(|_| api_rbac(RbacError::InvalidClient))?,
        )
    };

    if body.listen_port_start == 0 || body.listen_port_start > body.listen_port_end {
        return Err(api_rbac(RbacError::InvalidPortRange));
    }

    let mut bits = ProtocolSet::empty();
    for p in &body.protocols {
        match p.as_str() {
            "tcp" => bits |= ProtocolSet::TCP,
            "udp" => bits |= ProtocolSet::UDP,
            _ => return Err(api_rbac(RbacError::EmptyProtocolSet)),
        }
    }
    let protocols = ProtocolSet::non_empty(bits).map_err(api_rbac)?;

    let grant = Grant {
        id: GrantId::new(),
        user_id: user_id.clone(),
        client,
        listen_port_start: body.listen_port_start,
        listen_port_end: body.listen_port_end,
        protocols,
        note: body.note,
        created_at: Utc::now(),
    };
    state
        .operator_store
        .add_grant(grant.clone())
        .map_err(api_store)?;

    info!(
        event = "operator.grant_added",
        actor = %identity.user_id,
        target = %user_id,
        grant_id = %grant.id,
        outcome = "ok",
    );

    Ok((StatusCode::CREATED, Json(GrantView::from(grant))))
}

#[derive(Debug, Deserialize, Default)]
pub struct GrantsQuery {
    #[serde(default)]
    pub user_id: Option<String>,
}

pub async fn get_grants(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Query(q): Query<GrantsQuery>,
) -> Result<Json<Vec<GrantView>>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    // Accept reserved IDs (e.g. `_superadmin`) as a filter so the operator
    // UI can list grants for the bootstrap superadmin without a 400; mirrors
    // the `parse_stored_user_id` handling in the users/credentials handlers.
    let filter = match q.user_id.as_deref() {
        Some(s) => Some(parse_stored_user_id(s).map_err(api_rbac)?),
        None => None,
    };
    let grants = state.operator_store.list_grants(filter.as_ref());
    Ok(Json(grants.into_iter().map(GrantView::from).collect()))
}

#[derive(Debug, Serialize)]
pub struct DeleteGrantResponse {
    pub grant_id: String,
    pub removed_rule_ids: Vec<u64>,
}

pub async fn delete_grant(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(grant_id): Path<String>,
) -> Result<Json<DeleteGrantResponse>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let gid = ulid::Ulid::from_string(&grant_id)
        .map(GrantId)
        .map_err(|_| api_rbac(RbacError::GrantNotFound))?;

    let grant = state.operator_store.revoke_grant(&gid).map_err(api_store)?;

    // Cascade: re-evaluate every rule owned by `grant.user_id` against
    // the user's REMAINING grants. Any rule that no longer passes
    // enforce_push is removed.
    let owner = grant.user_id.clone();
    let remaining = state.operator_store.grants_for(&owner);
    // We need an OperatorIdentity for enforce_push. Build a stand-in
    // matching the grant's owner with role User (Superadmin would
    // bypass the check).
    let synthetic = OperatorIdentity {
        user_id: owner.clone(),
        role: OperatorRole::User,
    };
    let owned_rules = state.rules.list_owned_by(&owner).await;
    let mut removed = Vec::new();
    for rule in owned_rules {
        let push_req = rbac::PushRequest {
            client: &rule.client_name,
            listen_port_start: rule.listen_port,
            listen_port_end: rule.listen_port_end.unwrap_or(rule.listen_port),
            protocol: match rule.protocol {
                crate::rules::Protocol::Tcp => rbac::PushProtocol::Tcp,
                crate::rules::Protocol::Udp => rbac::PushProtocol::Udp,
            },
        };
        if rbac::enforce_push(&synthetic, &push_req, &remaining).is_err() {
            // Best-effort remove; skip on NotFound (concurrent removal).
            let _ = state.rules.remove(rule.id).await;
            let _ = state.rule_store.delete_rule(rule.id);
            removed.push(rule.id.0);
        }
    }

    info!(
        event = "operator.grant_revoked",
        actor = %identity.user_id,
        grant_id = %gid,
        target = %owner,
        cascaded_rules = removed.len(),
        outcome = "ok",
    );

    Ok(Json(DeleteGrantResponse {
        grant_id,
        removed_rule_ids: removed,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, Query, State};
    use axum::response::IntoResponse;
    use portunus_auth::{GrantId, OperatorRole, ProtocolSet, User};
    use portunus_core::{ClientId, PortRange};
    use std::sync::Arc;
    use tempfile::TempDir;

    use crate::clients::ConnectedClients;
    use crate::rules::Protocol;
    use crate::state::AppState;
    use crate::store::Store;
    use crate::store::operator_store::SqliteOperatorStore;
    use crate::store::token_store::SqliteTokenStore;

    /// Build an in-process `AppState` backed by a temp SQLite store, with a
    /// bootstrapped legacy superadmin plus a single non-superadmin user
    /// (`alice`). Mirrors the integration-test fixture so the handlers run
    /// against a real store without booting an HTTP server.
    fn build_state() -> (Arc<AppState>, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let store = Arc::new(Store::open(dir.path()).expect("open store"));
        let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&store)));
        let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&store)));
        operator_store
            .bootstrap_legacy_superadmin("grants-super")
            .expect("bootstrap superadmin");
        operator_store
            .add_user(User {
                id: UserId::from_str("alice").expect("valid user id"),
                display_name: "Alice".to_string(),
                role: OperatorRole::User,
                created_at: Utc::now(),
                disabled: false,
            })
            .expect("add alice");

        let state = Arc::new(
            AppState::new(
                tokens,
                operator_store,
                ConnectedClients::default(),
                None,
                0,
                "deadbeef",
                include_str!("../advertised/testdata/san_fixture.pem"),
                16,
                Arc::clone(&store),
            )
            .expect("AppState"),
        );
        (state, dir)
    }

    fn superadmin() -> OperatorIdentity {
        OperatorIdentity {
            user_id: UserId::superadmin(),
            role: OperatorRole::Superadmin,
        }
    }

    fn alice() -> OperatorIdentity {
        OperatorIdentity {
            user_id: UserId::from_str("alice").expect("valid user id"),
            role: OperatorRole::User,
        }
    }

    /// Convert an `ApiError` into its HTTP status code by routing through
    /// `IntoResponse`, since the struct's fields are private to `http.rs`.
    fn status_of(err: ApiError) -> StatusCode {
        err.into_response().status()
    }

    fn create_body(user_id: &str, client: &str, protocols: &[&str]) -> CreateGrantBody {
        CreateGrantBody {
            user_id: user_id.to_string(),
            client: client.to_string(),
            listen_port_start: 30000,
            listen_port_end: 30100,
            protocols: protocols.iter().map(|p| (*p).to_string()).collect(),
            note: None,
        }
    }

    // ---- api_store mapping ----

    #[test]
    fn api_store_maps_rbac_variant() {
        // GrantNotFound is one of the variants `as_rbac` recognises; it
        // should route through `api_rbac` and surface as 404.
        let err = api_store(IdentityStoreError::GrantNotFound(GrantId::new()));
        assert_eq!(status_of(err), StatusCode::NOT_FOUND);
    }

    #[test]
    fn api_store_maps_invalid_port_range() {
        let err = api_store(IdentityStoreError::InvalidPortRange { start: 10, end: 1 });
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn api_store_maps_hash_collision() {
        let err = api_store(IdentityStoreError::HashCollision);
        assert_eq!(status_of(err), StatusCode::CONFLICT);
    }

    #[test]
    fn api_store_maps_unmapped_to_internal() {
        let err = api_store(IdentityStoreError::WriteFailed("boom".into()));
        assert_eq!(status_of(err), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // ---- GrantView projection ----

    fn grant_with(scope: ClientScope, protocols: ProtocolSet) -> Grant {
        Grant {
            id: GrantId::new(),
            user_id: UserId::from_str("alice").unwrap(),
            client: scope,
            listen_port_start: 30000,
            listen_port_end: 30010,
            protocols,
            note: Some("a note".to_string()),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn grant_view_named_client_tcp_only() {
        let g = grant_with(
            ClientScope::Named(ClientName::new("edge-a").unwrap()),
            ProtocolSet::non_empty(ProtocolSet::TCP).unwrap(),
        );
        let view = GrantView::from(g);
        assert_eq!(view.client, "edge-a");
        assert_eq!(view.protocols, vec!["tcp".to_string()]);
        assert_eq!(view.note.as_deref(), Some("a note"));
    }

    #[test]
    fn grant_view_any_client_with_udp() {
        // Exercises the wildcard `*` rendering AND the UDP protocol push.
        let g = grant_with(
            ClientScope::Any,
            ProtocolSet::non_empty(ProtocolSet::UDP).unwrap(),
        );
        let view = GrantView::from(g);
        assert_eq!(view.client, "*");
        assert_eq!(view.protocols, vec!["udp".to_string()]);
    }

    #[test]
    fn grant_view_tcp_and_udp_ordering() {
        let g = grant_with(
            ClientScope::Any,
            ProtocolSet::non_empty(ProtocolSet::TCP | ProtocolSet::UDP).unwrap(),
        );
        let view = GrantView::from(g);
        assert_eq!(view.protocols, vec!["tcp".to_string(), "udp".to_string()]);
    }

    // ---- post_grants ----

    #[tokio::test]
    async fn post_grants_creates_named_grant() {
        let (state, _dir) = build_state();
        let (status, Json(view)) = post_grants(
            State(state.clone()),
            Extension(superadmin()),
            Json(create_body("alice", "edge-a", &["tcp", "udp"])),
        )
        .await
        .expect("grant created");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(view.user_id, "alice");
        assert_eq!(view.client, "edge-a");
        assert_eq!(view.protocols, vec!["tcp".to_string(), "udp".to_string()]);
        // The grant is persisted and visible via grants_for.
        let stored = state
            .operator_store
            .grants_for(&UserId::from_str("alice").unwrap());
        assert_eq!(stored.len(), 1);
    }

    #[tokio::test]
    async fn post_grants_wildcard_client() {
        let (state, _dir) = build_state();
        let (status, Json(view)) = post_grants(
            State(state),
            Extension(superadmin()),
            Json(create_body("alice", "*", &["tcp"])),
        )
        .await
        .expect("grant created");
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(view.client, "*");
    }

    #[tokio::test]
    async fn post_grants_requires_superadmin() {
        let (state, _dir) = build_state();
        let err = post_grants(
            State(state),
            Extension(alice()),
            Json(create_body("alice", "edge-a", &["tcp"])),
        )
        .await
        .expect_err("non-superadmin rejected");
        assert_eq!(status_of(err), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn post_grants_rejects_empty_protocols() {
        let (state, _dir) = build_state();
        let err = post_grants(
            State(state),
            Extension(superadmin()),
            Json(create_body("alice", "edge-a", &[])),
        )
        .await
        .expect_err("empty protocol set rejected");
        // RbacError::EmptyProtocolSet → 422 per the auth_layer status table.
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_grants_rejects_unknown_protocol() {
        let (state, _dir) = build_state();
        let err = post_grants(
            State(state),
            Extension(superadmin()),
            Json(create_body("alice", "edge-a", &["sctp"])),
        )
        .await
        .expect_err("unknown protocol rejected");
        // An unrecognised protocol token surfaces EmptyProtocolSet → 422.
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_grants_rejects_inverted_port_range() {
        let (state, _dir) = build_state();
        let mut body = create_body("alice", "edge-a", &["tcp"]);
        body.listen_port_start = 30100;
        body.listen_port_end = 30000;
        let err = post_grants(State(state), Extension(superadmin()), Json(body))
            .await
            .expect_err("inverted range rejected");
        // RbacError::InvalidPortRange → 422 per the auth_layer status table.
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_grants_rejects_zero_port() {
        let (state, _dir) = build_state();
        let mut body = create_body("alice", "edge-a", &["tcp"]);
        body.listen_port_start = 0;
        body.listen_port_end = 0;
        let err = post_grants(State(state), Extension(superadmin()), Json(body))
            .await
            .expect_err("zero start rejected");
        // A zero listen_port_start is also InvalidPortRange → 422.
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_grants_rejects_invalid_user_id() {
        let (state, _dir) = build_state();
        let err = post_grants(
            State(state),
            Extension(superadmin()),
            // Uppercase / illegal characters fail `UserId::from_str`.
            Json(create_body("Not A User", "edge-a", &["tcp"])),
        )
        .await
        .expect_err("invalid user id rejected");
        // RbacError::InvalidUserId → 422 per the auth_layer status table.
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_grants_rejects_invalid_client_name() {
        let (state, _dir) = build_state();
        // A whitespace-only client name is not the `*` wildcard, so it
        // reaches `ClientName::new`, which rejects it → InvalidClient.
        let err = post_grants(
            State(state),
            Extension(superadmin()),
            Json(create_body("alice", "   ", &["tcp"])),
        )
        .await
        .expect_err("invalid client name rejected");
        // RbacError::InvalidClient → 422 per the auth_layer status table.
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_grants_unknown_user_is_store_error() {
        let (state, _dir) = build_state();
        // `bob` parses as a valid user id but has no row → add_grant fails
        // with UserNotFound, which `as_rbac` maps to RbacError::UserNotFound.
        let err = post_grants(
            State(state),
            Extension(superadmin()),
            Json(create_body("bob", "edge-a", &["tcp"])),
        )
        .await
        .expect_err("unknown user rejected");
        assert_eq!(status_of(err), StatusCode::NOT_FOUND);
    }

    // ---- get_grants ----

    #[tokio::test]
    async fn get_grants_lists_all_without_filter() {
        let (state, _dir) = build_state();
        let _ = post_grants(
            State(state.clone()),
            Extension(superadmin()),
            Json(create_body("alice", "edge-a", &["tcp"])),
        )
        .await
        .expect("grant created");

        let Json(views) = get_grants(
            State(state),
            Extension(superadmin()),
            Query(GrantsQuery { user_id: None }),
        )
        .await
        .expect("list grants");
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].user_id, "alice");
    }

    #[tokio::test]
    async fn get_grants_filters_by_user_id() {
        let (state, _dir) = build_state();
        let _ = post_grants(
            State(state.clone()),
            Extension(superadmin()),
            Json(create_body("alice", "edge-a", &["tcp"])),
        )
        .await
        .expect("grant created");

        // Filter by a different (reserved) id resolves to zero matches but
        // still parses — `_superadmin` exercises the reserved-id branch.
        let Json(views) = get_grants(
            State(state),
            Extension(superadmin()),
            Query(GrantsQuery {
                user_id: Some("_superadmin".to_string()),
            }),
        )
        .await
        .expect("list grants");
        assert!(views.is_empty());
    }

    #[tokio::test]
    async fn get_grants_requires_superadmin() {
        let (state, _dir) = build_state();
        let err = get_grants(
            State(state),
            Extension(alice()),
            Query(GrantsQuery { user_id: None }),
        )
        .await
        .expect_err("non-superadmin rejected");
        assert_eq!(status_of(err), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_grants_rejects_invalid_filter() {
        let (state, _dir) = build_state();
        let err = get_grants(
            State(state),
            Extension(superadmin()),
            Query(GrantsQuery {
                user_id: Some("Bad Id".to_string()),
            }),
        )
        .await
        .expect_err("invalid filter rejected");
        assert_eq!(status_of(err), StatusCode::UNPROCESSABLE_ENTITY);
    }

    // ---- delete_grant ----

    /// Create a grant for `alice` and return its id string.
    async fn seed_grant(state: &Arc<AppState>) -> String {
        let (_, Json(view)) = post_grants(
            State(state.clone()),
            Extension(superadmin()),
            Json(create_body("alice", "edge-a", &["tcp"])),
        )
        .await
        .expect("grant created");
        view.grant_id
    }

    #[tokio::test]
    async fn delete_grant_requires_superadmin() {
        let (state, _dir) = build_state();
        let gid = seed_grant(&state).await;
        let err = delete_grant(State(state), Extension(alice()), Path(gid))
            .await
            .expect_err("non-superadmin rejected");
        assert_eq!(status_of(err), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn delete_grant_rejects_malformed_id() {
        let (state, _dir) = build_state();
        let err = delete_grant(
            State(state),
            Extension(superadmin()),
            Path("not-a-ulid".to_string()),
        )
        .await
        .expect_err("malformed id rejected");
        // GrantNotFound → 404.
        assert_eq!(status_of(err), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_grant_unknown_id_is_not_found() {
        let (state, _dir) = build_state();
        // Well-formed ULID that was never persisted.
        let err = delete_grant(
            State(state),
            Extension(superadmin()),
            Path(GrantId::new().to_string()),
        )
        .await
        .expect_err("unknown id rejected");
        assert_eq!(status_of(err), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_grant_succeeds_with_empty_cascade() {
        let (state, _dir) = build_state();
        let gid = seed_grant(&state).await;
        let Json(resp) = delete_grant(
            State(state.clone()),
            Extension(superadmin()),
            Path(gid.clone()),
        )
        .await
        .expect("grant revoked");
        assert_eq!(resp.grant_id, gid);
        assert!(resp.removed_rule_ids.is_empty());
        // The grant is gone.
        let stored = state
            .operator_store
            .grants_for(&UserId::from_str("alice").unwrap());
        assert!(stored.is_empty());
    }

    #[tokio::test]
    async fn delete_grant_cascades_orphaned_rule() {
        let (state, _dir) = build_state();
        let gid = seed_grant(&state).await;

        // Insert an in-memory rule owned by `alice`. Removing her only
        // grant leaves zero remaining grants, so the cascade re-evaluation
        // fails `enforce_push` and the rule is removed.
        let rule = state
            .rules
            .push_range(
                ClientId::new(),
                ClientName::new("edge-a").unwrap(),
                PortRange::single(30000),
                "10.0.0.1".to_string(),
                PortRange::single(8080),
                Protocol::Tcp,
                None,
                16,
                UserId::from_str("alice").unwrap(),
            )
            .await
            .expect("push rule");

        let Json(resp) = delete_grant(State(state.clone()), Extension(superadmin()), Path(gid))
            .await
            .expect("grant revoked");
        assert_eq!(resp.removed_rule_ids, vec![rule.id.0]);
        // The orphaned rule was evicted from the in-memory store.
        assert!(state.rules.get(rule.id).await.is_none());
    }
}
