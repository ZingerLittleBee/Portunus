//! 007-multi-target-failover T041 — `push-rule` CLI shape validation
//! for the new `--target` / `--targets-json` / `--health-check-interval-secs`
//! flags. Pre-flight only — the actual wire push is exercised
//! end-to-end by `crates/portunus-e2e/tests/multi_target_*.rs` (T018,
//! T027). This file pins down:
//!
//! - Legacy positional target still accepted (back-compat).
//! - Repeated `--target` accepted.
//! - `--target host:port@priority` parses the priority suffix.
//! - `--targets-json '[…]'` accepted.
//! - `--target` and `--targets-json` are mutually exclusive (clap
//!   `conflicts_with_all` → exit 2).
//! - Malformed `--target host:badport` rejected with exit 3 +
//!   `invalid_target_spec`.
//! - Malformed `--targets-json '{not-a-list}'` rejected with exit 3 +
//!   `invalid_targets_json`.
//! - `--health-check-interval-secs 0` rejected with exit 3 +
//!   `health_check_interval_out_of_range` (range 1..=3600).

use std::process::Command;

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_portunus-server")
}

/// Sanity: legacy positional target still passes pre-flight. Without
/// a server it fails at the HTTP step (exit 1) — that's fine; we just
/// need the validator to not bail out at exit 3.
#[test]
fn legacy_positional_target_still_accepted() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("1.2.3.4:80")
        .arg("--http-endpoint")
        .arg("127.0.0.1:1")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        code, 3,
        "legacy positional target MUST still pass pre-flight; got exit 3 with stderr={stderr}"
    );
}

/// Repeated `--target host:port` lists are accepted. Pre-flight
/// passes; HTTP step then fails (exit 1) against the unreachable
/// loopback port.
#[test]
fn repeated_target_flag_accepted() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("--target")
        .arg("1.2.3.4:80")
        .arg("--target")
        .arg("5.6.7.8:80")
        .arg("--http-endpoint")
        .arg("127.0.0.1:1")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        code, 3,
        "repeated --target MUST pass pre-flight; got exit 3 with stderr={stderr}"
    );
}

/// `--target host:port@priority` parses the optional priority suffix.
#[test]
fn target_with_explicit_priority_accepted() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("--target")
        .arg("1.2.3.4:80@0")
        .arg("--target")
        .arg("5.6.7.8:80@1")
        .arg("--http-endpoint")
        .arg("127.0.0.1:1")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        code, 3,
        "host:port@priority MUST parse; got exit 3 with stderr={stderr}"
    );
}

/// `--targets-json '[…]'` accepted as the alternative input shape.
#[test]
fn targets_json_flag_accepted() {
    let json = r#"[{"host":"1.2.3.4","port":80},{"host":"5.6.7.8","port":80,"priority":1}]"#;
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("--targets-json")
        .arg(json)
        .arg("--http-endpoint")
        .arg("127.0.0.1:1")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        code, 3,
        "well-formed --targets-json MUST pass pre-flight; got exit 3 with stderr={stderr}"
    );
}

/// `--target` + `--targets-json` together is rejected by clap's
/// `conflicts_with_all` at parse time. Clap exits 2 for parse errors.
#[test]
fn target_and_targets_json_mutually_exclusive() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("--target")
        .arg("1.2.3.4:80")
        .arg("--targets-json")
        .arg("[]")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    assert_eq!(
        code, 2,
        "clap MUST reject conflicting --target and --targets-json with exit 2"
    );
}

/// Malformed `host:badport` fails the spec parser with the
/// `invalid_target_spec` code (exit 3).
#[test]
fn malformed_target_spec_rejected() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("--target")
        .arg("1.2.3.4:not-a-port")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 3,
        "malformed target spec MUST exit 3, got {code}; stderr={stderr}"
    );
    assert!(
        stderr.contains("invalid_target_spec"),
        "stderr must mention invalid_target_spec, got: {stderr}"
    );
}

/// Malformed JSON body fails with `invalid_targets_json` (exit 3).
#[test]
fn malformed_targets_json_rejected() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("--targets-json")
        .arg("{not a list}")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 3,
        "malformed --targets-json MUST exit 3, got {code}; stderr={stderr}"
    );
    assert!(
        stderr.contains("invalid_targets_json"),
        "stderr must mention invalid_targets_json, got: {stderr}"
    );
}

/// `--health-check-interval-secs 0` is below the validator's lower
/// bound (1..=3600). Rejected with `health_check_interval_out_of_range`
/// (exit 3).
#[test]
fn health_check_interval_zero_rejected() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("--target")
        .arg("1.2.3.4:80")
        .arg("--health-check-interval-secs")
        .arg("0")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 3,
        "health_check_interval_secs=0 MUST exit 3, got {code}; stderr={stderr}"
    );
    assert!(
        stderr.contains("health_check_interval_out_of_range"),
        "stderr must mention health_check_interval_out_of_range, got: {stderr}"
    );
}

/// `--health-check-interval-secs 3601` is above the upper bound.
#[test]
fn health_check_interval_above_upper_bound_rejected() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("--target")
        .arg("1.2.3.4:80")
        .arg("--health-check-interval-secs")
        .arg("3601")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 3,
        "health_check_interval_secs=3601 MUST exit 3, got {code}; stderr={stderr}"
    );
    assert!(
        stderr.contains("health_check_interval_out_of_range"),
        "stderr must mention health_check_interval_out_of_range, got: {stderr}"
    );
}
