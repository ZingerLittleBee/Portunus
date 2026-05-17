//! Contract tests for live client enrollment creation over operator HTTP.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use portunus_auth::Authenticator;
use portunus_auth::{OperatorRole, User, UserId};
use portunus_core::ClientName;
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use portunus_server::store::Store;
use portunus_server::store::operator_store::SqliteOperatorStore;
use portunus_server::store::token_store::SqliteTokenStore;
use serde_json::json;
use std::str::FromStr;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPER_TOKEN: &str = "T-enrollment-super";

fn build_router() -> (axum::Router, Arc<SqliteTokenStore>, String, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let sqlite = Arc::new(Store::open(dir.path()).unwrap());
    let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&sqlite)));
    let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&sqlite)));
    operator_store
        .bootstrap_legacy_superadmin(SUPER_TOKEN)
        .expect("bootstrap superadmin");
    let alice_id = UserId::from_str("alice").expect("user id");
    operator_store
        .add_user(User {
            id: alice_id.clone(),
            display_name: "Alice".to_string(),
            role: OperatorRole::User,
            disabled: false,
            created_at: Utc::now(),
        })
        .expect("create alice");
    let (_cred, alice_token) = operator_store
        .issue_credential(&alice_id, Some("test".to_string()))
        .expect("issue alice credential");
    let state = Arc::new(
        AppState::new(
            Arc::clone(&tokens),
            operator_store,
            ConnectedClients::default(),
            Some("public.example:7443".to_string()),
            7443,
            "a".repeat(64),
            include_str!("../src/advertised/testdata/san_fixture.pem"),
            16,
            sqlite,
        )
        .expect("AppState"),
    );
    (http::router(state), tokens, alice_token, dir)
}

fn req_with_bearer(uri: &str, bearer: &str, body: serde_json::Value) -> Request<Body> {
    let body_bytes = serde_json::to_vec(&body).expect("body");
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("content-length", body_bytes.len().to_string())
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::from(body_bytes))
        .expect("request")
}

fn req(uri: &str, body: serde_json::Value) -> Request<Body> {
    req_with_bearer(uri, SUPER_TOKEN, body)
}

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    req(uri, body)
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("body json")
}

#[tokio::test]
async fn create_enrollment_returns_one_time_client_command_without_issuing_token() {
    let (router, tokens, _alice_token, _dir) = build_router();

    let resp = router
        .oneshot(req(
            "/v1/client-enrollments",
            json!({"name": "edge-01", "address": "edge.example.com", "ttl_secs": 300}),
        ))
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["client_name"], "edge-01");
    assert!(body["expires_at"].as_str().is_some_and(|v| !v.is_empty()));
    let command = body["command"].as_str().expect("command");
    assert!(command.starts_with("portunus-client enroll 'portunus://public.example:7443/enroll?"));
    assert!(command.contains("pin=sha256:"));
    assert!(command.contains("code="));
    assert!(command.contains("cert="));
    let uri = body["uri"].as_str().expect("uri");
    assert!(uri.starts_with("portunus://public.example:7443/enroll?"));
    assert!(uri.contains("pin=sha256:"));
    assert!(uri.contains("code="));
    assert!(uri.contains("cert="));
    assert_eq!(command, format!("portunus-client enroll '{uri}'"));
    assert!(tokens.list().expect("list tokens").is_empty());
}

#[tokio::test]
async fn create_enrollment_rejects_existing_client_name() {
    let (router, tokens, _alice_token, _dir) = build_router();
    tokens
        .issue(ClientName::new("edge-01").unwrap())
        .expect("seed client");

    let resp = router
        .oneshot(req(
            "/v1/client-enrollments",
            json!({"name": "edge-01", "address": "edge.example.com"}),
        ))
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "client_already_exists");
}

#[tokio::test]
async fn create_enrollment_requires_superadmin() {
    let (router, tokens, alice_token, _dir) = build_router();

    let resp = router
        .oneshot(req_with_bearer(
            "/v1/client-enrollments",
            &alice_token,
            json!({"name": "edge-01", "address": "edge.example.com"}),
        ))
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "role_required");
    assert!(tokens.list().expect("list tokens").is_empty());
}

#[tokio::test]
async fn direct_client_provision_endpoint_is_removed() {
    let (router, _tokens, _alice_token, _dir) = build_router();

    let resp = router
        .oneshot(post(
            "/v1/clients",
            json!({"name": "edge-01", "address": "edge.example.com"}),
        ))
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn existing_client_enrollment_does_not_rotate_until_redeemed() {
    let (router, tokens, _alice_token, _dir) = build_router();
    let name = ClientName::new("edge-01").unwrap();
    let old_token = tokens.issue(name.clone()).expect("seed client");

    let resp = router
        .oneshot(post(
            "/v1/clients/edge-01/enrollment",
            json!({"ttl_secs": 300}),
        ))
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp).await;
    assert_eq!(body["client_name"], "edge-01");
    let command = body["command"].as_str().expect("command");
    assert!(command.starts_with("portunus-client enroll 'portunus://public.example:7443/enroll?"));
    let uri = body["uri"].as_str().expect("uri");
    assert!(uri.starts_with("portunus://public.example:7443/enroll?"));
    assert_eq!(command, format!("portunus-client enroll '{uri}'"));
    assert_eq!(
        tokens
            .verify(&old_token)
            .expect("old token remains valid before redemption")
            .client_name,
        name
    );
}

#[tokio::test]
async fn old_reissue_endpoint_is_removed() {
    let (router, tokens, _alice_token, _dir) = build_router();
    tokens
        .issue(ClientName::new("edge-01").unwrap())
        .expect("seed client");

    let resp = router
        .oneshot(post("/v1/clients/edge-01/reissue", json!({})))
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
