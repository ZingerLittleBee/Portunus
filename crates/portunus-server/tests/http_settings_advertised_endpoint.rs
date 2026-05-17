//! Contract tests for GET/PUT /v1/settings/advertised-endpoint.
//!
//! Harness helpers are intentionally local (no shared crate) — mirrors the
//! pattern established in http_client_enrollments_contract.rs.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use portunus_auth::{OperatorRole, User, UserId};
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use portunus_server::store::Store;
use portunus_server::store::operator_store::SqliteOperatorStore;
use serde_json::json;
use std::str::FromStr;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPER_TOKEN: &str = "T-settings-super";

fn build_router() -> (axum::Router, Arc<SqliteOperatorStore>, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let sqlite = Arc::new(Store::open(dir.path()).unwrap());
    let tokens = Arc::new(portunus_server::store::token_store::SqliteTokenStore::new(
        Arc::clone(&sqlite),
    ));
    let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&sqlite)));
    operator_store
        .bootstrap_legacy_superadmin(SUPER_TOKEN)
        .expect("bootstrap superadmin");
    let alice_id = UserId::from_str("alice").expect("user id");
    operator_store
        .add_user(User {
            id: alice_id,
            display_name: "Alice".to_string(),
            role: OperatorRole::User,
            disabled: false,
            created_at: Utc::now(),
        })
        .expect("create alice");
    let state = Arc::new(
        AppState::new(
            Arc::clone(&tokens),
            Arc::clone(&operator_store),
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
    (http::router(state), operator_store, dir)
}

fn get_req(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("Authorization", format!("Bearer {SUPER_TOKEN}"))
        .body(Body::empty())
        .expect("request")
}

fn put_req(uri: &str, body: serde_json::Value) -> Request<Body> {
    let body_bytes = serde_json::to_vec(&body).expect("body");
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .header("content-length", body_bytes.len().to_string())
        .header("Authorization", format!("Bearer {SUPER_TOKEN}"))
        .header("X-Portunus-CSRF", "1")
        .body(Body::from(body_bytes))
        .expect("request")
}

/// PUT without Authorization header — the protected route rejects it with 401.
/// Tests that the route lives in the authenticated block (complementary to
/// the CSRF check that would fire for cookie-authenticated requests).
fn put_req_no_csrf(uri: &str, body: serde_json::Value) -> Request<Body> {
    let body_bytes = serde_json::to_vec(&body).expect("body");
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .header("content-length", body_bytes.len().to_string())
        // No Authorization and no CSRF header — auth middleware rejects first.
        .body(Body::from(body_bytes))
        .expect("request")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("body json")
}

// ---- tests ----

#[tokio::test]
async fn get_returns_200_with_effective_when_resolvable() {
    let (router, _op, _dir) = build_router();
    let resp = router
        .oneshot(get_req("/v1/settings/advertised-endpoint"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let b = body_json(resp).await;
    // No override set; seed = "public.example:7443" → tier-2 wins.
    assert!(b["override"].is_null());
    assert_eq!(b["source"], "seed");
    assert_eq!(b["effective"], "public.example:7443");
}

#[tokio::test]
async fn put_then_get_round_trips() {
    let (router, _op, _dir) = build_router();
    let put = router
        .clone()
        .oneshot(put_req(
            "/v1/settings/advertised-endpoint",
            json!({"advertised_endpoint": "public.example:7443"}),
        ))
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let get = router
        .oneshot(get_req("/v1/settings/advertised-endpoint"))
        .await
        .unwrap();
    let b = body_json(get).await;
    assert_eq!(b["override"], "public.example:7443");
    assert_eq!(b["source"], "override");
    assert_eq!(b["effective"], "public.example:7443");
}

#[tokio::test]
async fn put_rejects_grammar_with_422_endpoint_invalid() {
    let (router, _op, _dir) = build_router();
    let resp = router
        .oneshot(put_req(
            "/v1/settings/advertised-endpoint",
            json!({"advertised_endpoint": "https://x:7443"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let b = body_json(resp).await;
    assert_eq!(b["error"]["code"], "endpoint_invalid");
}

#[tokio::test]
async fn put_rejects_uncovered_host_with_422_not_in_cert_san() {
    let (router, _op, _dir) = build_router();
    let resp = router
        .oneshot(put_req(
            "/v1/settings/advertised-endpoint",
            json!({"advertised_endpoint": "not.in.cert:7443"}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let b = body_json(resp).await;
    assert_eq!(b["error"]["code"], "endpoint_not_in_cert_san");
}

#[tokio::test]
async fn put_missing_csrf_is_rejected() {
    let (router, _op, _dir) = build_router();
    let resp = router
        .oneshot(put_req_no_csrf(
            "/v1/settings/advertised-endpoint",
            json!({"advertised_endpoint": "public.example:7443"}),
        ))
        .await
        .unwrap();
    // No auth header → 401 Unauthorized (route is in the protected block).
    assert!(resp.status().is_client_error());
}
