//! US2 failure-path e2e tests.
//!
//! - T039: pushing to a disconnected client → exit 4 / HTTP 422 with
//!   `client_not_connected`. The rule is NOT stored.
//! - T040: pushing a rule whose port is already bound on the client lands the
//!   rule in `Failed(port_in_use)`. A second push with the same `(client,
//!   listen_port)` is blocked with `port_in_use` until `remove-rule` clears
//!   the slot (Q4).

mod common;

use std::net::{Ipv4Addr, TcpListener};
use std::time::Duration;

use common::{
    list_rules_http, provision_client_http, push_rule_http, remove_rule_http, spawn_client,
    spawn_server, wait_for,
};

/// FR-014 / Edge case "client not currently connected": the server MUST reject
/// `push-rule` with `client_not_connected` and MUST NOT persist the rule.
#[test]
fn test_push_to_disconnected_client() {
    let server = spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server listening");

    // Provision a client name but never bring up the client process.
    let _bundle = provision_client_http(&http, "ghost-edge");

    // Push to the unconnected client.
    let (status, body) = push_rule_http(&http, "ghost-edge", 19090, "127.0.0.1", 9, Some(2));
    assert_eq!(status.as_u16(), 422, "expected HTTP 422, got {status}");
    let code = body
        .pointer("/error/code")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "client_not_connected", "body={body}");

    // The rule must NOT have been stored — list-rules returns empty.
    let rules = list_rules_http(&http, None);
    assert_eq!(
        rules.as_array().map_or(usize::MAX, Vec::len),
        0,
        "no rule should be stored after a failed push: {rules}"
    );
}

/// FR-012 + Q4: a rule whose port is already bound on the client lands in
/// `Failed(port_in_use)`. Re-pushing the same `(client, listen_port)` must be
/// rejected with `port_in_use` until the operator explicitly removes the
/// failed rule.
#[test]
fn test_failed_blocks_port_reuse() {
    let server = spawn_server(&[]);
    let (grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server listening");

    let bundle = provision_client_http(&http, "edge-port");
    let _client = spawn_client(&bundle, &[]);

    // Wait for the client to register against the running server.
    let connected = wait_for(Duration::from_secs(5), || {
        let v = common::list_clients_http(&http);
        let conn = v.as_array()?.iter().any(|c| {
            c.get("client_name").and_then(|x| x.as_str()) == Some("edge-port")
                && c.get("connected").and_then(serde_json::Value::as_bool) == Some(true)
        });
        conn.then_some(())
    });
    assert!(
        connected.is_some(),
        "client never registered against {grpc}"
    );

    // Bind a busy port on this test process, then try to push a rule that
    // wants to listen on the same port. The client binds 0.0.0.0:port — to
    // make sure that conflicts with our pin we also bind 0.0.0.0.
    let occupy = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind busy port");
    let busy_port = occupy.local_addr().unwrap().port();

    let (status, body) = push_rule_http(&http, "edge-port", busy_port, "127.0.0.1", 9, Some(3));
    assert_eq!(status.as_u16(), 422, "expected 422, got {status}: {body}");
    let code = body
        .pointer("/error/code")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "activation_failed", "body={body}");
    let msg = body
        .pointer("/error/message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        msg.contains("port_in_use"),
        "expected port_in_use in message, got: {msg}"
    );

    // Q4: a re-push for the same (client, port) must be blocked.
    let (status2, body2) = push_rule_http(&http, "edge-port", busy_port, "127.0.0.1", 9, Some(2));
    assert_eq!(status2.as_u16(), 409, "expected 409, got {status2}");
    assert_eq!(
        body2
            .pointer("/error/code")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "port_in_use",
        "body={body2}"
    );

    // Find the failed rule's id, then remove it. After removal, the slot frees.
    let rules = list_rules_http(&http, Some("edge-port"));
    let arr = rules.as_array().expect("array");
    let failed = arr
        .iter()
        .find(|r| {
            r.pointer("/state/kind").and_then(|v| v.as_str()) == Some("failed")
                && r.get("listen_port").and_then(serde_json::Value::as_u64)
                    == Some(u64::from(busy_port))
        })
        .unwrap_or_else(|| panic!("no failed rule for port {busy_port} in {rules}"));
    let rule_id = failed
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .expect("id");
    let status = remove_rule_http(&http, rule_id);
    assert_eq!(status.as_u16(), 204);

    // Free the busy port so the next push has a chance to succeed.
    drop(occupy);

    // Now a fresh push for the same listen_port — the slot is free, no other
    // listener competes — must succeed (rule activates).
    let free_port = pick_free_port();
    let (status3, body3) = push_rule_http(&http, "edge-port", free_port, "127.0.0.1", 9, Some(3));
    assert!(
        status3.is_success(),
        "expected success after remove, got {status3}: {body3}"
    );
}

fn pick_free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind ephemeral")
        .local_addr()
        .unwrap()
        .port()
}
