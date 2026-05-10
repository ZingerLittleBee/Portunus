//! T026 (004-udp-forward) — US1 e2e smoke for single-port UDP rules.
//!
//! Spins up `portunus-server` + `portunus-client`, pushes one UDP rule
//! with an IP-literal target, and drives a UDP datagram through it.
//! The complete loop is:
//!     end-user UDP socket → server-side push-rule (HTTP)
//!         → portunus-client (UdpListener)
//!             → upstream UDP echo
//!                 → reply back via per-flow upstream socket
//!                     → end-user UDP socket
//!
//! Goals:
//!   * round-trip MUST be byte-identical (FR-002 / Constitution II);
//!   * the response body MUST carry `protocol: "udp"` (T021);
//!   * `rule-stats` JSON MUST report `protocol: "udp"` and the
//!     `datagrams_*` / `active_flows` counters (T040);
//!   * `/metrics` MUST emit exactly one row of
//!     `portunus_rule_udp_datagrams_in_total` for the rule (SC-004).

mod common;

use std::net::{Ipv4Addr, UdpSocket};
use std::time::Duration;

use serde_json::Value;

fn spawn_udp_echo() -> (Ipv4Addr, u16) {
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind echo");
    let port = sock.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let Ok((n, peer)) = sock.recv_from(&mut buf) else {
                break;
            };
            let _ = sock.send_to(&buf[..n], peer);
        }
    });
    (Ipv4Addr::LOCALHOST, port)
}

fn pick_free_udp_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind ephemeral")
        .local_addr()
        .unwrap()
        .port()
}

/// 004-udp-forward T049 helper: bind `n` consecutive UDP ports on
/// `0.0.0.0` and return the held probe sockets. The first port is the
/// range start. Caller drops the probes immediately before letting the
/// rule's listener re-bind. Mirrors the TCP `pick_consecutive_free`
/// pattern but on UDP so we can race-test parallel UDP listeners.
fn pick_consecutive_free_udp(n: u16) -> u16 {
    'outer: for _ in 0..50 {
        let Ok(probe) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
            continue;
        };
        let start = probe.local_addr().unwrap().port();
        if u32::from(start) + u32::from(n) > 65_536 {
            drop(probe);
            continue;
        }
        let mut probes: Vec<UdpSocket> = vec![probe];
        for offset in 1..n {
            if let Ok(s) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, start + offset)) {
                probes.push(s);
            } else {
                drop(probes);
                continue 'outer;
            }
        }
        drop(probes);
        return start;
    }
    panic!("could not find {n} consecutive free UDP ports after 50 attempts");
}

#[test]
#[allow(clippy::too_many_lines)]
fn test_udp_us1_happy_path() {
    let server = common::spawn_server(&[]);
    let (_grpc, http, metrics_addr) = server
        .wait_listening_full(Duration::from_secs(5))
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

    let (_echo_ip, echo_port) = spawn_udp_echo();
    let udp_listen = pick_free_udp_port();
    let (status, body) = common::push_rule_http_with_protocol(
        &http,
        "edge-01",
        udp_listen,
        "127.0.0.1",
        echo_port,
        "udp",
        Some(3),
    );
    if !status.is_success() {
        eprintln!("--- server stderr ---");
        for l in server.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
    }
    assert!(
        status.is_success(),
        "UDP push must succeed; got {status} body={body}"
    );
    // T021: response echoes the activated protocol so generic operator
    // tooling can rely on the field's presence.
    assert_eq!(
        body.get("protocol").and_then(|v| v.as_str()),
        Some("udp"),
        "PushRuleResponse.protocol must be 'udp'; body={body}"
    );
    let rule_id = body
        .get("rule_id")
        .and_then(serde_json::Value::as_u64)
        .expect("rule_id present");

    // Allow the client a moment to bind the listener.
    std::thread::sleep(Duration::from_millis(200));

    // Drive a datagram through the proxy.
    let user = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind user");
    user.set_read_timeout(Some(Duration::from_secs(3)))
        .expect("set timeout");
    let payload = b"hello-udp-e2e";
    user.send_to(payload, (Ipv4Addr::LOCALHOST, udp_listen))
        .expect("send to proxy");
    let mut buf = [0u8; 64];
    let (n, _from) = user.recv_from(&mut buf).expect("recv reply");
    assert_eq!(&buf[..n], payload, "UDP round-trip must be byte-equal");

    // Wait for one StatsReport tick (default 5s) so the server picks up
    // the per-rule UDP counters.
    let snap = common::wait_for(Duration::from_secs(8), || {
        let url = format!("http://{http}/v1/rules/{rule_id}/stats");
        let resp = reqwest::blocking::Client::new()
            .get(&url)
            .bearer_auth(common::TEST_OPERATOR_TOKEN)
            .send()
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().ok()?;
        let dgin = body
            .get("datagrams_in")
            .and_then(serde_json::Value::as_u64)?;
        if dgin >= 1 { Some(body) } else { None }
    });
    let snap = snap.expect("stats must report at least 1 datagram_in within 8s");

    // T040: protocol field surfaces; UDP counters present and non-zero.
    assert_eq!(
        snap.get("protocol").and_then(|v| v.as_str()),
        Some("udp"),
        "stats.protocol must be 'udp'; got {snap}"
    );
    assert!(
        snap.get("datagrams_in")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            >= 1,
        "datagrams_in must advance; got {snap}"
    );
    assert!(
        snap.get("datagrams_out")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            >= 1,
        "datagrams_out must advance; got {snap}"
    );

    // SC-004: exactly one row per rule for the UDP collectors. Fetch
    // /metrics directly and grep for the rule's label.
    let metrics_body = common::fetch_metrics_text(&metrics_addr);
    let pat = format!(
        "portunus_rule_udp_datagrams_in_total{{client=\"edge-01\",owner=\"_legacy\",rule=\"{rule_id}\"}}"
    );
    let matching = metrics_body.lines().filter(|l| l.starts_with(&pat)).count();
    assert_eq!(
        matching, 1,
        "SC-004: exactly one row per rule expected; got {matching} in:\n{metrics_body}"
    );
}

/// T026a (SC-003 hard-zero isolation under concurrent load).
///
/// Spawns N=1000 distinct end-user UDP sockets, each sending a unique
/// 8-byte payload (the source's local port encoded as a u32, repeated)
/// through a single UDP rule. Each socket asserts it receives back
/// EXACTLY its own payload — any cross-routing fails the test.
///
/// Why 1000: matches the per-rule `udp_max_flows_per_rule` default
/// (1024) so every flow fits comfortably inside the table cap. A
/// future tightening of the cap would have to size this test down.
///
/// `#[ignore]` on systems where the default RLIMIT_NOFILE makes 1000+
/// sockets unreliable (macOS GH-Actions in particular). Linux CI
/// runners with the standard 1024 ulimit comfortably handle this with
/// `ulimit -n 4096` or similar; the harness sets the soft limit at
/// process start where it can.
#[test]
#[ignore = "T026a: gated behind a higher RLIMIT_NOFILE; run explicitly with `cargo test --test udp_smoke -- --ignored`"]
fn test_udp_us1_thousand_source_isolation() {
    let server = common::spawn_server(&[]);
    let (_grpc, http, _metrics_addr) = server
        .wait_listening_full(Duration::from_secs(5))
        .expect("server listening");

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
    assert!(connected.is_some(), "client must connect within 5s");

    let (_echo_ip, echo_port) = spawn_udp_echo();
    let listen = pick_free_udp_port();
    let (status, _body) = common::push_rule_http_with_protocol(
        &http,
        "edge-01",
        listen,
        "127.0.0.1",
        echo_port,
        "udp",
        Some(3),
    );
    assert!(status.is_success(), "UDP push must succeed; got {status}");

    std::thread::sleep(Duration::from_millis(200));

    const N: usize = 1000;
    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        handles.push(std::thread::spawn(move || {
            let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind user");
            sock.set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set timeout");
            let port = sock.local_addr().unwrap().port();
            // Encode the local port as a payload — every source's
            // payload is unique by construction, so any reply that
            // matches a different source's payload is a misroute.
            let payload = port.to_be_bytes();
            let mut wide = Vec::with_capacity(8);
            wide.extend_from_slice(&payload);
            wide.extend_from_slice(&payload);
            wide.extend_from_slice(&payload);
            wide.extend_from_slice(&payload);
            sock.send_to(&wide, (Ipv4Addr::LOCALHOST, listen))
                .expect("send to proxy");
            let mut buf = [0u8; 16];
            let (n, _from) = sock.recv_from(&mut buf).expect("recv reply");
            assert_eq!(
                &buf[..n],
                wide.as_slice(),
                "SC-003 misroute: source port {port} got someone else's payload"
            );
        }));
    }

    let mut failures = 0_u32;
    for h in handles {
        if h.join().is_err() {
            failures += 1;
        }
    }
    assert_eq!(
        failures, 0,
        "{failures} of {N} flows misrouted or timed out"
    );
}

/// T043 (US2) — DNS-target UDP rule round-trip + DNS-failure
/// classification.
///
/// Two halves:
///   1. **Happy path**: push a UDP rule with target `localhost`. The
///      client resolves through the shared `LiveResolver`, binds a
///      per-flow upstream, and round-trips a datagram byte-equal.
///   2. **Failure path**: push a second UDP rule with target
///      `broken.invalid` (RFC 6761 §6.4 — guaranteed NXDOMAIN). Sending
///      a datagram MUST drop it without round-trip; the rule MUST stay
///      `active` (FR-012); the per-rule `dns_failures` counter MUST
///      advance via `/metrics`.
///
/// Skipped on hosts whose resolver hijacks NXDOMAIN for `.invalid` —
/// same gate as `dns_smoke::test_dns_us2_failure_active_rule_and_event`.
fn dns_hijack_detected() -> bool {
    use std::net::ToSocketAddrs;
    "broken.invalid:443"
        .to_socket_addrs()
        .map(|mut iter| iter.next().is_some())
        .unwrap_or(false)
}

#[test]
#[allow(clippy::too_many_lines)]
fn test_udp_us2_dns_target() {
    let server = common::spawn_server(&[]);
    let (_grpc, http, metrics_addr) = server
        .wait_listening_full(Duration::from_secs(5))
        .expect("server listening");

    let bundle = common::provision_client_http(&http, "edge-01");
    let client = common::spawn_client(&bundle, &[]);

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

    // ---- Happy path: localhost target ----
    let (_echo_ip, echo_port) = spawn_udp_echo();
    let dns_listen = pick_free_udp_port();
    let (status, body) = common::push_rule_http_with_protocol(
        &http,
        "edge-01",
        dns_listen,
        // localhost MUST classify as Target::Dns and round-trip through
        // cache + resolver; system resolver maps it to 127.0.0.1.
        "localhost",
        echo_port,
        "udp",
        Some(3),
    );
    assert!(
        status.is_success(),
        "DNS-target UDP push must succeed; got {status} body={body}"
    );
    assert_eq!(
        body.get("protocol").and_then(|v| v.as_str()),
        Some("udp"),
        "PushRuleResponse.protocol must echo udp; body={body}"
    );

    // Allow the client a moment to bind the listener.
    std::thread::sleep(Duration::from_millis(200));

    let user = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind user");
    user.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let payload = b"hello-udp-dns";
    user.send_to(payload, (Ipv4Addr::LOCALHOST, dns_listen))
        .expect("send to dns proxy");
    let mut buf = [0u8; 64];
    let (n, _from) = user.recv_from(&mut buf).expect("recv reply");
    assert_eq!(
        &buf[..n],
        payload,
        "DNS-target UDP round-trip must be byte-equal"
    );

    // ---- Failure path: broken.invalid ----
    if dns_hijack_detected() {
        eprintln!(
            "skipping NXDOMAIN half: local resolver hijacks .invalid — covered \
             by unit tests forwarder::udp::flow::tests::build_flow_dns_*"
        );
        return;
    }

    let bad_listen = pick_free_udp_port();
    let (status, body) = common::push_rule_http_with_protocol(
        &http,
        "edge-01",
        bad_listen,
        // broken.invalid is RFC 6761 §6.4 NXDOMAIN-guaranteed.
        "broken.invalid",
        9999,
        "udp",
        Some(3),
    );
    assert!(
        status.is_success(),
        "UDP push for unresolvable target MUST succeed (rule stays Active per FR-012); got {status} body={body}"
    );
    let bad_rule_id = body
        .get("rule_id")
        .and_then(serde_json::Value::as_u64)
        .expect("bad rule_id present");

    std::thread::sleep(Duration::from_millis(200));

    // Drive a datagram into the broken rule. The client-side resolver
    // returns NXDOMAIN → bumps dns_failures → drops the datagram. The
    // user socket MUST receive nothing (timeout) — recovery would
    // require the resolver to start working.
    let bad_user = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind user");
    bad_user
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    bad_user
        .send_to(b"unreachable", (Ipv4Addr::LOCALHOST, bad_listen))
        .expect("send to broken proxy");
    let mut bad_buf = [0u8; 64];
    let recv = bad_user.recv_from(&mut bad_buf);
    assert!(
        recv.is_err(),
        "datagram to NXDOMAIN-target MUST be dropped (no echo); got {recv:?}"
    );

    // Rule MUST remain Active (FR-012).
    let rules = common::list_rules_http(&http, Some("edge-01"));
    let bad_state = rules
        .as_array()
        .and_then(|arr| {
            arr.iter().find(|r| {
                r.get("listen_port").and_then(Value::as_u64) == Some(u64::from(bad_listen))
            })
        })
        .expect("bad rule listed");
    let kind = bad_state
        .pointer("/state/kind")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert_eq!(
        kind, "active",
        "DNS-failing UDP rule MUST stay Active (FR-012); got state={kind}"
    );

    // dns_failures counter MUST advance via /metrics. Poll because
    // StatsReport ticks at 5s and the per-rule label is set on the
    // first observe() call.
    let saw_failure = common::wait_for(Duration::from_secs(8), || {
        let body = common::fetch_metrics_text(&metrics_addr);
        let pat = format!(
            "portunus_rule_dns_failures_total{{client=\"edge-01\",owner=\"_legacy\",rule=\"{bad_rule_id}\"}}"
        );
        body.lines().find_map(|l| {
            if !l.starts_with(&pat) {
                return None;
            }
            // Last whitespace-separated token is the value.
            let n: u64 = l.split_whitespace().last()?.parse().ok()?;
            (n >= 1).then_some(n)
        })
    });
    if saw_failure.is_none() {
        eprintln!("--- client stderr (looking for dns_failed) ---");
        for l in client.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
        eprintln!("--- /metrics ---");
        eprintln!("{}", common::fetch_metrics_text(&metrics_addr));
    }
    assert!(
        saw_failure.is_some(),
        "FR-008: dns_failures counter MUST advance for the broken UDP rule within 8s"
    );
}

// ---- US3: UDP port-range rules ----

/// Spawn a UDP echo bound to the given port and return its local addr.
/// Used to set up a contiguous block of upstream echoes for range tests.
fn spawn_udp_echo_on(port: u16) {
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, port)).expect("bind echo on port");
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let Ok((n, peer)) = sock.recv_from(&mut buf) else {
                break;
            };
            let _ = sock.send_to(&buf[..n], peer);
        }
    });
}

/// T049: 10-port UDP range, one datagram per port; each MUST land at
/// the same-offset upstream and round-trip byte-equal.
#[test]
fn test_udp_us3_range_round_trip() {
    let server = common::spawn_server(&[]);
    let (_grpc, http, _metrics_addr) = server
        .wait_listening_full(Duration::from_secs(5))
        .expect("server listening");

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
    assert!(connected.is_some(), "client must connect within 5s");

    // Stand up echoes on a contiguous block of upstream ports.
    let target_start = pick_consecutive_free_udp(10);
    let target_end = target_start + 9;
    for p in target_start..=target_end {
        spawn_udp_echo_on(p);
    }

    let listen_start = pick_consecutive_free_udp(10);
    let listen_end = listen_start + 9;
    let (status, body) = common::push_rule_http_range_with_protocol(
        &http,
        "edge-01",
        listen_start,
        listen_end,
        "127.0.0.1",
        target_start,
        target_end,
        "udp",
        Some(3),
    );
    assert!(
        status.is_success(),
        "UDP range push must succeed; got {status} body={body}"
    );

    // Allow the client a moment to bind every listener.
    std::thread::sleep(Duration::from_millis(300));

    // One datagram per port, distinct payload per offset to detect cross-routing.
    for offset in 0..10u16 {
        let listen_port = listen_start + offset;
        let user = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind user");
        user.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let off_be = offset.to_be_bytes();
        let payload: [u8; 4] = [0xCA, 0xFE, off_be[0], off_be[1]];
        user.send_to(&payload, (Ipv4Addr::LOCALHOST, listen_port))
            .expect("send to range proxy");
        let mut buf = [0u8; 16];
        let (n, _from) = user.recv_from(&mut buf).expect("recv reply");
        assert_eq!(
            &buf[..n],
            &payload,
            "offset={offset} listen={listen_port} round-trip must be byte-equal"
        );
    }
}

/// T050: same setup as T049, then assert `/v1/rules/<id>/stats?per_port=true`
/// returns one entry per port with non-zero datagram + byte counters
/// for the ports actually exercised.
#[test]
fn test_udp_us3_per_port_stats() {
    let server = common::spawn_server(&[]);
    let (_grpc, http, _metrics_addr) = server
        .wait_listening_full(Duration::from_secs(5))
        .expect("server listening");

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
    assert!(connected.is_some(), "client must connect within 5s");

    let target_start = pick_consecutive_free_udp(10);
    for p in target_start..target_start + 10 {
        spawn_udp_echo_on(p);
    }
    let listen_start = pick_consecutive_free_udp(10);
    let (status, body) = common::push_rule_http_range_with_protocol(
        &http,
        "edge-01",
        listen_start,
        listen_start + 9,
        "127.0.0.1",
        target_start,
        target_start + 9,
        "udp",
        Some(3),
    );
    assert!(status.is_success(), "push: {status} body={body}");
    let rule_id = body
        .get("rule_id")
        .and_then(serde_json::Value::as_u64)
        .expect("rule_id present");

    std::thread::sleep(Duration::from_millis(300));

    // Drive at least one datagram through every port.
    for offset in 0..10u16 {
        let user = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind user");
        user.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let payload = format!("port-{offset:02}");
        user.send_to(
            payload.as_bytes(),
            (Ipv4Addr::LOCALHOST, listen_start + offset),
        )
        .expect("send");
        let mut buf = [0u8; 32];
        let (n, _) = user.recv_from(&mut buf).expect("recv");
        assert_eq!(&buf[..n], payload.as_bytes());
    }

    // Wait for the StatsReport tick (5s default) so the per-port slots
    // make it server-side; poll until the JSON shape we want appears.
    let snap = common::wait_for(Duration::from_secs(8), || {
        let url = format!("http://{http}/v1/rules/{rule_id}/stats?per_port=true");
        let resp = reqwest::blocking::Client::new()
            .get(&url)
            .bearer_auth(common::TEST_OPERATOR_TOKEN)
            .send()
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let body: Value = resp.json().ok()?;
        // Require all 10 entries, each with non-zero datagrams_in.
        let arr = body.get("per_port")?.as_array()?;
        if arr.len() != 10 {
            return None;
        }
        let all_busy = arr.iter().all(|e| {
            e.get("datagrams_in").and_then(Value::as_u64).unwrap_or(0) >= 1
                && e.get("datagrams_out").and_then(Value::as_u64).unwrap_or(0) >= 1
        });
        if all_busy { Some(body) } else { None }
    });
    let snap = snap.expect("per_port datagram counters must populate within 8s");
    let arr = snap
        .get("per_port")
        .and_then(Value::as_array)
        .expect("per_port array");
    for (offset, entry) in arr.iter().enumerate() {
        let listen_port = entry.get("listen_port").and_then(Value::as_u64).unwrap();
        assert_eq!(
            u16::try_from(listen_port).unwrap(),
            listen_start + u16::try_from(offset).unwrap(),
            "per_port entries must be ordered by listen_port"
        );
        assert!(
            entry.get("bytes_in").and_then(Value::as_u64).unwrap_or(0) >= 7,
            "bytes_in must reflect the 7-byte payload; entry={entry}"
        );
        assert!(
            entry.get("bytes_out").and_then(Value::as_u64).unwrap_or(0) >= 7,
            "bytes_out must reflect the echoed payload; entry={entry}"
        );
    }
}

// ---- US4: idle eviction + Welcome-driven tunables ----

/// T058 (US4): with `udp_max_flows_per_rule = 2`, sending from 3
/// distinct source ports MUST drop exactly the 3rd as overflow while
/// the first 2 keep working. The Prometheus
/// `portunus_rule_flows_dropped_overflow_total` counter MUST advance by
/// at least 1 (the test allows for retransmits adding more).
#[test]
fn test_udp_us4_overflow_drop() {
    // Seed a server with `udp_max_flows_per_rule = 2`. The toml is
    // otherwise default — listeners on LOCALHOST:0 etc.
    let server = common::spawn_server_with_toml(Some("udp_max_flows_per_rule = 2"), &[]);
    let (_grpc, http, metrics_addr) = server
        .wait_listening_full(Duration::from_secs(5))
        .expect("server listening");

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
    assert!(connected.is_some(), "client must connect within 5s");

    let (_echo_ip, echo_port) = spawn_udp_echo();
    let listen = pick_free_udp_port();
    let (status, body) = common::push_rule_http_with_protocol(
        &http,
        "edge-01",
        listen,
        "127.0.0.1",
        echo_port,
        "udp",
        Some(3),
    );
    assert!(status.is_success(), "push: {status} body={body}");
    let rule_id = body
        .get("rule_id")
        .and_then(serde_json::Value::as_u64)
        .expect("rule_id");

    std::thread::sleep(Duration::from_millis(200));

    // Open 3 distinct source sockets; the 3rd new flow MUST be dropped.
    // We DON'T assert each user.recv() — the dropped one will time out
    // (which is the expected behavior).
    let mut users: Vec<UdpSocket> = Vec::new();
    for i in 0..3u8 {
        let user = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind user");
        user.set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let payload = [0xCAu8, 0xFE, i, 0];
        user.send_to(&payload, (Ipv4Addr::LOCALHOST, listen))
            .expect("send");
        users.push(user);
    }

    // First two MUST receive their replies; third SHOULD time out.
    let mut received = 0;
    for u in &users {
        let mut buf = [0u8; 8];
        if u.recv_from(&mut buf).is_ok() {
            received += 1;
        }
    }
    assert_eq!(
        received, 2,
        "exactly 2 of 3 sources must receive; the 3rd is overflow-dropped"
    );

    // Wait for the StatsReport tick + assert the overflow counter
    // advanced via /metrics.
    let saw = common::wait_for(Duration::from_secs(8), || {
        let body = common::fetch_metrics_text(&metrics_addr);
        let pat = format!(
            "portunus_rule_flows_dropped_overflow_total{{client=\"edge-01\",owner=\"_legacy\",rule=\"{rule_id}\"}}"
        );
        body.lines().find_map(|l| {
            if !l.starts_with(&pat) {
                return None;
            }
            let n: u64 = l.split_whitespace().last()?.parse().ok()?;
            (n >= 1).then_some(n)
        })
    });
    assert!(
        saw.is_some(),
        "FR-014: flows_dropped_overflow_total MUST advance for the over-cap UDP rule within 8s"
    );
}

/// T059 (US4): with `udp_flow_idle_secs = 30` (the minimum allowed),
/// `active_flows` returns to 0 ~35 s after a burst from N distinct
/// sources, and a fresh send from one of the original sources opens a
/// brand-new upstream socket (proven by re-receiving the reply).
///
/// 35 s is a real wall-clock sleep — `#[ignore]`'d so the default
/// `cargo test` run stays sub-second; CI can opt in via
/// `cargo test -- --ignored` once nightly UDP-tuning soak coverage
/// is desired. The reaper-tick logic itself is verified by
/// `forwarder::udp::table::tests::sweep_evicts_idle_flows_*` in the
/// unit suite.
#[test]
#[ignore = "T059: real 35s sleep — unignore for nightly soak via `cargo test --test udp_smoke -- --ignored`"]
fn test_udp_us4_idle_eviction() {
    let server = common::spawn_server_with_toml(Some("udp_flow_idle_secs = 30"), &[]);
    let (_grpc, http, metrics_addr) = server
        .wait_listening_full(Duration::from_secs(5))
        .expect("server listening");

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
    assert!(connected.is_some(), "client must connect within 5s");

    let (_echo_ip, echo_port) = spawn_udp_echo();
    let listen = pick_free_udp_port();
    let (status, body) = common::push_rule_http_with_protocol(
        &http,
        "edge-01",
        listen,
        "127.0.0.1",
        echo_port,
        "udp",
        Some(3),
    );
    assert!(status.is_success(), "push: {status} body={body}");
    let rule_id = body
        .get("rule_id")
        .and_then(serde_json::Value::as_u64)
        .expect("rule_id");

    std::thread::sleep(Duration::from_millis(200));

    // Open 5 distinct sources; round-trip each so flows are live.
    let mut sticky_users: Vec<UdpSocket> = Vec::new();
    for i in 0..5u8 {
        let user = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind user");
        user.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        user.send_to(&[0xAB, i], (Ipv4Addr::LOCALHOST, listen))
            .expect("send");
        let mut buf = [0u8; 8];
        let _ = user.recv_from(&mut buf);
        sticky_users.push(user);
    }

    // Sleep past idle_window + a few sweep ticks. The reaper sweeps
    // every idle_window/4 == 7.5s, so 35s is enough for at least 4 ticks.
    std::thread::sleep(Duration::from_secs(35));

    // active_flows MUST be 0 (visible via /metrics).
    let drained = common::wait_for(Duration::from_secs(10), || {
        let body = common::fetch_metrics_text(&metrics_addr);
        let pat = format!(
            "portunus_rule_active_flows{{client=\"edge-01\",owner=\"_legacy\",rule=\"{rule_id}\"}}"
        );
        body.lines().find_map(|l| {
            if !l.starts_with(&pat) {
                return None;
            }
            let n: u64 = l.split_whitespace().last()?.parse().ok()?;
            (n == 0).then_some(())
        })
    });
    assert!(
        drained.is_some(),
        "active_flows MUST drop to 0 within 10s after idle_window expires"
    );

    // Re-send from one of the original sources — a fresh upstream
    // socket should be opened and the round-trip MUST still work.
    let user = &sticky_users[0];
    user.send_to(&[0xCD, 0xEF], (Ipv4Addr::LOCALHOST, listen))
        .expect("send after eviction");
    let mut buf = [0u8; 8];
    let (n, _) = user.recv_from(&mut buf).expect("post-eviction round-trip");
    assert_eq!(&buf[..n], &[0xCD, 0xEF]);
}

/// T051 (SC-004): a 10-port UDP range with traffic on a subset of ports
/// MUST emit exactly ONE row of `portunus_rule_udp_datagrams_in_total`
/// for the rule (NOT one per port). Cardinality budget is per-rule.
#[test]
fn test_udp_us3_metric_cardinality() {
    let server = common::spawn_server(&[]);
    let (_grpc, http, metrics_addr) = server
        .wait_listening_full(Duration::from_secs(5))
        .expect("server listening");

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
    assert!(connected.is_some(), "client must connect within 5s");

    let target_start = pick_consecutive_free_udp(10);
    for p in target_start..target_start + 10 {
        spawn_udp_echo_on(p);
    }
    let listen_start = pick_consecutive_free_udp(10);
    let (status, body) = common::push_rule_http_range_with_protocol(
        &http,
        "edge-01",
        listen_start,
        listen_start + 9,
        "127.0.0.1",
        target_start,
        target_start + 9,
        "udp",
        Some(3),
    );
    assert!(status.is_success(), "push: {status} body={body}");
    let rule_id = body
        .get("rule_id")
        .and_then(serde_json::Value::as_u64)
        .expect("rule_id present");

    std::thread::sleep(Duration::from_millis(300));

    // Hit only 3 of 10 ports.
    for offset in &[0u16, 4, 9] {
        let user = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind user");
        user.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        user.send_to(b"hi", (Ipv4Addr::LOCALHOST, listen_start + *offset))
            .expect("send");
        let mut buf = [0u8; 8];
        user.recv_from(&mut buf).expect("recv");
    }

    // Wait for stats to flush, then assert /metrics has exactly one row.
    let saw = common::wait_for(Duration::from_secs(8), || {
        let body = common::fetch_metrics_text(&metrics_addr);
        let pat = format!(
            "portunus_rule_udp_datagrams_in_total{{client=\"edge-01\",owner=\"_legacy\",rule=\"{rule_id}\"}}"
        );
        let count = body.lines().filter(|l| l.starts_with(&pat)).count();
        if count == 0 {
            return None;
        }
        // We do NOT want > 1; capture for diagnosis below.
        Some((count, body))
    });
    let (count, body) = saw.expect("UDP datagrams_in metric must appear within 8s");
    assert_eq!(
        count, 1,
        "SC-004: range rule MUST emit exactly one row of \
         portunus_rule_udp_datagrams_in_total (got {count}); /metrics:\n{body}"
    );
}
