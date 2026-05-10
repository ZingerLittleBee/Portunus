//! T014 (003-domain-name-forward) — `push-rule` CLI input-validation
//! integration test.
//!
//! The actual happy-path push (with a connected client + an active gRPC
//! channel) is exercised end-to-end by `crates/portunus-e2e/tests/dns_smoke.rs`
//! (T015). This file pins down the *pre-flight* slice that runs in
//! `rule_cli::push` before any HTTP socket is opened — so we can assert
//! exit codes and `error.code` strings deterministically without
//! standing up a server.

use std::process::Command;

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_portunus-server")
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

// --- 004-udp-forward T020 ---

/// `--protocol udp` MUST survive pre-flight protocol parsing (T017).
/// Without a running server the CLI fails at the HTTP step (exit 1),
/// but it MUST NOT bail out with `invalid_protocol` (exit 3) — that
/// would mean parse_protocol still rejects "udp".
#[test]
fn push_rule_accepts_udp_protocol_through_parser() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("6000")
        .arg("127.0.0.1:9999")
        .arg("--protocol")
        .arg("udp")
        .arg("--http-endpoint")
        .arg("127.0.0.1:1")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(
        code, 3,
        "parser must accept `udp`; got exit 3 with stderr={stderr}"
    );
    assert!(
        !stderr.contains("invalid_protocol"),
        "parser must not reject `udp` as invalid_protocol, got: {stderr}"
    );
}

/// 004-udp-forward T047: hostname validation MUST gate UDP rules
/// identically to TCP. The v0.3 `Target::parse` validator is shared
/// across protocols — this test pins down "no UDP escape hatch" by
/// driving `push-rule --protocol udp foo_bar.example:9999` and
/// asserting exit 3 + `invalid_target_host`. Pairs with the TCP test
/// `push_rule_rejects_invalid_target_host` (no `--protocol` flag).
#[test]
fn push_rule_rejects_invalid_target_host_for_udp() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("6000")
        .arg("foo_bar.example:9999")
        .arg("--protocol")
        .arg("udp")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 3,
        "UDP push with malformed hostname MUST exit 3 (input-validation), \
         got {code}; stderr={stderr}"
    );
    assert!(
        stderr.contains("invalid_target_host"),
        "UDP push must surface the same invalid_target_host code as TCP, \
         got: {stderr}"
    );
}

/// End-to-end variant of T020: with a connected v0.3-style client
/// (TCP-only Hello), `push-rule … --protocol udp` MUST exit 3 with
/// `unsupported_protocol` in stderr. With a v0.4 client (Hello carries
/// {TCP, UDP}) the same call must succeed.
///
/// Requires US1 forwarder wiring so the success-path RuleStatus echo
/// makes it back from the client; left ignored until the close of US1
/// (T040 enables).
#[test]
#[ignore = "T020 e2e — unignore at close of US1 (T040)"]
fn push_rule_rejects_udp_to_tcp_only_client_with_unsupported_protocol() {
    // Implementation deferred — see crates/portunus-e2e/tests/udp_smoke.rs
    // (US1) for the live-server harness.
}

/// 011-rate-limiting-qos T016: the push-rule subcommand accepts the
/// new `--bandwidth-in-bps`, `--bandwidth-out-bps`,
/// `--new-connections-per-sec`, `--concurrent-connections`, and the
/// three matching `--*-burst` flags. Pre-flight parsing must accept
/// them — without a running server on 127.0.0.1:1 the CLI fails at
/// the HTTP step with exit 1, NOT exit 2/3 (which would mean clap
/// or validator rejected the flags).
#[test]
fn push_rule_accepts_rate_limit_flags_through_clap() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("127.0.0.1:9000")
        .arg("--bandwidth-in-bps")
        .arg("1048576")
        .arg("--bandwidth-out-bps")
        .arg("1048576")
        .arg("--new-connections-per-sec")
        .arg("50")
        .arg("--concurrent-connections")
        .arg("100")
        .arg("--bandwidth-in-burst")
        .arg("2097152")
        .arg("--http-endpoint")
        .arg("127.0.0.1:1")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 1,
        "rate-limit flags should pass pre-flight; HTTP step then fails (exit 1). stderr={stderr}"
    );
    // The CLI failed at the HTTP step (`error: http: ...`), not at
    // clap parsing or pre-flight validation.
    assert!(
        stderr.contains("http"),
        "expected HTTP step failure, got stderr={stderr}"
    );
}

/// Negative case: clap rejects unparseable values for u64 / u32 caps.
/// `--bandwidth-in-bps abc` exits with clap's exit code 2 ("USAGE
/// error"), not 1 (HTTP) and not 3 (pre-flight validation).
#[test]
fn push_rule_rejects_non_numeric_rate_limit_value() {
    let out = Command::new(server_bin())
        .arg("push-rule")
        .arg("edge-01")
        .arg("8080")
        .arg("127.0.0.1:9000")
        .arg("--bandwidth-in-bps")
        .arg("not-a-number")
        .output()
        .expect("run push-rule");
    let code = out.status.code().expect("exit code");
    assert_eq!(
        code, 2,
        "clap should reject non-numeric value with exit 2 (usage error)"
    );
}

/// 011-rate-limiting-qos T028: the new `owner-cap` subcommand family
/// is wired through clap. `owner-cap --help` lists list/get/set/delete.
#[test]
fn owner_cap_subcommand_help_lists_all_actions() {
    let out = Command::new(server_bin())
        .arg("owner-cap")
        .arg("--help")
        .output()
        .expect("run owner-cap --help");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    for action in ["list", "get", "set", "delete"] {
        assert!(
            stdout.contains(action),
            "expected `{action}` subcommand to be advertised in `owner-cap --help`; got:\n{stdout}"
        );
    }
}

/// `owner-cap set` with no caps must short-circuit at exit 3 with a
/// `validation.rate_limit_no_caps_provided` code — reaching the HTTP
/// layer with an empty body would otherwise risk a 400 round-trip.
#[test]
fn owner_cap_set_rejects_empty_envelope_pre_flight() {
    let out = Command::new(server_bin())
        .arg("owner-cap")
        .arg("set")
        .arg("edge-01")
        .arg("alice")
        // Point at a port nothing listens on so any HTTP attempt
        // fails fast — but pre-flight should reject FIRST.
        .arg("--http-endpoint")
        .arg("127.0.0.1:1")
        .output()
        .expect("run owner-cap set");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 3,
        "empty cap envelope should exit 3 (pre-flight validation), got {code}; stderr={stderr}"
    );
    assert!(
        stderr.contains("rate_limit_no_caps_provided"),
        "stderr should carry the rate_limit_no_caps_provided code; got {stderr}"
    );
}

/// `owner-cap set` with at least one cap passes pre-flight; the HTTP
/// step then fails fast against 127.0.0.1:1 with exit 1.
#[test]
fn owner_cap_set_passes_preflight_with_one_cap() {
    let out = Command::new(server_bin())
        .arg("owner-cap")
        .arg("set")
        .arg("edge-01")
        .arg("alice")
        .arg("--bandwidth-in-bps")
        .arg("1048576")
        .arg("--http-endpoint")
        .arg("127.0.0.1:1")
        .output()
        .expect("run owner-cap set");
    let code = out.status.code().expect("exit code");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        code, 1,
        "single cap passes pre-flight; HTTP step fails (exit 1). stderr={stderr}"
    );
    assert!(
        stderr.contains("http"),
        "expected HTTP step failure, got stderr={stderr}"
    );
}
