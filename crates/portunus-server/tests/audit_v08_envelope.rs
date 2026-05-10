//! 008-sqlite-storage T066..T070 — envelope-mode contract for `GET /v1/audit`.
//!
//! Covers since/until/cursor parsing, time-range validation, opaque
//! cursor encode/decode, and pagination round-trip.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::SecondsFormat;
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::audit::{AuditEntry, AuditOutcome};
use portunus_server::operator::http;
use portunus_server::state::AppState;
use portunus_server::store::audit_writer;
use prometheus::{Gauge, IntCounter};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const SUPER: &str = "T066-super";

fn build() -> (axum::Router, Arc<AppState>, TempDir, CancellationToken) {
    let dir = TempDir::new().unwrap();
    let sqlite = Arc::new(portunus_server::store::Store::open(dir.path()).unwrap());
    let tokens = Arc::new(portunus_server::store::token_store::SqliteTokenStore::new(
        sqlite.clone(),
    ));
    let operator_store =
        Arc::new(portunus_server::store::operator_store::SqliteOperatorStore::new(sqlite.clone()));
    operator_store
        .bootstrap_legacy_superadmin(SUPER)
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
            sqlite.clone(),
        )
        .unwrap(),
    );
    // Wire the durable writer so we can seed audit rows.
    let cancel = CancellationToken::new();
    let drops = IntCounter::new("env_drops", "test").unwrap();
    let lag = Gauge::new("env_lag", "test").unwrap();
    let handle = audit_writer::spawn(sqlite, drops, lag, cancel.clone());
    state.audit.bind_durable_writer(handle);
    (http::router(state.clone()), state, dir, cancel)
}

fn req(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("Authorization", format!("Bearer {SUPER}"))
        .body(Body::empty())
        .unwrap()
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn seed(state: &Arc<AppState>, n: usize) {
    let base = chrono::Utc::now() - chrono::Duration::seconds(i64::try_from(n).unwrap());
    for i in 0..n {
        let ts = base + chrono::Duration::seconds(i64::try_from(i).unwrap());
        state.audit.push(AuditEntry {
            timestamp: ts,
            actor: format!("u-{i}"),
            role: Some(portunus_auth::OperatorRole::Superadmin),
            method: "GET".into(),
            path: "/v1/users".into(),
            outcome: if i % 2 == 0 {
                AuditOutcome::Allow
            } else {
                AuditOutcome::Deny
            },
            reason: None,
        });
    }
    // Allow the durable writer to flush.
    tokio::time::sleep(Duration::from_millis(250)).await;
}

#[tokio::test]
async fn since_returns_envelope_shape() {
    let (router, state, _d, _c) = build();
    seed(&state, 3).await;
    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(1))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    let resp = router
        .oneshot(req(&format!("/v1/audit?since={cutoff}&limit=10")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(v.get("entries").is_some(), "envelope shape: {v}");
    assert!(v.get("count").is_some());
    let entries = v["entries"].as_array().unwrap();
    assert!(entries.len() >= 3);
}

#[tokio::test]
async fn since_after_until_returns_400_invalid_time_range() {
    let (router, state, _d, _c) = build();
    seed(&state, 1).await;
    let s = chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let u = (chrono::Utc::now() - chrono::Duration::hours(1))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    let resp = router
        .oneshot(req(&format!("/v1/audit?since={s}&until={u}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "invalid_time_range");
}

#[tokio::test]
async fn invalid_rfc3339_returns_400_invalid_timestamp() {
    let (router, state, _d, _c) = build();
    seed(&state, 1).await;
    let resp = router
        .oneshot(req("/v1/audit?since=not-a-timestamp"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "invalid_timestamp");
}

#[tokio::test]
async fn invalid_cursor_returns_400_invalid_cursor() {
    let (router, state, _d, _c) = build();
    seed(&state, 1).await;
    let resp = router
        .oneshot(req("/v1/audit?cursor=!not-base64!"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "invalid_cursor");
}

#[tokio::test]
async fn cursor_pagination_walks_every_row_once() {
    let (router, state, _d, _c) = build();
    seed(&state, 7).await;
    let mut all_seen = Vec::<String>::new();
    let mut cursor: Option<String> = None;
    let cutoff = (chrono::Utc::now() - chrono::Duration::hours(1))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    for _ in 0..10 {
        let path = match &cursor {
            Some(c) => format!("/v1/audit?since={cutoff}&limit=2&cursor={c}"),
            None => format!("/v1/audit?since={cutoff}&limit=2"),
        };
        let resp = router.clone().oneshot(req(&path)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        for e in v["entries"].as_array().unwrap() {
            all_seen.push(e["actor"].as_str().unwrap().to_string());
        }
        match v.get("next_cursor").and_then(|x| x.as_str()) {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    // Each actor "u-0".."u-6" appears exactly once.
    let mut sorted = all_seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), all_seen.len(), "no duplicates: {all_seen:?}");
    assert_eq!(all_seen.len(), 7, "every seeded row reached: {all_seen:?}");
}

#[tokio::test]
async fn no_envelope_params_keeps_v07_array_shape() {
    let (router, state, _d, _c) = build();
    seed(&state, 2).await;
    let resp = router.oneshot(req("/v1/audit?limit=10")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert!(v.is_array(), "v0.7 callers must keep array root: {v}");
}
