mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

fn pick_free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind ephemeral")
        .local_addr()
        .unwrap()
        .port()
}

fn capture_stderr(stderr: std::process::ChildStderr) -> Arc<Mutex<Vec<String>>> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let copy = Arc::clone(&buf);
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            copy.lock().unwrap().push(line);
        }
    });
    buf
}

struct ServerProc {
    child: Child,
    stderr_lines: Arc<Mutex<Vec<String>>>,
}

impl Drop for ServerProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl ServerProc {
    fn wait_listening(&self) -> (String, String) {
        common::wait_for(Duration::from_secs(5), || {
            let lines = self.stderr_lines.lock().unwrap();
            for line in lines.iter().rev() {
                let parsed: serde_json::Value = serde_json::from_str(line).ok()?;
                let fields = parsed.get("fields")?;
                if fields.get("event").and_then(|v| v.as_str()) == Some("server.listening") {
                    let grpc = fields.get("grpc")?.as_str()?.to_string();
                    let http = fields.get("operator_http")?.as_str()?.to_string();
                    return Some((grpc, http));
                }
            }
            None
        })
        .expect("server listening")
    }
}

fn write_server_toml(data_dir: &Path, control_port: u16, http_port: u16, metrics_port: u16) {
    let body = format!(
        "control_listen = \"127.0.0.1:{control_port}\"\n\
         operator_http_listen = \"127.0.0.1:{http_port}\"\n\
         metrics_listen = \"127.0.0.1:{metrics_port}\"\n\
         operator_token = \"{}\"\n",
        common::TEST_OPERATOR_TOKEN,
    );
    std::fs::write(data_dir.join("server.toml"), body).unwrap();
}

fn spawn_server(data_dir: &Path) -> ServerProc {
    let mut cmd = Command::new(common::workspace_bin("portunus-server"));
    cmd.arg("--data-dir")
        .arg(data_dir)
        .arg("serve")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUST_LOG", "info");
    let mut child = cmd.spawn().expect("spawn portunus-server");
    let stderr = child.stderr.take().expect("stderr piped");
    ServerProc {
        child,
        stderr_lines: capture_stderr(stderr),
    }
}

fn spawn_backend() -> (u16, mpsc::Receiver<Vec<u8>>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind backend");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for incoming in listener.incoming().flatten() {
            let tx = tx.clone();
            thread::spawn(move || {
                let mut sock = incoming;
                let mut buf = [0u8; 512];
                let n = sock.read(&mut buf).expect("backend read");
                tx.send(buf[..n].to_vec()).expect("send capture");
                let _ = sock.write_all(b"ok");
            });
        }
    });
    (port, rx)
}

fn wait_rule_active(http: &str, client_name: &str, listen_port: u16) -> serde_json::Value {
    let active = common::wait_for(Duration::from_secs(10), || {
        let rules = common::list_rules_http(http, Some(client_name));
        let rule = rules.as_array()?.iter().find(|rule| {
            rule.get("listen_port").and_then(serde_json::Value::as_u64)
                == Some(u64::from(listen_port))
                && rule.pointer("/state/kind").and_then(|v| v.as_str()) == Some("active")
        })?;
        Some(rule.clone())
    });
    active.unwrap_or_else(|| {
        let rules = common::list_rules_http(http, Some(client_name));
        panic!("persisted rule did not replay to active: {rules}");
    })
}

#[test]
fn proxy_protocol_rule_persists_across_server_restart_and_replays() {
    let data_dir = TempDir::new().unwrap();
    let control_port = pick_free_port();
    let http_port = pick_free_port();
    let metrics_port = pick_free_port();
    write_server_toml(data_dir.path(), control_port, http_port, metrics_port);

    let server = spawn_server(data_dir.path());
    let (_grpc, http) = server.wait_listening();

    let bundle = common::provision_client_http(&http, "edge-proxy-persist");
    let _client = common::spawn_client(&bundle, &[]);
    assert!(
        common::wait_for(Duration::from_secs(5), || {
            let arr = common::list_clients_http(&http);
            arr.as_array()?
                .iter()
                .find(|v| {
                    v.get("client_name").and_then(|n| n.as_str()) == Some("edge-proxy-persist")
                        && v.get("connected").and_then(serde_json::Value::as_bool) == Some(true)
                })
                .map(|_| ())
        })
        .is_some(),
        "client never connected to first server"
    );

    let (backend_port, backend_rx) = spawn_backend();
    let listen_port = pick_free_port();
    let url = format!("http://{http}/v1/rules");
    let resp = reqwest::blocking::Client::new()
        .post(&url)
        .header(
            "Authorization",
            format!("Bearer {}", common::TEST_OPERATOR_TOKEN),
        )
        .json(&serde_json::json!({
            "client": "edge-proxy-persist",
            "listen_port": listen_port,
            "protocol": "tcp",
            "targets": [
                { "host": "127.0.0.1", "port": backend_port, "priority": 0, "proxy_protocol": "v1" }
            ],
            "ack_timeout_secs": 2
        }))
        .send()
        .expect("push rule");
    assert!(
        resp.status().is_success(),
        "push failed: {:?}",
        resp.text().ok()
    );

    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).expect("connect v1");
    conn.write_all(b"ping").unwrap();
    let first = backend_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("first backend capture");
    assert!(
        String::from_utf8_lossy(&first).starts_with("PROXY TCP4 "),
        "expected PROXY v1 prelude, got {:?}",
        String::from_utf8_lossy(&first)
    );
    drop(conn);

    drop(server);

    let server = spawn_server(data_dir.path());
    let (_grpc, http) = server.wait_listening();
    assert!(
        common::wait_for(Duration::from_secs(10), || {
            let arr = common::list_clients_http(&http);
            arr.as_array()?
                .iter()
                .find(|v| {
                    v.get("client_name").and_then(|n| n.as_str()) == Some("edge-proxy-persist")
                        && v.get("connected").and_then(serde_json::Value::as_bool) == Some(true)
                })
                .map(|_| ())
        })
        .is_some(),
        "client never reconnected to restarted server"
    );

    let rule = wait_rule_active(&http, "edge-proxy-persist", listen_port);
    assert_eq!(rule["targets"][0]["proxy_protocol"], "v1");

    let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)).expect("connect v2");
    conn.write_all(b"pong").unwrap();
    let second = backend_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("second backend capture after restart");
    assert!(
        String::from_utf8_lossy(&second).starts_with("PROXY TCP4 "),
        "expected PROXY v1 prelude after restart, got {:?}",
        String::from_utf8_lossy(&second)
    );
}
