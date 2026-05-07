//! Common helpers shared across e2e test files.
//!
//! - [`spawn_server`] launches the `forward-server` binary against a fresh
//!   temp config dir.
//! - [`spawn_client`] launches the `forward-client` binary against a bundle
//!   path produced via `provision-client`.
//!
//! Both helpers return a handle that kills the child on drop, so tests can
//! assert on side-effects without leaking processes.

#![allow(dead_code)]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn fresh_tempdir(label: &str) -> TempDir {
    TempDir::new().unwrap_or_else(|e| panic!("tempdir for {label}: {e}"))
}

/// Locate a workspace binary built by `cargo test` without relying on
/// `CARGO_BIN_EXE_*` env vars (which Cargo only injects for binaries that
/// belong to the *current* package, not workspace siblings).
pub fn workspace_bin(name: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR points to forward-e2e/. Walk up to the workspace
    // root, then descend into target/<profile>/<name>.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root resolvable")
        .to_path_buf();
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map_or_else(|_| workspace_root.join("target"), PathBuf::from);
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    // Tests run under the same profile that built them; check both.
    for profile in ["debug", "release"] {
        let candidate = target_dir.join(profile).join(&exe);
        if candidate.exists() {
            return candidate;
        }
    }
    // Last resort: the binary may not have been built yet (e.g., cross-crate
    // dev-dep on a `[[bin]]` target doesn't trigger a build). Build it now.
    let status = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--quiet")
        .arg("-p")
        .arg(name)
        .arg("--bin")
        .arg(name)
        .status()
        .unwrap_or_else(|e| panic!("cargo build --bin {name} failed to launch: {e}"));
    assert!(status.success(), "cargo build --bin {name} failed");
    let candidate = target_dir.join("debug").join(&exe);
    assert!(
        candidate.exists(),
        "binary still missing after build: {}",
        candidate.display()
    );
    candidate
}

fn cmd_for(name: &str) -> Command {
    Command::new(workspace_bin(name))
}

pub struct ServerHandle {
    pub child: Child,
    pub config_dir: TempDir,
    pub stderr_lines: Arc<Mutex<Vec<String>>>,
}

impl ServerHandle {
    /// Search the captured stderr lines for the most recent value of `field`
    /// in the structured event whose `event` field equals `event_name`.
    pub fn read_field(&self, event_name: &str, field: &str) -> Option<String> {
        let lines = self.stderr_lines.lock().unwrap();
        for line in lines.iter().rev() {
            let v: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // tracing-subscriber's JSON layer puts the message under `fields`.
            let fields = v.get("fields")?;
            if fields.get("event").and_then(|x| x.as_str()) == Some(event_name)
                && let Some(val) = fields.get(field).and_then(|x| x.as_str())
            {
                return Some(val.to_string());
            }
        }
        None
    }

    /// Block (with timeout) until the server logs `server.listening`, then
    /// return `(grpc_addr, operator_http_addr)`.
    pub fn wait_listening(&self, timeout: Duration) -> Option<(String, String)> {
        wait_for(timeout, || {
            let grpc = self.read_field("server.listening", "grpc")?;
            let http = self.read_field("server.listening", "operator_http")?;
            Some((grpc, http))
        })
    }

    /// Same as [`Self::wait_listening`] but additionally returns the
    /// `metrics` endpoint advertised in the `server.listening` event.
    pub fn wait_listening_full(&self, timeout: Duration) -> Option<(String, String, String)> {
        wait_for(timeout, || {
            let grpc = self.read_field("server.listening", "grpc")?;
            let http = self.read_field("server.listening", "operator_http")?;
            let metrics = self.read_field("server.listening", "metrics")?;
            Some((grpc, http, metrics))
        })
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn capture_stderr(stderr: ChildStderr) -> Arc<Mutex<Vec<String>>> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let buf_clone = Arc::clone(&buf);
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            buf_clone.lock().unwrap().push(line);
        }
    });
    buf
}

pub struct ClientHandle {
    pub child: Child,
    pub stderr_lines: Arc<Mutex<Vec<String>>>,
}

impl ClientHandle {
    pub fn stderr_contains(&self, needle: &str) -> bool {
        let lines = self.stderr_lines.lock().unwrap();
        lines.iter().any(|l| l.contains(needle))
    }
}

impl Drop for ClientHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Launch `forward-server serve` against a fresh temp config dir.
/// Caller is responsible for waiting for readiness (e.g., by polling the
/// operator HTTP listener).
pub fn spawn_server(extra_args: &[&str]) -> ServerHandle {
    let config_dir = fresh_tempdir("server config");
    let mut cmd = cmd_for("forward-server");
    cmd.arg("--config-dir")
        .arg(config_dir.path())
        .arg("serve")
        .args(extra_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUST_LOG", "info");
    let mut child = cmd.spawn().expect("spawn forward-server");
    let stderr = child.stderr.take().expect("server stderr piped");
    let stderr_lines = capture_stderr(stderr);
    ServerHandle {
        child,
        config_dir,
        stderr_lines,
    }
}

/// Launch `forward-client --bundle <path>`.
pub fn spawn_client(bundle_path: &Path, extra_args: &[&str]) -> ClientHandle {
    let mut cmd = cmd_for("forward-client");
    cmd.arg("--bundle")
        .arg(bundle_path)
        .args(extra_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUST_LOG", "info");
    let mut child = cmd.spawn().expect("spawn forward-client");
    let stderr = child.stderr.take().expect("client stderr piped");
    let stderr_lines = capture_stderr(stderr);
    ClientHandle {
        child,
        stderr_lines,
    }
}

/// Run `forward-server provision-client <name>` synchronously; return the
/// path to the generated bundle file.
pub fn provision_client(config_dir: &Path, name: &str) -> PathBuf {
    provision_client_with_endpoint(config_dir, name, None)
}

/// Same as [`provision_client`] but lets the caller override the endpoint
/// that gets baked into the bundle (needed because the running server binds
/// to an OS-assigned port).
pub fn provision_client_with_endpoint(
    config_dir: &Path,
    name: &str,
    advertised: Option<&str>,
) -> PathBuf {
    let out = fresh_tempdir("bundle out").keep();
    let bundle = out.join(format!("{name}.bundle.json"));
    let mut cmd = cmd_for("forward-server");
    cmd.arg("--config-dir").arg(config_dir);
    if let Some(ep) = advertised {
        cmd.arg("--advertised-endpoint").arg(ep);
    }
    cmd.arg("provision-client")
        .arg(name)
        .arg("--out")
        .arg(&bundle);
    let status = cmd.status().expect("run provision-client");
    assert!(status.success(), "provision-client failed: {status:?}");
    bundle
}

/// Hit the *running* server's loopback HTTP `GET /v1/clients` and return the
/// parsed JSON array.  We must use the live server because connected-client
/// state is in-memory — the CLI's offline `list-clients` would always show
/// `connected: false`.
pub fn list_clients_http(operator_http_addr: &str) -> serde_json::Value {
    let url = format!("http://{operator_http_addr}/v1/clients");
    let resp = reqwest::blocking::get(&url).expect("GET /v1/clients");
    assert!(
        resp.status().is_success(),
        "list-clients HTTP failed: {resp:?}"
    );
    resp.json().expect("parse JSON body")
}

/// Provision a client via the running server's HTTP API. Returns the path to
/// a written `<name>.bundle.json` alongside the parsed bundle. This is the
/// workflow tests want: the offline CLI mutates the on-disk token store but
/// not the live server's in-memory cache, so the resulting token would be
/// rejected. Going through HTTP keeps both views consistent.
pub fn provision_client_http(operator_http_addr: &str, name: &str) -> PathBuf {
    let url = format!("http://{operator_http_addr}/v1/clients");
    let resp = reqwest::blocking::Client::new()
        .post(&url)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .expect("POST /v1/clients");
    assert!(
        resp.status().is_success(),
        "provision HTTP failed: {resp:?}"
    );
    let bundle_value: serde_json::Value = resp.json().expect("parse bundle JSON");
    let out_dir = fresh_tempdir("bundle out").keep();
    let path = out_dir.join(format!("{name}.bundle.json"));
    std::fs::write(&path, serde_json::to_vec_pretty(&bundle_value).unwrap()).expect("write bundle");
    path
}

/// Push a rule via the running server's HTTP API. Returns the
/// `(http_status, parsed_body)` so the caller can assert specifics.
pub fn push_rule_http(
    operator_http_addr: &str,
    client: &str,
    listen_port: u16,
    target_host: &str,
    target_port: u16,
    ack_timeout_secs: Option<u64>,
) -> (reqwest::StatusCode, serde_json::Value) {
    push_rule_http_full(
        operator_http_addr,
        client,
        listen_port,
        None,
        target_host,
        target_port,
        None,
        ack_timeout_secs,
    )
}

/// Range-aware push (002-port-range-forward). When `listen_port_end` and
/// `target_port_end` are `Some`, both fields are sent — the server
/// enforces co-presence and equal length. Backwards-compatible with
/// the pre-002 `push_rule_http` shape: pass `None` for both ends to
/// get the v0.1.0 single-port body.
#[allow(clippy::too_many_arguments)]
pub fn push_rule_http_full(
    operator_http_addr: &str,
    client: &str,
    listen_port: u16,
    listen_port_end: Option<u16>,
    target_host: &str,
    target_port: u16,
    target_port_end: Option<u16>,
    ack_timeout_secs: Option<u64>,
) -> (reqwest::StatusCode, serde_json::Value) {
    let url = format!("http://{operator_http_addr}/v1/rules");
    let mut body = serde_json::json!({
        "client": client,
        "listen_port": listen_port,
        "target_host": target_host,
        "target_port": target_port,
        "protocol": "tcp",
    });
    if let Some(end) = listen_port_end {
        body["listen_port_end"] = serde_json::Value::Number(end.into());
    }
    if let Some(end) = target_port_end {
        body["target_port_end"] = serde_json::Value::Number(end.into());
    }
    if let Some(secs) = ack_timeout_secs {
        body["ack_timeout_secs"] = serde_json::Value::Number(secs.into());
    }
    let resp = reqwest::blocking::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .expect("POST /v1/rules");
    let status = resp.status();
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    (status, body)
}

/// 003-domain-name-forward T037 helper: push with explicit
/// `prefer_ipv6` field. Single-port shape only (DNS rules in US3
/// don't exercise port ranges).
#[allow(dead_code)]
pub fn push_rule_http_with_prefer_ipv6(
    operator_http_addr: &str,
    client: &str,
    listen_port: u16,
    target_host: &str,
    target_port: u16,
    prefer_ipv6: Option<bool>,
    ack_timeout_secs: Option<u64>,
) -> (reqwest::StatusCode, serde_json::Value) {
    let url = format!("http://{operator_http_addr}/v1/rules");
    let mut body = serde_json::json!({
        "client": client,
        "listen_port": listen_port,
        "target_host": target_host,
        "target_port": target_port,
        "protocol": "tcp",
    });
    if let Some(v) = prefer_ipv6 {
        body["prefer_ipv6"] = serde_json::Value::Bool(v);
    }
    if let Some(secs) = ack_timeout_secs {
        body["ack_timeout_secs"] = serde_json::Value::Number(secs.into());
    }
    let resp = reqwest::blocking::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .expect("POST /v1/rules");
    let status = resp.status();
    let body: serde_json::Value = resp.json().unwrap_or(serde_json::Value::Null);
    (status, body)
}

pub fn remove_rule_http(operator_http_addr: &str, rule_id: u64) -> reqwest::StatusCode {
    let url = format!("http://{operator_http_addr}/v1/rules/{rule_id}");
    reqwest::blocking::Client::new()
        .delete(&url)
        .send()
        .expect("DELETE /v1/rules/{rule_id}")
        .status()
}

pub fn list_rules_http(operator_http_addr: &str, client_filter: Option<&str>) -> serde_json::Value {
    let mut url = format!("http://{operator_http_addr}/v1/rules");
    if let Some(c) = client_filter {
        url = format!("{url}?client={c}");
    }
    let resp = reqwest::blocking::get(&url).expect("GET /v1/rules");
    assert!(
        resp.status().is_success(),
        "list-rules HTTP failed: {resp:?}"
    );
    resp.json().expect("parse JSON body")
}

/// Revoke a client via the running server's HTTP API.
pub fn revoke_http(operator_http_addr: &str, name: &str) -> reqwest::StatusCode {
    let url = format!("http://{operator_http_addr}/v1/clients/{name}/revoke");
    let resp = reqwest::blocking::Client::new()
        .post(&url)
        .send()
        .expect("POST revoke");
    resp.status()
}

/// Fetch the rule-stats snapshot for `rule_id`. Returns `None` if the server
/// answers 404 (no `StatsReport` observed yet) so callers can spin in
/// `wait_for`.
pub fn rule_stats_http(operator_http_addr: &str, rule_id: u64) -> Option<serde_json::Value> {
    let url = format!("http://{operator_http_addr}/v1/rules/{rule_id}/stats");
    let resp = reqwest::blocking::get(&url).expect("GET /v1/rules/{rule_id}/stats");
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return None;
    }
    assert!(resp.status().is_success(), "rule-stats failed: {resp:?}");
    Some(resp.json().expect("parse JSON body"))
}

/// Same as [`rule_stats_http`] but appends `?per_port=true` to opt into
/// the per-port detail surfaced for range rules (002-port-range-forward,
/// T046). Returns `None` if the server has no cached stats yet.
pub fn rule_stats_http_per_port(
    operator_http_addr: &str,
    rule_id: u64,
) -> Option<serde_json::Value> {
    let url = format!("http://{operator_http_addr}/v1/rules/{rule_id}/stats?per_port=true");
    let resp = reqwest::blocking::get(&url).expect("GET /v1/rules/{rule_id}/stats?per_port=true");
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return None;
    }
    assert!(resp.status().is_success(), "rule-stats failed: {resp:?}");
    Some(resp.json().expect("parse JSON body"))
}

/// Fetch raw `/metrics` body from the metrics endpoint. Used by the
/// observability tests to assert collector shapes.
pub fn fetch_metrics_text(metrics_addr: &str) -> String {
    let url = format!("http://{metrics_addr}/metrics");
    let resp = reqwest::blocking::get(&url).expect("GET /metrics");
    assert!(resp.status().is_success(), "/metrics failed: {resp:?}");
    resp.text().expect("metrics body")
}

/// Run `forward-server revoke <name>`. Returns the exit status.
pub fn revoke(config_dir: &Path, name: &str) -> std::process::ExitStatus {
    cmd_for("forward-server")
        .arg("--config-dir")
        .arg(config_dir)
        .arg("revoke")
        .arg(name)
        .status()
        .expect("run revoke")
}

/// Block (with a timeout) until `predicate` returns `Some(value)`.
pub fn wait_for<T>(timeout: Duration, mut predicate: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(v) = predicate() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}
