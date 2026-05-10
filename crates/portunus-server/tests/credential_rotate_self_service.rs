//! T046 (005-multi-user-rbac, US4) — credential rotation self-service.
//!
//! Alice authenticates with her current credential and rotates it
//! herself: the response carries a fresh `token`, the old token then
//! 401s on subsequent requests, and the new token works. The credential
//! list shows the old credential as `revoked` and the new one as `active`.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPER_TOKEN: &str = "T046-super";

fn build_router() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let sqlite_store =
        std::sync::Arc::new(portunus_server::store::Store::open(dir.path()).unwrap());
    let tokens = Arc::new(portunus_server::store::token_store::SqliteTokenStore::new(
        std::sync::Arc::clone(&sqlite_store),
    ));
    let operator_store = Arc::new(
        portunus_server::store::operator_store::SqliteOperatorStore::new(std::sync::Arc::clone(
            &sqlite_store,
        )),
    );
    operator_store
        .bootstrap_legacy_superadmin(SUPER_TOKEN)
        .expect("bootstrap");
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
    (http::router(state), dir)
}

fn req(method: &str, uri: &str, bearer: &str, body: serde_json::Value) -> Request<Body> {
    let body_bytes = serde_json::to_vec(&body).expect("body");
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {bearer}"));
    if method == "GET" || method == "DELETE" {
        return b.body(Body::empty()).expect("req");
    }
    b = b.header("content-length", body_bytes.len().to_string());
    b.body(Body::from(body_bytes)).expect("req")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 16384).await.expect("body bytes");
    serde_json::from_slice(&bytes).expect("body json")
}

#[tokio::test]
async fn alice_can_rotate_her_own_credential_using_old_token() {
    let (router, _d) = build_router();
    // Bootstrap alice + her credential via the superadmin.
    let resp = router
        .clone()
        .oneshot(req(
            "POST",
            "/v1/users",
            SUPER_TOKEN,
            json!({"user_id": "alice", "display_name": "Alice"}),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = router
        .clone()
        .oneshot(req(
            "POST",
            "/v1/users/alice/credentials",
            SUPER_TOKEN,
            json!({"label": "laptop"}),
        ))
        .await
        .expect("oneshot");
    let body = body_json(resp).await;
    let old_token = body["token"].as_str().expect("token").to_string();
    let cred_id = body["credential_id"].as_str().expect("cred").to_string();

    // Alice rotates with HER OWN token.
    let resp = router
        .clone()
        .oneshot(req(
            "POST",
            &format!("/v1/users/alice/credentials/{cred_id}/rotate"),
            &old_token,
            json!({"label": "rotated"}),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let new_body = body_json(resp).await;
    let new_token = new_body["token"].as_str().expect("new token");
    let new_cred_id = new_body["credential_id"].as_str().expect("new cred id");
    assert_ne!(new_token, old_token);
    assert_ne!(new_cred_id, cred_id);

    // Old token: 401 on the next request.
    let resp = router
        .clone()
        .oneshot(req(
            "GET",
            "/v1/users/alice/credentials",
            &old_token,
            json!(null),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // New token works.
    let resp = router
        .clone()
        .oneshot(req(
            "GET",
            "/v1/users/alice/credentials",
            new_token,
            json!(null),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let listing = body_json(resp).await;
    let arr = listing.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    let old_entry = arr
        .iter()
        .find(|c| c["credential_id"] == cred_id)
        .expect("old credential entry");
    assert_eq!(old_entry["status"], "revoked");
    assert!(old_entry["revoked_at"].is_string());
    let new_entry = arr
        .iter()
        .find(|c| c["credential_id"] == new_cred_id)
        .expect("new credential entry");
    assert_eq!(new_entry["status"], "active");
}
