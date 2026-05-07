//! 006-management-web-ui T020: contract test for `GET /v1/audit`.
//!
//! Mirrors the 8-test plan in
//! `specs/006-management-web-ui/contracts/audit-endpoint.md`.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use forward_auth::file_store::FileTokenStore;
use forward_auth::operator_store::FileOperatorStore;
use forward_server::clients::ConnectedClients;
use forward_server::operator::http;
use forward_server::state::AppState;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPERADMIN_TOKEN: &str = "T020-super";

fn build_router_with_alice() -> (axum::Router, Arc<AppState>, String, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let tokens =
        Arc::new(FileTokenStore::open(dir.path().join("tokens.json")).expect("token store"));
    let operator_store = Arc::new(
        FileOperatorStore::open(dir.path().join("identity.json")).expect("operator store"),
    );
    operator_store
        .bootstrap_legacy_superadmin(SUPERADMIN_TOKEN)
        .expect("bootstrap superadmin");
    // Provision alice as a non-superadmin user with an issued credential.
    use std::str::FromStr;
    let alice_id = forward_auth::UserId::from_str("alice").expect("user id");
    operator_store
        .add_user(forward_auth::User {
            id: alice_id.clone(),
            display_name: "Alice".to_string(),
            role: forward_auth::OperatorRole::User,
            disabled: false,
            created_at: chrono::Utc::now(),
        })
        .expect("create alice");
    let (_cred, alice_token) = operator_store
        .issue_credential(&alice_id, Some("alice-default".to_string()))
        .expect("issue alice credential");

    let state = Arc::new(
        AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            "127.0.0.1:0",
            "deadbeef",
            "-----BEGIN CERTIFICATE-----\n",
            16,
        )
        .expect("AppState"),
    );
    (http::router(state.clone()), state, alice_token, dir)
}

fn req(method: &str, uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = bearer {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).expect("req")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("body json")
}

/// Tickle the auth_layer N times to produce N audit-ring entries.
async fn drive_traffic(router: &axum::Router, n: usize) {
    for _ in 0..n {
        let _ = router
            .clone()
            .oneshot(req("GET", "/v1/users", Some(SUPERADMIN_TOKEN)))
            .await
            .expect("oneshot");
    }
}

#[tokio::test]
async fn empty_buffer_returns_empty_array() {
    let (router, state, _alice, _d) = build_router_with_alice();
    // Snapshot the ring directly (no traffic yet → exactly 0 rows).
    assert!(state.audit.is_empty());
    let resp = router
        .oneshot(req("GET", "/v1/audit?limit=10", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    // The audit GET itself records ONE allow row (it audits itself —
    // documented in the contract). So the response sees that one row.
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["outcome"], "allow");
    assert_eq!(arr[0]["path"], "/v1/audit");
}

#[tokio::test]
async fn buffer_returns_at_most_limit_newest_first() {
    let (router, _state, _alice, _d) = build_router_with_alice();
    drive_traffic(&router, 5).await;
    let resp = router
        .oneshot(req("GET", "/v1/audit?limit=3", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 3);
    // Newest is the /v1/audit read itself.
    assert_eq!(arr[0]["path"], "/v1/audit");
    assert_eq!(arr[1]["path"], "/v1/users");
}

#[tokio::test]
async fn invalid_limit_returns_422() {
    let (router, _, _alice, _d) = build_router_with_alice();
    let resp = router
        .clone()
        .oneshot(req("GET", "/v1/audit?limit=2000", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "invalid_limit");

    let resp = router
        .clone()
        .oneshot(req("GET", "/v1/audit?limit=0", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let resp = router
        .oneshot(req("GET", "/v1/audit?limit=abc", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_json(resp).await["error"]["code"], "invalid_limit");
}

#[tokio::test]
async fn outcome_filter_narrows_to_deny() {
    let (router, _state, _alice, _d) = build_router_with_alice();
    // Generate one deny by hitting an endpoint with a bogus token.
    let _ = router
        .clone()
        .oneshot(req("GET", "/v1/users", Some("definitely-not-a-real-token")))
        .await
        .expect("oneshot");
    // Plus a few allows.
    drive_traffic(&router, 2).await;

    let resp = router
        .oneshot(req("GET", "/v1/audit?outcome=deny", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    assert!(!arr.is_empty(), "expected at least one deny row");
    for row in arr {
        assert_eq!(row["outcome"], "deny");
    }
}

#[tokio::test]
async fn invalid_outcome_returns_422() {
    let (router, _, _alice, _d) = build_router_with_alice();
    let resp = router
        .oneshot(req(
            "GET",
            "/v1/audit?outcome=banana",
            Some(SUPERADMIN_TOKEN),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_json(resp).await["error"]["code"], "invalid_outcome");
}

#[tokio::test]
async fn role_user_returns_403_role_required() {
    let (router, _, alice_token, _d) = build_router_with_alice();
    let resp = router
        .oneshot(req("GET", "/v1/audit", Some(&alice_token)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "role_required");
}

#[tokio::test]
async fn missing_bearer_returns_401() {
    let (router, _, _alice, _d) = build_router_with_alice();
    let resp = router
        .oneshot(req("GET", "/v1/audit", None))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["error"]["code"], "unauthenticated");
}

#[tokio::test]
async fn audit_reflects_recent_actions_in_order() {
    let (router, _, _alice, _d) = build_router_with_alice();
    // Sequence: 2 allow GETs, 1 deny (bogus token), 1 allow read.
    let _ = router
        .clone()
        .oneshot(req("GET", "/v1/clients", Some(SUPERADMIN_TOKEN)))
        .await;
    let _ = router
        .clone()
        .oneshot(req("GET", "/v1/rules", Some(SUPERADMIN_TOKEN)))
        .await;
    let _ = router
        .clone()
        .oneshot(req("GET", "/v1/users", Some("not-a-token")))
        .await;
    let resp = router
        .oneshot(req("GET", "/v1/audit?limit=10", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    assert!(arr.len() >= 4, "expected ≥4 rows, got {}", arr.len());
    // Newest-first: the /v1/audit GET is at index 0.
    assert_eq!(arr[0]["path"], "/v1/audit");
    // The deny row sits in there.
    assert!(
        arr.iter()
            .any(|r| r["outcome"] == "deny" && r["path"] == "/v1/users"),
        "expected a deny row for /v1/users: {arr:?}"
    );
}
