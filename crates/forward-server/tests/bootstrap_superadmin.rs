//! T029 (005-multi-user-rbac, US2) — `bootstrap-superadmin` + `gen-token`
//! CLI behaviour, exercised as child processes via `assert_cmd`.

use std::process::Command;

use tempfile::TempDir;

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_forward-server")
}

#[test]
fn bootstrap_superadmin_writes_identity_and_prints_token_once() {
    let dir = TempDir::new().expect("tempdir");
    let out = Command::new(server_bin())
        .arg("--config-dir")
        .arg(dir.path())
        .arg("bootstrap-superadmin")
        .arg("--name")
        .arg("ops")
        .output()
        .expect("run bootstrap-superadmin");
    assert!(
        out.status.success(),
        "first bootstrap should exit 0; status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("superadmin user_id=_superadmin token="),
        "stdout shape mismatch: {stdout}"
    );
    let token = stdout.split("token=").nth(1).expect("token segment").trim();
    assert!(
        token.len() >= 32 && token.len() <= 64,
        "token length out of band: `{token}`"
    );
    let identity_path = dir.path().join("identity.json");
    assert!(identity_path.exists(), "identity.json should be written");
    let body = std::fs::read_to_string(&identity_path).expect("read identity.json");
    assert!(body.contains("\"_superadmin\""), "{body}");
}

#[test]
fn bootstrap_superadmin_second_run_returns_already_bootstrapped() {
    let dir = TempDir::new().expect("tempdir");
    // First run.
    let first = Command::new(server_bin())
        .arg("--config-dir")
        .arg(dir.path())
        .arg("bootstrap-superadmin")
        .output()
        .expect("first");
    assert!(first.status.success());

    // Second run.
    let second = Command::new(server_bin())
        .arg("--config-dir")
        .arg(dir.path())
        .arg("bootstrap-superadmin")
        .output()
        .expect("second");
    assert!(!second.status.success(), "second run must fail");
    assert_eq!(
        second.status.code(),
        Some(2),
        "exit code must be 2 (already_bootstrapped); got {:?}; stderr={}",
        second.status.code(),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("already_bootstrapped"),
        "stderr should mention already_bootstrapped"
    );
}

#[test]
fn gen_token_prints_one_token_to_stdout() {
    let out = Command::new(server_bin())
        .arg("gen-token")
        .output()
        .expect("run gen-token");
    assert!(out.status.success(), "gen-token must exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let token = stdout.trim();
    assert!(
        token.len() >= 32 && !token.contains(' '),
        "expected one URL-safe-base64 token; got `{stdout}`"
    );
    // Two consecutive runs MUST produce different tokens.
    let out2 = Command::new(server_bin())
        .arg("gen-token")
        .output()
        .expect("run gen-token #2");
    let token2 = String::from_utf8_lossy(&out2.stdout).trim().to_string();
    assert_ne!(token, token2, "gen-token must be non-deterministic");
}
