//! T015 (003-domain-name-forward) — US1 e2e smoke for DNS-target rules.
//!
//! Spins up `forward-server` + `forward-client`, pushes one rule with a
//! DNS hostname target and one rule with an IP-literal target, then
//! drives bytes through both. Goals:
//!   - DNS rule round-trips identically to an IP rule (FR-002 / Constitution II).
//!   - IP rule remains byte-identical to the v0.2.0 hot path (additive
//!     guarantee from `plan.md`).
//!
//! Hostname: we use `localhost`, which every Unix box resolves to
//! 127.0.0.1 via `/etc/hosts` without network access. That lets us
//! exercise the full client-side `Hostname → Target::Dns → cache →
//! hickory → dial` chain in US1, before T024's hosts-file/mini-resolver
//! injection lands in US2.

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::Duration;

use serde_json::Value;

fn spawn_echo() -> (Ipv4Addr, u16) {
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
    (Ipv4Addr::LOCALHOST, addr.port())
}

fn pick_free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind ephemeral")
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn test_dns_us1_happy_path() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server listening");

    let bundle = common::provision_client_http(&http, "edge-01");
    let client = common::spawn_client(&bundle, &[]);

    // Wait for the client to register as connected.
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
    assert!(connected.is_some(), "edge-01 must connect within 5s");

    // ---- DNS-target rule ----
    let (_echo_ip, echo_port) = spawn_echo();
    let dns_listen = pick_free_port();
    let (status, body) = common::push_rule_http(
        &http,
        "edge-01",
        dns_listen,
        // `localhost` MUST classify as Target::Dns and round-trip
        // through cache + hickory; system resolver maps it to 127.0.0.1.
        "localhost",
        echo_port,
        Some(3),
    );
    assert!(
        status.is_success(),
        "DNS-target push must succeed: status={status} body={body}"
    );

    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, dns_listen))
        .expect("connect to DNS-target proxy");
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(b"dns-roundtrip").unwrap();
    let mut buf = [0u8; 13];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"dns-roundtrip", "DNS-target round-trip mismatch");
    drop(conn);

    // ---- IP-target rule (regression guard for v0.2.0 byte-identical path) ----
    let (_echo_ip2, echo_port2) = spawn_echo();
    let ip_listen = pick_free_port();
    let (status, body) = common::push_rule_http(
        &http,
        "edge-01",
        ip_listen,
        "127.0.0.1",
        echo_port2,
        Some(3),
    );
    assert!(
        status.is_success(),
        "IP-target push must succeed: status={status} body={body}"
    );

    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, ip_listen))
        .expect("connect to IP-target proxy");
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(b"ip-roundtrip-still-works").unwrap();
    let mut buf = [0u8; 24];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(
        &buf, b"ip-roundtrip-still-works",
        "IP-target round-trip mismatch — v0.2.0 byte path regressed"
    );
}
