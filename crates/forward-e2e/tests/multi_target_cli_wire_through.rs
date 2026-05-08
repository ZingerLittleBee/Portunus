//! End-to-end coverage for 007-multi-target-failover US4 (T040).
//!
//! Drives the operator surface from the OUTSIDE — via the `push-rule`
//! CLI binary — instead of POSTing JSON directly to `/v1/rules`. This
//! is the operator-facing equivalent of `multi_target_passive_failover`
//! and proves:
//!
//!   1. `forward-server push-rule … --target host:port --target host:port`
//!      lands a working multi-target rule on a connected client.
//!   2. The data plane bytes-round-trip via the secondary when the
//!      primary is unreachable (same acceptance scenario as US1, but
//!      via the CLI seam).
//!   3. The legacy positional form still works alongside the new flag
//!      form: pushing `push-rule edge-01 PORT host:port` produces a
//!      single-target rule whose data plane stays byte-identical to
//!      v0.6.0 (Constitution Principle II — verified structurally by
//!      pushing both shapes back-to-back without any code-path branch).

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

fn test_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn server_bin() -> std::path::PathBuf {
    common::workspace_bin("forward-server")
}

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

fn pick_unreachable_port() -> u16 {
    let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("probe");
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    port
}

#[test]
#[allow(clippy::too_many_lines)]
fn cli_push_rule_with_two_targets_lands_failover_route() {
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

    // Drive the push via the CLI binary — the same surface an operator
    // would use. Clap parses two `--target` flags into the repeating
    // arg vector; `rule_cli::push` assembles the multi-target body.
    let token = common::TEST_OPERATOR_TOKEN;
    let listen_str = listen_port.to_string();
    let primary = format!("{echo_host}:{primary_port}");
    let secondary = format!("{echo_host}:{secondary_port}");
    let out = Command::new(server_bin())
        .env("FORWARD_OPERATOR_TOKEN", token)
        .arg("push-rule")
        .arg("edge-01")
        .arg(&listen_str)
        .arg("--target")
        .arg(&primary)
        .arg("--target")
        .arg(&secondary)
        .arg("--http-endpoint")
        .arg(&http)
        .arg("--ack-timeout")
        .arg("5")
        .output()
        .expect("run push-rule CLI");
    assert!(
        out.status.success(),
        "push-rule CLI failed: status={:?} stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Bytes round-trip via the secondary — the primary is unreachable
    // so the dial loop falls through and the echo server replies.
    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
        .unwrap_or_else(|e| panic!("connect to listen {listen_port}: {e}"));
    conn.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let payload = b"cli-failover";
    conn.write_all(payload).unwrap();
    let mut buf = [0u8; 12];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(
        &buf, payload,
        "expected payload to echo back via secondary upstream"
    );

    drop(conn);
}

/// 007-multi-target-failover T040 (back-compat half): the legacy
/// positional `push-rule edge-01 PORT HOST:PORT` shape still works
/// alongside the new flag form. Single-target rules pushed this way
/// MUST stay byte-identical to v0.6.0 — verified structurally by the
/// fact that no `targets[]` is sent on the wire (no failover state
/// allocated client-side; see `forwarder::run` dispatch).
#[test]
fn cli_legacy_positional_target_still_works_alongside_multi_target() {
    let _g = test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server should be listening");

    let bundle = common::provision_client_http(&http, "edge-02");
    let _client = common::spawn_client(&bundle, &[]);

    let connected = common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("edge-02")
                    && v.get("connected").and_then(Value::as_bool).unwrap_or(false)
            })
            .cloned()
    });
    assert!(connected.is_some(), "edge-02 should connect within 5s");

    let (echo_host, echo_port) = spawn_echo();
    let listen_port = pick_listen_port();

    let token = common::TEST_OPERATOR_TOKEN;
    let listen_str = listen_port.to_string();
    let target = format!("{echo_host}:{echo_port}");
    let out = Command::new(server_bin())
        .env("FORWARD_OPERATOR_TOKEN", token)
        .arg("push-rule")
        .arg("edge-02")
        .arg(&listen_str)
        .arg(&target) // legacy positional — exercises the v0.6.0 hot path
        .arg("--http-endpoint")
        .arg(&http)
        .arg("--ack-timeout")
        .arg("5")
        .output()
        .expect("run push-rule CLI");
    assert!(
        out.status.success(),
        "legacy push-rule CLI failed: status={:?} stdout={} stderr={}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
        .unwrap_or_else(|e| panic!("connect to listen {listen_port}: {e}"));
    conn.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let payload = b"legacy-path";
    conn.write_all(payload).unwrap();
    let mut buf = [0u8; 11];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, payload, "legacy single-target round-trip MUST work");
    drop(conn);
}
