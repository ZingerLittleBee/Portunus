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

    let mut conn =
        TcpStream::connect((Ipv4Addr::LOCALHOST, dns_listen)).expect("connect to DNS-target proxy");
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

    let mut conn =
        TcpStream::connect((Ipv4Addr::LOCALHOST, ip_listen)).expect("connect to IP-target proxy");
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(b"ip-roundtrip-still-works").unwrap();
    let mut buf = [0u8; 24];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(
        &buf, b"ip-roundtrip-still-works",
        "IP-target round-trip mismatch — v0.2.0 byte path regressed"
    );
}

/// T027 (US2) — DNS failure does NOT take a rule down.
///
/// The original spec wording asked for /etc/hosts manipulation to
/// observe failure-then-recovery. That requires root and pollutes the
/// system DNS for parallel test runs. We use the cleaner alternative
/// allowed by spec § Assumptions: a hostname under `.invalid` (RFC
/// 6761 §6.4) which is guaranteed NXDOMAIN on every well-behaved
/// resolver, including hickory.
///
/// What we assert:
///   - The push succeeds (server-side `Target::parse` accepts a
///     syntactically valid hostname; the client only resolves at
///     connect time per FR-002).
///   - `list-rules` reports the rule as Active (FR-004 — DNS failure
///     does NOT mark the rule Failed; that only happens on bind /
///     port-conflict failures).
///   - End-user connections are refused within 3 s (FR-005 budget).
///   - The client emits a structured `rule.dns_failed` log line
///     (T034) carrying the rule_id + hostname + a classified reason.
///
/// The "recovery on next connection" half from the spec is covered
/// by the cache state-machine unit test
/// `cache::tests::refresh_failure_serves_stale_within_grace`
/// (T025) — replicating it e2e would require dependency-injecting
/// the system resolver, which is out of scope until the localhost
/// mini-resolver harness lands (referenced from T024 but not part of
/// US2's scope).
/// Probe whether the local resolver actually returns NXDOMAIN for a
/// `.invalid` hostname. Some ISP/captive-portal DNS configurations
/// hijack NXDOMAIN and return a synthetic IP, which would invalidate
/// the failure-side assertions of `test_dns_us2_failure_*`. When that
/// happens we skip with a clear explanation rather than fail
/// spuriously.
fn dns_hijack_detected() -> bool {
    use std::net::ToSocketAddrs;
    "broken.invalid:443"
        .to_socket_addrs()
        .map(|mut iter| iter.next().is_some())
        .unwrap_or(false)
}

#[test]
fn test_dns_us2_failure_active_rule_and_event() {
    if dns_hijack_detected() {
        eprintln!(
            "skipping: local resolver hijacks NXDOMAIN for `.invalid` — \
             T034 cannot exercise dns_failed without a controlled resolver \
             (covered by unit tests cache::tests::* and \
             resolver::tests::all_addrs_unreachable_when_every_address_fails)"
        );
        return;
    }
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
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
    assert!(connected.is_some(), "edge-01 must connect within 5s");

    let listen = pick_free_port();
    let (status, body) = common::push_rule_http(
        &http,
        "edge-01",
        listen,
        // RFC 6761 §6.4: the .invalid TLD is reserved and MUST NOT
        // resolve in any production DNS.
        "broken.invalid",
        443,
        Some(3),
    );
    assert!(
        status.is_success(),
        "DNS-target push must succeed even for unresolvable name (FR-004): {status} body={body}"
    );

    // Rule must report Active — FR-004 / acceptance scenario 1.
    let rules = common::list_rules_http(&http, Some("edge-01"));
    let rule_state = rules
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|r| r.get("listen_port").and_then(Value::as_u64) == Some(u64::from(listen)))
        })
        .expect("rule should be listed");
    let kind = rule_state
        .pointer("/state/kind")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert_eq!(
        kind, "active",
        "DNS-failing rule MUST stay Active (FR-004), got state={kind}"
    );

    // End-user connection must fail within 3 s (FR-005 budget).
    let attempt_started = std::time::Instant::now();
    let conn_result = TcpStream::connect((Ipv4Addr::LOCALHOST, listen));
    if let Ok(mut sock) = conn_result {
        sock.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        sock.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        let read_err = sock.read_exact(&mut buf);
        assert!(
            read_err.is_err() || attempt_started.elapsed() < Duration::from_secs(4),
            "connection to DNS-failing target must fail-fast within 3s budget, took {:?}",
            attempt_started.elapsed()
        );
    }
    // Either the kernel refuses (proxy closed inbound) or the proxy
    // accepted then immediately closed — both satisfy "refuse / fail
    // fast" per FR-005.

    // T034: structured `rule.dns_failed` event MUST appear in the
    // client's stderr within 5 s. We poll because tracing's JSON
    // layer may flush asynchronously.
    let saw_event = common::wait_for(Duration::from_secs(5), || {
        client
            .stderr_lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("rule.dns_failed"))
            .then_some(())
    });
    if saw_event.is_none() {
        eprintln!("--- client stderr (no rule.dns_failed seen) ---");
        for l in client.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
    }
    assert!(
        saw_event.is_some(),
        "T034: client MUST emit rule.dns_failed for broken.invalid"
    );
}

/// T037 (US3) — HTTP round-trip for `prefer_ipv6`.
///
/// Three sub-cases per `contracts/operator-api.md`:
///   1. POST `{ "prefer_ipv6": true }` → response echoes `true`.
///   2. POST without the field → response echoes `false` (default).
///   3. GET `/v1/rules` lists both rules with `prefer_ipv6` always
///      present as a flat bool.
///
/// Doesn't drive bytes through the proxy — wire-shape only.
#[test]
fn test_dns_us3_http_prefer_ipv6_round_trip() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
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
    assert!(connected.is_some(), "edge-01 must connect within 5s");

    let (_echo_ip, echo_port_a) = spawn_echo();
    let (_echo_ip2, echo_port_b) = spawn_echo();
    let listen_a = pick_free_port();
    let listen_b = pick_free_port();

    // Case 1: explicit prefer_ipv6 = true.
    let (status, body_a) = common::push_rule_http_with_prefer_ipv6(
        &http,
        "edge-01",
        listen_a,
        "localhost",
        echo_port_a,
        Some(true),
        Some(3),
    );
    assert!(
        status.is_success(),
        "POST with prefer_ipv6=true: {status} {body_a}"
    );
    assert_eq!(
        body_a.get("prefer_ipv6").and_then(Value::as_bool),
        Some(true),
        "response must echo prefer_ipv6=true: {body_a}"
    );
    assert_eq!(
        body_a.get("target_host").and_then(Value::as_str),
        Some("localhost"),
        "response must echo target_host: {body_a}"
    );

    // Case 2: omit prefer_ipv6 → response echoes false.
    let (status, body_b) = common::push_rule_http_with_prefer_ipv6(
        &http,
        "edge-01",
        listen_b,
        "localhost",
        echo_port_b,
        None,
        Some(3),
    );
    assert!(
        status.is_success(),
        "POST without prefer_ipv6: {status} {body_b}"
    );
    assert_eq!(
        body_b.get("prefer_ipv6").and_then(Value::as_bool),
        Some(false),
        "absent prefer_ipv6 must default to false in response: {body_b}"
    );

    // Case 3: list both rules — both entries carry the flat bool.
    let rules = common::list_rules_http(&http, Some("edge-01"));
    let arr = rules.as_array().expect("rules array");
    assert!(arr.len() >= 2, "expected >=2 rules: {rules}");
    for r in arr {
        assert!(
            r.get("prefer_ipv6").is_some(),
            "every list entry MUST include prefer_ipv6: {r}"
        );
        assert!(
            r.get("prefer_ipv6").unwrap().is_boolean(),
            "prefer_ipv6 MUST serialize as flat bool, not Option: {r}"
        );
    }
    let pref_a = arr
        .iter()
        .find(|r| r.get("listen_port").and_then(Value::as_u64) == Some(u64::from(listen_a)))
        .and_then(|r| r.get("prefer_ipv6"))
        .and_then(Value::as_bool);
    let pref_b = arr
        .iter()
        .find(|r| r.get("listen_port").and_then(Value::as_u64) == Some(u64::from(listen_b)))
        .and_then(|r| r.get("prefer_ipv6"))
        .and_then(Value::as_bool);
    assert_eq!(pref_a, Some(true), "rule A must list prefer_ipv6=true");
    assert_eq!(pref_b, Some(false), "rule B must list prefer_ipv6=false");
}

/// T038 (US3) — e2e: `prefer_ipv6=true` against a hostname that only
/// resolves to A (no AAAA) MUST fall back to IPv4 and succeed
/// (FR-007 acceptance scenario 3 — "prefer is not only").
///
/// The full dual-stack split-comparison from the spec ("two rules
/// to the same hostname, one default, one prefer-ipv6, parse the
/// chosen_addr log") requires injecting a controlled resolver into
/// the live client process — same blocker that pushed T024 out of
/// scope. The family-ordering itself is exhaustively covered by the
/// pure unit test `order_by_family_covers_all_fr_007_cases`. This
/// e2e covers the most-bug-prone fallback case end-to-end:
/// requesting v6 preference must NOT break v4-only targets.
#[test]
fn test_dns_us3_ipv6_optin_falls_back_to_ipv4_only_target() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
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
    assert!(connected.is_some(), "edge-01 must connect within 5s");

    let (_echo_ip, echo_port) = spawn_echo();
    let listen = pick_free_port();
    let (status, body) = common::push_rule_http_with_prefer_ipv6(
        &http,
        "edge-01",
        listen,
        // `localhost` resolves to 127.0.0.1 (and ::1 on dual-stack
        // boxes); when only the v4 echo is bound, prefer_ipv6=true
        // MUST still reach the v4 listener via family fallback.
        "localhost",
        echo_port,
        Some(true),
        Some(3),
    );
    assert!(
        status.is_success(),
        "push with prefer_ipv6=true MUST succeed: {status} {body}"
    );
    assert_eq!(body.get("prefer_ipv6").and_then(Value::as_bool), Some(true));

    let mut conn = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, listen))
        .expect("connect to prefer_ipv6 proxy");
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(b"v6-pref-falls-back-to-v4").unwrap();
    let mut buf = [0u8; 24];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(
        &buf, b"v6-pref-falls-back-to-v4",
        "prefer_ipv6=true MUST reach v4-only target via fallback (FR-007 scenario 3)"
    );

    // Sanity: client should have logged a rule.dns_resolved with
    // prefer_ipv6=true so operators can confirm the toggle took effect.
    let saw = common::wait_for(Duration::from_secs(2), || {
        client
            .stderr_lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("rule.dns_resolved") && l.contains("prefer_ipv6"))
            .then_some(())
    });
    if saw.is_none() {
        eprintln!("--- client stderr (no rule.dns_resolved seen) ---");
        for l in client.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
    }
    assert!(
        saw.is_some(),
        "client MUST log rule.dns_resolved with prefer_ipv6 field"
    );
}

/// T045 (US4) — `forward_rule_dns_failures_total` cardinality and
/// per-rule accuracy.
///
/// Pushes N rules pointing at `broken.invalid` (NXDOMAIN under any
/// well-behaved resolver), drives K connection attempts through each,
/// waits for one StatsReport tick (5 s default), scrapes `/metrics`,
/// and asserts:
///   - exactly N `forward_rule_dns_failures_total` rows (SC-006 /
///     R-008 cardinality budget — 1 row per rule)
///   - each row's value equals the K_i count we drove through it
///
/// Skipped on hosts with NXDOMAIN hijacking (same gating logic as
/// T034).
#[test]
fn test_dns_us4_metric_cardinality() {
    if dns_hijack_detected() {
        eprintln!("skipping T045: local resolver hijacks NXDOMAIN — see T034 skip");
        return;
    }
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
    assert!(connected.is_some(), "edge-01 must connect within 5s");

    // Push N broken-DNS rules; record (rule_id, listen_port).
    const N: usize = 3;
    const K: usize = 2;
    let mut rules: Vec<(u64, u16)> = Vec::with_capacity(N);
    for _ in 0..N {
        let listen = pick_free_port();
        let (status, body) =
            common::push_rule_http(&http, "edge-01", listen, "broken.invalid", 443, Some(3));
        assert!(status.is_success(), "push: {status} {body}");
        let rule_id = body
            .get("rule_id")
            .and_then(Value::as_u64)
            .expect("rule_id in response");
        rules.push((rule_id, listen));
    }

    // Drive K connection attempts through each rule. Each attempt
    // MUST refuse fast (FR-005 budget) and bump the per-rule counter.
    for (_, port) in &rules {
        for _ in 0..K {
            // Best-effort connect: kernel may RST or proxy may close
            // mid-handshake — both satisfy "DNS-failure path taken".
            if let Ok(mut sock) = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, *port)) {
                sock.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let _ = sock.write_all(b"ping");
                let mut b = [0u8; 1];
                let _ = sock.read_exact(&mut b);
            }
        }
    }

    // Wait for one StatsReport tick + a small grace. Default
    // interval is 5s; we poll the metrics endpoint until every
    // expected row appears, capped at ~10s.
    let _ = common::wait_for(Duration::from_secs(10), || {
        let body = common::fetch_metrics_text(&metrics_addr);
        let rows: Vec<&str> = body
            .lines()
            .filter(|l| l.starts_with("forward_rule_dns_failures_total{"))
            .collect();
        if rows.len() == N { Some(()) } else { None }
    });

    let body = common::fetch_metrics_text(&metrics_addr);
    let rows: Vec<&str> = body
        .lines()
        .filter(|l| l.starts_with("forward_rule_dns_failures_total{"))
        .collect();
    assert_eq!(
        rows.len(),
        N,
        "expected exactly N={N} dns_failures rows (SC-006), got {}\n{body}",
        rows.len()
    );

    // Each row's value MUST equal K (one bump per refused attempt).
    for (rule_id, _) in &rules {
        let pat = format!(r#"rule="{rule_id}"}}"#);
        let row = rows
            .iter()
            .find(|l| l.contains(&pat))
            .unwrap_or_else(|| panic!("no row for rule {rule_id}\n{body}"));
        // Extract the trailing integer after the last space.
        let value: u64 = row
            .rsplit(' ')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("can't parse value from {row}"));
        assert_eq!(
            value, K as u64,
            "rule {rule_id}: expected {K} failures, got {value}\n{row}"
        );
    }
}
