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
fn enroll_client_accepts_friendly_display_name() {
    // 015-client-stable-id: client_name is a free-form display field now.
    // Uppercase, spaces, and dots are all valid (previously rejected by the
    // strict DNS-label rule). Identity is the system-generated client_id.
    let data_dir = TempDir::new().expect("data tempdir");
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data_dir.path())
        .arg("enroll-client")
        .arg("Acme Prod – East.1")
        .output()
        .expect("run enroll-client");

    assert!(
        out.status.success(),
        "friendly display name should be accepted: status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("client_name=Acme Prod – East.1"));
}

#[test]
fn enroll_client_rejects_empty_name() {
    // Only empty / whitespace-only / control-char / >255-byte names are
    // rejected after the 015 relaxation (exit 3 == invalid client name).
    let data_dir = TempDir::new().expect("data tempdir");
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data_dir.path())
        .arg("enroll-client")
        .arg("   ")
        .output()
        .expect("run enroll-client");

    let code = out.status.code().expect("exit code");
    assert_eq!(code, 3, "whitespace-only name should exit 3, got {code}");
}
