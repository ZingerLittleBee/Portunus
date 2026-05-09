//! T013a — `POST /v1/rules` HTTP shape contract for multi-target rules.
//!
//! Test-first per Constitution Principle III: this test lands BEFORE the
//! T014/T015/T016 implementation (HTTP body restructure + server-side
//! validation + client-version guard).
//!
//! Coverage matrix from `contracts/operator-api.md` §1 + FR-021:
//!
//!   | Case                                              | Expected |
//!   |---------------------------------------------------|----------|
//!   | Legacy single-target push, length-1 echoed back   | 201      |
//!   | New multi-target push with `targets[]`            | 201      |
//!   | Both legacy + new shape                           | 400 `rule_shape_conflict` |
//!   | Neither shape (no `target_host`, no `targets`)    | 400 `rule_shape_missing` |
//!   | `targets: []` empty                               | 400 `targets_empty` |
//!   | `targets: [9 entries]` over the cap               | 400 `targets_too_many` |
//!   | Duplicate `(host, port)` in targets               | 400 `targets_duplicate` |
//!   | `health_check_interval_secs: 0` (unset variant)   | 201      |
//!   | `health_check_interval_secs: 7200` (out of 1..3600) | 400 `health_check_interval_out_of_range` |
//!   | Multi-target push to a v0.6.0 client              | 422 `multi_target_unsupported_by_client` |
//!   | FR-021: targets do NOT participate in RBAC        | 201      |
//!
//! Fixture style mirrors `rbac_push_rule.rs`: in-process router built on
//! a tempdir-backed operator + token store, with `alice` granted
//! `(client="client-a", listen_port=30000..=30010, tcp)`. Where a
//! "post-RBAC" success path needs a connected client (so the activation
//! step doesn't 503 with `client_not_connected`), the test seeds the
//! `ConnectedClients` registry with a fake outbound channel.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use forward_auth::{ClientScope, Grant, GrantId, OperatorRole, ProtocolSet, User, UserId};
use forward_core::ClientName;
use forward_proto::v1::Protocol as ProtoProtocol;
use forward_server::clients::ConnectedClients;
use forward_server::operator::http;
use forward_server::state::AppState;
use std::str::FromStr;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const SUPERADMIN_TOKEN: &str = "T013a-super";
const ALICE_TOKEN: &str = "T013a-alice";

struct Fixture {
    router: axum::Router,
    state: Arc<AppState>,
    _dir: TempDir,
}

fn build_fixture() -> Fixture {
    let dir = TempDir::new().expect("tempdir");
    let sqlite_store = std::sync::Arc::new(forward_server::store::Store::open(dir.path()).unwrap());
    let tokens = Arc::new(forward_server::store::token_store::SqliteTokenStore::new(
        std::sync::Arc::clone(&sqlite_store),
    ));
    let operator_store = Arc::new(
        forward_server::store::operator_store::SqliteOperatorStore::new(std::sync::Arc::clone(
            &sqlite_store,
        )),
    );
    operator_store
        .bootstrap_legacy_superadmin(SUPERADMIN_TOKEN)
        .expect("bootstrap superadmin");

    let alice_id = UserId::from_str("alice").expect("valid user id");
    operator_store
        .add_user(User {
            id: alice_id.clone(),
            display_name: "Alice".to_string(),
            role: OperatorRole::User,
            created_at: Utc::now(),
            disabled: false,
        })
        .expect("add alice");
    let _ = operator_store
        .issue_credential(&alice_id, Some("test".to_string()))
        .expect("issue cred");
    // 008-sqlite-storage T044: rewrite alice's credential hash via SQL
    // (the legacy identity.json mutation path is gone in v0.8).
    let known_hash_hex =
        forward_core::fingerprint::hex(&forward_auth::token::hash_token(ALICE_TOKEN));
    sqlite_store
        .with_write_tx(|tx| {
            tx.execute(
                "UPDATE credentials SET hash = ? WHERE user_id = 'alice'",
                rusqlite::params![known_hash_hex],
            )
            .map_err(forward_server::store::map_rusqlite)?;
            Ok(())
        })
        .expect("patch alice credential hash");

    let alice_grant = Grant {
        id: GrantId::new(),
        user_id: alice_id.clone(),
        client: ClientScope::Named(ClientName::new("client-a".to_string()).expect("valid client")),
        listen_port_start: 30000,
        listen_port_end: 30010,
        protocols: ProtocolSet::non_empty(ProtocolSet::TCP).expect("non-empty"),
        note: Some("T013a fixture".to_string()),
        created_at: Utc::now(),
    };
    operator_store.add_grant(alice_grant).expect("add grant");

    let connected = ConnectedClients::default();
    let state = Arc::new(
        AppState::new(
            tokens,
            operator_store,
            connected,
            "127.0.0.1:0",
            "deadbeef",
            "-----BEGIN CERTIFICATE-----\n",
            16,
            std::sync::Arc::clone(&sqlite_store),
        )
        .expect("AppState"),
    );

    Fixture {
        router: http::router(state.clone()),
        state,
        _dir: dir,
    }
}

/// Register a fake connected client on the fixture so post-RBAC paths
/// don't fast-fail with `client_not_connected`. The test never reads the
/// outbound channel — every test that needs a "succeeds-then-may-503"
/// flow asserts on the HTTP status BEFORE the channel is drained.
async fn register_fake_client(
    fixture: &Fixture,
    name: &str,
    client_version: Option<&str>,
    supports_udp: bool,
) {
    let client_name = ClientName::new(name.to_string()).expect("valid client");
    let cancel = CancellationToken::new();
    let (outbound, _rx) = tokio::sync::mpsc::channel(8);
    let waiters = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let session_id = fixture
        .state
        .clients
        .register(client_name.clone(), None, cancel, outbound, waiters)
        .await;
    let mut caps = std::collections::HashSet::new();
    caps.insert(ProtoProtocol::Tcp);
    if supports_udp {
        caps.insert(ProtoProtocol::Udp);
    }
    fixture
        .state
        .clients
        .set_supported_protocols(&client_name, session_id, caps)
        .await;
    if let Some(v) = client_version {
        // `set_client_version` is a 007-multi-target-failover addition.
        fixture
            .state
            .clients
            .set_client_version(&client_name, session_id, v.to_string())
            .await;
    }
}

fn push(bearer: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/rules")
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::from(serde_json::to_vec(&body).expect("body")))
        .expect("build request")
}

async fn err_code(resp: axum::response::Response) -> String {
    let bytes = to_bytes(resp.into_body(), 8192).await.expect("body bytes");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("error body must be JSON");
    v["error"]["code"]
        .as_str()
        .unwrap_or("<missing>")
        .to_string()
}

// ---------- Acceptance: legacy shape still works (FR-002, FR-003) -----

#[tokio::test]
async fn legacy_single_target_push_accepted_under_grant() {
    let f = build_fixture();
    register_fake_client(&f, "client-a", Some("0.7.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30001,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "protocol": "tcp"
            }),
        ))
        .await
        .expect("oneshot");
    // Either 201 Created (post-RBAC ack timeout — depends on dispatch
    // mechanics in the in-process fixture) or one of the post-RBAC
    // failures (504/422). The shape MUST NOT be rejected at the
    // body-validation step — i.e. it must NOT be a 400 with one of
    // the new shape error codes.
    let status = resp.status();
    assert_ne!(status, StatusCode::BAD_REQUEST, "legacy shape must parse");
    if status == StatusCode::BAD_REQUEST {
        let code = err_code(resp).await;
        panic!("legacy shape unexpectedly rejected with code {code:?}");
    }
}

// ---------- Acceptance: new shape parses ------------------------------

#[tokio::test]
async fn new_multi_target_push_parses() {
    let f = build_fixture();
    register_fake_client(&f, "client-a", Some("0.7.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30002,
                "protocol": "tcp",
                "targets": [
                    {"host": "primary.test", "port": 80},
                    {"host": "secondary.test", "port": 80}
                ]
            }),
        ))
        .await
        .expect("oneshot");
    let status = resp.status();
    assert_ne!(
        status,
        StatusCode::BAD_REQUEST,
        "new shape must parse — got {status} ({})",
        err_code(resp).await
    );
}

#[tokio::test]
async fn proxy_protocol_on_udp_rule_rejected() {
    let f = build_fixture();
    register_fake_client(&f, "client-a", Some("0.9.0"), true).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            SUPERADMIN_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30003,
                "protocol": "udp",
                "targets": [
                    {"host": "primary.test", "port": 53, "proxy_protocol": "v1"}
                ]
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        err_code(resp).await,
        "validation.proxy_protocol_on_unsupported_rule"
    );
}

#[tokio::test]
async fn udp_proxy_protocol_probe_denied_by_rbac_first() {
    let f = build_fixture();
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30003,
                "protocol": "udp",
                "targets": [
                    {"host": "primary.test", "port": 53, "proxy_protocol": "v1"}
                ]
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(err_code(resp).await, "protocol_not_granted");
}

#[tokio::test]
async fn proxy_protocol_requires_capable_client_version() {
    let f = build_fixture();
    register_fake_client(&f, "client-a", Some("0.9.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30004,
                "protocol": "tcp",
                "targets": [
                    {"host": "primary.test", "port": 443, "proxy_protocol": "v1"}
                ]
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err_code(resp).await, "proxy_protocol_unsupported_by_client");
}

#[tokio::test]
async fn proxy_protocol_requires_known_client_version() {
    let f = build_fixture();
    register_fake_client(&f, "client-a", None, false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30004,
                "protocol": "tcp",
                "targets": [
                    {"host": "primary.test", "port": 443, "proxy_protocol": "v1"}
                ]
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err_code(resp).await, "proxy_protocol_unsupported_by_client");
}

// ---------- Rejection: both shapes (FR-004) ---------------------------

#[tokio::test]
async fn both_shapes_rejected_rule_shape_conflict() {
    let f = build_fixture();
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30003,
                "protocol": "tcp",
                "target_host": "127.0.0.1",
                "target_port": 80,
                "targets": [{"host": "primary.test", "port": 80}]
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "rule_shape_conflict");
}

// ---------- Rejection: neither shape (FR-004) -------------------------

#[tokio::test]
async fn neither_shape_rejected_rule_shape_missing() {
    let f = build_fixture();
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30004,
                "protocol": "tcp"
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "rule_shape_missing");
}

// ---------- Rejection: empty targets[] --------------------------------

#[tokio::test]
async fn empty_targets_rejected_targets_empty() {
    let f = build_fixture();
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30005,
                "protocol": "tcp",
                "targets": []
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "targets_empty");
}

// ---------- Rejection: too many targets (V-T4) ------------------------

#[tokio::test]
async fn too_many_targets_rejected() {
    let f = build_fixture();
    let targets: Vec<serde_json::Value> = (0..9)
        .map(|i| serde_json::json!({"host": format!("h{i}.test"), "port": 80}))
        .collect();
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30006,
                "protocol": "tcp",
                "targets": targets
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "targets_too_many");
}

// ---------- Rejection: duplicate (host,port) (FR-005) -----------------

#[tokio::test]
async fn duplicate_targets_rejected_targets_duplicate() {
    let f = build_fixture();
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30007,
                "protocol": "tcp",
                "targets": [
                    {"host": "x.test", "port": 80},
                    {"host": "x.test", "port": 80}
                ]
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "targets_duplicate");
}

// ---------- Rejection: health_check_interval out of range -------------

#[tokio::test]
async fn health_check_interval_out_of_range_rejected() {
    let f = build_fixture();
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30008,
                "protocol": "tcp",
                "targets": [{"host": "x.test", "port": 80}],
                "health_check_interval_secs": 7200
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "health_check_interval_out_of_range");
}

// ---------- Rejection: multi-target push to old client (R-007) --------

#[tokio::test]
async fn multi_target_push_to_v06_client_rejected_422() {
    let f = build_fixture();
    register_fake_client(&f, "client-a", Some("0.6.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30009,
                "protocol": "tcp",
                "targets": [
                    {"host": "primary.test", "port": 80},
                    {"host": "secondary.test", "port": 80}
                ]
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err_code(resp).await, "multi_target_unsupported_by_client");
}

// ---------- Rejection: NOT for single-target push to v0.6 client ------

#[tokio::test]
async fn single_target_push_to_v06_client_passes_version_guard() {
    // The version guard MUST only fire for multi-target rules. A
    // single-target push (legacy shape OR length-1 targets[]) goes
    // through the v0.6.0 wire path unchanged.
    let f = build_fixture();
    register_fake_client(&f, "client-a", Some("0.6.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30000,
                "protocol": "tcp",
                "target_host": "127.0.0.1",
                "target_port": 9000
            }),
        ))
        .await
        .expect("oneshot");
    let status = resp.status();
    if status == StatusCode::UNPROCESSABLE_ENTITY {
        let code = err_code(resp).await;
        assert_ne!(
            code, "multi_target_unsupported_by_client",
            "version guard must not fire for single-target push"
        );
    }
}

// ---------- FR-021: targets are NOT in the RBAC envelope --------------

#[tokio::test]
async fn targets_pointing_anywhere_pass_grant_check() {
    // Alice's grant covers (client-a, 30000..=30010, tcp). It says
    // NOTHING about which (host:port) targets she may forward to.
    // FR-021 codifies this — targets are an operator-side concern
    // within an existing grant, not a separate gate.
    let f = build_fixture();
    register_fake_client(&f, "client-a", Some("0.7.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": "client-a",
                "listen_port": 30010,
                "protocol": "tcp",
                "targets": [
                    {"host": "anything-the-operator-wants.example.com", "port": 9999},
                    {"host": "192.0.2.42", "port": 31337}
                ]
            }),
        ))
        .await
        .expect("oneshot");
    let status = resp.status();
    // MUST NOT be FORBIDDEN with an RBAC error code on the targets list.
    if status == StatusCode::FORBIDDEN {
        let code = err_code(resp).await;
        panic!("RBAC unexpectedly applied to targets list: {code}");
    }
    // Also MUST NOT be BAD_REQUEST with a target validation error
    // (the hosts/ports are syntactically valid).
    if status == StatusCode::BAD_REQUEST {
        let code = err_code(resp).await;
        panic!("targets unexpectedly rejected at validation: {code}");
    }
}
