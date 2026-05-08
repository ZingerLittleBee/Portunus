//! T027 (005-multi-user-rbac, US2) — `/v1/users/{id}/credentials` contract.
//!
//! Verifies issue-once token semantics, listing without token leaks,
//! revoke side-effects, and the cross-user `not_owner` gate.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use forward_server::clients::ConnectedClients;
use forward_server::operator::http;
use forward_server::state::AppState;
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPER_TOKEN: &str = "T027-super";

fn build_router() -> (axum::Router, TempDir) {
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

async fn create_user(router: &axum::Router, id: &str) {
    let resp = router
        .clone()
        .oneshot(req(
            "POST",
            "/v1/users",
            SUPER_TOKEN,
            json!({"user_id": id, "display_name": id}),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn issue_credential_returns_token_once() {
    let (router, _d) = build_router();
    create_user(&router, "alice").await;

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
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v = body_json(resp).await;
    let token = v["token"].as_str().expect("token in response");
    assert!(token.len() >= 32, "token is short: {token}");
    let cred_id = v["credential_id"].as_str().expect("cred id");

    // GET listing must NOT include token field anywhere.
    let resp = router
        .oneshot(req(
            "GET",
            "/v1/users/alice/credentials",
            SUPER_TOKEN,
            json!(null),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let arr = body_json(resp).await;
    let arr = arr.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert!(arr[0].get("token").is_none(), "list MUST NOT carry token");
    assert_eq!(arr[0]["credential_id"], cred_id);
    assert_eq!(arr[0]["status"], "active");
}

#[tokio::test]
async fn revoke_credential_invalidates_subsequent_verify() {
    let (router, _d) = build_router();
    create_user(&router, "alice").await;

    let issue = router
        .clone()
        .oneshot(req(
            "POST",
            "/v1/users/alice/credentials",
            SUPER_TOKEN,
            json!({}),
        ))
        .await
        .expect("oneshot");
    let body = body_json(issue).await;
    let alice_token = body["token"].as_str().expect("token").to_string();
    let cred_id = body["credential_id"].as_str().expect("cred").to_string();

    // Alice can use her token now.
    let resp = router
        .clone()
        .oneshot(req(
            "GET",
            "/v1/users/alice/credentials",
            &alice_token,
            json!(null),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);

    // Superadmin revokes alice's credential.
    let resp = router
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/v1/users/alice/credentials/{cred_id}"),
            SUPER_TOKEN,
            json!(null),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Alice's old token now fails with 401 credential_invalid.
    let resp = router
        .oneshot(req(
            "GET",
            "/v1/users/alice/credentials",
            &alice_token,
            json!(null),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rotate_credential_returns_new_token_invalidates_old() {
    let (router, _d) = build_router();
    create_user(&router, "alice").await;
    let issue = router
        .clone()
        .oneshot(req(
            "POST",
            "/v1/users/alice/credentials",
            SUPER_TOKEN,
            json!({}),
        ))
        .await
        .expect("oneshot");
    let body = body_json(issue).await;
    let old_token = body["token"].as_str().expect("token").to_string();
    let old_cred = body["credential_id"].as_str().expect("cred").to_string();

    let resp = router
        .clone()
        .oneshot(req(
            "POST",
            &format!("/v1/users/alice/credentials/{old_cred}/rotate"),
            &old_token,
            json!({"label": "rotated"}),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let new_body = body_json(resp).await;
    let new_token = new_body["token"].as_str().expect("new token");
    assert_ne!(new_token, old_token);

    // Old token: 401.
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

    // New token: 200.
    let resp = router
        .oneshot(req(
            "GET",
            "/v1/users/alice/credentials",
            new_token,
            json!(null),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn cross_user_credential_issue_returns_403_not_owner() {
    let (router, _d) = build_router();
    create_user(&router, "alice").await;
    create_user(&router, "bob").await;
    let alice_issue = router
        .clone()
        .oneshot(req(
            "POST",
            "/v1/users/alice/credentials",
            SUPER_TOKEN,
            json!({}),
        ))
        .await
        .expect("oneshot");
    let alice_token = body_json(alice_issue).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = router
        .oneshot(req(
            "POST",
            "/v1/users/bob/credentials",
            &alice_token,
            json!({}),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
