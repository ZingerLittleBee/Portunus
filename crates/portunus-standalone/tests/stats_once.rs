//! e2e: spawn daemon, then run `portunus-standalone stats --once`,
//! parse the returned JSON, assert shape.

use std::process::{Command, Stdio};
use std::time::Duration;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::tempdir;

#[test]
fn stats_once_prints_hello_and_snapshot() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("stats.sock");
    let config = dir.path().join("portunus.toml");
    std::fs::write(
        &config,
        format!(
            r#"
[stats]
socket_path = "{}"
refresh_ms = 250

[[rule]]
name = "smoke"
protocol = "tcp"
listen_port = 19191
target = "127.0.0.1:7"
"#,
            sock.display()
        ),
    )
    .unwrap();

    // Spawn daemon.
    let mut daemon = Command::cargo_bin("portunus-standalone")
        .unwrap()
        .arg("--config")
        .arg(&config)
        .arg("--log-level")
        .arg("warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Wait for socket to appear (poll up to 5 s).
    let mut waited = 0;
    while !sock.exists() && waited < 50 {
        std::thread::sleep(Duration::from_millis(100));
        waited += 1;
    }
    assert!(sock.exists(), "stats socket did not appear");

    // Run `stats --once`.
    let out = Command::cargo_bin("portunus-standalone")
        .unwrap()
        .arg("stats")
        .arg("--socket")
        .arg(&sock)
        .arg("--once")
        .output()
        .unwrap();
    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(
        out.status.success(),
        "stats --once failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("output must be valid JSON");
    assert_eq!(json["hello"]["v"], 1);
    assert_eq!(json["hello"]["rules"][0]["name"], "smoke");
    assert!(json["snapshot"]["uptime_ms"].as_u64().is_some());
    assert!(json["snapshot"]["r"][0]["id"].as_str().is_some());
}
