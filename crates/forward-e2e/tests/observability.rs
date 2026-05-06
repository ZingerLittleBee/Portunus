//! T055-T057 — US3 observability acceptance.
//!
//! Walks the three US3 acceptance scenarios end-to-end:
//! 1. Push a rule, drive a known number of bytes, confirm `rule-stats`
//!    snapshot is within ±1 KB of the actual transfer (FR-018, SC-005).
//! 2. `GET /metrics` exposes the five Prometheus collectors named in T056
//!    with the expected label sets.
//! 3. Structured-log shape check: every event includes `event` + a timestamp
//!    plus the right id fields (`client_name` for client events, `rule_id`
//!    for rule events).
//!
//! These are wall-clock tests: they wait up to 10 s for a `StatsReport` (the
//! client emits one every 5 s by default) and assert tolerances against the
//! ±1 KB target from spec.md.

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::Duration;

use serde_json::Value;

/// Spawn an in-process echo server. Returns `(host, port)`.
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

/// Push a rule and drive `payload_size` bytes through it round-trip via the
/// echo target. Returns `(rule_id, listen_port)`.
fn push_and_drive(
    http: &str,
    listen_port: u16,
    echo_host: &str,
    echo_port: u16,
    payload_size: usize,
) -> u64 {
    let (status, body) =
        common::push_rule_http(http, "edge-01", listen_port, echo_host, echo_port, Some(2));
    assert!(status.is_success(), "push must succeed: {status} {body}");
    let rule_id = body
        .get("rule_id")
        .and_then(Value::as_u64)
        .expect("rule_id in push response");

    // Echo the payload — bytes_in == bytes_out == payload_size.
    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).expect("connect proxy");
    conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let payload: Vec<u8> = (0..payload_size)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect();
    conn.write_all(&payload).unwrap();
    conn.shutdown(std::net::Shutdown::Write).unwrap();
    let mut received = Vec::with_capacity(payload_size);
    conn.read_to_end(&mut received).unwrap();
    assert_eq!(received.len(), payload_size, "echo size mismatch");
    rule_id
}

/// T055: per-rule byte counters land within ±1KB tolerance after a 5 s
/// settle (client emits `StatsReport` on a 5 s interval by default).
#[test]
fn test_stats_within_tolerance() {
    let server = common::spawn_server(&[]);
    let (_grpc, http, _metrics) = server
        .wait_listening_full(Duration::from_secs(5))
        .expect("server listening");

    let (echo_host, echo_port) = spawn_echo();
    let bundle = common::provision_client_http(&http, "edge-01");
    let _client = common::spawn_client(&bundle, &[]);
    common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("edge-01")
                    && v.get("connected").and_then(Value::as_bool).unwrap_or(false)
            })
            .cloned()
    })
    .expect("client connects");

    let listen_port = pick_free_port();
    let payload_size: usize = 50_000;
    let rule_id = push_and_drive(&http, listen_port, &echo_host, echo_port, payload_size);

    // Wait up to 10 s (client emits every 5 s by default; pad for jitter).
    let snap = common::wait_for(Duration::from_secs(12), || {
        common::rule_stats_http(&http, rule_id)
    })
    .expect("rule-stats snapshot must arrive within 12s");

    let bytes_in = snap.get("bytes_in").and_then(Value::as_u64).unwrap();
    let bytes_out = snap.get("bytes_out").and_then(Value::as_u64).unwrap();
    let tolerance: u64 = 1024; // ±1 KB per FR-018 / SC-005
    let expected = payload_size as u64;
    assert!(
        bytes_in.abs_diff(expected) <= tolerance,
        "bytes_in {bytes_in} not within ±{tolerance} of {expected}"
    );
    assert!(
        bytes_out.abs_diff(expected) <= tolerance,
        "bytes_out {bytes_out} not within ±{tolerance} of {expected}"
    );
    assert_eq!(snap["client_name"], "edge-01");
}

/// T056: `/metrics` returns Prometheus text containing the five required
/// collectors. Auth-failure metric requires at least one bad-token attempt
/// to materialise (label-bearing counters don't render until first inc).
#[test]
fn test_prometheus_endpoint() {
    let server = common::spawn_server(&[]);
    let (_grpc, http, metrics) = server
        .wait_listening_full(Duration::from_secs(5))
        .expect("server listening");

    let (echo_host, echo_port) = spawn_echo();
    let bundle = common::provision_client_http(&http, "edge-01");
    let _client = common::spawn_client(&bundle, &[]);
    common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("edge-01")
                    && v.get("connected").and_then(Value::as_bool).unwrap_or(false)
            })
            .cloned()
    })
    .expect("client connects");

    // Trigger an auth failure so `forward_auth_failures_total` materialises
    // with a label. The bogus client connects with a mutated token.
    let bogus_path = server.config_dir.path().join("bogus.bundle.json");
    let mut bogus: Value =
        serde_json::from_str(&std::fs::read_to_string(&bundle).unwrap()).unwrap();
    bogus["client_name"] = Value::String("bogus".into());
    bogus["token"] = Value::String("Aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh-iii".into());
    std::fs::write(&bogus_path, serde_json::to_vec_pretty(&bogus).unwrap()).unwrap();
    let _bad = common::spawn_client(&bogus_path, &[]);

    // Push a rule + drive bytes so byte/active gauges materialise.
    let listen_port = pick_free_port();
    let _rule_id = push_and_drive(&http, listen_port, &echo_host, echo_port, 2_048);

    // Allow one StatsReport tick.
    let body = common::wait_for(Duration::from_secs(12), || {
        let body = common::fetch_metrics_text(&metrics);
        if body.contains("forward_rule_bytes_in_total{")
            && body.contains("forward_auth_failures_total{")
        {
            Some(body)
        } else {
            None
        }
    })
    .expect("/metrics must expose populated collectors within 12s");

    for needle in [
        "forward_clients_connected",
        "forward_auth_failures_total",
        "forward_rule_bytes_in_total",
        "forward_rule_bytes_out_total",
        "forward_rule_active_connections",
    ] {
        assert!(
            body.contains(needle),
            "/metrics missing {needle}; body:\n{body}"
        );
    }
}

/// T057: walk the three US3 acceptance scenarios + assert structured log
/// shape on representative events.
#[test]
fn test_user_story_3_acceptance() {
    let server = common::spawn_server(&[]);
    let (_grpc, http, metrics) = server
        .wait_listening_full(Duration::from_secs(5))
        .expect("server listening");

    let (echo_host, echo_port) = spawn_echo();
    let bundle = common::provision_client_http(&http, "edge-01");
    let _client = common::spawn_client(&bundle, &[]);
    common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("edge-01")
                    && v.get("connected").and_then(Value::as_bool).unwrap_or(false)
            })
            .cloned()
    })
    .expect("client connects");

    // ---- Scenario 1: rule-stats reports byte counters per rule ----
    let listen_port = pick_free_port();
    let payload_size: usize = 10_000;
    let rule_id = push_and_drive(&http, listen_port, &echo_host, echo_port, payload_size);
    let snap = common::wait_for(Duration::from_secs(12), || {
        common::rule_stats_http(&http, rule_id)
    })
    .expect("rule-stats snapshot must arrive");
    assert!(snap["bytes_in"].as_u64().unwrap() >= u64::try_from(payload_size).unwrap() - 1024);

    // ---- Scenario 2: /metrics exposes Prometheus collectors ----
    let body = common::fetch_metrics_text(&metrics);
    assert!(
        body.contains("forward_rule_bytes_in_total"),
        "metrics body: {body}"
    );
    assert!(
        body.contains("forward_clients_connected"),
        "metrics body: {body}"
    );

    // ---- Scenario 3: structured logs include required fields ----
    // Every line emitted to stderr should be valid JSON when not blank, and
    // representative events must carry their identity fields.
    let lines = server.stderr_lines.lock().unwrap().clone();
    let mut saw_connect = false;
    let mut saw_stats_report = false;
    let mut saw_rule_push = false;
    for line in &lines {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        // tracing-subscriber's JSON layer wraps the user fields under "fields"
        // and always carries a top-level "timestamp".
        assert!(
            v.get("timestamp").is_some(),
            "log line missing timestamp: {line}"
        );
        let Some(fields) = v.get("fields") else {
            continue;
        };
        let event = fields.get("event").and_then(Value::as_str).unwrap_or("");
        match event {
            "client.connected" => {
                assert!(
                    fields.get("client_name").is_some(),
                    "client.connected without client_name: {line}"
                );
                saw_connect = true;
            }
            "client.stats_report" => {
                assert!(
                    fields.get("client_name").is_some(),
                    "client.stats_report without client_name: {line}"
                );
                saw_stats_report = true;
            }
            "audit.rule_push" => {
                // We expect both "sent" + "activated" entries to carry rule_id.
                assert!(
                    fields.get("rule_id").is_some(),
                    "audit.rule_push without rule_id: {line}"
                );
                saw_rule_push = true;
            }
            _ => {}
        }
    }
    assert!(saw_connect, "expected at least one client.connected event");
    assert!(
        saw_stats_report,
        "expected at least one client.stats_report event after 5s"
    );
    assert!(saw_rule_push, "expected at least one audit.rule_push event");
}
