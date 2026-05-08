//! T025 — `provision-client` integration test.
//!
//! Covers:
//! - First invocation writes a `.bundle.json` matching the schema in
//!   `data-model.md` and exits 0.
//! - Second invocation for the same name returns exit 2
//!   (`client_already_exists`).

use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_forward-server")
}

#[test]
fn provision_writes_bundle_then_rejects_duplicate() {
    let config_dir = TempDir::new().expect("config tempdir");
    let data_dir = TempDir::new().expect("data tempdir");
    let bundle_dir = TempDir::new().expect("bundle tempdir");
    let bundle_path = bundle_dir.path().join("edge-01.bundle.json");

    // First invocation: success.
    let out = Command::new(server_bin())
        .arg("--config-dir")
        .arg(config_dir.path())
        .arg("--data-dir")
        .arg(data_dir.path())
        .arg("provision-client")
        .arg("edge-01")
        .arg("--out")
        .arg(&bundle_path)
        .output()
        .expect("run provision-client");
    assert!(
        out.status.success(),
        "first provision failed: status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(bundle_path.exists(), "bundle file not written");

    // Schema check (data-model.md § CredentialBundle).
    let raw = std::fs::read_to_string(&bundle_path).expect("read bundle");
    let v: Value = serde_json::from_str(&raw).expect("bundle is valid JSON");
    assert_eq!(v["version"], 1);
    assert_eq!(v["client_name"], "edge-01");
    assert!(v["server_endpoint"].as_str().unwrap().contains(':'));
    let fp = v["server_cert_sha256"].as_str().expect("fingerprint hex");
    assert_eq!(fp.len(), 64, "sha256 fingerprint should be 64 hex chars");
    assert!(
        fp.chars().all(|c| c.is_ascii_hexdigit()),
        "fingerprint must be hex"
    );
    assert!(
        v["server_cert_pem"]
            .as_str()
            .unwrap()
            .contains("BEGIN CERTIFICATE"),
        "bundle must carry server cert PEM"
    );
    let token = v["token"].as_str().expect("token string");
    assert!(token.len() >= 32, "token too short: {}", token.len());

    // 0600 permission on the bundle file.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&bundle_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "bundle should be 0600, got {mode:o}");
    }

    // Second invocation: should fail with exit code 2 (client_already_exists).
    let dup = Command::new(server_bin())
        .arg("--config-dir")
        .arg(config_dir.path())
        .arg("--data-dir")
        .arg(data_dir.path())
        .arg("provision-client")
        .arg("edge-01")
        .arg("--out")
        .arg(bundle_dir.path().join("edge-01.dup.json"))
        .output()
        .expect("run duplicate provision-client");
    let code = dup.status.code().expect("exit code");
    assert_eq!(
        code,
        2,
        "duplicate provision should exit 2, got {code}; stderr={}",
        String::from_utf8_lossy(&dup.stderr)
    );
    let stderr = String::from_utf8_lossy(&dup.stderr);
    assert!(
        stderr.contains("client_already_exists"),
        "stderr should mention client_already_exists, got: {stderr}"
    );
}

#[test]
fn provision_rejects_invalid_name() {
    let config_dir = TempDir::new().expect("config tempdir");
    let data_dir = TempDir::new().expect("data tempdir");
    let out = Command::new(server_bin())
        .arg("--config-dir")
        .arg(config_dir.path())
        .arg("--data-dir")
        .arg(data_dir.path())
        .arg("provision-client")
        .arg("Edge-01") // uppercase forbidden by ClientName rules
        .output()
        .expect("run provision-client");
    let code = out.status.code().expect("exit code");
    assert_eq!(code, 3, "invalid name should exit 3, got {code}");
}
