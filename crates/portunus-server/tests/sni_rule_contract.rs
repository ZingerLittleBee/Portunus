//! 009-tls-sni-routing — `POST /v1/rules` contract tests for the SNI
//! selector. Consolidates tasks **T011** (grammar + applicability),
//! **T012** (capability gate), **T030** (overlap matrix), and **T031**
//! (legacy↔SNI mode lock) into one fixture-shared file.
//!
//! Test-first per Constitution Principle III: this test lands BEFORE
//! the data-plane implementation in Phase 3 (US1 MVP). It exercises the
//! HTTP/router surface only; the actual TLS-byte forwarding is locked
//! down by `sni_route` integration tests in `portunus-client`.
//!
//! References:
//! - `specs/009-tls-sni-routing/contracts/operator-api.md` — error codes.
//! - `specs/009-tls-sni-routing/data-model.md` § Overlap matrix.
//! - `specs/009-tls-sni-routing/research.md` R-001..R-015.

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

const SUPERADMIN_TOKEN: &str = "T011-super";
const ALICE_TOKEN: &str = "T011-alice";
const CLIENT: &str = "client-sni";

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
        note: Some("009-tls-sni fixture".to_string()),
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

// ============================================================
//   T011 — `sni_pattern` grammar + applicability validation
// ============================================================

#[tokio::test]
async fn sni_on_udp_rule_rejected() {
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.9.0"), true).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30001,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "protocol": "udp",
                "sni_pattern": "api.example.com",
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.sni_on_unsupported_rule");
}

#[tokio::test]
async fn sni_on_range_rule_rejected() {
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.9.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30002,
                "listen_port_end": 30005,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "target_port_end": 9003,
                "protocol": "tcp",
                "sni_pattern": "api.example.com",
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.sni_on_unsupported_rule");
}

#[tokio::test]
async fn sni_pattern_empty_rejected() {
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.9.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30010,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "protocol": "tcp",
                "sni_pattern": "",
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.sni_pattern_malformed");
}

#[tokio::test]
async fn sni_pattern_with_inner_wildcard_rejected() {
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.9.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30011,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "protocol": "tcp",
                "sni_pattern": "api.*.example.com",
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.sni_pattern_malformed");
}

#[tokio::test]
async fn sni_pattern_top_level_wildcard_rejected() {
    // FR-011: `*.com` is too broad — wildcard requires multi-label parent.
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.9.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30012,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "protocol": "tcp",
                "sni_pattern": "*.com",
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.sni_pattern_malformed");
}

#[tokio::test]
async fn sni_pattern_illegal_char_rejected() {
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.9.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30013,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "protocol": "tcp",
                "sni_pattern": "api_example.com",
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.sni_pattern_malformed");
}

// 009-tls-sni-routing T050: extended wildcard grammar — `*` must be
// the entire leftmost label. Patterns where `*` is glued to other
// chars (`foo*.example.com`) or appears with no following dot
// (`*example.com`) MUST be refused at validation time, never reach
// the routing table.

#[tokio::test]
async fn sni_pattern_partial_label_wildcard_rejected() {
    // `foo*.example.com`: `*` is not the full leftmost label.
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.9.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30014,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "protocol": "tcp",
                "sni_pattern": "foo*.example.com",
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.sni_pattern_malformed");
}

#[tokio::test]
async fn sni_pattern_wildcard_without_dot_rejected() {
    // `*example.com`: no dot after `*`, so `*.` strip never happens
    // and the body still contains `*` — caught by the illegal-char
    // check.
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.9.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30015,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "protocol": "tcp",
                "sni_pattern": "*example.com",
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(err_code(resp).await, "validation.sni_pattern_malformed");
}

// ============================================================
//   T012 — `sni_pattern` capability gate (HTTP 422)
// ============================================================

#[tokio::test]
async fn sni_unsupported_by_v07_client_returns_422() {
    // FR-018: client connected at v0.7.0 cannot decode sni_pattern.
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.7.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30020,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "protocol": "tcp",
                "sni_pattern": "api.example.com",
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err_code(resp).await, "sni_unsupported_by_client");
}

#[tokio::test]
async fn sni_unsupported_by_v08_client_returns_422() {
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.8.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30021,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "protocol": "tcp",
                "sni_pattern": "api.example.com",
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err_code(resp).await, "sni_unsupported_by_client");
}

#[tokio::test]
async fn sni_pattern_in_new_targets_shape_also_gated() {
    // T028: capability gate also fires on the new targets[] shape.
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.7.0"), false).await;
    let resp = f
        .router
        .clone()
        .oneshot(push(
            ALICE_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30022,
                "protocol": "tcp",
                "sni_pattern": "api.example.com",
                "targets": [
                    {"host": "primary.test", "port": 80}
                ],
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(err_code(resp).await, "sni_unsupported_by_client");
}

// ============================================================
//   T030 — overlap matrix (data-model.md §Overlap)
// ============================================================
//
// Direct against `ServerRuleStore` (the authoritative overlap engine);
// the HTTP path tests above already cover the wire-shape cases.
//
// Every planted rule is marked Active because the overlap check
// only fires against rules in `Active`/`Failed` state (v0.7
// semantics; preserved by v0.9).

use portunus_core::PortRange;
use portunus_server::rules::{Protocol, RuleStoreError, ServerRuleStore};

async fn plant_active_legacy(
    store: &ServerRuleStore,
    client: &ClientName,
    listen_port: u16,
    target_host: &str,
    target_port: u16,
) {
    let rule = store
        .push_range_with_targets(
            client.clone(),
            PortRange::single(listen_port),
            target_host.into(),
            PortRange::single(target_port),
            Protocol::Tcp,
            None,
            16,
            UserId::superadmin(),
            Vec::new(),
            None,
            None,
            None,
        )
        .await
        .expect("plant_active_legacy push");
    store.mark_active(rule.id).await.expect("mark_active");
}

async fn plant_active_sni(
    store: &ServerRuleStore,
    client: &ClientName,
    listen_port: u16,
    target_host: &str,
    target_port: u16,
    pattern: Option<&str>,
) {
    let rule = store
        .push_with_sni(
            client.clone(),
            listen_port,
            target_host.into(),
            target_port,
            Protocol::Tcp,
            pattern.map(str::to_string),
        )
        .await
        .expect("plant_active_sni push");
    store.mark_active(rule.id).await.expect("mark_active");
}

#[tokio::test]
async fn overlap_matrix_legacy_legacy_collides() {
    let store = ServerRuleStore::new();
    let client = ClientName::new("c1".to_string()).unwrap();
    plant_active_legacy(&store, &client, 443, "10.0.0.1", 9001).await;
    let err = store
        .push_range_with_targets(
            client,
            PortRange::single(443),
            "10.0.0.2".into(),
            PortRange::single(9002),
            Protocol::Tcp,
            None,
            16,
            UserId::superadmin(),
            Vec::new(),
            None,
            None,
            None,
        )
        .await
        .expect_err("second legacy push must collide");
    assert!(
        matches!(err, RuleStoreError::PortInUse { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn overlap_matrix_legacy_then_sni_returns_legacy_to_sni() {
    // Mirror direction (FR-015): legacy first, SNI candidate second.
    let store = ServerRuleStore::new();
    let client = ClientName::new("c3".to_string()).unwrap();
    plant_active_legacy(&store, &client, 443, "10.0.0.1", 9001).await;
    let err = store
        .push_with_sni(
            client,
            443,
            "10.0.0.2".into(),
            9002,
            Protocol::Tcp,
            Some("api.example.com".into()),
        )
        .await
        .expect_err("SNI push onto legacy listener must refuse");
    assert!(
        matches!(err, RuleStoreError::LegacyToSniUnsupported { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn overlap_matrix_sni_distinct_patterns_coexist() {
    let store = ServerRuleStore::new();
    let client = ClientName::new("c4".to_string()).unwrap();
    plant_active_sni(
        &store,
        &client,
        443,
        "10.0.0.1",
        9001,
        Some("api.example.com"),
    )
    .await;
    plant_active_sni(
        &store,
        &client,
        443,
        "10.0.0.2",
        9002,
        Some("admin.example.com"),
    )
    .await;
}

#[tokio::test]
async fn overlap_matrix_sni_duplicate_pattern_refused() {
    let store = ServerRuleStore::new();
    let client = ClientName::new("c5".to_string()).unwrap();
    plant_active_sni(
        &store,
        &client,
        443,
        "10.0.0.1",
        9001,
        Some("api.example.com"),
    )
    .await;
    let err = store
        .push_with_sni(
            client,
            443,
            "10.0.0.2".into(),
            9002,
            Protocol::Tcp,
            Some("api.example.com".into()),
        )
        .await
        .expect_err("duplicate SNI pattern must refuse");
    assert!(
        matches!(err, RuleStoreError::SniRouteDuplicate { .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn overlap_matrix_sni_then_fallback_coexists() {
    // (Some(_), None): exact + fallback can coexist on the same listener.
    let store = ServerRuleStore::new();
    let client = ClientName::new("c6".to_string()).unwrap();
    plant_active_sni(
        &store,
        &client,
        443,
        "10.0.0.1",
        9001,
        Some("api.example.com"),
    )
    .await;
    plant_active_sni(&store, &client, 443, "10.0.0.99", 9999, None).await;
}

#[tokio::test]
async fn overlap_matrix_two_fallbacks_refused() {
    let store = ServerRuleStore::new();
    let client = ClientName::new("c7".to_string()).unwrap();
    plant_active_sni(
        &store,
        &client,
        443,
        "10.0.0.1",
        9001,
        Some("api.example.com"),
    )
    .await;
    plant_active_sni(&store, &client, 443, "10.0.0.2", 9002, None).await;
    let err = store
        .push_with_sni(client, 443, "10.0.0.3".into(), 9003, Protocol::Tcp, None)
        .await
        .expect_err("second fallback must refuse");
    assert!(
        matches!(err, RuleStoreError::SniFallbackDuplicate { .. }),
        "got {err:?}"
    );
}

// ============================================================
//   T031 — legacy↔SNI mode lock surfaces over HTTP (409)
// ============================================================

#[tokio::test]
async fn http_legacy_listener_refuses_sni_candidate() {
    let f = build_fixture();
    register_fake_client(&f, CLIENT, Some("0.9.0"), false).await;
    // Plant a legacy rule at :30030 directly (bypass the wire ack)
    // and mark it Active so the overlap check will see it.
    let client = ClientName::new(CLIENT.to_string()).expect("name");
    let rule = f
        .state
        .rules
        .push_range_with_targets(
            client.clone(),
            portunus_core::PortRange::single(30030),
            "10.0.0.1".into(),
            portunus_core::PortRange::single(9001),
            portunus_server::rules::Protocol::Tcp,
            None,
            16,
            UserId::superadmin(),
            Vec::new(),
            None,
            None,
            None,
        )
        .await
        .expect("legacy rule plant");
    f.state
        .rules
        .mark_active(rule.id)
        .await
        .expect("mark_active");
    // Now an SNI candidate on the same port must collide.
    let resp = f
        .router
        .clone()
        .oneshot(push(
            SUPERADMIN_TOKEN,
            serde_json::json!({
                "client": CLIENT,
                "listen_port": 30030,
                "target_host": "10.0.0.2",
                "target_port": 9002,
                "protocol": "tcp",
                "sni_pattern": "api.example.com",
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(err_code(resp).await, "conflict.legacy_to_sni_unsupported");
}

// Note: pushing a `sni_pattern: None` rule onto a listener already in
// SNI mode is the legitimate FALLBACK path (FR-014). It is NOT an
// attempt to flip the listener back to legacy — at the API level the
// two intents are wire-indistinguishable, and the design treats the
// presence of any sibling with `sni_pattern = Some` as "the listener
// is in SNI mode, so this is a fallback". Hence there is no symmetric
// `http_sni_listener_refuses_legacy_candidate` test — a follow-up
// fallback push is asserted under
// `overlap_matrix_sni_then_fallback_coexists` above. The
// LegacyToSniUnsupported gate fires only in the "legacy listener
// existed first, candidate brings sni_pattern" direction (FR-015).
