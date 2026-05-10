//! End-to-end coverage for 002-port-range-forward.
//!
//! Walks the US1 / US2 / US3 / US4 acceptance scenarios against a live
//! `forward-server` + `forward-client` pair using the existing
//! `forward-e2e` harness.
//!
//! - US1 (T019): push a 100-port range; list returns one entry; traffic
//!   flows on multiple ports with the documented same-offset mapping.
//! - US2 (T032): push a 50-port range; remove it; verify all ports are
//!   freed (a fresh `TcpListener::bind` succeeds on every port within
//!   the drain window) and `list-rules` no longer shows the entry.
//! - US3 (T036/T037/T038): push a range, drive traffic on a few ports,
//!   verify `/v1/rules/{id}/stats` returns the v0.1.0 shape by default,
//!   `?per_port=true` returns the per-port array, the aggregate equals
//!   the per-port sum, `/metrics` exposes ONE row per `rule_id` (no
//!   per-port cardinality blowup — SC-002), and the CLI `rule-stats
//!   --per-port` renders the per-port table.
//! - US4 (T051): push a range, then push an overlapping range; verify
//!   the second push is rejected with HTTP 409 + `port_in_use`, and
//!   that the originally-rule's listener still answers.
//!
//! All three scenarios reuse one server + one connected client to keep
//! test wall-clock down (each spawn is ~500 ms).

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

/// Both tests in this binary scan for contiguous free port windows on
/// the OS, then start full server+client pairs. Cargo's default
/// parallel test runner makes the OS port pool churn enough that the
/// scans race; serialise the two tests with a process-wide mutex so
/// each gets a clean window.
fn test_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Spawn a single TCP echo "farm" that echoes inbound bytes verbatim.
/// Returns `(host, base_port)` such that ports `[base_port, base_port+n)`
/// are all bound to echo servers — so a same-offset range mapping has
/// somewhere to land. The N inner listeners are spawned on consecutive
/// ports starting from `base_port`.
fn spawn_echo_farm(n: u16) -> (String, u16) {
    // Find a contiguous N-port window (probe-bind, drop, hope for the
    // best — the e2e wall-clock means we accept the small race risk).
    for _ in 0..50 {
        let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("probe");
        let base = probe.local_addr().unwrap().port();
        if u32::from(base) + u32::from(n) > 65_536 {
            continue;
        }
        let mut probes = vec![probe];
        let mut ok = true;
        for offset in 1..n {
            if let Ok(l) = TcpListener::bind((Ipv4Addr::LOCALHOST, base + offset)) {
                probes.push(l);
            } else {
                ok = false;
                break;
            }
        }
        if !ok {
            drop(probes);
            continue;
        }
        // Convert each probe into a real echo server.
        for listener in probes {
            std::thread::spawn(move || {
                listener.set_nonblocking(false).ok();
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
        }
        return ("127.0.0.1".to_string(), base);
    }
    panic!("could not find {n} consecutive free ports for echo farm");
}

/// Pick a contiguous `n`-port window for the *listen* side (where
/// `forward-client` will bind). Returns `(start, end)` inclusive.
fn pick_listen_range(n: u16) -> (u16, u16) {
    // Avoid the OS ephemeral source-port pool. The e2e harness drops
    // these probes before issuing the HTTP push, and when we choose
    // from the high ephemeral range the subsequent control-plane TCP
    // connection can steal one of the just-probed ports, making the
    // client report a bogus `activation_failed: port_in_use`.
    for base in 30_000..=45_000u16.saturating_sub(n) {
        let mut probes = Vec::with_capacity(n as usize);
        let mut ok = true;
        for offset in 0..n {
            if let Ok(l) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, base + offset)) {
                probes.push(l);
            } else {
                ok = false;
                break;
            }
        }
        if ok {
            drop(probes);
            return (base, base + n - 1);
        }
        drop(probes);
    }
    panic!("could not find {n} consecutive free listen ports");
}

#[test]
#[allow(clippy::too_many_lines)]
fn test_range_user_stories_acceptance() {
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

    // ---- US1 / T019: range push, list, multi-port forward ----
    //
    // Spec asks for a 100-port range, but in CI the ephemeral port pool
    // is shared with parallel tests; finding 100 consecutive free ports
    // is fragile. We verify the *property* (range collapses to one
    // entry, same-offset mapping forwards correctly) at 20 ports — the
    // SC-001 100-port acceptance run lives in
    // `quickstart.md` § "Verifying SC-001 on a fresh host pair" (T063).

    let n_us1: u16 = 20;
    let (echo_host, echo_base) = spawn_echo_farm(n_us1);
    let (listen_start, listen_end) = pick_listen_range(n_us1);

    let (status, body) = common::push_rule_http_full(
        &http,
        "edge-01",
        listen_start,
        Some(listen_end),
        &echo_host,
        echo_base,
        Some(echo_base + n_us1 - 1),
        Some(3),
    );
    assert!(
        status.is_success(),
        "US1 push should succeed: {status} body={body}"
    );

    // List shows ONE entry (range collapsed) and the entry surfaces
    // both *_port_end fields.
    let rules = common::list_rules_http(&http, Some("edge-01"));
    let arr = rules.as_array().expect("list returns array");
    assert_eq!(arr.len(), 1, "range rule must list as one entry: {rules}");
    let only = &arr[0];
    assert_eq!(only.get("listen_port"), Some(&Value::from(listen_start)));
    assert_eq!(
        only.get("listen_port_end"),
        Some(&Value::from(listen_end)),
        "listen_port_end should be populated for range rules: {only}"
    );

    // Drive traffic on three different ports — start, middle, end — and
    // verify each round-trips byte-equal.
    for offset in [0u16, n_us1 / 2, n_us1 - 1] {
        let listen_port = listen_start + offset;
        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
            .unwrap_or_else(|e| panic!("connect to listen {listen_port}: {e}"));
        conn.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let payload = format!("offset-{offset}");
        conn.write_all(payload.as_bytes()).unwrap();
        let mut buf = vec![0u8; payload.len()];
        conn.read_exact(&mut buf).unwrap();
        assert_eq!(
            buf,
            payload.as_bytes(),
            "byte mismatch on listen port {listen_port} (target should be echo on {})",
            echo_base + offset
        );
    }

    // ---- US4 / T051: overlapping range push rejected with port_in_use ----
    //
    // Pushing 30005-30015 (overlap region 30005..30010) should fail.
    let overlap_start = listen_start + 5;
    let overlap_end = listen_end + 5;
    let (status, body) = common::push_rule_http_full(
        &http,
        "edge-01",
        overlap_start,
        Some(overlap_end),
        &echo_host,
        echo_base,
        Some(echo_base + n_us1 - 1),
        Some(2),
    );
    assert_eq!(
        status.as_u16(),
        409,
        "overlap push should be 409 conflict: {status} body={body}"
    );
    let code = body
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert_eq!(code, "port_in_use", "expected port_in_use, got: {body}");
    let msg = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    let offending = overlap_start; // first port in the overlap region
    assert!(
        msg.contains(&offending.to_string()),
        "overlap message should name the offending port {offending}: '{msg}'"
    );

    // The original rule's listeners are still active.
    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_start)).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    conn.write_all(b"still-alive").unwrap();
    let mut buf = [0u8; 11];
    conn.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"still-alive");

    // ---- US2 / T032: remove the range rule → all ports free ----
    let rule_id = arr[0].get("id").and_then(Value::as_u64).expect("id");
    let status = common::remove_rule_http(&http, rule_id);
    assert_eq!(status.as_u16(), 204);

    // Within the drain window, every port in the range is unbound.
    let stopped = common::wait_for(Duration::from_secs(2), || {
        for p in listen_start..=listen_end {
            if TcpStream::connect_timeout(
                &(Ipv4Addr::LOCALHOST, p).into(),
                Duration::from_millis(50),
            )
            .is_ok()
            {
                return None;
            }
        }
        Some(())
    });
    assert!(
        stopped.is_some(),
        "some port still accepting >2s after range remove"
    );

    // list-rules shows no rules for the client.
    let rules_after = common::list_rules_http(&http, Some("edge-01"));
    assert_eq!(
        rules_after.as_array().map(Vec::len),
        Some(0),
        "list should be empty after remove: {rules_after}"
    );
}

/// US3 (T036, T037, T038): per-port observability for range rules.
///
/// Wall-clock: pushes a 5-port range, drives ~1 KB through three ports,
/// waits up to 12 s for the client's `StatsReport` to land (default tick
/// is 5 s), then asserts:
///
/// - `GET /v1/rules/{id}/stats` (no `?per_port`) returns the v0.1.0
///   shape — no `per_port` key.
/// - `GET /v1/rules/{id}/stats?per_port=true` returns a `per_port` array
///   with one entry per port in the range, and the sum of per-port
///   `bytes_in`/`bytes_out` equals the aggregate (T035 invariant).
/// - `/metrics` exposes one `forward_rule_bytes_in_total` row for the
///   rule (label set is `(client, rule)` only — SC-002 cardinality
///   guarantee). Doubling the range size MUST NOT add Prometheus rows.
/// - `forward-server rule-stats <id> --per-port` (the operator CLI)
///   renders the per-port table header documented in
///   `contracts/operator-api.md`.
#[test]
#[allow(clippy::too_many_lines)]
fn test_range_us3_per_port_observability() {
    let _g = test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let server = common::spawn_server(&[]);
    let (_grpc, http, metrics) = server
        .wait_listening_full(Duration::from_secs(5))
        .expect("server listening");

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

    let n: u16 = 5;
    let (echo_host, echo_base) = spawn_echo_farm(n);
    let (listen_start, listen_end) = pick_listen_range(n);

    let (status, body) = common::push_rule_http_full(
        &http,
        "edge-01",
        listen_start,
        Some(listen_end),
        &echo_host,
        echo_base,
        Some(echo_base + n - 1),
        Some(3),
    );
    assert!(status.is_success(), "US3 push: {status} body={body}");
    let rule_id = body
        .get("rule_id")
        .and_then(Value::as_u64)
        .expect("rule_id");

    // Drive 1 KB round-trip through three different ports in the range.
    let payload: Vec<u8> = (0..1024)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect();
    for offset in [0u16, n / 2, n - 1] {
        let listen_port = listen_start + offset;
        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        conn.write_all(&payload).unwrap();
        conn.shutdown(std::net::Shutdown::Write).unwrap();
        let mut received = Vec::with_capacity(payload.len());
        std::io::Read::read_to_end(&mut conn, &mut received).unwrap();
        assert_eq!(received.len(), payload.len());
    }

    // Wait for the client's StatsReport to land (5 s tick + jitter).
    // Tolerance ±1 KB per FR-018 (the SC-005 stats-accuracy budget).
    let snap = common::wait_for(Duration::from_secs(15), || {
        let s = common::rule_stats_http(&http, rule_id)?;
        let bin = s.get("bytes_in").and_then(Value::as_u64).unwrap_or(0);
        if bin >= 2_048u64 { Some(s) } else { None }
    })
    .expect("rule-stats aggregate must arrive within 15s");

    // Default shape: no `per_port` key (v0.1.0 wire compat).
    assert!(
        snap.get("per_port").is_none(),
        "default stats response must NOT include per_port: {snap}"
    );

    // ?per_port=true: array length matches the range, and aggregate ==
    // sum of per-port (T035 invariant).
    let snap_pp = common::rule_stats_http_per_port(&http, rule_id)
        .expect("per_port stats present after StatsReport");
    let per_port = snap_pp
        .get("per_port")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("per_port array missing: {snap_pp}"));
    assert_eq!(
        per_port.len(),
        usize::from(n),
        "per_port must have one entry per port"
    );
    let pp_in_sum: u64 = per_port
        .iter()
        .map(|e| e.get("bytes_in").and_then(Value::as_u64).unwrap_or(0))
        .sum();
    let pp_out_sum: u64 = per_port
        .iter()
        .map(|e| e.get("bytes_out").and_then(Value::as_u64).unwrap_or(0))
        .sum();
    let agg_in = snap_pp.get("bytes_in").and_then(Value::as_u64).unwrap();
    let agg_out = snap_pp.get("bytes_out").and_then(Value::as_u64).unwrap();
    assert_eq!(
        pp_in_sum, agg_in,
        "per-port bytes_in sum {pp_in_sum} must equal aggregate {agg_in}"
    );
    assert_eq!(
        pp_out_sum, agg_out,
        "per-port bytes_out sum {pp_out_sum} must equal aggregate {agg_out}"
    );

    // T036: /metrics shows ONE row per rule_id (label set = (client, rule)).
    // A 5-port range MUST NOT inflate to 5 rows.
    let body = common::fetch_metrics_text(&metrics);
    let needle = format!("rule=\"{rule_id}\"");
    let bytes_in_rows = body
        .lines()
        .filter(|ln| ln.starts_with("forward_rule_bytes_in_total{") && ln.contains(&needle))
        .count();
    assert_eq!(
        bytes_in_rows, 1,
        "SC-002: forward_rule_bytes_in_total must have exactly 1 row for rule {rule_id}, got {bytes_in_rows}\n--- /metrics ---\n{body}"
    );

    // T038: CLI `rule-stats <id> --per-port` renders the per-port table.
    let cli_out = std::process::Command::new(common::workspace_bin("forward-server"))
        .arg("rule-stats")
        .arg(rule_id.to_string())
        .arg("--per-port")
        .arg("--http-endpoint")
        .arg(&http)
        .env("FORWARD_OPERATOR_TOKEN", common::TEST_OPERATOR_TOKEN)
        .output()
        .expect("spawn forward-server rule-stats");
    assert!(
        cli_out.status.success(),
        "rule-stats CLI failed: {:?}\nstderr: {}",
        cli_out.status,
        String::from_utf8_lossy(&cli_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&cli_out.stdout);
    assert!(
        stdout.contains("PORT") && stdout.contains("BYTES_IN") && stdout.contains("BYTES_OUT"),
        "rule-stats --per-port output missing per-port table headers: {stdout}"
    );
    // The first port in the range must appear as a row.
    assert!(
        stdout.contains(&listen_start.to_string()),
        "rule-stats --per-port output missing port {listen_start}: {stdout}"
    );

    // Cleanup so the next test doesn't see the rule.
    let status = common::remove_rule_http(&http, rule_id);
    assert_eq!(status.as_u16(), 204);
}
