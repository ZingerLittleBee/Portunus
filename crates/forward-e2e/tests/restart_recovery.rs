//! T054c — restart recovery (SC-005).
//!
//! Provision a client, push a rule, transfer some bytes, kill the client
//! process, restart it with the same bundle, re-push the same rule (after
//! the previous one was implicitly torn down by the disconnect), and assert
//! that forwarding resumes within 5 s of the re-push call.

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

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

fn wait_connected(http: &str, name: &str) -> bool {
    common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some(name)
                    && v.get("connected")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
            })
            .map(|_| ())
    })
    .is_some()
}

#[test]
fn test_repush_after_client_restart_resumes_within_5s() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server listening");

    let (echo_host, echo_port) = spawn_echo();
    let bundle = common::provision_client_http(&http, "edge-restart");

    // First incarnation of the client.
    let client_v1 = common::spawn_client(&bundle, &[]);
    assert!(
        wait_connected(&http, "edge-restart"),
        "v1: client never connected"
    );

    let listen_port = pick_free_port();
    let (status, body) = common::push_rule_http(
        &http,
        "edge-restart",
        listen_port,
        &echo_host,
        echo_port,
        Some(2),
    );
    assert!(status.is_success(), "v1 push failed: {status} body={body}");
    let rule_id = body
        .get("rule_id")
        .and_then(serde_json::Value::as_u64)
        .expect("rule_id");

    // Some bytes flow through.
    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).expect("connect v1");
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(b"v1-payload").unwrap();
    let mut buf = [0u8; 10];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"v1-payload");
    drop(conn);

    // Kill v1. Drop fires SIGKILL on the child.
    drop(client_v1);

    // Wait for the server to notice the disconnect (cancel_token cleared by
    // pump exit), then clean up the now-orphaned rule from the previous
    // incarnation. (Rules don't persist across restarts per spec.)
    common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        let connected = arr.as_array()?.iter().any(|v| {
            v.get("client_name").and_then(|n| n.as_str()) == Some("edge-restart")
                && v.get("connected")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
        });
        // Predicate fires when client is *no longer* connected.
        (!connected).then_some(())
    })
    .expect("server should observe v1 disconnect within 5s");
    let _ = common::remove_rule_http(&http, rule_id);

    // Second incarnation, same bundle.
    let _client_v2 = common::spawn_client(&bundle, &[]);
    assert!(
        wait_connected(&http, "edge-restart"),
        "v2: client never re-connected"
    );

    // Re-push the same rule. Measure wall-clock from re-push to first byte
    // forwarded successfully — must be < 5 s (SC-005).
    let repush_started = Instant::now();
    let (status, body) = common::push_rule_http(
        &http,
        "edge-restart",
        listen_port,
        &echo_host,
        echo_port,
        Some(2),
    );
    assert!(status.is_success(), "v2 push failed: {status} body={body}");
    // Forwarding works.
    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).expect("connect v2");
    conn.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    conn.write_all(b"v2-payload").unwrap();
    let mut buf = [0u8; 10];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"v2-payload");
    let resumed_after = repush_started.elapsed();
    assert!(
        resumed_after < Duration::from_secs(5),
        "SC-005: forwarding must resume within 5s, took {resumed_after:?}"
    );

    // Sanity: the rule shows up as Active under the same client name.
    let rules = common::list_rules_http(&http, Some("edge-restart"));
    let active = rules.as_array().and_then(|arr| {
        arr.iter().find(|r| {
            r.pointer("/state/kind").and_then(|v| v.as_str()) == Some("active")
                && r.get("listen_port").and_then(serde_json::Value::as_u64)
                    == Some(u64::from(listen_port))
        })
    });
    let active = active.unwrap_or_else(|| panic!("expected active rule in {rules}"));
    let _: &Value = active;
}
