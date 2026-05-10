//! T037 (005-multi-user-rbac, US2) — end-to-end smoke for the
//! user / credential / grant CLI subcommands.
//!
//! Spawns a real `portunus-server`, then exercises each subcommand
//! through the binary, asserting it round-trips through the live
//! HTTP router. Uses the same `operator_token` shortcut as the rest
//! of the e2e suite to keep the bootstrap path identical.

mod common;

use std::process::Command;
use std::time::Duration;

use serde_json::Value;

fn run_subcmd(args: &[&str]) -> std::process::Output {
    Command::new(common::workspace_bin("portunus-server"))
        .args(args)
        .env("PORTUNUS_OPERATOR_TOKEN", common::TEST_OPERATOR_TOKEN)
        .output()
        .expect("spawn portunus-server")
}

#[test]
fn cli_user_credential_grant_roundtrip() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(10))
        .expect("server should listen within 10s");

    // user-add alice
    let out = run_subcmd(&[
        "user-add",
        "alice",
        "--display-name",
        "Alice",
        "--http-endpoint",
        &http,
    ]);
    assert!(
        out.status.success(),
        "user-add: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("user-add stdout JSON");
    assert_eq!(v["user_id"], "alice");

    // credential-issue alice → capture alice's token
    let out = run_subcmd(&[
        "credential-issue",
        "alice",
        "--label",
        "laptop",
        "--http-endpoint",
        &http,
    ]);
    assert!(
        out.status.success(),
        "credential-issue: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("credential-issue stdout JSON");
    let alice_token = v["token"].as_str().expect("token field").to_string();
    let alice_cred_id = v["credential_id"]
        .as_str()
        .expect("credential_id")
        .to_string();
    assert!(alice_token.len() >= 32);

    // credential-list alice → stays present, no token leak
    let out = run_subcmd(&["credential-list", "alice", "--http-endpoint", &http]);
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).expect("list JSON");
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert!(
        arr[0].get("token").is_none(),
        "token must NOT appear in list"
    );

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

    // credential-rotate alice using HER OWN token (US4 self-service path)
    let out = Command::new(common::workspace_bin("portunus-server"))
        .args([
            "credential-rotate",
            "alice",
            &alice_cred_id,
            "--http-endpoint",
            &http,
        ])
        .env("PORTUNUS_OPERATOR_TOKEN", &alice_token)
        .output()
        .expect("spawn rotate");
    assert!(
        out.status.success(),
        "rotate: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).expect("rotate JSON");
    let new_token = v["token"].as_str().expect("new token").to_string();
    assert_ne!(new_token, alice_token);

    // grant-revoke
    let out = run_subcmd(&["grant-revoke", &grant_id, "--http-endpoint", &http]);
    assert!(out.status.success(), "grant-revoke: {}", out.status);

    // user-remove alice (cascades credentials + grants)
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
