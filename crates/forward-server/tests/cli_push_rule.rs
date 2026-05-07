//! T014 (003-domain-name-forward) — `push-rule` CLI input-validation
//! integration test.
//!
//! The actual happy-path push (with a connected client + an active gRPC
//! channel) is exercised end-to-end by `crates/forward-e2e/tests/dns_smoke.rs`
//! (T015). This file pins down the *pre-flight* slice that runs in
//! `rule_cli::push` before any HTTP socket is opened — so we can assert
//! exit codes and `error.code` strings deterministically without
//! standing up a server.

use std::process::Command;

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_forward-server")
}

/// Bad hostname (RFC 1123 underscore) MUST fail with exit code 3 and
/// `invalid_target_host` in stderr — surfaced from `Target::parse`
/// → `OperatorError::InvalidTargetHost` → `code_to_exit("invalid_target_host")`.
/// The CLI rejects it BEFORE attempting HTTP, so the test does not
/// need a running server.
#[test]
fn push_rule_rejects_invalid_target_host() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("foo_bar.example:80")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 3,
        "invalid target_host should exit 3 (input-validation family), got {code}; stderr={stderr}"
    );
    assert!(
        stderr.contains("invalid_target_host"),
        "stderr should mention invalid_target_host, got: {stderr}"
    );
}

/// Per-label too-long surfaces the dedicated subcategory code so
/// operator tooling can pattern-match without re-reading the message.
#[test]
fn push_rule_rejects_label_too_long_with_subcode() {
    let long_label = "a".repeat(64);
    let host = format!("{long_label}.example.com:80");
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg(host)
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 3,
        "oversized label should exit 3, got {code}; stderr={stderr}"
    );
    assert!(
        stderr.contains("invalid_target_host_label_too_long"),
        "stderr should carry the label-too-long subcode, got: {stderr}"
    );
}

/// Sanity: a syntactically valid hostname survives pre-flight
/// validation. Without a server on 127.0.0.1:7080 the CLI then fails
/// at the HTTP step with exit 1 — that's evidence we got past
/// `Target::parse` rather than being rejected as `invalid_target_host`
/// (which would be exit 3).
#[test]
fn push_rule_accepts_valid_dns_target_host_through_validator() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("echo.test:41000")
        // Point at a port nothing is listening on so the HTTP step
        // fails fast and deterministically (kernel ECONNREFUSED).
        .arg("--http-endpoint")
        .arg("127.0.0.1:1")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        code, 3,
        "validator should accept echo.test as a hostname; got exit 3 with stderr={stderr}"
    );
    assert!(
        !stderr.contains("invalid_target_host"),
        "validator should not flag echo.test, got: {stderr}"
    );
}
