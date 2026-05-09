//! 008-sqlite-storage T041 — multi-target rule CRUD round-trip
//! preserves the v0.7 wire shape under SQLite-backed storage.
//!
//! Pushes a multi-target rule via the internal `state.rules` API
//! (the HTTP push path requires a fully-mocked client ack flow which
//! `rules_multi_target_contract.rs` already exercises end-to-end).
//! Then walks the read + delete halves of the contract through HTTP
//! and asserts the v0.7 array root + per-target shape.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use forward_auth::UserId;
use forward_core::{ClientName, PortRange, RuleTarget};
use forward_server::clients::ConnectedClients;
use forward_server::operator::http;
use forward_server::rules::Protocol;
use forward_server::state::AppState;
use forward_server::store::operator_store::SqliteOperatorStore;
use forward_server::store::token_store::SqliteTokenStore;
use forward_server::store::{Store, audit_writer};
use prometheus::{Gauge, IntCounter};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const SUPER: &str = "T041-super";

fn build() -> (axum::Router, Arc<AppState>, TempDir, CancellationToken) {
    let dir = TempDir::new().unwrap();
    let sqlite = Arc::new(Store::open(dir.path()).unwrap());
    let tokens = Arc::new(SqliteTokenStore::new(sqlite.clone()));
    let operator_store = Arc::new(SqliteOperatorStore::new(sqlite.clone()));
    operator_store.bootstrap_legacy_superadmin(SUPER).unwrap();

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
    let drops = IntCounter::new("crud_drops", "test").unwrap();
    let lag = Gauge::new("crud_lag", "test").unwrap();
    let h = audit_writer::spawn(sqlite, drops, lag, cancel.clone());
    state.audit.bind_durable_writer(h);
    (http::router(state.clone()), state, dir, cancel)
}

fn req(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {SUPER}"))
        .body(Body::empty())
        .unwrap()
}

async fn json_body(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 256 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn multi_target_rule_round_trip_preserves_v07_shape() {
    let (router, state, _d, _c) = build();
    let client = ClientName::new("client-multi".to_string()).unwrap();
    let owner = UserId::reserved("_superadmin");

    // Insert a multi-target rule directly via the in-memory rule
    // registry, mirroring what `cli::push_multi_target` does after
    // RBAC + validation. Then mark it Active so it surfaces in the
    // `/v1/rules` array (v0.7 lists Active and Failed rules).
    let rule = state
        .rules
        .push_range_with_targets(
            client.clone(),
            PortRange::single(30101),
            "10.0.0.1".into(),
            PortRange::single(9001),
            Protocol::Tcp,
            None,
            16,
            owner,
            vec![
                RuleTarget {
                    host: "10.0.0.1".into(),
                    port: 9001,
                    priority: 0,
                    proxy_protocol: Some(forward_core::ProxyProtocolVersion::V1),
                },
                RuleTarget {
                    host: "10.0.0.2".into(),
                    port: 9002,
                    priority: 1,
                    proxy_protocol: None,
                },
            ],
            None,
            None, // 009-tls-sni-routing: no SNI selector
        )
        .await
        .expect("push_range_with_targets");
    state.rules.mark_active(rule.id).await.expect("mark_active");
    let rule_id = rule.id.0;

    // 1) LIST via /v1/rules — array root, our rule present, targets[]
    //    visible with priority + per-target health key.
    let resp = router
        .clone()
        .oneshot(req(Method::GET, "/v1/rules"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let arr = json_body(resp).await;
    let arr = arr.as_array().expect("v0.7 array root");
    let mine = arr
        .iter()
        .find(|r| r["id"].as_u64() == Some(rule_id))
        .unwrap_or_else(|| {
            panic!(
                "pushed rule (rule_id={rule_id}) not in list; got {} rule(s): {arr:?}",
                arr.len()
            )
        });
    let targets = mine["targets"].as_array().expect("targets[] present");
    assert_eq!(targets.len(), 2, "two targets persisted");
    for t in targets {
        let obj = t.as_object().unwrap();
        for k in ["host", "port", "priority", "health"] {
            assert!(obj.contains_key(k), "target missing `{k}`: {t}");
        }
    }
    assert_eq!(targets[0]["proxy_protocol"], "v1");
    assert!(targets[1]["proxy_protocol"].is_null());
    assert_eq!(mine["client_name"], "client-multi");
    assert_eq!(mine["protocol"], "tcp");

    // 2) DELETE via /v1/rules/{id}
    let resp = router
        .clone()
        .oneshot(req(Method::DELETE, &format!("/v1/rules/{rule_id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 3) Confirm gone — re-list and assert it's no longer there.
    let resp = router
        .clone()
        .oneshot(req(Method::GET, "/v1/rules"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let arr_after = json_body(resp).await;
    let arr_after = arr_after.as_array().expect("array");
    assert!(
        arr_after.iter().all(|r| r["id"].as_u64() != Some(rule_id)),
        "rule should be gone after DELETE: {arr_after:?}"
    );
}
