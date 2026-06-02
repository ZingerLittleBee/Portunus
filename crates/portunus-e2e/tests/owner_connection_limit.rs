//! 011-rate-limiting-qos T081/T082 — owner `concurrent_connections`
//! cap enforced over a real gRPC push to a real `portunus-client`,
//! and the same cap survives a server-AND-client restart.
//!
//! Reads the data-plane outcome with raw `TcpStream`s and the
//! enforcement counter with the `/metrics` scrape (the cap surface
//! has no per-owner JSON stats endpoint — Prometheus is the system
//! of record for the cumulative reject counter).

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const CLIENT_NAME: &str = "edge-01";
// `operator_token` shortcut in `spawn_server_with_toml` bootstraps the
// `_legacy` superadmin; every rule pushed through HTTP therefore lands
// with `owner_user_id="_legacy"`, which is the owner_id the cap
// envelope must be keyed on.
const OWNER_ID: &str = "_legacy";

// ---------------------------------------------------------------------
// Helpers private to this test file.
// ---------------------------------------------------------------------

fn spawn_echo() -> (String, u16) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind echo");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming().flatten() {
            thread::spawn(move || {
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

fn wait_connected(http: &str, name: &str, timeout: Duration) -> bool {
    common::wait_for(timeout, || {
        let arr = common::list_clients_http(http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some(name)
                    && v.get("connected")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
            })
            .map(|_| ())
    })
    .is_some()
}

/// PUT `/v1/clients/{client}/owners/{owner}/rate-limit` with the given JSON body.
fn put_owner_rate_limit(
    http: &str,
    client: &str,
    owner: &str,
    body: &serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    // 015-client-stable-id: the operator surface addresses clients by
    // their stable id, so resolve the display name to its client_id.
    let client_id = common::client_id_for_name(http, client);
    let url = format!("http://{http}/v1/clients/{client_id}/owners/{owner}/rate-limit");
    let resp = reqwest::blocking::Client::new()
        .put(&url)
        .header("Authorization", "Bearer test-operator-token-005")
        .json(body)
        .send()
        .expect("PUT owner rate-limit");
    let status = resp.status();
    let json: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// GET `/v1/clients/{client}/owners/{owner}/rate-limit`. Returns
/// `(status, body)`.
fn get_owner_rate_limit(
    http: &str,
    client: &str,
    owner: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let client_id = common::client_id_for_name(http, client);
    let url = format!("http://{http}/v1/clients/{client_id}/owners/{owner}/rate-limit");
    let resp = reqwest::blocking::Client::new()
        .get(&url)
        .header("Authorization", "Bearer test-operator-token-005")
        .send()
        .expect("GET owner rate-limit");
    let status = resp.status();
    let json: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// Block until the spawned `ClientHandle`'s stderr shows a
/// `control.owner_rate_limit_set` event for the expected owner (or
/// timeout). The client emits this event from `apply_owner_rate_limit_update`
/// the moment the server's push lands.
fn wait_owner_cap_applied(client: &common::ClientHandle, owner: &str, timeout: Duration) -> bool {
    common::wait_for(timeout, || {
        client
            .stderr_contains("control.owner_rate_limit_set")
            .then_some(())
            .and_then(|()| {
                let lines = client.stderr_lines.lock().unwrap();
                let needle_event = "\"event\":\"control.owner_rate_limit_set\"";
                let owner_marker = format!("\"owner_id\":\"{owner}\"");
                lines
                    .iter()
                    .any(|line| line.contains(needle_event) && line.contains(&owner_marker))
                    .then_some(())
            })
    })
    .is_some()
}

/// Block until `/metrics` shows a non-zero
/// `portunus_rate_limit_reject_total{...owner=<owner>...reason="owner_concurrent"...}`.
/// Label order in Prometheus exposition is implementation-defined, so we
/// match on substrings of the label set rather than a fixed prefix.
/// Returns the observed counter value (>= 1).
fn wait_owner_concurrent_reject(metrics_addr: &str, client_name: &str, owner: &str) -> u64 {
    let metric = "portunus_rate_limit_reject_total{";
    let must_contain = [
        format!("client=\"{client_name}\""),
        format!("owner=\"{owner}\""),
        "reason=\"owner_concurrent\"".to_string(),
        "rule=\"\"".to_string(),
    ];
    common::wait_for(Duration::from_secs(15), || {
        let body = common::fetch_metrics_text(metrics_addr);
        for line in body.lines() {
            if line.starts_with('#') || !line.starts_with(metric) {
                continue;
            }
            if !must_contain.iter().all(|m| line.contains(m.as_str())) {
                continue;
            }
            // Split off the value at the last whitespace.
            let val_str = line.rsplit_once(char::is_whitespace).map(|(_, v)| v)?;
            // Prometheus exposition is plain f64. The reject counter is
            // a non-negative integer in the well-formed case, but we
            // round-trip through f64::parse to tolerate scientific
            // notation, then re-floor to u64. Negative / NaN / huge
            // values are bugs; surface them as "not yet >= 1" so we
            // keep polling rather than panicking on a transient line.
            let val: f64 = val_str.parse().ok()?;
            // Prometheus counters reset to 0 on restart and only ever
            // grow; for our reject-counter assertion we only need to
            // tell "0" from "≥ 1". Comparing against 1.0 directly
            // sidesteps the u64↔f64 cast.
            if !val.is_finite() || val < 1.0 {
                continue;
            }
            // Round the bounded counter into a u64 for callers that
            // want the exact integer (we cap at u32::MAX which fits
            // losslessly in u64).
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let int = val.min(f64::from(u32::MAX)) as u64;
            return Some(int);
        }
        None
    })
    .unwrap_or_else(|| {
        let body = common::fetch_metrics_text(metrics_addr);
        panic!(
            "owner_concurrent reject counter never ticked for owner={owner}; \
metrics body was:\n{body}"
        )
    })
}

/// Spawn a server with explicit, pre-chosen ports for the gRPC and
/// operator-HTTP listeners. Required by t082 because the client bundle
/// bakes in the gRPC endpoint at provision time, and the bundle must
/// stay valid across a server restart.
fn spawn_server_with_fixed_ports(
    data_dir: &Path,
    grpc_port: u16,
    http_port: u16,
) -> (Child, Arc<Mutex<Vec<String>>>) {
    let body = format!(
        "control_listen = \"127.0.0.1:{grpc_port}\"\n\
         operator_http_listen = \"127.0.0.1:{http_port}\"\n\
         metrics_listen = \"127.0.0.1:0\"\n\
         operator_token = \"test-operator-token-005\"\n",
    );
    std::fs::write(data_dir.join("server.toml"), body).expect("write server.toml");
    let bin = common::workspace_bin("portunus-server");
    let mut child = Command::new(&bin)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("serve")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUST_LOG", "info")
        .spawn()
        .expect("spawn portunus-server with fixed ports");
    let stderr = child.stderr.take().expect("stderr piped");
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let lines_c = Arc::clone(&lines);
    thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let r = BufReader::new(stderr);
        for line in r.lines().map_while(Result::ok) {
            lines_c.lock().unwrap().push(line);
        }
    });
    (child, lines)
}

/// Probe whether the cap is enforced: open conn-A, exchange "hello",
/// then open conn-B and expect Ok(0) / Err on read (RST or FIN before
/// any echo). The data-plane semantics mirror the plan's t081 sketch.
fn assert_cap_enforced(listen_port: u16) {
    // Retry the conn_a side of the probe a few times: between phases
    // the proxy task tail (post-conn_a-close on the prior probe) and
    // the listener's per-rule accept loop race against the OS-level
    // socket reaper, so a stray RST on the very first connect after
    // a phase boundary is benign — only a *persistent* reset is a
    // cap-enforcement failure.
    let mut last_err = None;
    let mut conn_a = None;
    for attempt in 0..5u8 {
        match try_echo_conn_a(listen_port) {
            Ok(c) => {
                conn_a = Some(c);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(200 * u64::from(attempt + 1)));
            }
        }
    }
    let conn_a = conn_a.unwrap_or_else(|| {
        panic!("conn_a never produced a clean echo across 5 attempts; last error: {last_err:?}")
    });

    let mut conn_b =
        TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).expect("connect conn_b");
    conn_b
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut buf_b = [0u8; 1];
    match conn_b.read(&mut buf_b) {
        // FIN/RST encodes as Ok(0) or an Err. Both satisfy the cap
        // contract: conn_b must close before any echo bytes flow.
        Ok(0) | Err(_) => {}
        Ok(n) => panic!("rejected conn_b should not deliver bytes, got {n}"),
    }
    drop(conn_a);
    drop(conn_b);
}

fn try_echo_conn_a(listen_port: u16) -> std::io::Result<TcpStream> {
    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))?;
    conn.set_read_timeout(Some(Duration::from_secs(3)))?;
    conn.write_all(b"hello")?;
    let mut buf = [0u8; 5];
    conn.read_exact(&mut buf)?;
    if &buf != b"hello" {
        return Err(std::io::Error::other(format!("bad echo: {buf:?}")));
    }
    Ok(conn)
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

#[test]
fn t081_owner_concurrent_cap_one_allowed_second_rst() {
    let server = common::spawn_server(&[]);
    let (_grpc, http, metrics) = server
        .wait_listening_full(Duration::from_secs(15))
        .expect("server listening");

    // Bring the client online so the v0.11-only owner-cap PUT is
    // not gated on `client_version=unknown`.
    let bundle = common::provision_client_http(&http, CLIENT_NAME);
    let _client = common::spawn_client(&bundle, &[]);
    assert!(
        wait_connected(&http, CLIENT_NAME, Duration::from_secs(10)),
        "client never connected"
    );
    // The client emits `hello` with `client_version` once connected;
    // give the server a beat to record it before the v0.11 gate runs.
    thread::sleep(Duration::from_millis(500));

    let (echo_host, echo_port) = spawn_echo();
    let listen_port = pick_free_port();

    // Push a plain TCP rule. owner_user_id is inherited from the
    // `_legacy` superadmin baked in by `operator_token`.
    // Push via the multi-target `targets[]` shape so the server emits
    // `Rule.owner_id` on the wire — the legacy single-target push path
    // hard-codes `owner_id: None`, which leaves the client without an
    // OwnerRateLimitHandle and the cap goes unenforced. A single-element
    // `targets` array is the smallest valid v0.7+ shape.
    let (status, body) = common::push_rule_http_targets(
        &http,
        CLIENT_NAME,
        listen_port,
        &[(echo_host.as_str(), echo_port)],
        None,
        Some(2),
    );
    assert!(status.is_success(), "push failed: {status} body={body}");

    // PUT the owner concurrent cap of 1.
    let (st, _resp) = put_owner_rate_limit(
        &http,
        CLIENT_NAME,
        OWNER_ID,
        &serde_json::json!({ "concurrent_connections": 1 }),
    );
    assert_eq!(
        st,
        reqwest::StatusCode::OK,
        "PUT owner rate-limit must succeed for v0.11 client",
    );

    // Wait for the push to land on the client.
    assert!(
        wait_owner_cap_applied(&_client, OWNER_ID, Duration::from_secs(5)),
        "client never logged control.owner_rate_limit_set for {OWNER_ID}"
    );

    // First conn admits + round-trips; second conn closed without echo.
    assert_cap_enforced(listen_port);

    // The cumulative reject counter must tick (StatsReport runs on a
    // 1 s tick; allow a generous window for the next report).
    let n = wait_owner_concurrent_reject(&metrics, CLIENT_NAME, OWNER_ID);
    assert!(n >= 1, "OwnerConcurrent reject counter must be >= 1");
}

#[test]
fn t082_owner_cap_survives_server_and_client_restart() {
    // Pre-allocate ports so the bundle issued in pass-1 stays valid
    // after the server is restarted in pass-2. There is a tiny race
    // between releasing the ephemeral port and the server binding it,
    // but it is identical to the race every other e2e test that uses
    // `pick_free_port` relies on.
    let grpc_port = pick_free_port();
    let http_port = pick_free_port();

    // Pass 1 — bootstrap, push rule, set cap, prove enforcement, stop.
    let data_dir = tempfile::TempDir::new().expect("tempdir");

    let (mut server_v1, stderr_lines_v1) =
        spawn_server_with_fixed_ports(data_dir.path(), grpc_port, http_port);
    let listening_v1 = common::wait_for(Duration::from_secs(15), || {
        let lines = stderr_lines_v1.lock().unwrap();
        for line in lines.iter().rev() {
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let fields = v.get("fields")?;
            if fields.get("event").and_then(|x| x.as_str()) == Some("server.listening")
                && let (Some(grpc), Some(http), Some(metrics)) = (
                    fields.get("grpc").and_then(|x| x.as_str()),
                    fields.get("operator_http").and_then(|x| x.as_str()),
                    fields.get("metrics").and_then(|x| x.as_str()),
                )
            {
                return Some((grpc.to_string(), http.to_string(), metrics.to_string()));
            }
        }
        None
    })
    .expect("server v1 never listened");
    let (grpc_v1, http_v1, metrics_v1) = listening_v1;
    // Sanity: the gRPC listener really used our pinned port.
    let grpc_v1_socket: SocketAddr = grpc_v1.parse().expect("grpc addr parses");
    assert_eq!(grpc_v1_socket.port(), grpc_port, "v1 used pinned grpc port");

    let bundle = common::provision_client_http(&http_v1, CLIENT_NAME);
    let bundle_path: PathBuf = bundle.clone();
    let client_v1 = common::spawn_client(&bundle, &[]);
    assert!(
        wait_connected(&http_v1, CLIENT_NAME, Duration::from_secs(10)),
        "v1: client never connected"
    );
    thread::sleep(Duration::from_millis(500));

    let (echo_host, echo_port) = spawn_echo();
    let listen_port = pick_free_port();
    let (status, body) = common::push_rule_http_targets(
        &http_v1,
        CLIENT_NAME,
        listen_port,
        &[(echo_host.as_str(), echo_port)],
        None,
        Some(2),
    );
    assert!(status.is_success(), "v1 push failed: {status} body={body}");
    let (st, _resp) = put_owner_rate_limit(
        &http_v1,
        CLIENT_NAME,
        OWNER_ID,
        &serde_json::json!({ "concurrent_connections": 1 }),
    );
    assert_eq!(st, reqwest::StatusCode::OK, "v1 PUT owner cap");
    assert!(
        wait_owner_cap_applied(&client_v1, OWNER_ID, Duration::from_secs(5)),
        "v1: client never applied owner cap"
    );
    assert_cap_enforced(listen_port);
    let _ = wait_owner_concurrent_reject(&metrics_v1, CLIENT_NAME, OWNER_ID);

    // Stop the CLIENT first (drops in-memory rate-limit state), then
    // the server. The handles' Drop kills the child and reaps.
    drop(client_v1);
    let _ = server_v1.kill();
    let _ = server_v1.wait();
    // Brief pause for the SQLite WAL flush + lockfile release.
    thread::sleep(Duration::from_millis(300));

    // Pass 2 — same data dir, same pinned ports, same bundle.
    let (mut server_v2, stderr_lines_v2) =
        spawn_server_with_fixed_ports(data_dir.path(), grpc_port, http_port);
    let listening_v2 = common::wait_for(Duration::from_secs(15), || {
        let lines = stderr_lines_v2.lock().unwrap();
        for line in lines.iter().rev() {
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let fields = v.get("fields")?;
            if fields.get("event").and_then(|x| x.as_str()) == Some("server.listening")
                && let (Some(grpc), Some(http), Some(metrics)) = (
                    fields.get("grpc").and_then(|x| x.as_str()),
                    fields.get("operator_http").and_then(|x| x.as_str()),
                    fields.get("metrics").and_then(|x| x.as_str()),
                )
            {
                return Some((grpc.to_string(), http.to_string(), metrics.to_string()));
            }
        }
        None
    })
    .expect("server v2 never listened");
    let (_grpc_v2, http_v2, metrics_v2) = listening_v2;

    // The persisted cap must still be in SQLite immediately, before any
    // client has reconnected.
    let (g_st, g_body) = get_owner_rate_limit(&http_v2, CLIENT_NAME, OWNER_ID);
    assert_eq!(g_st, reqwest::StatusCode::OK, "GET owner cap post-restart");
    assert_eq!(
        g_body
            .get("rate_limit")
            .and_then(|rl| rl.get("concurrent_connections"))
            .and_then(serde_json::Value::as_u64),
        Some(1),
        "concurrent_connections must round-trip from SQLite: {g_body}",
    );

    // Reattach the client with the original bundle.
    let client_v2 = common::spawn_client(&bundle_path, &[]);
    assert!(
        wait_connected(&http_v2, CLIENT_NAME, Duration::from_secs(15)),
        "v2: client never reconnected"
    );
    // Welcome-replay (T029 in 011) pushes the persisted owner cap back.
    assert!(
        wait_owner_cap_applied(&client_v2, OWNER_ID, Duration::from_secs(10)),
        "v2: welcome-replay never re-pushed owner cap"
    );

    // 008-sqlite-storage T028: rules are hydrated from SQLite on
    // server restart. The hydrated rule attempts to re-bind the
    // original `listen_port`, but the kernel may still have it in
    // TIME_WAIT from the just-closed conn_a/conn_b sockets, so the
    // hydrated rule frequently lands in `Failed { port_in_use }` and
    // the client never gets a listener back on that port. The
    // persistence we care about here is the **owner cap envelope**,
    // not the rule. Drop the old rule (if hydrated) and push a fresh
    // single-target rule on a new ephemeral port — the welcome-replay
    // OwnerRateLimitUpdate is what we want to drive the new rule's
    // owner-cap binding.
    {
        let existing = common::list_rules_http(&http_v2, Some(CLIENT_NAME));
        if let Some(arr) = existing.as_array() {
            for r in arr {
                if let Some(id) = r.get("id").and_then(serde_json::Value::as_u64) {
                    let _ = common::remove_rule_http(&http_v2, id);
                }
            }
        }
    }
    let listen_port_v2 = pick_free_port();
    let (status, body) = common::push_rule_http_targets(
        &http_v2,
        CLIENT_NAME,
        listen_port_v2,
        &[(echo_host.as_str(), echo_port)],
        None,
        Some(2),
    );
    assert!(status.is_success(), "v2 push failed: {status} body={body}");
    // Wait for the kernel listener to actually accept.
    let bound = common::wait_for(Duration::from_secs(10), || {
        TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, listen_port_v2)),
            Duration::from_millis(200),
        )
        .ok()
        .map(drop)
    });
    assert!(
        bound.is_some(),
        "v2 listener never accepted on port {listen_port_v2}"
    );
    let listen_port = listen_port_v2;

    // Cap is still enforced after restart.
    assert_cap_enforced(listen_port);
    let _ = wait_owner_concurrent_reject(&metrics_v2, CLIENT_NAME, OWNER_ID);

    // Explicit teardown for clarity (Drop would handle it anyway).
    drop(client_v2);
    let _ = server_v2.kill();
    let _ = server_v2.wait();
    let _ = stderr_lines_v1;
    let _ = stderr_lines_v2;
    // Hold the tempdir alive until both servers are reaped.
    drop(data_dir);
}
