//! T037 (005-multi-user-rbac, US2) — end-to-end smoke for the
//! user / grant CLI subcommands.
//!
//! Spawns a real `portunus-server`, then exercises each subcommand
//! through the binary, asserting it round-trips through the live
//! HTTP router. Uses the same `operator_token` shortcut as the rest
//! of the e2e suite to keep the bootstrap path identical.

mod common;

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

fn run_subcmd(args: &[&str]) -> std::process::Output {
    Command::new(common::workspace_bin("portunus-server"))
        .args(args)
        .env("PORTUNUS_OPERATOR_TOKEN", common::TEST_OPERATOR_TOKEN)
        .output()
        .expect("spawn portunus-server")
}

/// Run a subcommand that reads a value from the first stdin line
/// (e.g. `user-add --password-stdin`), feeding `stdin_line` to it.
fn run_subcmd_with_stdin(args: &[&str], stdin_line: &str) -> std::process::Output {
    let mut child = Command::new(common::workspace_bin("portunus-server"))
        .args(args)
        .env("PORTUNUS_OPERATOR_TOKEN", common::TEST_OPERATOR_TOKEN)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn portunus-server");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(format!("{stdin_line}\n").as_bytes())
        .expect("write password to stdin");
    child.wait_with_output().expect("wait portunus-server")
}

#[test]
fn cli_user_grant_roundtrip() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(10))
        .expect("server should listen within 10s");

    // user-add alice (initial password piped via stdin)
    let out = run_subcmd_with_stdin(
        &[
            "user-add",
            "alice",
            "--display-name",
            "Alice",
            "--password-stdin",
            "--http-endpoint",
            &http,
        ],
        "correct horse battery staple",
    );
    assert!(
        out.status.success(),
        "user-add: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("user-add stdout JSON");
    assert_eq!(v["user_id"], "alice");

    // grant-add alice client-a 30000-30010 tcp
    let out = run_subcmd(&[
        "grant-add",
        "--user-id",
        "alice",
        "--client",
        "client-a",
        "--listen-port-start",
        "30000",
        "--listen-port-end",
        "30010",
        "--protocols",
        "tcp",
        "--http-endpoint",
        &http,
    ]);
    assert!(
        out.status.success(),
        "grant-add: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("grant-add JSON");
    let grant_id = v["grant_id"].as_str().expect("grant_id").to_string();

    // grant-list (filter by alice)
    let out = run_subcmd(&["grant-list", "--user-id", "alice", "--http-endpoint", &http]);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("list JSON");
    assert_eq!(v.as_array().unwrap().len(), 1);

    // grant-revoke
    let out = run_subcmd(&["grant-revoke", &grant_id, "--http-endpoint", &http]);
    assert!(out.status.success(), "grant-revoke: {}", out.status);

    // user-remove alice (cascades grants)
    let out = run_subcmd(&["user-remove", "alice", "--http-endpoint", &http]);
    assert!(
        out.status.success(),
        "user-remove: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("remove JSON");
    assert_eq!(v["user_id"], "alice");

    // user-list now only shows the legacy superadmin.
    let out = run_subcmd(&["user-list", "--http-endpoint", &http]);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("list JSON");
    assert_eq!(v.as_array().unwrap().len(), 1);
}

#[test]
fn cli_without_token_exits_4_unauthenticated() {
    // Spawn server so the CLI's HTTP probe doesn't fail at the socket level.
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(10))
        .expect("listening");

    let out = Command::new(common::workspace_bin("portunus-server"))
        .args(["user-list", "--http-endpoint", &http])
        // Deliberately omit both current and legacy token env vars.
        .env_remove("PORTUNUS_OPERATOR_TOKEN")
        .env_remove("PORTUNUS_OPERATOR_TOKEN")
        .output()
        .expect("spawn");
    assert_eq!(
        out.status.code(),
        Some(4),
        "must exit 4 when token missing; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}
