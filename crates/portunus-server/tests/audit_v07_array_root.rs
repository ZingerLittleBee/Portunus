//! 008-sqlite-storage T026 — v0.7 callers MUST keep getting the array
//! root from `GET /v1/audit?limit=&outcome=`.
//!
//! Byte-stability per FR-008: clients written against v0.7 (no
//! `since` / `until` / `cursor` query params) cannot tell SQLite is
//! under the hood. The response root is a JSON array; each entry has
//! the v0.6 / v0.7 field set (`timestamp`, `actor`, `role`, `method`,
//! `path`, `outcome`, optionally `reason`).

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::audit::{AuditEntry, AuditOutcome};
use portunus_server::operator::http;
use portunus_server::state::AppState;
use portunus_server::store::audit_writer;
use prometheus::{Gauge, IntCounter};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const SUPER: &str = "T026-super";

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
    let cancel = CancellationToken::new();
    let drops = IntCounter::new("v07_drops", "test").unwrap();
    let lag = Gauge::new("v07_lag", "test").unwrap();
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

#[tokio::test]
async fn v07_callers_get_array_root_with_v07_field_set() {
    let (router, state, _d, _c) = build();
    // Seed a deterministic mix of allow + deny rows so the outcome
    // filter has something to chew on.
    for i in 0..4 {
        state.audit.push(AuditEntry {
            timestamp: chrono::Utc::now() - chrono::Duration::seconds(i64::from(4 - i)),
            actor: format!("user-{i}"),
            role: Some(portunus_auth::OperatorRole::User),
            method: "GET".into(),
            path: "/v1/users".into(),
            outcome: if i % 2 == 0 {
                AuditOutcome::Allow
            } else {
                AuditOutcome::Deny
            },
            reason: if i % 2 == 0 {
                None
            } else {
                Some("not granted".into())
            },
            action: None,
            resource_kind: None,
            resource_value: None,
            details: None,
        });
    }
    tokio::time::sleep(Duration::from_millis(250)).await;

    let resp = router
        .clone()
        .oneshot(req("/v1/audit?limit=10"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v.is_array(), "v0.7 callers MUST get array root: {v}");
    let rows = v.as_array().unwrap();
    assert!(!rows.is_empty(), "expected ≥1 row: {v}");

    // Every row carries the v0.6 / v0.7 field set; envelope-only fields
    // (`next_cursor`, `count`) MUST NOT leak into the array shape.
    for row in rows {
        let obj = row.as_object().expect("entry must be a JSON object");
        for k in ["timestamp", "actor", "method", "path", "outcome"] {
            assert!(
                obj.contains_key(k),
                "v0.7 entry missing required field `{k}`: {row}"
            );
        }
        assert!(
            !obj.contains_key("next_cursor") && !obj.contains_key("count"),
            "envelope-only fields leaked into v0.7 entry: {row}"
        );
        let outcome = obj["outcome"].as_str().unwrap();
        assert!(
            outcome == "allow" || outcome == "deny",
            "outcome must be `allow` or `deny`, got {outcome}"
        );
    }
}

#[tokio::test]
async fn v07_outcome_filter_narrows_array() {
    let (router, state, _d, _c) = build();
    state.audit.push(AuditEntry {
        timestamp: chrono::Utc::now(),
        actor: "u-allow".into(),
        role: Some(portunus_auth::OperatorRole::Superadmin),
        method: "GET".into(),
        path: "/v1/users".into(),
        outcome: AuditOutcome::Allow,
        reason: None,
        action: None,
        resource_kind: None,
        resource_value: None,
        details: None,
    });
    state.audit.push(AuditEntry {
        timestamp: chrono::Utc::now(),
        actor: "u-deny".into(),
        role: Some(portunus_auth::OperatorRole::User),
        method: "DELETE".into(),
        path: "/v1/users/foo".into(),
        outcome: AuditOutcome::Deny,
        reason: Some("forbidden".into()),
        action: None,
        resource_kind: None,
        resource_value: None,
        details: None,
    });
    tokio::time::sleep(Duration::from_millis(250)).await;

    let resp = router
        .oneshot(req("/v1/audit?limit=10&outcome=deny"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let rows = v.as_array().unwrap();
    for row in rows {
        assert_eq!(
            row["outcome"], "deny",
            "outcome=deny filter leaked an allow row: {row}"
        );
    }
}
