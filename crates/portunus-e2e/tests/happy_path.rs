//! T026 + T027 — US1 happy path coverage.
//!
//! Spins up `portunus-server`, provisions `edge-01`, starts `portunus-client`
//! against the issued bundle, and asserts the connected-state shows up via
//! the loopback operator HTTP API within 5 s (acceptance scenario #1).

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use serde_json::Value;

#[test]
fn test_list_clients_after_connect() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server should log listening event within 5s");

    let bundle = common::provision_client_http(&http, "edge-01");
    let client = common::spawn_client(&bundle, &[]);

    // Acceptance scenario #1: client appears as connected within 5 s.
    let view = common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        let edge = arr
            .as_array()?
            .iter()
            .find(|v| v.get("client_name").and_then(|n| n.as_str()) == Some("edge-01"))?;
        if edge.get("connected")?.as_bool()? {
            Some(edge.clone())
        } else {
            None
        }
    });
    if view.is_none() {
        eprintln!("--- server stderr ---");
        for l in server.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
        eprintln!("--- client stderr ---");
        for l in client.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
    }
    let edge = view.expect("edge-01 should be reported connected within 5s");
    assert_eq!(edge["client_name"], "edge-01");
    assert_eq!(edge["connected"], true);
    assert!(
        edge.get("remote_addr").is_some(),
        "remote_addr field present"
    );
    assert!(
        edge.get("connected_at").is_some(),
        "connected_at field present"
    );
}

/// Walks the four US1 acceptance scenarios in one run:
/// 1. Provision + connect → appears connected.
/// 2. Bad token → never appears connected, server logs `auth_failure`.
/// 3. Revoked token → server logs reason `token_revoked`, client never appears.
/// 4. Pin mismatch → client refuses TLS, exits with `bundle pin mismatch`.
#[test]
#[allow(clippy::too_many_lines)] // intentional: one test walks all 4 US1 scenarios end-to-end
fn test_user_story_1_acceptance() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server should be listening");

    // ---- Scenario 1: happy path ----
    let bundle = common::provision_client_http(&http, "edge-01");
    let good_client = common::spawn_client(&bundle, &[]);
    let connected = common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("edge-01")
                    && v.get("connected")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
            })
            .cloned()
    });
    if connected.is_none() {
        eprintln!("--- server stderr ---");
        for l in server.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
        eprintln!("--- client stderr ---");
        for l in good_client.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
    }
    assert!(
        connected.is_some(),
        "scenario 1: edge-01 should appear connected"
    );

    // ---- Scenario 2: bad token (provisioned client, mutated token) ----
    let bad_bundle_path = server.config_dir.path().join("bad.bundle.json");
    let mut bad: Value = serde_json::from_str(&std::fs::read_to_string(&bundle).unwrap()).unwrap();
    // Replace the token with garbage that has the same length shape.
    bad["client_name"] = Value::String("bogus".into());
    bad["token"] = Value::String("Aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh-iii".into());
    std::fs::write(&bad_bundle_path, serde_json::to_vec_pretty(&bad).unwrap()).unwrap();
    let _bad_client = common::spawn_client(&bad_bundle_path, &[]);
    // Wait briefly for the auth failure to materialise on the server side.
    let auth_failure_seen = common::wait_for(Duration::from_secs(5), || {
        // Look for the audit / auth_failure structured event in stderr.
        let lines = server.stderr_lines.lock().unwrap();
        lines
            .iter()
            .any(|l| l.contains("auth.failure"))
            .then_some(())
    });
    assert!(
        auth_failure_seen.is_some(),
        "scenario 2: server should log auth_failure for bogus token"
    );
    // bogus client never appears connected.
    let arr = common::list_clients_http(&http);
    let bogus_present = arr
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.get("client_name").and_then(|n| n.as_str()) == Some("bogus"));
    assert!(
        !bogus_present,
        "scenario 2: 'bogus' was never provisioned, must not appear"
    );

    // ---- Scenario 3: revoked token ----
    let edge2_bundle = common::provision_client_http(&http, "edge-02");
    let revoke_status = common::revoke_http(&http, "edge-02");
    assert!(
        revoke_status.is_success(),
        "revoke should succeed: {revoke_status}"
    );
    let _revoked_client = common::spawn_client(&edge2_bundle, &[]);
    let revoke_event_seen = common::wait_for(Duration::from_secs(5), || {
        let lines = server.stderr_lines.lock().unwrap();
        lines
            .iter()
            .any(|l| l.contains("token_revoked"))
            .then_some(())
    });
    assert!(
        revoke_event_seen.is_some(),
        "scenario 3: server stderr should contain 'token_revoked'"
    );

    // ---- Scenario 4: pin mismatch ----
    let pin_mismatch_path = server.config_dir.path().join("pin-mismatch.bundle.json");
    let mut tampered: Value =
        serde_json::from_str(&std::fs::read_to_string(&bundle).unwrap()).unwrap();
    // Flip one byte of the fingerprint hex to force the bundle's pin check
    // (CredentialBundle::verify_pin_consistency) to fire — the client refuses
    // to dial out at all.
    let original = tampered["server_cert_sha256"].as_str().unwrap().to_string();
    let mut chars: Vec<char> = original.chars().collect();
    chars[0] = if chars[0] == '0' { 'f' } else { '0' };
    tampered["server_cert_sha256"] = Value::String(chars.into_iter().collect());
    std::fs::write(
        &pin_mismatch_path,
        serde_json::to_vec_pretty(&tampered).unwrap(),
    )
    .unwrap();
    let mut bad_pin_client = common::spawn_client(&pin_mismatch_path, &[]);
    // Client should exit non-zero quickly because the bundle fails pin check
    // at load time.
    let exit = common::wait_for(Duration::from_secs(5), || {
        bad_pin_client.child.try_wait().ok().flatten()
    });
    let status = exit.expect("client must exit on pin mismatch within 5s");
    assert!(
        !status.success(),
        "scenario 4: client must exit non-zero on pin mismatch, got {status:?}"
    );
    assert!(
        bad_pin_client.stderr_contains("bundle_load_failed")
            || bad_pin_client.stderr_contains("pin mismatch"),
        "scenario 4: client stderr should report pin mismatch / bundle_load_failed"
    );
}

/// Spawn a tiny in-process TCP echo server and return its `(host, port)`.
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

/// T043 — US2 acceptance scenarios end-to-end. Walks all 5:
/// 1. Push → Active within 1 s + bytes flow through (FR-009..014, FR-012's <1s).
/// 2. 100 KB transfer is byte-equal (cheap stand-in for the full T041's 100 MB).
/// 3. Port conflict on the client → `Failed(port_in_use)` surfaced to operator.
/// 4. Push for an unconnected client → `client_not_connected`, rule not stored.
/// 5. Remove rule → listener stops within 1 s (FR-014/FR-016).
#[test]
#[allow(clippy::too_many_lines)]
fn test_user_story_2_acceptance() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server listening");

    // Bring up the echo target + the connected client.
    let (echo_host, echo_port) = spawn_echo();
    let bundle = common::provision_client_http(&http, "edge-01");
    let _client = common::spawn_client(&bundle, &[]);
    let connected = common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("edge-01")
                    && v.get("connected").and_then(Value::as_bool).unwrap_or(false)
            })
            .cloned()
    });
    assert!(connected.is_some(), "edge-01 must connect within 5s");

    // ---- Scenario 1: push → Active within 1 s + bytes flow ----
    let listen_port = pick_free_port();
    let push_started = Instant::now();
    let (status, body) = common::push_rule_http(
        &http,
        "edge-01",
        listen_port,
        &echo_host,
        echo_port,
        Some(2),
    );
    let activation_latency = push_started.elapsed();
    assert!(
        status.is_success(),
        "push must succeed: {status} body={body}"
    );
    // FR-012: client acknowledges within 1 s. We measure the operator's
    // wall-clock view of the push: HTTP returns only after the client emits
    // RuleStatus(Activated), so the round-trip ≈ activation latency.
    assert!(
        activation_latency < Duration::from_secs(1),
        "FR-012: activation must complete within 1s, got {activation_latency:?}"
    );

    // Bytes flow through the listener.
    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).expect("connect proxy");
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    const PAYLOAD: &[u8] = b"Portunus-hello";
    conn.write_all(PAYLOAD).unwrap();
    let mut buf = [0u8; PAYLOAD.len()];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, PAYLOAD);

    // ---- Scenario 2: 100 KB byte-equal transfer ----
    // (T041 covers the 100 MB version against the client lib directly.)
    let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let bulk = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).expect("connect bulk");
    bulk.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let payload_send = payload.clone();
    let send_thread = std::thread::spawn(move || {
        let mut writer = bulk;
        writer.write_all(&payload_send).unwrap();
        writer.shutdown(std::net::Shutdown::Write).unwrap();
        let mut received = Vec::with_capacity(100_000);
        writer.read_to_end(&mut received).unwrap();
        received
    });
    let received = send_thread.join().unwrap();
    assert_eq!(received.len(), payload.len(), "100KB length mismatch");
    assert_eq!(received, payload, "bytes must be byte-equal");

    // ---- Scenario 3: port conflict on the client ----
    let occupy = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("bind busy");
    let busy_port = occupy.local_addr().unwrap().port();
    let (status, body) =
        common::push_rule_http(&http, "edge-01", busy_port, &echo_host, echo_port, Some(3));
    assert_eq!(
        status.as_u16(),
        422,
        "expected 422 on conflict, got {status}: {body}"
    );
    assert_eq!(
        body.pointer("/error/code")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "activation_failed"
    );
    drop(occupy);

    // ---- Scenario 4: push to an unconnected client ----
    let _ghost_bundle = common::provision_client_http(&http, "ghost");
    let (status, body) = common::push_rule_http(
        &http,
        "ghost",
        pick_free_port(),
        &echo_host,
        echo_port,
        Some(2),
    );
    assert_eq!(status.as_u16(), 422);
    assert_eq!(
        body.pointer("/error/code")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        "client_not_connected"
    );
    // Rule must not be stored.
    let ghost_rules = common::list_rules_http(&http, Some("ghost"));
    assert_eq!(ghost_rules.as_array().map(Vec::len), Some(0));

    // ---- Scenario 5: remove rule → listener stops within 1 s ----
    let rules = common::list_rules_http(&http, Some("edge-01"));
    let active = rules
        .as_array()
        .and_then(|arr| {
            arr.iter().find(|r| {
                r.pointer("/state/kind").and_then(|v| v.as_str()) == Some("active")
                    && r.get("listen_port").and_then(serde_json::Value::as_u64)
                        == Some(u64::from(listen_port))
            })
        })
        .unwrap_or_else(|| panic!("expected active rule for {listen_port} in {rules}"));
    let rule_id = active
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .expect("id");

    let remove_started = Instant::now();
    let status = common::remove_rule_http(&http, rule_id);
    assert_eq!(status.as_u16(), 204);

    // After remove, fresh connect attempts must be refused within 1 s.
    let stopped = common::wait_for(Duration::from_secs(1), || {
        TcpStream::connect_timeout(
            &(Ipv4Addr::LOCALHOST, listen_port).into(),
            Duration::from_millis(100),
        )
        .err()
        .map(|_| ())
    });
    assert!(
        stopped.is_some(),
        "listener still accepting >1s after remove (took {:?})",
        remove_started.elapsed()
    );
}
