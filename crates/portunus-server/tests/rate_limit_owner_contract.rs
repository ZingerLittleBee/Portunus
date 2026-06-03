//! 011-rate-limiting-qos T024 — REST contract tests for the per-owner
//! cap envelope (`/v1/clients/{client_id}/owners/{owner_id}/rate-limit`).
//!
//! Pins `specs/011-rate-limiting-qos/contracts/operator-api.md` §2:
//! - `GET` returns 200/404
//! - `PUT` validates body, capability-gates against pre-0.11 clients,
//!   pushes `OwnerRateLimitUpdate{SET}` to connected clients
//! - `DELETE` is idempotent (204 on first and replay), pushes
//!   `OwnerRateLimitUpdate{REMOVE}` to connected clients
//! - `GET /v1/clients/{id}/owners` lists every owner with rules or
//!   caps, with `has_rate_limit` true iff a cap row exists
//!
//! Test-first per Constitution Principle III.

#![allow(clippy::wildcard_imports)]

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use portunus_auth::{ClientScope, Grant, GrantId, OperatorRole, ProtocolSet, User, UserId};
use portunus_core::ClientName;
use portunus_proto::v1::{Protocol as ProtoProtocol, server_message::Payload};
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const SUPERADMIN_TOKEN: &str = "T024-super";
const CLIENT: &str = "edge-rl-owner";

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
    let alice_grant = Grant {
        id: GrantId::new(),
        user_id: alice_id,
        client: ClientScope::Named(ClientName::new(CLIENT.to_string()).expect("valid client")),
        listen_port_start: 30000,
        listen_port_end: 30100,
        protocols: ProtocolSet::non_empty(ProtocolSet::TCP).expect("non-empty"),
        note: None,
        created_at: Utc::now(),
    };
    operator_store.add_grant(alice_grant).expect("add grant");

    let connected = ConnectedClients::default();
    let state = Arc::new(
        AppState::new(
            tokens,
            operator_store,
            connected,
            None,
            0,
            "deadbeef",
            include_str!("../src/advertised/testdata/san_fixture.pem"),
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

/// Register a fake connected client that captures every server-message
/// the gRPC channel would have emitted, so PUT/DELETE can assert that
/// `OwnerRateLimitUpdate` was sent. Optional `client_version` exercises
/// the capability gate.
/// 015-client-stable-id: provision a client into the token store (the
/// authoritative roster) and return its server-assigned `client_id`
/// (ULID string). The operator surface now addresses clients by this
/// id, so tests resolve it once here and use it to build URLs.
fn provision_client(fixture: &Fixture, name: &str) -> String {
    let client_name = ClientName::new(name.to_string()).expect("valid client");
    fixture
        .state
        .tokens
        .issue_with_address(client_name.clone(), None)
        .expect("issue token");
    fixture
        .state
        .tokens
        .list()
        .expect("list clients")
        .into_iter()
        .find(|p| p.client_name == client_name)
        .expect("provisioned client present")
        .client_id
        .to_string()
}

async fn register_fake_client(
    fixture: &Fixture,
    name: &str,
    client_version: Option<&str>,
) -> (
    String,
    tokio::sync::Mutex<
        tokio::sync::mpsc::Receiver<Result<portunus_proto::v1::ServerMessage, tonic::Status>>,
    >,
) {
    let client_name = ClientName::new(name.to_string()).expect("valid client");
    let client_id_str = provision_client(fixture, name);
    let client_id = portunus_core::ClientId::from_str(&client_id_str).expect("valid ulid");
    let cancel = CancellationToken::new();
    let (outbound, rx) = tokio::sync::mpsc::channel(8);
    let waiters: Arc<
        tokio::sync::Mutex<HashMap<String, oneshot::Sender<portunus_proto::v1::RuleStatus>>>,
    > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let session_id = fixture
        .state
        .clients
        .register(
            client_id,
            client_name.clone(),
            None,
            cancel,
            outbound,
            waiters,
        )
        .await;
    let mut caps = HashSet::new();
    caps.insert(ProtoProtocol::Tcp);
    fixture
        .state
        .clients
        .set_supported_protocols(&client_id, session_id, caps)
        .await;
    if let Some(v) = client_version {
        fixture
            .state
            .clients
            .set_client_version(&client_id, session_id, v.to_string())
            .await;
    }
    (client_id_str, tokio::sync::Mutex::new(rx))
}

async fn drain_owner_rate_limit_update(
    rx: &tokio::sync::Mutex<
        tokio::sync::mpsc::Receiver<Result<portunus_proto::v1::ServerMessage, tonic::Status>>,
    >,
) -> Option<portunus_proto::v1::OwnerRateLimitUpdate> {
    let mut guard = rx.lock().await;
    let timeout = tokio::time::Duration::from_millis(200);
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(timeout, guard.recv()).await {
        if let Some(Payload::OwnerRateLimitUpdate(update)) = msg.payload {
            return Some(update);
        }
    }
    None
}

fn req_get(uri: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap()
}

fn req_put(uri: &str, bearer: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::from(serde_json::to_vec(&body).expect("body")))
        .unwrap()
}

fn req_delete(uri: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap()
}

async fn response_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 16384).await.expect("body bytes");
    serde_json::from_slice(&bytes).expect("JSON body")
}

async fn err_code(resp: axum::response::Response) -> String {
    let v = response_json(resp).await;
    v["error"]["code"]
        .as_str()
        .unwrap_or("<missing>")
        .to_string()
}

// ============================================================
//   GET — happy path & 404
// ============================================================

#[tokio::test]
async fn t024_get_returns_404_when_envelope_absent() {
    let f = build_fixture();
    let cid = provision_client(&f, CLIENT);
    let resp = f
        .router
        .clone()
        .oneshot(req_get(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert_eq!(err_code(resp).await, "owner_rate_limit_not_found");
}

#[tokio::test]
async fn t024_put_then_get_returns_envelope() {
    let f = build_fixture();
    let (cid, _rx) = register_fake_client(&f, CLIENT, Some("0.11.0")).await;
    let put_resp = f
        .router
        .clone()
        .oneshot(req_put(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
            serde_json::json!({
                "bandwidth_in_bps": 5_242_880u64,
                "concurrent_connections": 50,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(put_resp.status(), StatusCode::OK);
    let put_body = response_json(put_resp).await;
    assert_eq!(put_body["owner_id"], "alice");
    assert_eq!(put_body["client_name"], CLIENT);
    assert_eq!(put_body["rate_limit"]["bandwidth_in_bps"], 5_242_880);
    assert!(put_body["updated_at_unix_ms"].as_u64().unwrap() > 0);

    // Read back returns the same envelope.
    let get_resp = f
        .router
        .clone()
        .oneshot(req_get(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
        ))
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_body = response_json(get_resp).await;
    assert_eq!(get_body["rate_limit"], put_body["rate_limit"]);
}

// ============================================================
//   PUT — validation surface (mirror of T009 per-rule contract)
// ============================================================

#[tokio::test]
async fn t024_put_rejects_cap_zero() {
    let f = build_fixture();
    let (cid, _rx) = register_fake_client(&f, CLIENT, Some("0.11.0")).await;
    let resp = f
        .router
        .clone()
        .oneshot(req_put(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
            serde_json::json!({"bandwidth_in_bps": 0u64}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.rate_limit_cap_zero");
}

#[tokio::test]
async fn t024_put_rejects_burst_without_rate() {
    let f = build_fixture();
    let (cid, _rx) = register_fake_client(&f, CLIENT, Some("0.11.0")).await;
    let resp = f
        .router
        .clone()
        .oneshot(req_put(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
            serde_json::json!({"bandwidth_in_burst": 10u64}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        err_code(resp).await,
        "validation.rate_limit_burst_without_rate"
    );
}

#[tokio::test]
async fn t024_put_rejects_burst_out_of_range() {
    let f = build_fixture();
    let (cid, _rx) = register_fake_client(&f, CLIENT, Some("0.11.0")).await;
    // burst = rate * 100 → outside the [rate/100, rate*60] band.
    let resp = f
        .router
        .clone()
        .oneshot(req_put(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
            serde_json::json!({
                "bandwidth_in_bps": 1_000u64,
                "bandwidth_in_burst": 100_000u64,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.rate_limit_burst_range");
}

#[tokio::test]
async fn t024_put_rejects_concurrent_connections_burst() {
    let f = build_fixture();
    let (cid, _rx) = register_fake_client(&f, CLIENT, Some("0.11.0")).await;
    let resp = f
        .router
        .clone()
        .oneshot(req_put(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
            serde_json::json!({
                "concurrent_connections": 100,
                "concurrent_connections_burst": 10u32,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        err_code(resp).await,
        "validation.rate_limit_burst_unsupported"
    );
}

// ============================================================
//   PUT — capability gate
// ============================================================

#[tokio::test]
async fn t024_put_rejects_pre_011_client_with_422() {
    let f = build_fixture();
    let (cid, _rx) = register_fake_client(&f, CLIENT, Some("0.10.5")).await;
    let resp = f
        .router
        .clone()
        .oneshot(req_put(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
            serde_json::json!({"concurrent_connections": 5}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err_code(resp).await, "rate_limit_unsupported_by_client");
}

#[tokio::test]
async fn t070_concurrent_only_put_get_round_trip_v011_client() {
    let f = build_fixture();
    let (cid, rx) = register_fake_client(&f, CLIENT, Some("0.11.0")).await;
    let url = format!("/v1/clients/{cid}/owners/alice/rate-limit");

    // PUT — set ONLY concurrent_connections; all other rate-limit
    // fields must remain absent in the response (no defaulting).
    let body = serde_json::json!({ "concurrent_connections": 100 });
    let put_resp = f
        .router
        .clone()
        .oneshot(req_put(&url, SUPERADMIN_TOKEN, body.clone()))
        .await
        .unwrap();
    assert_eq!(put_resp.status(), StatusCode::OK, "PUT body={body}");
    let put_json = response_json(put_resp).await;
    assert_eq!(put_json["rate_limit"]["concurrent_connections"], 100);
    for field in [
        "bandwidth_in_bps",
        "bandwidth_out_bps",
        "new_connections_per_sec",
        "bandwidth_in_burst",
        "bandwidth_out_burst",
        "new_connections_burst",
    ] {
        assert!(
            put_json["rate_limit"][field].is_null(),
            "field {field} should be null after concurrent-only PUT, got {:?}",
            put_json["rate_limit"][field]
        );
    }

    // GET round-trip — same shape.
    let get_resp = f
        .router
        .clone()
        .oneshot(req_get(&url, SUPERADMIN_TOKEN))
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_json = response_json(get_resp).await;
    assert_eq!(get_json["rate_limit"]["concurrent_connections"], 100);
    assert!(get_json["rate_limit"]["bandwidth_in_bps"].is_null());

    // gRPC push capture — the server-to-client envelope must mirror the API.
    let push = drain_owner_rate_limit_update(&rx)
        .await
        .expect("push must arrive");
    assert_eq!(
        push.action,
        portunus_proto::v1::OwnerRateLimitAction::Set as i32
    );
    assert_eq!(push.owner_id, "alice");
    let rl = push.rate_limit.as_ref().expect("rate_limit present");
    assert_eq!(rl.concurrent_connections, Some(100));
    assert_eq!(rl.bandwidth_in_bps, None);
    assert_eq!(rl.bandwidth_out_bps, None);
    assert_eq!(rl.new_connections_per_sec, None);
}

#[tokio::test]
async fn t071_concurrent_put_returns_422_for_pre_011_client() {
    let f = build_fixture();
    let (cid, rx) = register_fake_client(&f, CLIENT, Some("0.10.0")).await;
    let url = format!("/v1/clients/{cid}/owners/alice/rate-limit");

    let body = serde_json::json!({ "concurrent_connections": 100 });
    let put_resp = f
        .router
        .clone()
        .oneshot(req_put(&url, SUPERADMIN_TOKEN, body))
        .await
        .unwrap();
    assert_eq!(
        put_resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "expected capability gate to reject"
    );
    assert_eq!(err_code(put_resp).await, "rate_limit_unsupported_by_client");

    // No push must have been emitted for this client.
    assert!(drain_owner_rate_limit_update(&rx).await.is_none());

    // No persisted row.
    let get_resp = f
        .router
        .clone()
        .oneshot(req_get(&url, SUPERADMIN_TOKEN))
        .await
        .unwrap();
    assert_eq!(get_resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn t024_put_rejects_disconnected_client_with_422() {
    let f = build_fixture();
    // Provisioned (so the id resolves) but NOT connected — the
    // capability gate falls back to "unknown" which is below 0.11.
    let cid = provision_client(&f, CLIENT);
    let resp = f
        .router
        .clone()
        .oneshot(req_put(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
            serde_json::json!({"concurrent_connections": 5}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err_code(resp).await, "rate_limit_unsupported_by_client");
}

// ============================================================
//   PUT/DELETE — server-message push to the connected client
// ============================================================

#[tokio::test]
async fn t024_put_pushes_owner_rate_limit_update_set() {
    let f = build_fixture();
    let (cid, rx) = register_fake_client(&f, CLIENT, Some("0.11.0")).await;
    let resp = f
        .router
        .clone()
        .oneshot(req_put(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
            serde_json::json!({"bandwidth_in_bps": 1_048_576u64}),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let push = drain_owner_rate_limit_update(&rx)
        .await
        .expect("push must arrive");
    assert_eq!(push.client_name, CLIENT);
    assert_eq!(push.owner_id, "alice");
    assert_eq!(
        push.action,
        portunus_proto::v1::OwnerRateLimitAction::Set as i32
    );
    let body = push.rate_limit.expect("SET carries body");
    assert_eq!(body.bandwidth_in_bps, Some(1_048_576));
}

#[tokio::test]
async fn t024_delete_pushes_owner_rate_limit_update_remove() {
    let f = build_fixture();
    let (cid, rx) = register_fake_client(&f, CLIENT, Some("0.11.0")).await;
    // Seed an envelope first.
    let _ = f
        .router
        .clone()
        .oneshot(req_put(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
            serde_json::json!({"bandwidth_in_bps": 1_048_576u64}),
        ))
        .await
        .unwrap();
    // Drain the SET push.
    let _ = drain_owner_rate_limit_update(&rx).await;

    let del_resp = f
        .router
        .clone()
        .oneshot(req_delete(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
        ))
        .await
        .unwrap();
    assert_eq!(del_resp.status(), StatusCode::NO_CONTENT);
    let push = drain_owner_rate_limit_update(&rx)
        .await
        .expect("REMOVE push must arrive");
    assert_eq!(
        push.action,
        portunus_proto::v1::OwnerRateLimitAction::Remove as i32
    );
    assert_eq!(push.owner_id, "alice");
    assert!(push.rate_limit.is_none());
}

// ============================================================
//   DELETE idempotence
// ============================================================

#[tokio::test]
async fn t024_delete_is_idempotent_on_replay() {
    let f = build_fixture();
    let (cid, _rx) = register_fake_client(&f, CLIENT, Some("0.11.0")).await;
    let url = format!("/v1/clients/{cid}/owners/alice/rate-limit");
    // First DELETE on absent envelope returns 204 (idempotent contract).
    let resp1 = f
        .router
        .clone()
        .oneshot(req_delete(&url, SUPERADMIN_TOKEN))
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::NO_CONTENT);
    // Second DELETE replay also returns 204.
    let resp2 = f
        .router
        .clone()
        .oneshot(req_delete(&url, SUPERADMIN_TOKEN))
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::NO_CONTENT);
}

// ============================================================
//   GET /v1/clients/{id}/owners — listing
// ============================================================

#[tokio::test]
async fn t024_list_owners_returns_empty_when_no_rules_or_caps() {
    let f = build_fixture();
    let cid = provision_client(&f, CLIENT);
    let resp = f
        .router
        .clone()
        .oneshot(req_get(
            &format!("/v1/clients/{cid}/owners"),
            SUPERADMIN_TOKEN,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert!(body.is_array());
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn t024_list_owners_marks_has_rate_limit_when_envelope_present() {
    let f = build_fixture();
    let (cid, _rx) = register_fake_client(&f, CLIENT, Some("0.11.0")).await;
    // Seed alice's cap; no rules under alice.
    let _ = f
        .router
        .clone()
        .oneshot(req_put(
            &format!("/v1/clients/{cid}/owners/alice/rate-limit"),
            SUPERADMIN_TOKEN,
            serde_json::json!({"concurrent_connections": 10}),
        ))
        .await
        .unwrap();
    let resp = f
        .router
        .clone()
        .oneshot(req_get(
            &format!("/v1/clients/{cid}/owners"),
            SUPERADMIN_TOKEN,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["owner_id"], "alice");
    assert_eq!(arr[0]["has_rate_limit"], true);
    assert_eq!(arr[0]["rule_count"], 0);
}

// ============================================================
//   PUT — replace existing envelope
// ============================================================

#[tokio::test]
async fn t024_put_replaces_existing_envelope() {
    let f = build_fixture();
    let (cid, _rx) = register_fake_client(&f, CLIENT, Some("0.11.0")).await;
    let url = format!("/v1/clients/{cid}/owners/alice/rate-limit");
    // First PUT.
    let _ = f
        .router
        .clone()
        .oneshot(req_put(
            &url,
            SUPERADMIN_TOKEN,
            serde_json::json!({"bandwidth_in_bps": 1_048_576u64}),
        ))
        .await
        .unwrap();
    // Second PUT replaces the body.
    let resp = f
        .router
        .clone()
        .oneshot(req_put(
            &url,
            SUPERADMIN_TOKEN,
            serde_json::json!({
                "bandwidth_in_bps": 2_097_152u64,
                "concurrent_connections": 10,
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = response_json(resp).await;
    assert_eq!(body["rate_limit"]["bandwidth_in_bps"], 2_097_152);
    assert_eq!(body["rate_limit"]["concurrent_connections"], 10);
}
