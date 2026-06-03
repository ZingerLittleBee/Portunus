//! 015-client-stable-id (US1: T024) — friendly client names over the
//! operator API.
//!
//! `client_name` is a free-form display field (FR-011/FR-013): uppercase,
//! spaces, dots, underscores, en-dashes and Unicode all round-trip
//! verbatim; only empty/whitespace-only, control characters, and names
//! over the byte cap are rejected — each with the `invalid_name` code.
//! Creating a second client with an identical display name succeeds with
//! no warning (names are NOT an identity).

use std::str::FromStr;
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
use portunus_server::store::token_store::SqliteTokenStore;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const SUPER_TOKEN: &str = "T-friendly-super";

fn build_router() -> (axum::Router, TempDir) {
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
            tokens,
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
    (http::router(state), dir)
}

fn enroll_req(name: &str) -> Request<Body> {
    let body = serde_json::to_vec(&json!({"name": name, "address": "edge.example.com"})).unwrap();
    Request::builder()
        .method("POST")
        .uri("/v1/client-enrollments")
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
async fn friendly_names_round_trip_verbatim() {
    // A name a strict DNS-label rule would have rejected: capitals, a
    // space, an en-dash. And a fully non-ASCII name.
    for name in ["Acme Prod – East", "北京边缘节点", "edge_01.prod"] {
        let (router, _dir) = build_router();
        let resp = router.oneshot(enroll_req(name)).await.expect("oneshot");
        assert_eq!(
            resp.status(),
            StatusCode::CREATED,
            "name {name:?} must enroll"
        );
        let body = body_json(resp).await;
        assert_eq!(
            body["client_name"], name,
            "display name must be stored and echoed verbatim"
        );
    }
}

#[tokio::test]
async fn bad_names_are_rejected_with_invalid_name() {
    let long = "x".repeat(256); // CLIENT_NAME_MAX_BYTES is 255
    let cases = [
        ("", "empty"),
        ("   ", "whitespace-only"),
        ("edge\u{0001}01", "control char"),
        (long.as_str(), "256 bytes"),
    ];
    for (name, why) in cases {
        let (router, _dir) = build_router();
        let resp = router.oneshot(enroll_req(name)).await.expect("oneshot");
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "{why} name must be rejected"
        );
        let body = body_json(resp).await;
        assert_eq!(
            body["error"]["code"], "invalid_name",
            "{why}: field-specific error code"
        );
    }
}

#[tokio::test]
async fn duplicate_display_name_enrolls_without_warning() {
    // FR-013: names are non-unique. A second enrollment for an existing
    // display name succeeds — it mints a fresh stable id at redeem.
    let (router, _dir) = build_router();
    let resp = router
        .clone()
        .oneshot(enroll_req("Acme Prod – East"))
        .await
        .expect("first");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = router
        .oneshot(enroll_req("Acme Prod – East"))
        .await
        .expect("second");
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "duplicate display name must be accepted with no warning"
    );
}
