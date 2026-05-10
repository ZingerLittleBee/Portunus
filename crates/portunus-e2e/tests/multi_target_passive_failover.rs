//! End-to-end coverage for 007-multi-target-failover US1 (T018).
//!
//! Spawns `portunus-server` + `portunus-client`, pushes a 2-target rule
//! whose primary points at an unreachable port and whose secondary is a
//! live echo server, then opens a TCP connection to the rule's listen
//! port and asserts the bytes round-trip via the secondary.
//!
//! Acceptance scenarios from spec §US1:
//!   1. New connection succeeds via secondary within ≤1 connect retry.
//!   2. Bytes through the secondary are byte-equal to what was sent.
//!   3. The dial-failure on the primary increments
//!      `target_failovers_total` (asserted via the rule-stats HTTP
//!      surface — covered fully by Phase 5; here we only verify the
//!      data-plane bytes round-trip).

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

/// Process-wide serialisation — the e2e harness's port scans race when
/// run in parallel with other e2e tests.
fn test_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Spawn one TCP echo server bound to an OS-assigned port and return
/// `(host, port)`.
fn spawn_echo() -> (String, u16) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("echo bind");
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut sock = stream;
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
    ("127.0.0.1".to_string(), port)
}

/// Pick a free listen port and verify it stays free for one round-trip.
fn pick_listen_port() -> u16 {
    for _ in 0..50 {
        let probe = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("probe");
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        if let Ok(verify) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)) {
            drop(verify);
            return port;
        }
    }
    panic!("no free listen port after 50 attempts");
}

/// Pick a port that is currently UNBOUND but reserved long enough that
/// no other process should reasonably grab it before the rule push
/// completes. Used as the "unreachable primary" target — connects
/// against this port get `ECONNREFUSED` synchronously, which the
/// failover dial loop attributes as a connect-failure on target index
/// 0 and falls through to the secondary.
fn pick_unreachable_port() -> u16 {
    // Bind, capture the port, drop the listener immediately. The
    // OS may reassign it but the small window between drop and our
    // first connect attempt is tight enough in practice.
    let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("probe");
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    port
}

#[test]
#[allow(clippy::too_many_lines)]
fn passive_failover_to_secondary_when_primary_unreachable() {
    let _g = test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server should be listening");

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
    assert!(connected.is_some(), "edge-01 should connect within 5s");

    let (echo_host, secondary_port) = spawn_echo();
    let primary_port = pick_unreachable_port();
    let listen_port = pick_listen_port();

    let (status, body) = common::push_rule_http_targets(
        &http,
        "edge-01",
        listen_port,
        &[
            (echo_host.as_str(), primary_port),
            (echo_host.as_str(), secondary_port),
        ],
        None,
        Some(5),
    );
    assert!(
        status.is_success(),
        "multi-target push should succeed: {status} body={body}"
    );

    // The rule activated. Punch a connection through and confirm the
    // bytes echo back via the secondary. The first dial attempt
    // hits `primary_port` (refused — primary is unreachable) and the
    // failover loop falls through to `secondary_port`, where the echo
    // server round-trips the payload.
    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
        .unwrap_or_else(|e| panic!("connect to listen {listen_port}: {e}"));
    conn.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let payload = b"failover-works";
    conn.write_all(payload).unwrap();
    let mut buf = [0u8; 14];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(
        &buf, payload,
        "expected payload to echo back via secondary upstream"
    );

    drop(conn);
}
