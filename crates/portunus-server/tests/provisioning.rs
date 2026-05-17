//! `enroll-client` integration tests.

use std::process::Command;

use tempfile::TempDir;

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_portunus-server")
}

#[test]
fn enroll_client_prints_one_time_command() {
    let data_dir = TempDir::new().expect("data tempdir");

    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data_dir.path())
        .arg("enroll-client")
        .arg("edge-01")
        .arg("--address")
        .arg("edge.example.com")
        .output()
        .expect("run enroll-client");

    assert!(
        out.status.success(),
        "enroll-client failed: status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("client_name=edge-01"));
    assert!(stdout.contains("expires_at="));
    assert!(stdout.contains("portunus-client enroll 'portunus://"));
    assert!(stdout.contains("pin=sha256:"));
    assert!(stdout.contains("code="));
    assert!(stdout.contains("cert="));
}

#[test]
fn enroll_client_rejects_invalid_name() {
    let data_dir = TempDir::new().expect("data tempdir");
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data_dir.path())
        .arg("enroll-client")
        .arg("Edge-01")
        .output()
        .expect("run enroll-client");

    let code = out.status.code().expect("exit code");
    assert_eq!(code, 3, "invalid name should exit 3, got {code}");
}
