//! 006-management-web-ui T022: contract test for `GET /v1/users/me`.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use forward_server::clients::ConnectedClients;
use forward_server::operator::http;
use forward_server::state::AppState;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPERADMIN_TOKEN: &str = "T022-super";

fn build_router_with_alice() -> (axum::Router, String, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let sqlite_store = std::sync::Arc::new(forward_server::store::Store::open(dir.path()).unwrap());
    let tokens = Arc::new(forward_server::store::token_store::SqliteTokenStore::new(
        std::sync::Arc::clone(&sqlite_store),
    ));
    let operator_store = Arc::new(
        forward_server::store::operator_store::SqliteOperatorStore::new(std::sync::Arc::clone(
            &sqlite_store,
        )),
    );
    operator_store
        .bootstrap_legacy_superadmin(SUPERADMIN_TOKEN)
        .expect("bootstrap superadmin");
    use std::str::FromStr;
    let alice_id = forward_auth::UserId::from_str("alice").expect("user id");
    operator_store
        .add_user(forward_auth::User {
            id: alice_id.clone(),
            display_name: "Alice — payments".to_string(),
            role: forward_auth::OperatorRole::User,
            disabled: false,
            created_at: chrono::Utc::now(),
        })
        .expect("create alice");
    let (_, alice_token) = operator_store
        .issue_credential(&alice_id, None)
        .expect("issue alice cred");
    let state = Arc::new(
        AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            "127.0.0.1:0",
            "deadbeef",
            "-----BEGIN CERTIFICATE-----\n",
            16,
            std::sync::Arc::clone(&sqlite_store),
        )
        .expect("AppState"),
    );
    (http::router(state), alice_token, dir)
}

fn req(bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri("/v1/users/me");
    if let Some(t) = bearer {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).expect("req")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 8 * 1024).await.expect("body");
    serde_json::from_slice(&bytes).expect("body json")
}

#[tokio::test]
async fn superadmin_returns_role_superadmin() {
    let (router, _alice, _d) = build_router_with_alice();
    let resp = router
        .oneshot(req(Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["role"], "superadmin");
    assert!(v["user_id"].as_str().is_some(), "user_id missing");
    assert!(v["display_name"].as_str().is_some(), "display_name missing");
}

#[tokio::test]
async fn user_returns_role_user() {
    let (router, alice_token, _d) = build_router_with_alice();
    let resp = router
        .oneshot(req(Some(&alice_token)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["role"], "user");
    assert_eq!(v["user_id"], "alice");
    assert_eq!(v["display_name"], "Alice — payments");
}

#[tokio::test]
async fn missing_bearer_returns_401() {
    let (router, _alice, _d) = build_router_with_alice();
    let resp = router.oneshot(req(None)).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn invalid_token_returns_401() {
    let (router, _alice, _d) = build_router_with_alice();
    let resp = router
        .oneshot(req(Some("not-a-real-token")))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
