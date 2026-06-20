//! Security regression (v2.0 audit): client-lifecycle and per-owner
//! rate-limit admin routes MUST reject a non-superadmin (User-role)
//! caller.
//!
//! Before this fix, `post_revoke` / `delete_client` / `put_client` /
//! `patch_client_name` (http.rs) and the four owner-cap handlers
//! (owner_cap.rs) took no `OperatorIdentity` and performed no role
//! check. Any authenticated User could therefore revoke (→ disconnect a
//! live data-plane session), delete, re-address, or rename ANY tenant's
//! client, and read/mutate ANY owner's rate-limit cap — a cross-tenant
//! privilege escalation / IDOR. These routes are administrative and must
//! be superadmin-only, matching the sibling enrollment routes
//! (`post_client_enrollments` / `post_client_reenrollment`) and the
//! per-(user, client) traffic-quota CRUD (`quota_http.rs`).
//!
//! Note: `GET /v1/clients` is intentionally NOT covered here — the Web
//! UI exposes the client list to User-role operators by design, so its
//! correct hardening is grant-scoped read filtering rather than a
//! blanket superadmin gate. Tracked separately.

#![allow(clippy::wildcard_imports)]

use std::str::FromStr;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use portunus_auth::{OperatorRole, User, UserId};
use portunus_core::ClientName;
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPERADMIN_TOKEN: &str = "rbac-authz-super";
const VICTIM_CLIENT: &str = "victim-edge";

struct Fixture {
    router: axum::Router,
    alice_token: String,
    victim_client_id: String,
    _dir: TempDir,
}

fn build_fixture() -> Fixture {
    let dir = TempDir::new().expect("tempdir");
    let sqlite_store = Arc::new(portunus_server::store::Store::open(dir.path()).unwrap());
    let tokens = Arc::new(portunus_server::store::token_store::SqliteTokenStore::new(
        Arc::clone(&sqlite_store),
    ));
    let operator_store = Arc::new(
        portunus_server::store::operator_store::SqliteOperatorStore::new(Arc::clone(&sqlite_store)),
    );
    operator_store
        .bootstrap_legacy_superadmin(SUPERADMIN_TOKEN)
        .expect("bootstrap superadmin");

    // A plain User-role operator with a valid bearer credential.
    let alice_id = UserId::from_str("alice").expect("valid user id");
    operator_store
        .add_user(User {
            id: alice_id.clone(),
            display_name: "Alice".to_string(),
            role: OperatorRole::User,
            created_at: Utc::now(),
            disabled: false,
        })
        .expect("add alice");
    let (_cred, alice_token) = operator_store
        .seed_credential_for_test(&alice_id, Some("authz-test".to_string()))
        .expect("issue alice credential");

    // Provision a victim client; the operator surface addresses it by
    // its server-assigned stable id (ULID).
    let victim_name = ClientName::new(VICTIM_CLIENT.to_string()).expect("valid client");
    tokens
        .issue_with_address(victim_name.clone(), None)
        .expect("issue victim token");
    let victim_client_id = tokens
        .list()
        .expect("list clients")
        .into_iter()
        .find(|p| p.client_name == victim_name)
        .expect("victim present")
        .client_id
        .to_string();

    let state = Arc::new(
        AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            None,
            0,
            "deadbeef",
            include_str!("../src/advertised/testdata/san_fixture.pem"),
            16,
            Arc::clone(&sqlite_store),
        )
        .expect("AppState"),
    );

    Fixture {
        router: http::router(state.clone()),
        alice_token,
        victim_client_id,
        _dir: dir,
    }
}

fn req(method: &str, uri: &str, bearer: &str, body: Option<serde_json::Value>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {bearer}"));
    let body = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&v).expect("serialize body"))
        }
        None => Body::empty(),
    };
    builder.body(body).expect("build request")
}

async fn err_code(resp: axum::response::Response) -> String {
    let bytes = to_bytes(resp.into_body(), 8192).await.expect("body bytes");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("error body must be JSON");
    v["error"]["code"]
        .as_str()
        .unwrap_or("<missing>")
        .to_string()
}

/// Drive `request` against a fresh fixture and assert the User-role
/// caller is rejected with `403 role_required`.
async fn assert_user_forbidden(method: &str, path_tmpl: &str, body: Option<serde_json::Value>) {
    let f = build_fixture();
    let uri = path_tmpl.replace("{id}", &f.victim_client_id);
    let resp = f
        .router
        .clone()
        .oneshot(req(method, &uri, &f.alice_token, body))
        .await
        .expect("oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "User-role caller must be forbidden on {method} {path_tmpl}"
    );
    assert_eq!(
        err_code(resp).await,
        "role_required",
        "expected role_required on {method} {path_tmpl}"
    );
}

#[tokio::test]
async fn user_cannot_revoke_client() {
    assert_user_forbidden("POST", "/v1/clients/{id}/revoke", None).await;
}

#[tokio::test]
async fn user_cannot_delete_client() {
    assert_user_forbidden("DELETE", "/v1/clients/{id}", None).await;
}

#[tokio::test]
async fn user_cannot_update_client_address() {
    assert_user_forbidden(
        "PUT",
        "/v1/clients/{id}",
        Some(serde_json::json!({ "address": "10.0.0.9:9000" })),
    )
    .await;
}

#[tokio::test]
async fn user_cannot_rename_client() {
    assert_user_forbidden(
        "PATCH",
        "/v1/clients/{id}/name",
        Some(serde_json::json!({ "client_name": "hijacked" })),
    )
    .await;
}

#[tokio::test]
async fn user_cannot_read_owner_rate_limit() {
    assert_user_forbidden("GET", "/v1/clients/{id}/owners/bob/rate-limit", None).await;
}

#[tokio::test]
async fn user_cannot_put_owner_rate_limit() {
    assert_user_forbidden(
        "PUT",
        "/v1/clients/{id}/owners/bob/rate-limit",
        Some(serde_json::json!({ "bandwidth_in_bps": 1 })),
    )
    .await;
}

#[tokio::test]
async fn user_cannot_delete_owner_rate_limit() {
    assert_user_forbidden("DELETE", "/v1/clients/{id}/owners/bob/rate-limit", None).await;
}

#[tokio::test]
async fn user_cannot_list_owners_under_client() {
    assert_user_forbidden("GET", "/v1/clients/{id}/owners", None).await;
}

/// Sanity: the superadmin is NOT blocked by the new role gate (the
/// listing succeeds with 200 for an admin caller).
#[tokio::test]
async fn superadmin_can_list_owners_under_client() {
    let f = build_fixture();
    let uri = format!("/v1/clients/{}/owners", f.victim_client_id);
    let resp = f
        .router
        .clone()
        .oneshot(req("GET", &uri, SUPERADMIN_TOKEN, None))
        .await
        .expect("oneshot");
    assert_ne!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "superadmin must not be blocked by the role gate"
    );
    assert_eq!(resp.status(), StatusCode::OK);
}
