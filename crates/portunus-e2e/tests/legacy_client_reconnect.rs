//! 015-client-stable-id (US4: T039) — transparent upgrade for a
//! pre-upgrade client bundle.
//!
//! A bundle issued before the stable-id refactor carries only a bearer
//! token, no `client_id`. After the upgrade the server assigns every
//! client a stable id, but identity on the data-plane is resolved from
//! the authenticated token — never from a wire-carried name or id — so a
//! legacy bundle MUST reconnect and forward traffic with no
//! re-enrollment (SC-005 / FR-009).
//!
//! This test reproduces that bundle by provisioning a client normally
//! and then stripping the `client_id` key from the saved bundle JSON,
//! which is byte-identical to a bundle written by a pre-015 client.

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::Duration;

use serde_json::Value;

/// Tiny in-process TCP echo server; returns its `(host, port)`.
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

#[test]
fn legacy_bundle_without_client_id_reconnects_and_forwards() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server should be listening within 5s");

    // Provision normally, then derive a legacy bundle by dropping the
    // `client_id` key — exactly the shape a pre-015 client persisted.
    let bundle = common::provision_client_http(&http, "edge-legacy");
    let mut json: Value =
        serde_json::from_str(&std::fs::read_to_string(&bundle).unwrap()).expect("bundle JSON");
    assert!(
        json.get("client_id").is_some(),
        "sanity: a freshly-provisioned bundle carries a client_id; stripping it must be meaningful"
    );
    json.as_object_mut().unwrap().remove("client_id");
    let legacy_path = server.data_dir.path().join("legacy.bundle.json");
    std::fs::write(&legacy_path, serde_json::to_vec_pretty(&json).unwrap()).unwrap();

    // The legacy bundle connects: identity is token-resolved server-side.
    let client = common::spawn_client(&legacy_path, &[]);
    let connected = common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("edge-legacy")
                    && v.get("connected").and_then(Value::as_bool).unwrap_or(false)
            })
            .cloned()
    });
    if connected.is_none() {
        eprintln!("--- server stderr ---");
        for l in server.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
        eprintln!("--- client stderr ---");
        for l in client.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
    }
    assert!(
        connected.is_some(),
        "legacy bundle (no client_id) must reconnect with no re-enrollment"
    );

    // And it forwards traffic: push a rule, then prove bytes round-trip.
    let (echo_host, echo_port) = spawn_echo();
    let listen_port = pick_free_port();
    let (status, body) = common::push_rule_http(
        &http,
        "edge-legacy",
        listen_port,
        &echo_host,
        echo_port,
        Some(2),
    );
    assert!(
        status.is_success(),
        "push must succeed: {status} body={body}"
    );

    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).expect("connect proxy");
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    const PAYLOAD: &[u8] = b"legacy-bundle-still-forwards";
    conn.write_all(PAYLOAD).unwrap();
    let mut buf = [0u8; PAYLOAD.len()];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, PAYLOAD, "SC-005: legacy client must forward bytes");
}
