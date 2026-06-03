//! 015-client-stable-id (US2) — contract tests for identity-safe rename
//! over operator HTTP: `PATCH /v1/clients/{client_id}/name`.
//!
//! Renaming addresses the client by its stable `client_id`, so the id,
//! bearer token, and every id-keyed row survive a display-name change.
//! Unknown / malformed ids are 404.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use portunus_auth::Authenticator;
use portunus_core::ClientName;
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use portunus_server::store::Store;
use portunus_server::store::operator_store::SqliteOperatorStore;
use portunus_server::store::token_store::SqliteTokenStore;
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPER_TOKEN: &str = "T-rename-super";

fn build_router() -> (axum::Router, Arc<SqliteTokenStore>, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let sqlite = Arc::new(Store::open(dir.path()).unwrap());
    let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&sqlite)));
    let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&sqlite)));
    operator_store
        .bootstrap_legacy_superadmin(SUPER_TOKEN)
        .expect("bootstrap superadmin");
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
    (http::router(state), tokens, dir)
}

fn patch(uri: &str, body: serde_json::Value) -> Request<Body> {
    let body_bytes = serde_json::to_vec(&body).expect("body");
    Request::builder()
        .method("PATCH")
        .uri(uri)
        .header("content-type", "application/json")
        .header("content-length", body_bytes.len().to_string())
        .header("Authorization", format!("Bearer {SUPER_TOKEN}"))
        .body(Body::from(body_bytes))
        .expect("request")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("body json")
}

#[tokio::test]
async fn rename_changes_display_name_but_keeps_identity_and_token() {
    let (router, tokens, _dir) = build_router();
    let token = tokens
        .issue(ClientName::new("edge-01").unwrap())
        .expect("seed client");
    let client_id = tokens.verify(&token).unwrap().client_id;

    let resp = router
        .oneshot(patch(
            &format!("/v1/clients/{client_id}/name"),
            json!({"client_name": "Acme Prod – East"}),
        ))
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["client_name"], "Acme Prod – East");
    assert_eq!(
        body["client_id"],
        client_id.to_string(),
        "identity is unchanged by rename"
    );

    // The bearer token still authenticates and now resolves to the new name.
    let after = tokens.verify(&token).expect("token still valid");
    assert_eq!(after.client_id, client_id);
    assert_eq!(after.client_name.as_str(), "Acme Prod – East");
}

#[tokio::test]
async fn rename_unknown_id_is_404() {
    let (router, _tokens, _dir) = build_router();
    let unknown = portunus_core::ClientId::new();

    let resp = router
        .oneshot(patch(
            &format!("/v1/clients/{unknown}/name"),
            json!({"client_name": "whatever"}),
        ))
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "client_not_found");
}

#[tokio::test]
async fn rename_malformed_id_is_404() {
    let (router, _tokens, _dir) = build_router();

    let resp = router
        .oneshot(patch(
            "/v1/clients/not-a-ulid/name",
            json!({"client_name": "whatever"}),
        ))
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn rename_rejects_empty_name() {
    let (router, tokens, _dir) = build_router();
    let token = tokens
        .issue(ClientName::new("edge-01").unwrap())
        .expect("seed client");
    let client_id = tokens.verify(&token).unwrap().client_id;

    let resp = router
        .oneshot(patch(
            &format!("/v1/clients/{client_id}/name"),
            json!({"client_name": "   "}),
        ))
        .await
        .expect("oneshot");

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
