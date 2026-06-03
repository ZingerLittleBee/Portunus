//! 015-client-stable-id (US2: T029) — rename does not drop a live session.
//!
//! A client's identity is its stable `client_id`; the display name is a
//! free-form label. Renaming a connected client must therefore leave its
//! gRPC control stream — and every active forwarding rule on it — intact
//! (FR-009/FR-010). This test connects a client, proves traffic flows,
//! renames it mid-session (to a brand-new name, then to a name that
//! confirms the id is what binds the rule), and proves the same listener
//! keeps forwarding with no reconnect.

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::Duration;

use serde_json::Value;

fn spawn_echo() -> (String, u16) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind echo");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut sock = incoming;
                let mut buf = [0u8; 4096];
                loop {
                    match sock.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if sock.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    (addr.ip().to_string(), addr.port())
}

fn pick_free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind ephemeral")
        .local_addr()
        .unwrap()
        .port()
}

fn echo_roundtrip(listen_port: u16, payload: &[u8]) {
    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).expect("connect proxy");
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(payload).unwrap();
    let mut buf = vec![0u8; payload.len()];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(buf, payload, "bytes must round-trip through the listener");
}

#[test]
fn rename_keeps_live_session_and_rule_forwarding() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server listening");

    let (echo_host, echo_port) = spawn_echo();
    let bundle = common::provision_client_http(&http, "edge-before");
    let _client = common::spawn_client(&bundle, &[]);
    let connected = common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("edge-before")
                    && v.get("connected").and_then(Value::as_bool).unwrap_or(false)
            })
            .cloned()
    });
    assert!(connected.is_some(), "edge-before must connect within 5s");

    // Push a rule and prove it forwards before the rename.
    let listen_port = pick_free_port();
    let (status, body) = common::push_rule_http(
        &http,
        "edge-before",
        listen_port,
        &echo_host,
        echo_port,
        Some(2),
    );
    assert!(status.is_success(), "push must succeed: {status} {body}");
    echo_roundtrip(listen_port, b"before-rename");

    // Rename mid-session to a free-form display name. The stable id — and
    // the gRPC session keyed on it — is untouched.
    let status = common::rename_http(&http, "edge-before", "Edge After – 东区");
    assert!(status.is_success(), "rename must succeed: {status}");

    // The client is still connected under the SAME identity, now shown
    // with the new display name (no reconnect, no flap).
    let still_connected = common::wait_for(Duration::from_secs(3), || {
        let arr = common::list_clients_http(&http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("Edge After – 东区")
                    && v.get("connected").and_then(Value::as_bool).unwrap_or(false)
            })
            .cloned()
    });
    assert!(
        still_connected.is_some(),
        "renamed client must remain connected under its new display name"
    );

    // The pre-existing listener keeps forwarding — the rule rode the
    // rename because it is keyed on the id, not the name.
    echo_roundtrip(listen_port, b"after-rename-same-session");

    // The old display name no longer resolves (it was relabeled, not
    // duplicated) — listing by the new name is the single source of truth.
    let arr = common::list_clients_http(&http);
    let old_present = arr
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.get("client_name").and_then(|n| n.as_str()) == Some("edge-before"));
    assert!(
        !old_present,
        "the old display name must be gone after rename"
    );
}
