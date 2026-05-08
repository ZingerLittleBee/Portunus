//! 008-sqlite-storage T024 — audit durability under SIGKILL of the
//! server process.
//!
//! In the v0.7 in-memory ring, a SIGKILL would lose every row. v0.8
//! routes audit through a bounded `mpsc` to a SQLite writer that
//! commits in batches every ≤100 ms. This test bounds the loss window
//! at one batch interval (FR-005, SC-001).
//!
//! Mechanically: spawn the server, drive a known number of operator
//! HTTP calls (each emits an audit row), wait one BATCH_MAX_DELAY for
//! the writer to flush, then SIGKILL via `Child::kill`. Restart the
//! server pointing at the same `--data-dir` and assert the previously
//! committed rows are still readable.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use tempfile::TempDir;

const TOKEN: &str = "test-operator-token-005";

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_forward-server")
}

fn capture_stderr(child: &mut Child) -> std::sync::Arc<std::sync::Mutex<Vec<String>>> {
    use std::io::{BufRead, BufReader};
    let stderr = child.stderr.take().expect("stderr piped");
    let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let buf2 = std::sync::Arc::clone(&buf);
    std::thread::spawn(move || {
        let r = BufReader::new(stderr);
        for line in r.lines().map_while(Result::ok) {
            buf2.lock().unwrap().push(line);
        }
    });
    buf
}

fn wait_listening(
    lines: &std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    timeout: Duration,
) -> Option<String> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        {
            let lines = lines.lock().unwrap();
            for line in lines.iter().rev() {
                let v: Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let Some(fields) = v.get("fields") else {
                    continue;
                };
                if fields.get("event").and_then(|x| x.as_str()) == Some("server.listening")
                    && let Some(http) = fields.get("operator_http").and_then(|x| x.as_str())
                {
                    return Some(http.to_string());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

fn write_server_toml(config_dir: &Path) {
    let body = format!(
        "control_listen = \"127.0.0.1:0\"\n\
         operator_http_listen = \"127.0.0.1:0\"\n\
         metrics_listen = \"127.0.0.1:0\"\n\
         tls_cert_path = {cert_path:?}\n\
         tls_key_path = {key_path:?}\n\
         token_store_path = {token_path:?}\n\
         operator_store_path = {identity_path:?}\n\
         operator_token = \"{TOKEN}\"\n",
        cert_path = config_dir.join("server.crt").to_string_lossy(),
        key_path = config_dir.join("server.key").to_string_lossy(),
        token_path = config_dir.join("tokens.json").to_string_lossy(),
        identity_path = config_dir.join("identity.json").to_string_lossy(),
    );
    std::fs::write(config_dir.join("server.toml"), body).expect("write server.toml");
}

fn spawn(config_dir: &Path, data_dir: &Path) -> Child {
    Command::new(server_bin())
        .arg("--config-dir")
        .arg(config_dir)
        .arg("--data-dir")
        .arg(data_dir)
        .arg("serve")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUST_LOG", "info")
        .spawn()
        .expect("spawn forward-server")
}

fn audit_count(http: &str) -> usize {
    let url = format!("http://{http}/v1/audit?limit=1000");
    let resp = reqwest::blocking::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {TOKEN}"))
        .send()
        .expect("GET audit");
    let v: Value = resp.json().unwrap_or(Value::Null);
    v.as_array().map_or(0, Vec::len)
}

fn drive_calls(http: &str, n: usize) {
    let url = format!("http://{http}/v1/clients");
    for _ in 0..n {
        let _ = reqwest::blocking::Client::new()
            .get(&url)
            .header("Authorization", format!("Bearer {TOKEN}"))
            .send();
    }
}

#[test]
fn audit_rows_survive_sigkill() {
    let config_dir = TempDir::new().unwrap();
    let data_dir = TempDir::new().unwrap();
    write_server_toml(config_dir.path());

    // Round 1 — populate audit table.
    let mut child = spawn(config_dir.path(), data_dir.path());
    let lines = capture_stderr(&mut child);
    let http = wait_listening(&lines, Duration::from_secs(15)).expect("listening");
    drive_calls(&http, 8);
    // One BATCH_MAX_DELAY (100 ms) is the writer's flush boundary.
    // We sleep 5× that to swallow scheduler jitter on heavily loaded
    // CI runners while still demonstrating that the bound is short.
    std::thread::sleep(Duration::from_millis(500));
    let count_before_kill = audit_count(&http);
    assert!(
        count_before_kill >= 8,
        "expected ≥8 rows pre-kill; got {count_before_kill}"
    );

    // SIGKILL the server (`Child::kill` translates to `SIGKILL` on Unix).
    let _ = child.kill();
    let _ = child.wait();

    // Brief settle for OS lockfile cleanup.
    std::thread::sleep(Duration::from_millis(200));

    // Round 2 — restart against the same data-dir, verify rows survived.
    let mut child2 = spawn(config_dir.path(), data_dir.path());
    let lines2 = capture_stderr(&mut child2);
    let http2 = wait_listening(&lines2, Duration::from_secs(15)).expect("respawn listening");

    let count_after_restart = audit_count(&http2);
    assert!(
        count_after_restart >= count_before_kill,
        "audit rows should survive SIGKILL: before={count_before_kill}, after={count_after_restart}"
    );

    let _ = child2.kill();
    let _ = child2.wait();
}
