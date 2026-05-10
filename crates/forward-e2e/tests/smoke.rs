//! Phase 2 smoke test: confirms the workspace can locate both binaries via
//! `assert_cmd` and that they at least respond to `--help`. The real e2e
//! coverage lands in T026/T027 (US1 happy path).

mod common;

use std::process::Command;

#[test]
fn forward_server_help_works() {
    let out = Command::new(common::workspace_bin("forward-server"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Portunus control plane"));
    assert!(stdout.contains("provision-client"));
}

#[test]
fn forward_client_help_works() {
    let out = Command::new(common::workspace_bin("forward-client"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Portunus edge client"));
    assert!(stdout.contains("--bundle"));
}
