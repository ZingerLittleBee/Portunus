//! T016 (005-multi-user-rbac, US1) — pin the v0.4 → v0.5 breaking change.
//!
//! A `GET /v1/rules` issued without an `Authorization` header against the
//! v0.5 router (with at least one superadmin in the operator store) MUST
//! return 401 with `error.code = "unauthenticated"`.
//!
//! This test is what catches a future regression that silently re-enables
//! unauthenticated access to the operator API — a protection that is the
//! whole point of v0.5.0 (FR-001 / Constitution Principle I).

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use tempfile::TempDir;
use tower::ServiceExt;

const BOOTSTRAP_TOKEN: &str = "T016-bootstrap-token";

fn build_router_with_superadmin() -> (axum::Router, TempDir) {
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
        .bootstrap_legacy_superadmin(BOOTSTRAP_TOKEN)
        .expect("bootstrap superadmin");
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

#[tokio::test]
async fn get_rules_without_authorization_returns_401_unauthenticated() {
    let (router, _dir) = build_router_with_superadmin();
    let req = Request::builder()
        .uri("/v1/rules")
        .method("GET")
        .body(Body::empty())
        .expect("build request");
    let resp = router.oneshot(req).await.expect("router oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::UNAUTHORIZED,
        "v0.5 must reject unauthenticated requests"
    );
    let body_bytes = to_bytes(resp.into_body(), 4096).await.expect("body bytes");
    let body: serde_json::Value =
        serde_json::from_slice(&body_bytes).expect("error body must be JSON");
    assert_eq!(
        body["error"]["code"].as_str(),
        Some("unauthenticated"),
        "error.code MUST be `unauthenticated`; got body = {body}"
    );
}

#[tokio::test]
async fn get_rules_with_valid_bootstrap_token_succeeds() {
    let (router, _dir) = build_router_with_superadmin();
    let req = Request::builder()
        .uri("/v1/rules")
        .method("GET")
        .header("Authorization", format!("Bearer {BOOTSTRAP_TOKEN}"))
        .body(Body::empty())
        .expect("build request");
    let resp = router.oneshot(req).await.expect("router oneshot");
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "valid bootstrap token MUST be accepted"
    );
}
