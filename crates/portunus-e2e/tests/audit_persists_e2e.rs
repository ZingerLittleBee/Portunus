//! 008-sqlite-storage T028 — full server↔client smoke covering audit
//! retention across a server restart.
//!
//! 1. Spawn the server (fresh `--data-dir`).
//! 2. Drive a few *denied* operator HTTP calls — each generates an
//!    audit row via the `auth_layer` middleware. (Successful reads are
//!    no longer audited; denials always are, so they are the reliable
//!    way to seed audit rows over real HTTP.)
//! 3. Snapshot `GET /v1/audit?limit=100`.
//! 4. SIGTERM the server; respawn with the SAME `--data-dir`.
//! 5. Re-snapshot `GET /v1/audit?limit=100` and assert the pre-restart
//!    rows are still there (durable WAL recovery).

#![allow(dead_code)]

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

fn audit_get(http: &str) -> Value {
    let url = format!("http://{http}/v1/audit?limit=100");
    let resp = reqwest::blocking::Client::new()
        .get(url)
        .header("Authorization", "Bearer test-operator-token-005")
        .send()
        .expect("GET /v1/audit");
    assert!(resp.status().is_success(), "status: {}", resp.status());
    resp.json().expect("audit json")
}

/// Fire a request with a bogus bearer token. The auth_layer denies it
/// (401) and records a `deny` audit row — our reliable seed now that
/// successful reads are not audited.
fn denied_request(http: &str) {
    let url = format!("http://{http}/v1/clients");
    let _ = reqwest::blocking::Client::new()
        .get(url)
        .header("Authorization", "Bearer not-a-valid-token")
        .send();
}

/// Spawn `portunus-server serve` against an existing data dir
/// (used to restart while reusing v0.8 SQLite state).
fn respawn_server(data_dir: &Path) -> std::process::Child {
    let bin = common::workspace_bin("portunus-server");
    Command::new(&bin)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("serve")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUST_LOG", "info")
        .spawn()
        .expect("respawn portunus-server")
}

#[test]
fn audit_rows_survive_server_restart() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(15))
        .expect("server listening");

    // Drive five denied calls. Each one walks the auth layer and
    // produces a `deny` audit row; we don't care about the response
    // bodies, only that an audit row is recorded.
    for _ in 0..5 {
        denied_request(&http);
    }
    // Allow the durable writer (BATCH_MAX_DELAY = 100 ms) to flush.
    std::thread::sleep(Duration::from_millis(500));

    let before = audit_get(&http);
    let before_rows = before.as_array().expect("array").len();
    assert!(
        before_rows >= 5,
        "expected ≥5 audit rows pre-restart; got {before_rows}"
    );

    // Kill the running child but keep `server` alive — its TempDirs
    // back the data we want the second server to read.
    let mut server = server;
    let data_dir = server.data_dir.path().to_path_buf();
    let _ = server.child.kill();
    let _ = server.child.wait();

    // Brief pause for filesystem flush + lockfile release.
    std::thread::sleep(Duration::from_millis(200));

    let mut child2 = respawn_server(&data_dir);
    // Re-attach a stderr capture so we can wait for `server.listening`.
    let stderr = child2.stderr.take().expect("stderr piped");
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let lines2 = std::sync::Arc::clone(&lines);
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader};
        let r = BufReader::new(stderr);
        for line in r.lines().map_while(Result::ok) {
            lines2.lock().unwrap().push(line);
        }
    });

    // Wait for the second server to advertise its operator HTTP port.
    let http2 = common::wait_for(Duration::from_secs(15), || {
        let lines = lines.lock().unwrap();
        for line in lines.iter().rev() {
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let fields = v.get("fields")?;
            if fields.get("event").and_then(|x| x.as_str()) == Some("server.listening")
                && let Some(http) = fields.get("operator_http").and_then(|x| x.as_str())
            {
                return Some(http.to_string());
            }
        }
        None
    })
    .expect("respawned server listening");

    let after = audit_get(&http2);
    let after_rows = after.as_array().expect("array").len();
    assert!(
        after_rows >= before_rows,
        "audit rows should survive restart: before={before_rows}, after={after_rows}"
    );

    let _ = child2.kill();
    let _ = child2.wait();
    // server (with its TempDirs) drops here; the second child has
    // already exited so the lock release is clean.
    drop(server);
}
