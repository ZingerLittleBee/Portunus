//! 015-client-stable-id (US3: T034) — stable id-based addressing.
//!
//! Client-scoped operator routes address the client by its stable
//! `client_id`. A rename changes only the display name, so the same id
//! keeps resolving to the same client (FR-012). An unknown or malformed
//! id returns a clean 404 on every client-scoped route — never a 5xx,
//! and never a signal that disambiguates a colliding display name
//! (Constitution V).

use std::str::FromStr;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use portunus_auth::Authenticator;
use portunus_auth::{OperatorRole, User, UserId};
use portunus_core::{ClientId, ClientName};
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use portunus_server::store::Store;
use portunus_server::store::operator_store::SqliteOperatorStore;
use portunus_server::store::token_store::SqliteTokenStore;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const SUPER_TOKEN: &str = "T-addressing-super";

fn build_router() -> (axum::Router, Arc<SqliteTokenStore>, TempDir) {
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

fn get(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("Authorization", format!("Bearer {SUPER_TOKEN}"))
        .body(Body::empty())
        .expect("request")
}

fn patch_name(client_id: &str, name: &str) -> Request<Body> {
    let body = serde_json::to_vec(&json!({ "client_name": name })).unwrap();
    Request::builder()
        .method("PATCH")
        .uri(format!("/v1/clients/{client_id}/name"))
        .header("content-type", "application/json")
        .header("content-length", body.len().to_string())
        .header("Authorization", format!("Bearer {SUPER_TOKEN}"))
        .body(Body::from(body))
        .expect("request")
}

async fn body_json(resp: axum::response::Response) -> Value {
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.expect("bytes");
    serde_json::from_slice(&bytes).expect("json")
}

#[tokio::test]
async fn id_resolves_to_same_client_after_rename() {
    let (router, tokens, _dir) = build_router();
    let token = tokens
        .issue(ClientName::new("edge-before").unwrap())
        .expect("seed client");
    let client_id = tokens.verify(&token).unwrap().client_id;

    // Rename addresses the client by its stable id.
    let resp = router
        .clone()
        .oneshot(patch_name(&client_id.to_string(), "Edge After – 东区"))
        .await
        .expect("rename");
    assert_eq!(resp.status(), StatusCode::OK);

    // The same id still resolves to the same client, now with the new
    // display name — listing keys on the id, not the name.
    let resp = router.oneshot(get("/v1/clients")).await.expect("list");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let row = body
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v.get("client_id").and_then(Value::as_str) == Some(&client_id.to_string()))
        .expect("the renamed client must still be addressable by its id");
    assert_eq!(
        row["client_name"], "Edge After – 东区",
        "the stable id now carries the new display name"
    );
}

#[tokio::test]
async fn unknown_and_malformed_ids_are_404_on_client_scoped_routes() {
    let (router, _tokens, _dir) = build_router();
    let unknown = ClientId::new().to_string(); // valid ULID, no such client
    let malformed = "not-a-ulid";

    // A representative spread of client-scoped routes. Each must 404 for
    // both an unknown-but-valid id and a malformed id — never 5xx, and
    // identically whether or not a colliding display name exists.
    let cases = [
        format!("/v1/clients/{unknown}/quotas"),
        format!("/v1/clients/{unknown}/owners"),
        format!("/v1/clients/{malformed}/quotas"),
        format!("/v1/clients/{malformed}/owners"),
    ];
    for uri in cases {
        let resp = router.clone().oneshot(get(&uri)).await.expect("oneshot");
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "{uri} must return a clean 404"
        );
    }

    // The rename route too: unknown/malformed id → 404 (not 409/422).
    for id in [unknown.as_str(), malformed] {
        let resp = router
            .clone()
            .oneshot(patch_name(id, "whatever"))
            .await
            .expect("rename");
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "rename of {id} must 404"
        );
    }
}
