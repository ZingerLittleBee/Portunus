//! Phase A3 — contract test for `GET /v1/traffic/global`.
//!
//! Two scenarios:
//!   1. tenant (non-superadmin) GET -> 403
//!   2. superadmin GET -> 200, samples aggregated across all users/clients

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use portunus_server::traffic_quotas::samples;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPERADMIN_TOKEN: &str = "PhaseA3-super";

fn build_router_with_alice() -> (axum::Router, Arc<AppState>, String, TempDir) {
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
        .bootstrap_legacy_superadmin(SUPERADMIN_TOKEN)
        .expect("bootstrap superadmin");
    // Provision alice as a non-superadmin tenant with an issued credential.
    use std::str::FromStr;
    let alice_id = portunus_auth::UserId::from_str("alice").expect("user id");
    operator_store
        .add_user(portunus_auth::User {
            id: alice_id.clone(),
            display_name: "Alice".to_string(),
            role: portunus_auth::OperatorRole::User,
            disabled: false,
            created_at: chrono::Utc::now(),
        })
        .expect("create alice");
    let (_cred, alice_token) = operator_store
        .issue_credential(&alice_id, Some("alice-default".to_string()))
        .expect("issue alice credential");

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
    (http::router(state.clone()), state, alice_token, dir)
}

fn req(method: &str, uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = bearer {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).expect("req")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("body json")
}

/// Seed two 1m traffic samples in distinct (user, client) cells at the
/// same minute boundary so the global aggregate must merge them.
fn seed_two_users(state: &AppState, ts_minute: i64) {
    samples::upsert_1m_delta(&state.store, "alice", "edge-a", ts_minute, 100, 200)
        .expect("seed alice");
    samples::upsert_1m_delta(&state.store, "bob", "edge-b", ts_minute, 300, 400)
        .expect("seed bob");
}

/// Pick a minute boundary that lies inside the 1m bucket's retention
/// window (7 days) so `serve_traffic` doesn't reject the query with
/// `traffic_bucket_out_of_retention`. Aligns to a 1m boundary so the
/// upsert + query share the same ts.
fn recent_minute() -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0);
    let two_min_ago = now - 120;
    two_min_ago - (two_min_ago % 60)
}

#[tokio::test]
async fn tenant_get_global_traffic_is_forbidden() {
    let (router, state, alice_token, _d) = build_router_with_alice();
    let ts = recent_minute();
    seed_two_users(&state, ts);

    let uri = format!("/v1/traffic/global?from={}&to={}&bucket=1m", ts - 1, ts + 60);
    let resp = router
        .oneshot(req("GET", &uri, Some(&alice_token)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn superadmin_get_global_traffic_aggregates_across_users() {
    let (router, state, _alice_token, _d) = build_router_with_alice();
    let ts = recent_minute();
    seed_two_users(&state, ts);

    let uri = format!("/v1/traffic/global?from={}&to={}&bucket=1m", ts - 1, ts + 60);
    let resp = router
        .oneshot(req("GET", &uri, Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["bucket"], "1m");
    assert_eq!(body["total_bytes_in"], 400);
    assert_eq!(body["total_bytes_out"], 600);
    let samples = body["samples"].as_array().expect("samples array");
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0]["ts"], ts);
    assert_eq!(samples[0]["bytes_in"], 400);
    assert_eq!(samples[0]["bytes_out"], 600);
}
