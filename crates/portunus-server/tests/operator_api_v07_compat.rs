//! 008-sqlite-storage T038..T040 — operator-API v0.7 byte-stability.
//!
//! Confirms `GET /v1/users`, `GET /v1/rules`, `GET /v1/users/me`, and
//! `GET /v1/users/{id}` retain the v0.7 wire shape after the SQLite
//! migration. Per FR-008, v0.7 callers MUST not be able to tell the
//! backend changed.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use portunus_server::store::operator_store::SqliteOperatorStore;
use portunus_server::store::token_store::SqliteTokenStore;
use portunus_server::store::{Store, audit_writer};
use prometheus::{Gauge, IntCounter};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const SUPER: &str = "T038-super";

fn build() -> (
    axum::Router,
    Arc<AppState>,
    String,
    TempDir,
    CancellationToken,
) {
    let dir = TempDir::new().unwrap();
    let sqlite = Arc::new(Store::open(dir.path()).unwrap());
    let tokens = Arc::new(SqliteTokenStore::new(sqlite.clone()));
    let operator_store = Arc::new(SqliteOperatorStore::new(sqlite.clone()));
    operator_store
        .bootstrap_legacy_superadmin(SUPER)
        .expect("bootstrap");
    // Seed a non-super user with one credential and one grant so the
    // shape is non-trivial.
    use std::str::FromStr;
    let alice = portunus_auth::UserId::from_str("alice").unwrap();
    operator_store
        .add_user(portunus_auth::User {
            id: alice.clone(),
            display_name: "Alice".into(),
            role: portunus_auth::OperatorRole::User,
            disabled: false,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    let (_cred, alice_token) = operator_store
        .seed_credential_for_test(&alice, Some("alice-default".into()))
        .unwrap();
    let state = Arc::new(
        AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            None,
            0,
            "deadbeef",
            include_str!("../src/advertised/testdata/san_fixture.pem"),
            16,
            sqlite.clone(),
        )
        .unwrap(),
    );
    let cancel = CancellationToken::new();
    let drops = IntCounter::new("compat_drops", "test").unwrap();
    let lag = Gauge::new("compat_lag", "test").unwrap();
    let h = audit_writer::spawn(sqlite, drops, lag, cancel.clone());
    state.audit.bind_durable_writer(h);
    (http::router(state.clone()), state, alice_token, dir, cancel)
}

fn req(method: Method, uri: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap()
}

async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn get_v1_users_keeps_v07_shape() {
    let (router, _state, _alice, _d, _c) = build();
    let resp = router
        .oneshot(req(Method::GET, "/v1/users", SUPER))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let arr = v.as_array().expect("v0.7 root must be array");
    assert!(!arr.is_empty(), "expected at least the seeded users: {v}");
    let alice = arr
        .iter()
        .find(|u| u["user_id"] == "alice")
        .expect("seeded alice present");
    let obj = alice.as_object().unwrap();
    for k in [
        "user_id",
        "display_name",
        "role",
        "disabled",
        "created_at",
        "grant_count",
    ] {
        assert!(obj.contains_key(k), "missing v0.7 field `{k}`: {alice}");
    }
    assert_eq!(alice["role"], "user");
    assert_eq!(alice["grant_count"], 0);
}

#[tokio::test]
async fn get_v1_rules_keeps_v07_array_root() {
    let (router, _state, _alice, _d, _c) = build();
    let resp = router
        .oneshot(req(Method::GET, "/v1/rules", SUPER))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    assert!(v.is_array(), "v0.7 /v1/rules MUST return array root: {v}");
}

#[tokio::test]
async fn get_v1_users_me_keeps_v07_shape_for_user() {
    let (router, _state, alice_token, _d, _c) = build();
    let resp = router
        .oneshot(req(Method::GET, "/v1/users/me", &alice_token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let obj = v.as_object().expect("/v1/users/me must be JSON object");
    for k in ["user_id", "display_name", "role"] {
        assert!(obj.contains_key(k), "missing v0.7 field `{k}`: {v}");
    }
    assert_eq!(obj["role"], "user");
    assert_eq!(obj["user_id"], "alice");
}

#[tokio::test]
async fn get_v1_users_id_keeps_v07_shape() {
    let (router, _state, _alice, _d, _c) = build();
    let resp = router
        .oneshot(req(Method::GET, "/v1/users/alice", SUPER))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = json_body(resp).await;
    let obj = v.as_object().expect("/v1/users/{id} must be JSON object");
    for k in [
        "user_id",
        "display_name",
        "role",
        "disabled",
        "created_at",
        "grant_count",
    ] {
        assert!(obj.contains_key(k), "missing v0.7 field `{k}`: {v}");
    }
    assert_eq!(obj["user_id"], "alice");
}
