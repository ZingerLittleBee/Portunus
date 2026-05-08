//! T014 (005-multi-user-rbac, US1) — RBAC enforcement on `POST /v1/rules`.
//!
//! Each test seeds the operator store with one superadmin and one
//! constrained user (`alice` with grant `client-a, 30000..=30010, [tcp]`),
//! constructs the v0.5 router in-process, and asserts that the auth +
//! RBAC layer returns the right HTTP status / `error.code` for the FR-008
//! denial taxonomy:
//!
//! - missing Authorization header → 401 `unauthenticated`
//! - garbage bearer → 401 `credential_invalid`
//! - alice push to `client-b` → 403 `client_not_granted`
//! - alice push port outside grant → 403 `port_outside_grant`
//! - alice push UDP on TCP-only grant → 403 `protocol_not_granted`
//!
//! The success path (alice push within grant) is NOT exercised here
//! because rule activation requires a connected gRPC client, which a
//! pure in-process test cannot stand up. The existing v0.4 e2e suite
//! (now bearer-authed) covers that with a real client.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use forward_auth::{ClientScope, Grant, GrantId, OperatorRole, ProtocolSet, User, UserId};
use forward_core::ClientName;
use forward_server::clients::ConnectedClients;
use forward_server::operator::http;
use forward_server::state::AppState;
use std::str::FromStr;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPERADMIN_TOKEN: &str = "T014-super";
const ALICE_TOKEN: &str = "T014-alice";

struct Fixture {
    router: axum::Router,
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

    // Add alice + a TCP-only grant for client-a, ports 30000..=30010.
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
    let (_alice_cred, _raw) = operator_store
        .issue_credential(&alice_id, Some("test".to_string()))
        .expect("issue cred");
    // 008-sqlite-storage T044: inject a known token by patching the
    // SQLite-backed credential row directly. The file-store path that
    // used to mutate identity.json + reload is gone in v0.8.
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
        note: Some("US1 test fixture".to_string()),
        created_at: Utc::now(),
    };
    operator_store.add_grant(alice_grant).expect("add grant");

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

    Fixture {
        router: http::router(state),
        _dir: dir,
    }
}

fn push_rule_request(
    bearer: Option<&str>,
    client: &str,
    listen_port: u16,
    protocol: &str,
) -> Request<Body> {
    let body = serde_json::json!({
        "client": client,
        "listen_port": listen_port,
        "target_host": "127.0.0.1",
        "target_port": 9000,
        "protocol": protocol,
    });
    let mut req = Request::builder()
        .method("POST")
        .uri("/v1/rules")
        .header("content-type", "application/json");
    if let Some(t) = bearer {
        req = req.header("Authorization", format!("Bearer {t}"));
    }
    req.body(Body::from(serde_json::to_vec(&body).expect("body")))
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

#[tokio::test]
async fn missing_auth_header_returns_401_unauthenticated() {
    let f = build_fixture();
    let resp = f
        .router
        .oneshot(push_rule_request(None, "client-a", 30001, "tcp"))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(err_code(resp).await, "unauthenticated");
}

#[tokio::test]
async fn garbage_bearer_returns_401_credential_invalid() {
    let f = build_fixture();
    let resp = f
        .router
        .oneshot(push_rule_request(
            Some("not-a-real-token"),
            "client-a",
            30001,
            "tcp",
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(err_code(resp).await, "credential_invalid");
}

#[tokio::test]
async fn alice_push_to_disallowed_client_returns_403_client_not_granted() {
    let f = build_fixture();
    let resp = f
        .router
        .oneshot(push_rule_request(
            Some(ALICE_TOKEN),
            "client-b",
            30001,
            "tcp",
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(err_code(resp).await, "client_not_granted");
}

#[tokio::test]
async fn alice_push_outside_port_grant_returns_403_port_outside_grant() {
    let f = build_fixture();
    let resp = f
        .router
        .oneshot(push_rule_request(
            Some(ALICE_TOKEN),
            "client-a",
            30099,
            "tcp",
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(err_code(resp).await, "port_outside_grant");
}

#[tokio::test]
async fn alice_push_udp_on_tcp_only_grant_returns_403_protocol_not_granted() {
    let f = build_fixture();
    let resp = f
        .router
        .oneshot(push_rule_request(
            Some(ALICE_TOKEN),
            "client-a",
            30001,
            "udp",
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(err_code(resp).await, "protocol_not_granted");
}

#[tokio::test]
async fn alice_push_within_grant_passes_rbac_then_fails_on_no_client() {
    // Inside-grant push gets past RBAC; the next stage tries to dispatch
    // over gRPC to a client named "client-a" which is not connected in
    // this in-process test. The fixture asserts the failure is the
    // *post-RBAC* one (`client_not_connected`, HTTP 503 / 502 / 4xx),
    // never `port_outside_grant` / `protocol_not_granted` / `client_not_granted`.
    let f = build_fixture();
    let resp = f
        .router
        .oneshot(push_rule_request(
            Some(ALICE_TOKEN),
            "client-a",
            30001,
            "tcp",
        ))
        .await
        .expect("oneshot");
    let status = resp.status();
    let code = err_code(resp).await;
    assert_ne!(status, StatusCode::FORBIDDEN, "RBAC must allow this push");
    assert_ne!(code, "client_not_granted");
    assert_ne!(code, "port_outside_grant");
    assert_ne!(code, "protocol_not_granted");
    assert_ne!(code, "unauthenticated");
    // Concretely: the rule push reaches `cli::push_rule` which fails
    // with `client_not_connected` because no `Hello` ever landed.
    assert_eq!(
        code, "client_not_connected",
        "expected failure to be the post-RBAC client-not-connected; got status={status} code={code}"
    );
}
