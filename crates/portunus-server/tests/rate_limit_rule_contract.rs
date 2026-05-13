//! 011-rate-limiting-qos T009 — `POST /v1/rules` contract tests for the
//! per-rule cap envelope. Pin the operator-API surface documented in
//! `specs/011-rate-limiting-qos/contracts/operator-api.md` §1 and
//! `wire.md` §4 against the implementation in
//! `portunus-server::operator::http::post_rules`.
//!
//! Coverage:
//! - Happy path — rate_limit body persists & echoes in response.
//! - 4 validation error subcategories (cap_zero, burst_without_rate,
//!   burst_range, burst_unsupported).
//! - 422 capability gate against pre-0.11 client_version.
//! - Legacy `target_host` shape rejects rate_limit.
//! - byte-stable: omitted body keeps response unchanged.
//!
//! Test-first per Constitution Principle III: lands BEFORE the
//! data-plane wiring (T017..T023). Exercises the HTTP/router surface
//! only; data-plane enforcement is in `portunus-client`.

#![allow(clippy::wildcard_imports)]

use std::str::FromStr;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use portunus_auth::{ClientScope, Grant, GrantId, OperatorRole, ProtocolSet, User, UserId};
use portunus_core::ClientName;
use portunus_proto::v1::Protocol as ProtoProtocol;
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const SUPERADMIN_TOKEN: &str = "T009-super";
const ALICE_TOKEN: &str = "T009-alice";
const CLIENT: &str = "edge-rl";

struct Fixture {
    router: axum::Router,
    state: Arc<AppState>,
    _dir: TempDir,
}

fn build_fixture() -> Fixture {
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
    let known_hash_hex =
        portunus_core::fingerprint::hex(&portunus_auth::token::hash_token(ALICE_TOKEN));
    sqlite_store
        .with_write_tx(|tx| {
            tx.execute(
                "UPDATE credentials SET hash = ? WHERE user_id = 'alice'",
                rusqlite::params![known_hash_hex],
            )
            .map_err(portunus_server::store::map_rusqlite)?;
            Ok(())
        })
        .expect("patch alice credential hash");

    let alice_grant = Grant {
        id: GrantId::new(),
        user_id: alice_id.clone(),
        client: ClientScope::Named(ClientName::new(CLIENT.to_string()).expect("valid client")),
        listen_port_start: 30000,
        listen_port_end: 30100,
        protocols: ProtocolSet::non_empty(ProtocolSet::TCP | ProtocolSet::UDP).expect("non-empty"),
        note: Some("011-rate-limiting fixture".to_string()),
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

/// Spawns a background task that drains every `RuleUpdate` the server
/// emits and resolves the matching waiter with `Activated`. This lets
/// the HTTP `POST /v1/rules` handler complete end-to-end without a real
/// gRPC stream — a contract-test approximation of a healthy v0.11
/// client.
async fn register_fake_client(
    fixture: &Fixture,
    name: &str,
    client_version: Option<&str>,
) -> Arc<tokio::sync::Mutex<Vec<portunus_proto::v1::RuleUpdate>>> {
    use portunus_proto::v1::server_message::Payload;
    let client_name = ClientName::new(name.to_string()).expect("valid client");
    let cancel = CancellationToken::new();
    let (outbound, mut rx) = tokio::sync::mpsc::channel(8);
    let seen_updates = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let waiters: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<
                String,
                tokio::sync::oneshot::Sender<portunus_proto::v1::RuleStatus>,
            >,
        >,
    > = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let session_id = fixture
        .state
        .clients
        .register(client_name.clone(), None, cancel, outbound, waiters.clone())
        .await;
    let mut caps = std::collections::HashSet::new();
    caps.insert(ProtoProtocol::Tcp);
    fixture
        .state
        .clients
        .set_supported_protocols(&client_name, session_id, caps)
        .await;
    if let Some(v) = client_version {
        fixture
            .state
            .clients
            .set_client_version(&client_name, session_id, v.to_string())
            .await;
    }

    // Auto-ack: drain RuleUpdate messages and resolve waiters with
    // `Activated`. Mirrors the success path that grpc/service.rs runs
    // when a real client sends back `RuleStatus`.
    let seen_updates_bg = Arc::clone(&seen_updates);
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let Ok(server_msg) = msg else { continue };
            let Some(Payload::RuleUpdate(update)) = server_msg.payload else {
                continue;
            };
            seen_updates_bg.lock().await.push(update.clone());
            let request_id = update.request_id.clone();
            let rule_id = update.rule.as_ref().map(|r| r.rule_id).unwrap_or_default();
            let mut guard = waiters.lock().await;
            if let Some(tx) = guard.remove(&request_id) {
                let _ = tx.send(portunus_proto::v1::RuleStatus {
                    request_id,
                    rule_id,
                    outcome: portunus_proto::v1::ActivationOutcome::Activated as i32,
                    reason: String::new(),
                });
            }
        }
    });
    seen_updates
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

fn put_rule(rule_id: u64, bearer: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(format!("/v1/rules/{rule_id}"))
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::from(serde_json::to_vec(&body).expect("body")))
        .expect("build request")
}

async fn response_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 16384).await.expect("body bytes");
    serde_json::from_slice(&bytes).expect("body must be JSON")
}

async fn err_code(resp: axum::response::Response) -> String {
    let v = response_json(resp).await;
    v["error"]["code"]
        .as_str()
        .unwrap_or("<missing>")
        .to_string()
}

// ============================================================
//   Happy path — rate_limit body persists & echoes
// ============================================================

#[tokio::test]
async fn rule_with_rate_limit_persists_and_echoes_in_response() {
    let f = build_fixture();
    let _updates = register_fake_client(&f, CLIENT, Some("0.11.0")).await;

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30001,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9001}],
                "rate_limit": {
                    "bandwidth_in_bps": 1_048_576u64,
                    "concurrent_connections": 100,
                    "new_connections_per_sec": 50,
                },
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = response_json(resp).await;
    let rl = &body["rate_limit"];
    assert_eq!(rl["bandwidth_in_bps"], 1_048_576);
    assert_eq!(rl["concurrent_connections"], 100);
    assert_eq!(rl["new_connections_per_sec"], 50);
    assert!(
        rl["bandwidth_out_bps"].is_null()
            || !rl.as_object().unwrap().contains_key("bandwidth_out_bps")
    );
}

#[tokio::test]
async fn rule_without_rate_limit_omits_field_in_response() {
    // Byte-stable: a rule pushed without rate_limit must omit the
    // field from the response so pre-0.11 callers see an unchanged body.
    let f = build_fixture();
    let _updates = register_fake_client(&f, CLIENT, Some("0.11.0")).await;

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30002,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9001}],
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = response_json(resp).await;
    assert!(!body.as_object().unwrap().contains_key("rate_limit"));
}

// ============================================================
//   Validation errors — 4 stable subcategory codes
// ============================================================

#[tokio::test]
async fn rate_limit_cap_zero_returns_400() {
    let f = build_fixture();
    let _updates = register_fake_client(&f, CLIENT, Some("0.11.0")).await;

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30010,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9001}],
                "rate_limit": {"bandwidth_in_bps": 0},
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.rate_limit_cap_zero");
}

#[tokio::test]
async fn rate_limit_burst_without_rate_returns_400() {
    let f = build_fixture();
    let _updates = register_fake_client(&f, CLIENT, Some("0.11.0")).await;

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30011,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9001}],
                "rate_limit": {"bandwidth_in_burst": 1024},
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        err_code(resp).await,
        "validation.rate_limit_burst_without_rate"
    );
}

#[tokio::test]
async fn rate_limit_burst_below_floor_returns_400() {
    let f = build_fixture();
    let _updates = register_fake_client(&f, CLIENT, Some("0.11.0")).await;

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30012,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9001}],
                "rate_limit": {
                    "bandwidth_in_bps": 1_000_000,
                    "bandwidth_in_burst": 100, // below 10_000 floor
                },
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.rate_limit_burst_range");
}

#[tokio::test]
async fn rate_limit_concurrent_burst_reserved_returns_400() {
    let f = build_fixture();
    let _updates = register_fake_client(&f, CLIENT, Some("0.11.0")).await;

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30013,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9001}],
                "rate_limit": {
                    "concurrent_connections": 100,
                    "concurrent_connections_burst": 50, // reserved — must reject
                },
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        err_code(resp).await,
        "validation.rate_limit_burst_unsupported"
    );
}

// ============================================================
//   Capability gate — 422 against pre-0.11 client
// ============================================================

#[tokio::test]
async fn rate_limit_unsupported_by_v010_client_returns_422() {
    let f = build_fixture();
    let _updates = register_fake_client(&f, CLIENT, Some("0.10.0")).await;

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30020,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9001}],
                "rate_limit": {"bandwidth_in_bps": 1_048_576u64},
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err_code(resp).await, "rate_limit_unsupported_by_client");
}

#[tokio::test]
async fn rate_limit_unsupported_by_unknown_version_returns_422() {
    // No Hello received yet — gate conservatively.
    let f = build_fixture();
    let _updates = register_fake_client(&f, CLIENT, None).await;

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30021,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9001}],
                "rate_limit": {"concurrent_connections": 50},
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err_code(resp).await, "rate_limit_unsupported_by_client");
}

#[tokio::test]
async fn rule_without_rate_limit_passes_through_for_pre_011_client() {
    // The capability gate must not fire when the rule carries no
    // rate_limit field — pre-0.11 clients keep working unchanged.
    let f = build_fixture();
    let _updates = register_fake_client(&f, CLIENT, Some("0.10.0")).await;

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30022,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9001}],
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);
}

// ============================================================
//   Legacy shape rejects rate_limit
// ============================================================

#[tokio::test]
async fn rate_limit_on_legacy_target_host_shape_returns_400() {
    let f = build_fixture();
    let _updates = register_fake_client(&f, CLIENT, Some("0.11.0")).await;

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30030,
                "protocol": "tcp",
                "target_host": "127.0.0.1",
                "target_port": 9001,
                "rate_limit": {"bandwidth_in_bps": 1_048_576u64},
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        err_code(resp).await,
        "validation.rate_limit_on_legacy_shape"
    );
}

#[tokio::test]
async fn uncapped_rule_emits_owner_id_when_owner_cap_exists() {
    let f = build_fixture();
    let updates = register_fake_client(&f, CLIENT, Some("0.11.0")).await;

    let put_owner = Request::builder()
        .method("PUT")
        .uri(format!("/v1/clients/{CLIENT}/owners/alice/rate-limit"))
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {SUPERADMIN_TOKEN}"))
        .body(Body::from(
            serde_json::to_vec(&serde_json::json!({
                "bandwidth_in_bps": 1_048_576u64
            }))
            .expect("body"),
        ))
        .expect("build request");
    let put_resp = f
        .router
        .clone()
        .oneshot(put_owner)
        .await
        .expect("owner cap");
    assert_eq!(put_resp.status(), StatusCode::OK);

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30033,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9033}],
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let seen = updates.lock().await;
    let pushed = seen
        .iter()
        .find(|u| u.rule.as_ref().is_some_and(|r| r.listen_port == 30033))
        .expect("rule update captured");
    let owner_id = pushed
        .rule
        .as_ref()
        .and_then(|r| r.owner_id.clone())
        .expect("owner_id must be present when owner cap exists");
    assert_eq!(owner_id, "alice");
}

// Regression: the legacy single-target wire shape (target_host /
// target_port, no targets[]) previously hard-coded owner_id: None on
// the wire even when the rule's owner had an active cap. The client
// then never installed an OwnerRateLimitHandle for that rule and the
// cap was silently un-enforced. See operator/cli.rs::push_rule.
#[tokio::test]
async fn legacy_target_host_shape_emits_owner_id_on_push() {
    let f = build_fixture();
    let updates = register_fake_client(&f, CLIENT, Some("0.11.0")).await;

    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30034,
                "protocol": "tcp",
                "target_host": "127.0.0.1",
                "target_port": 9034,
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let seen = updates.lock().await;
    let pushed = seen
        .iter()
        .find(|u| u.rule.as_ref().is_some_and(|r| r.listen_port == 30034))
        .expect("rule update captured");
    let owner_id = pushed
        .rule
        .as_ref()
        .and_then(|r| r.owner_id.clone())
        .expect("legacy single-target push must emit owner_id");
    assert_eq!(owner_id, "alice");
}

#[tokio::test]
async fn update_rule_rate_limit_persists_and_echoes_in_response() {
    let f = build_fixture();
    let _updates = register_fake_client(&f, CLIENT, Some("0.11.0")).await;

    let create = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30040,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9040}],
                "rate_limit": {
                    "bandwidth_in_bps": 1_048_576u64,
                    "bandwidth_out_bps": 1_048_576u64
                }
            }),
        ))
        .await
        .expect("create");
    assert_eq!(create.status(), StatusCode::CREATED);
    let created = response_json(create).await;
    let rule_id = created["rule_id"].as_u64().expect("rule_id");

    let update = f
        .router
        .clone()
        .oneshot(put_rule(
            rule_id,
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30040,
                "protocol": "tcp",
                "targets": [{"host": "127.0.0.1", "port": 9040}],
                "rate_limit": {
                    "bandwidth_in_bps": 102_400_u64,
                    "bandwidth_out_bps": 102_400_u64
                }
            }),
        ))
        .await
        .expect("update");
    assert_eq!(update.status(), StatusCode::OK);
    let body = response_json(update).await;
    assert_eq!(body["rule_id"], rule_id);
    assert_eq!(body["rate_limit"]["bandwidth_in_bps"], 102_400);
    assert_eq!(body["rate_limit"]["bandwidth_out_bps"], 102_400);
}
