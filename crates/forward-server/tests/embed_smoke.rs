//! 006-management-web-ui T064: smoke test for the embedded SPA.
//!
//! Confirms that:
//! 1. `GET /` returns the bundled `index.html`.
//! 2. An arbitrary client-side route also returns `index.html` (SPA
//!    history fallback).
//! 3. The /v1/* surface still wins on overlap (auth_layer → 401).

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use forward_server::clients::ConnectedClients;
use forward_server::operator::http;
use forward_server::operator::webui::serve_webui;
use forward_server::state::AppState;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPER: &str = "T064-super";

fn build() -> (axum::Router, TempDir) {
    let dir = TempDir::new().unwrap();
    let sqlite_store = Arc::new(forward_server::store::Store::open(dir.path()).unwrap());
    let tokens = Arc::new(forward_server::store::token_store::SqliteTokenStore::new(std::sync::Arc::clone(&sqlite_store)));
    let store = Arc::new(forward_server::store::operator_store::SqliteOperatorStore::new(std::sync::Arc::clone(&sqlite_store)));
    store.bootstrap_legacy_superadmin(SUPER).unwrap();
    let state = Arc::new(
        AppState::new(
            tokens,
            store,
            ConnectedClients::default(),
            "127.0.0.1:0",
            "deadbeef",
            "-----BEGIN CERTIFICATE-----\n",
            16,
            sqlite_store,
        )
        .unwrap(),
    );
    let router = http::router(state).fallback(serve_webui);
    (router, dir)
}

async fn body_text(resp: axum::response::Response) -> String {
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap_or_default()
}

#[tokio::test]
async fn root_serves_index_html() {
    let (router, _d) = build();
    let resp = router
        .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    assert!(ct.starts_with("text/html"), "got content-type: {ct:?}");
    let body = body_text(resp).await;
    assert!(!body.is_empty(), "index body must not be empty");
    assert!(
        body.contains("<html") || body.contains("<!doctype"),
        "expected HTML markers, got: {}",
        &body[..body.len().min(200)]
    );
}

#[tokio::test]
async fn unknown_spa_route_falls_back_to_index() {
    let (router, _d) = build();
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/users/alice/credentials")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_text(resp).await;
    assert!(
        body.contains("<html") || body.contains("<!doctype"),
        "SPA fallback should serve index.html"
    );
}

#[tokio::test]
async fn v1_routes_still_take_precedence() {
    let (router, _d) = build();
    // /v1/users with no bearer → 401 (auth_layer wins, NOT the SPA fallback).
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/v1/users")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
