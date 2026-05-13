//! Owner-client concurrent-connection plan, Task 8 — CLI smoke test
//! for `owner-cap set --concurrent-connections N` standalone.
//!
//! Pins the end-to-end CLI round-trip for the v1.3 owner-level
//! concurrent_connections cap: invoke the real `portunus-server`
//! binary against an in-process axum HTTP harness with a fake v0.11.0
//! client registered in the shared `ConnectedClients`, then assert
//! (1) text-mode stdout matches `owner-cap set client=… owner=…` and
//! (2) `owner-cap get … --format json` reports
//! `rate_limit.concurrent_connections == 100` with every other cap
//! field null.
//!
//! The capability gate at the HTTP layer requires the connected
//! client to advertise version ≥ 0.11.0 (mirrors t070/t071 in
//! `rate_limit_owner_contract.rs`); registering the fake client via
//! the shared `AppState` is what unblocks the PUT happy path.

#![allow(clippy::wildcard_imports)]

use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;

use chrono::Utc;
use portunus_auth::{ClientScope, Grant, GrantId, OperatorRole, ProtocolSet, User, UserId};
use portunus_core::ClientName;
use portunus_proto::v1::Protocol as ProtoProtocol;
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const SUPERADMIN_TOKEN: &str = "T008-cli-super";
const CLIENT: &str = "edge-01";
const OWNER: &str = "alice";

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_portunus-server")
}

struct Harness {
    _dir: TempDir,
    state: Arc<AppState>,
    addr: std::net::SocketAddr,
}

async fn build_harness() -> Harness {
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

    // Add the owner as a real user with a grant on the target client,
    // so the operator HTTP layer's capability checks see a valid
    // owner_id (mirrors `build_fixture` in rate_limit_owner_contract.rs).
    let alice_id = UserId::from_str(OWNER).expect("valid user id");
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

    let router = http::router(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    tokio::spawn(async move {
        axum::serve(listener, router).await.expect("axum serve");
    });

    Harness {
        _dir: dir,
        state,
        addr,
    }
}

/// Register a fake client at the requested protocol version so the
/// PUT capability gate (≥ 0.11.0 for `concurrent_connections`) is
/// satisfied. Mirrors `register_fake_client` in
/// `rate_limit_owner_contract.rs` but drops the gRPC outbound capture
/// — this test only cares about the HTTP/CLI surface.
async fn register_fake_client(harness: &Harness, name: &str, version: &str) {
    let client_name = ClientName::new(name.to_string()).expect("valid client");
    let cancel = CancellationToken::new();
    let (outbound, _rx) = tokio::sync::mpsc::channel(8);
    let waiters: Arc<
        tokio::sync::Mutex<HashMap<String, oneshot::Sender<portunus_proto::v1::RuleStatus>>>,
    > = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let session_id = harness
        .state
        .clients
        .register(client_name.clone(), None, cancel, outbound, waiters)
        .await;
    let mut caps = HashSet::new();
    caps.insert(ProtoProtocol::Tcp);
    harness
        .state
        .clients
        .set_supported_protocols(&client_name, session_id, caps)
        .await;
    harness
        .state
        .clients
        .set_client_version(&client_name, session_id, version.to_string())
        .await;
}

/// 011-rate-limiting-qos owner-client concurrent-cap plan (Task 8):
/// `owner-cap set <client> <owner> --concurrent-connections 100` is a
/// valid invocation on its own (no bandwidth caps required), pushes
/// to a v0.11.0 client, and round-trips through `owner-cap get … json`
/// with all other rate-limit fields null.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owner_cap_set_concurrent_only_round_trips_via_cli() {
    let harness = build_harness().await;
    register_fake_client(&harness, CLIENT, "0.11.0").await;
    let endpoint = harness.addr.to_string();

    // --- SET ---
    // Drive the actual `portunus-server` binary so the test exercises
    // the full clap → owner_cap_cli::set → HTTP path, not an
    // in-process shortcut.
    let set_out = tokio::task::spawn_blocking({
        let endpoint = endpoint.clone();
        move || {
            Command::new(server_bin())
                .arg("owner-cap")
                .arg("set")
                .arg(CLIENT)
                .arg(OWNER)
                .arg("--concurrent-connections")
                .arg("100")
                .arg("--http-endpoint")
                .arg(&endpoint)
                .env("PORTUNUS_OPERATOR_TOKEN", SUPERADMIN_TOKEN)
                .output()
                .expect("run owner-cap set")
        }
    })
    .await
    .expect("join set");

    let set_stdout = String::from_utf8_lossy(&set_out.stdout);
    let set_stderr = String::from_utf8_lossy(&set_out.stderr);
    assert!(
        set_out.status.success(),
        "owner-cap set should succeed; stdout={set_stdout} stderr={set_stderr}"
    );
    // Format pinned by `owner_cap_cli.rs:282`:
    //   `owner-cap set client=<name> owner=<id> updated_at_unix_ms=<ms>`
    assert!(
        set_stdout.contains("owner-cap set"),
        "stdout should contain `owner-cap set`; got {set_stdout}"
    );
    assert!(
        set_stdout.contains(&format!("client={CLIENT}")),
        "stdout should carry client={CLIENT}; got {set_stdout}"
    );
    assert!(
        set_stdout.contains(&format!("owner={OWNER}")),
        "stdout should carry owner={OWNER}; got {set_stdout}"
    );

    // --- GET --format json ---
    let get_out = tokio::task::spawn_blocking({
        let endpoint = endpoint.clone();
        move || {
            Command::new(server_bin())
                .arg("owner-cap")
                .arg("get")
                .arg(CLIENT)
                .arg(OWNER)
                .arg("--format")
                .arg("json")
                .arg("--http-endpoint")
                .arg(&endpoint)
                .env("PORTUNUS_OPERATOR_TOKEN", SUPERADMIN_TOKEN)
                .output()
                .expect("run owner-cap get")
        }
    })
    .await
    .expect("join get");

    let get_stdout = String::from_utf8_lossy(&get_out.stdout);
    let get_stderr = String::from_utf8_lossy(&get_out.stderr);
    assert!(
        get_out.status.success(),
        "owner-cap get should succeed; stdout={get_stdout} stderr={get_stderr}"
    );

    let view: serde_json::Value = serde_json::from_str(get_stdout.trim()).unwrap_or_else(|e| {
        panic!("owner-cap get --format json must produce parseable JSON: {e}; stdout={get_stdout}")
    });

    assert_eq!(view["client_name"], CLIENT);
    assert_eq!(view["owner_id"], OWNER);
    assert_eq!(
        view["rate_limit"]["concurrent_connections"], 100,
        "concurrent_connections should round-trip as 100; got {view}"
    );
    // Concurrent-only invocation must leave every other cap field
    // absent — `skip_serializing_if = Option::is_none` on the wire,
    // surfaces as JSON `null` on the client-side `Option` shape.
    for field in [
        "bandwidth_in_bps",
        "bandwidth_out_bps",
        "new_connections_per_sec",
        "bandwidth_in_burst",
        "bandwidth_out_burst",
        "new_connections_burst",
    ] {
        assert!(
            view["rate_limit"][field].is_null(),
            "field {field} should be null after concurrent-only set; got {:?} (full view: {view})",
            view["rate_limit"][field]
        );
    }
}
