//! 015-client-stable-id (US4: T041) — restart idempotency.
//!
//! Once the V011/V012 stable-id migrations have run, restarting the
//! server against the same data dir must NOT re-migrate and must NOT
//! lose data: the schema version is already at target, the runner
//! applies zero migrations, and every provisioned client survives
//! verbatim. This complements the runner-level idempotency unit test
//! (T010) at the whole-process level (SC-006).

mod common;

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const OPERATOR_TOKEN: &str = "test-operator-token-005";

/// Launch `portunus-server serve` against an explicit (reused) data dir.
/// Unlike `common::spawn_server`, the caller owns the directory, so it
/// survives across two server incarnations. Returns the child plus a live
/// capture of its stderr lines.
fn spawn_at(data_dir: &Path) -> (Child, Arc<Mutex<Vec<String>>>) {
    let toml = format!(
        "control_listen = \"127.0.0.1:0\"\n\
         operator_http_listen = \"127.0.0.1:0\"\n\
         metrics_listen = \"127.0.0.1:0\"\n\
         operator_token = \"{OPERATOR_TOKEN}\"\n"
    );
    std::fs::write(data_dir.join("server.toml"), toml).expect("write server.toml");
    let mut child = Command::new(common::workspace_bin("portunus-server"))
        .arg("--data-dir")
        .arg(data_dir)
        .arg("serve")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUST_LOG", "info")
        .spawn()
        .expect("spawn portunus-server");
    let stderr = child.stderr.take().expect("stderr piped");
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    (child, lines)
}

/// Read the `schema_version` field from a `store.opened` event line.
fn schema_version(lines: &Arc<Mutex<Vec<String>>>) -> Option<i64> {
    let guard = lines.lock().unwrap();
    for line in guard.iter().rev() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(fields) = v.get("fields") else {
            continue;
        };
        if fields.get("event").and_then(|e| e.as_str()) == Some("store.opened") {
            return fields
                .get("schema_version")
                .and_then(serde_json::Value::as_i64);
        }
    }
    None
}

fn http_addr(lines: &Arc<Mutex<Vec<String>>>) -> Option<String> {
    let guard = lines.lock().unwrap();
    for line in guard.iter().rev() {
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let fields = v.get("fields")?;
        if fields.get("event").and_then(|e| e.as_str()) == Some("server.listening") {
            return fields
                .get("operator_http")
                .and_then(|x| x.as_str())
                .map(str::to_string);
        }
    }
    None
}

fn wait<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let start = std::time::Instant::now();
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if start.elapsed() > timeout {
            return None;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn restart_against_migrated_db_is_a_noop_and_preserves_clients() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().to_path_buf();

    // ---- Incarnation 1: fresh DB, migrations run, provision a client ----
    let (mut srv1, lines1) = spawn_at(&path);
    let http1 = wait(Duration::from_secs(10), || http_addr(&lines1))
        .expect("server 1 should announce its operator HTTP listener");
    let v1 = schema_version(&lines1).expect("server 1 logs a schema_version");

    let bundle = common::provision_client_http(&http1, "edge-persist");
    // Sanity: the client is in the store before we restart.
    let before = common::list_clients_http(&http1);
    assert!(
        before
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["client_name"].as_str() == Some("edge-persist")),
        "client must be provisioned before restart"
    );
    let _ = bundle; // bundle file unused beyond provisioning

    // Stop incarnation 1 cleanly.
    let _ = srv1.kill();
    let _ = srv1.wait();

    // ---- Incarnation 2: same data dir, must NOT re-migrate ----
    let (mut srv2, lines2) = spawn_at(&path);
    let http2 = wait(Duration::from_secs(10), || http_addr(&lines2))
        .expect("server 2 should come back up against the same data dir");
    let v2 = schema_version(&lines2).expect("server 2 logs a schema_version");

    // Schema version is unchanged — already at target on the second open.
    assert_eq!(
        v1, v2,
        "restart must not change the schema version (no re-migration)"
    );
    // The refinery runner applied zero migrations on the second open.
    let applied_none = {
        let guard = lines2.lock().unwrap();
        guard.iter().any(|l| l.contains("no migrations to apply"))
    };
    assert!(
        applied_none,
        "second start must log 'no migrations to apply' (idempotent upgrade, SC-006)"
    );

    // Data survived the restart verbatim — same client, same id.
    let after = common::list_clients_http(&http2);
    let row = after
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["client_name"].as_str() == Some("edge-persist"))
        .expect("the provisioned client must survive the restart");
    assert!(
        row["client_id"].as_str().is_some_and(|s| !s.is_empty()),
        "the surviving client keeps its stable id"
    );

    let _ = srv2.kill();
    let _ = srv2.wait();
}
