//! 013-traffic-quotas (v1.4.0) — end-to-end coverage for the hard-kill
//! enforcement path + recovery via `PATCH clear_period_usage`.
//!
//! Walks the minimum viable acceptance flow:
//!
//!     1. spawn server + connect a real client (v1.4 reports Hello version).
//!     2. as superadmin (the `_legacy` operator_token shortcut), create
//!        `alice` + a credential + a grant for `edge-test, 30000..30005, tcp`.
//!     3. PUT a 1 MiB monthly quota for (alice, edge-test). Server pushes
//!        a `TrafficQuotaUpdate{SET}` to the client, which installs the
//!        per-(user, client) `QuotaHandle` in its scope manager.
//!     4. As `alice` (her bearer token), push a TCP rule
//!        `listen=30000 -> 127.0.0.1:<echo>`. Rule activates with
//!        `owner_user_id = alice`. The rule install path looks up the
//!        already-installed quota handle by owner_id and wires it into
//!        the forwarder copy hook.
//!     5. Drive ~512 KiB through the rule — the bytes must flow.
//!     6. Drive another ~2 MiB through. The copy hook saturates the
//!        budget mid-stream; the connection closes with fewer bytes
//!        delivered. The `GET /quotas/{c}/status` response MUST eventually
//!        report `exhausted: true` (server-side observation lags by one
//!        5 s StatsReport tick).
//!     7. `PATCH clear_period_usage = true`. Server clears `bytes_used`,
//!        `exhausted_at = None`, and re-pushes `TrafficQuotaUpdate{SET}`
//!        with fresh budget. A new connection through the rule MUST
//!        succeed again.
//!
//! The other three scenarios in
//! `docs/superpowers/plans/2026-05-14-traffic-quotas-and-history.md`
//! Task H1 (time-travel rollover, replay-order capture, sample
//! coverage) need helpers that don't exist in `common/mod.rs` yet —
//! deferred.
//
// TODO(v1.5): period_rollover_advances_period — requires a server-side
// manual rollover trigger or a clock-injection seam.
// TODO(v1.5): reconnect_replay_order_quota_before_rule — needs a wire-
// stream-capturing client harness.
// TODO(v1.5): traffic_samples_appear_for_all_pairs — needs the
// `/v1/users/{u}/traffic` query plumbed through `common/`.

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};

const SUPER: &str = common::TEST_OPERATOR_TOKEN;

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::new()
}

fn post(addr: &str, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let url = format!("http://{addr}{path}");
    let resp = http()
        .post(&url)
        .header("Authorization", bearer(token))
        .json(&body)
        .send()
        .expect("POST send");
    let status = resp.status();
    let v = resp.json().unwrap_or(Value::Null);
    (status, v)
}

fn put(addr: &str, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let url = format!("http://{addr}{path}");
    let resp = http()
        .put(&url)
        .header("Authorization", bearer(token))
        .json(&body)
        .send()
        .expect("PUT send");
    let status = resp.status();
    let v = resp.json().unwrap_or(Value::Null);
    (status, v)
}

fn patch(addr: &str, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let url = format!("http://{addr}{path}");
    let resp = http()
        .patch(&url)
        .header("Authorization", bearer(token))
        .json(&body)
        .send()
        .expect("PATCH send");
    let status = resp.status();
    let v = resp.json().unwrap_or(Value::Null);
    (status, v)
}

fn get(addr: &str, path: &str, token: &str) -> (StatusCode, Value) {
    let url = format!("http://{addr}{path}");
    let resp = http()
        .get(&url)
        .header("Authorization", bearer(token))
        .send()
        .expect("GET send");
    let status = resp.status();
    let v = resp.json().unwrap_or(Value::Null);
    (status, v)
}

/// Spawn a tiny in-process TCP echo server and return its `(host, port)`.
fn spawn_echo() -> (String, u16) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind echo");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for incoming in listener.incoming().flatten() {
            std::thread::spawn(move || {
                let mut sock = incoming;
                let mut buf = vec![0u8; 65_536];
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

/// Open a TCP connection through the proxy and try to push `bytes` of
/// payload, then read back whatever the echo returns until EOF or
/// `timeout` elapses. Returns the bytes successfully echoed back.
///
/// Used for both halves of the test: when budget is healthy the full
/// payload should round-trip; when budget straddles zero the echo
/// stops short and the proxy half-closes.
fn drive_through(listen_port: u16, bytes: usize, timeout: Duration) -> Vec<u8> {
    let mut stream =
        TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).expect("connect to proxy listener");
    stream.set_read_timeout(Some(timeout)).unwrap();
    stream.set_write_timeout(Some(timeout)).unwrap();

    // Build a payload with a recognisable pattern so we can spot
    // misroutes (not strictly necessary here but cheap insurance).
    let payload: Vec<u8> = (0..bytes)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect();

    // Write in a dedicated thread so we don't deadlock on a tiny
    // socket buffer when the echo gets cut off mid-stream.
    let writer_stream = stream.try_clone().expect("clone stream");
    let payload_for_writer = payload.clone();
    let writer = std::thread::spawn(move || {
        let mut w = writer_stream;
        // write_all can fail mid-way once the proxy half-closes — that
        // is expected for the over-cap leg.
        let _ = w.write_all(&payload_for_writer);
        let _ = w.shutdown(std::net::Shutdown::Write);
    });

    let mut received: Vec<u8> = Vec::with_capacity(bytes);
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        // Both EOF (Ok(0)) and timeout/reset (Err) mean "we got all we
        // are going to get"; merge the arms to satisfy clippy and keep
        // the truncation-friendly semantics.
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        received.extend_from_slice(&buf[..n]);
        if received.len() >= bytes {
            break;
        }
    }
    let _ = writer.join();
    let _ = stream.shutdown(std::net::Shutdown::Both);
    received
}

#[test]
#[allow(clippy::too_many_lines)]
fn quota_hard_kill_then_recovery_via_reset() {
    // ----- 1. Server + connected client -----
    let server = common::spawn_server(&[]);
    let (_grpc, http_addr) = server
        .wait_listening(Duration::from_secs(10))
        .expect("server listening");

    let bundle = common::provision_client_http(&http_addr, "edge-test");
    let client_handle = common::spawn_client(&bundle, &[]);

    // Wait for the client to register as connected — only then does the
    // server know its Hello.client_version and accept quota PUTs.
    let connected = common::wait_for(Duration::from_secs(10), || {
        let arr = common::list_clients_http(&http_addr);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("edge-test")
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
        for l in client_handle.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
    }
    assert!(connected.is_some(), "edge-test must connect within 10s");

    // 015-client-stable-id: the quota surface addresses clients by their
    // stable id. Resolve it once now that the client is provisioned.
    let edge_id = common::client_id_for_name(&http_addr, "edge-test");
    let quota_path = format!("/v1/users/alice/quotas/{edge_id}");
    let quota_status_path = format!("/v1/users/alice/quotas/{edge_id}/status");

    // ----- 2. Create alice + grant -----
    let (st, body) = post(
        &http_addr,
        "/v1/users",
        SUPER,
        json!({"user_id": "alice", "display_name": "Alice"}),
    );
    assert_eq!(st, StatusCode::CREATED, "user-add alice; body={body}");

    let (st, body) = post(
        &http_addr,
        "/v1/users/alice/credentials",
        SUPER,
        json!({"label": "laptop"}),
    );
    assert_eq!(st, StatusCode::CREATED, "credential issue; body={body}");
    let alice_token = body["token"]
        .as_str()
        .expect("alice token in body")
        .to_string();

    // Free a single port for the proxy listener. We pick + drop so the
    // ephemeral port goes back to the kernel; the client re-binds it
    // when the rule activates.
    let listen_port = {
        let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("probe bind");
        let p = probe.local_addr().unwrap().port();
        drop(probe);
        p
    };

    let (st, body) = post(
        &http_addr,
        "/v1/grants",
        SUPER,
        json!({
            "user_id": "alice",
            "client": "edge-test",
            "listen_port_start": listen_port,
            "listen_port_end": listen_port,
            "protocols": ["tcp"],
        }),
    );
    assert_eq!(st, StatusCode::CREATED, "grant-add; body={body}");

    // ----- 3. PUT a 1 MiB monthly quota BEFORE pushing the rule -----
    //
    // Replay order (C5) says quotas land before rules on reconnect; the
    // live install order (D3) is symmetric — pushing the quota first
    // lets the rule's quota lookup at install time find a handle and
    // wire it into the forwarder copy hook.
    let quota_cap: i64 = 1024 * 1024; // 1 MiB
    let (st, body) = put(
        &http_addr,
        &quota_path,
        SUPER,
        json!({"monthly_bytes": quota_cap}),
    );
    assert_eq!(st, StatusCode::OK, "PUT quota; body={body}");
    assert_eq!(body["monthly_bytes"].as_i64(), Some(quota_cap));
    assert_eq!(body["exhausted"].as_bool(), Some(false));

    // Give the client a beat to apply the TrafficQuotaUpdate before the
    // rule install runs its quota_scope.lookup().
    std::thread::sleep(Duration::from_millis(300));

    // ----- 4. As alice, push the TCP rule (owner=alice) -----
    let (echo_host, echo_port) = spawn_echo();
    let (st, body) = post(
        &http_addr,
        "/v1/rules",
        &alice_token,
        json!({
            "client": "edge-test",
            "listen_port": listen_port,
            "target_host": &echo_host,
            "target_port": echo_port,
            "protocol": "tcp",
        }),
    );
    assert!(st.is_success(), "push rule as alice; got {st} body={body}");
    let rule_id = body
        .get("rule_id")
        .and_then(Value::as_u64)
        .expect("rule_id");

    // Tiny settle for the listener to be readable.
    std::thread::sleep(Duration::from_millis(200));

    // ----- 5. Under-budget transfer: 512 KiB round-trips intact -----
    let under_budget: usize = 512 * 1024;
    let received = drive_through(listen_port, under_budget, Duration::from_secs(8));
    assert_eq!(
        received.len(),
        under_budget,
        "under-budget transfer must round-trip in full; got {} of {under_budget}",
        received.len()
    );

    // ----- 6. Over-budget transfer: budget straddles, copy half-closes -----
    //
    // We push ~2 MiB; the quota cap is 1 MiB, and 512 KiB already
    // flowed, so the budget can deliver at most ~512 KiB more in each
    // direction before the consume hook returns Exhausted and the
    // copy_one_dir loop shuts the writer half. The end-user read side
    // will then drain whatever's in flight and observe EOF.
    let over_budget: usize = 2 * 1024 * 1024;
    let received = drive_through(listen_port, over_budget, Duration::from_secs(8));
    assert!(
        received.len() < over_budget,
        "over-budget transfer MUST be truncated; got {} of {over_budget}",
        received.len()
    );

    // ----- 6b. Server-side status reflects exhaustion within one tick -----
    //
    // The server learns of exhaustion via the 5 s StatsReport tick from
    // the client + the TrafficAggregator.record() codepath. Poll the
    // status endpoint with a generous wall clock to absorb scheduling
    // jitter on slow CI.
    let exhausted = common::wait_for(Duration::from_secs(15), || {
        let (st, body) = get(&http_addr, &quota_status_path, SUPER);
        if !st.is_success() {
            return None;
        }
        if body.get("exhausted").and_then(Value::as_bool) == Some(true)
            && body.get("exhausted_at").and_then(Value::as_i64).is_some()
        {
            return Some(body);
        }
        None
    });
    if exhausted.is_none() {
        eprintln!("--- server stderr (looking for traffic_quota.exhausted) ---");
        for l in server.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
        eprintln!("--- client stderr ---");
        for l in client_handle.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
    }
    let exhausted = exhausted.expect(
        "GET /quotas/.../status must report exhausted=true within 15s after over-budget transfer",
    );
    assert!(
        exhausted["current_period_bytes_used"].as_i64().unwrap_or(0) >= quota_cap,
        "bytes_used must have reached the cap; got {exhausted}"
    );

    // ----- 7. PATCH clear_period_usage and verify recovery -----
    let (st, body) = patch(
        &http_addr,
        &quota_path,
        SUPER,
        json!({"clear_period_usage": true}),
    );
    assert_eq!(st, StatusCode::OK, "PATCH clear; body={body}");
    assert_eq!(body["exhausted"].as_bool(), Some(false));
    assert_eq!(body["current_period_bytes_used"].as_i64(), Some(0));

    // Let the client process the re-pushed TrafficQuotaUpdate (SET with
    // fresh budget).
    std::thread::sleep(Duration::from_millis(500));

    // New connection through the rule. With the budget freshly cleared
    // a small transfer must round-trip again.
    let recovery_payload: usize = 256 * 1024;
    let received = drive_through(listen_port, recovery_payload, Duration::from_secs(8));
    assert_eq!(
        received.len(),
        recovery_payload,
        "post-recovery transfer must round-trip; got {} of {recovery_payload}",
        received.len()
    );

    // Sanity: server still reports a sane (non-exhausted) snapshot;
    // bytes_used has begun accumulating again.
    let (st, body) = get(&http_addr, &quota_status_path, SUPER);
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        body["exhausted"].as_bool(),
        Some(false),
        "post-clear status must NOT be exhausted; body={body}"
    );
    assert_eq!(
        body["monthly_bytes"].as_i64(),
        Some(quota_cap),
        "monthly cap preserved across clear; body={body}"
    );
    // Rule still present.
    let rules = common::list_rules_http(&http_addr, Some("edge-test"));
    let still_active = rules.as_array().is_some_and(|arr| {
        arr.iter()
            .any(|r| r.get("id").and_then(Value::as_u64) == Some(rule_id))
    });
    assert!(still_active, "rule {rule_id} must still be listed");
}
