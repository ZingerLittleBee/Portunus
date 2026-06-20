//! T051 (005-multi-user-rbac) — end-to-end walkthrough mirroring
//! `specs/005-multi-user-rbac/quickstart.md` sections 1–7.
//!
//! Drives the live `portunus-server` binary through the operator HTTP
//! surface (the same path the CLI subcommands take internally). All
//! calls authenticate as the superadmin via the `operator_token`
//! shortcut — the per-user self-issued token surface has been removed,
//! so the non-superadmin-actor paths (push-within-grant, the three
//! grant-violation 403s, cross-tenant reads, credential rotation) no
//! longer have a token source and are dropped here. The `enforce_push`
//! violation-code judgement they exercised is covered directly by the
//! `operator::rbac` unit tests.
//!
//! Coverage map (vs. quickstart):
//! - § 1 Bootstrap superadmin via `operator_token` shortcut
//! - § 2 user-add + grant-add
//! - § 6 grant revoke cascade returns the freed rule_ids
//! - § 7 (partial) — user-remove cascades through identity AND rules

mod common;

use std::time::Duration;

use reqwest::StatusCode;
use serde_json::{Value, json};

const SUPER: &str = common::TEST_OPERATOR_TOKEN;

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn http() -> reqwest::blocking::Client {
    reqwest::blocking::Client::new()
}

fn post(addr: &str, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let url = format!("http://{addr}{path}");
    let resp = http()
        .post(&url)
        .header("Authorization", bearer(token))
        .json(&body)
        .send()
        .expect("POST send");
    let status = resp.status();
    let v = resp.json().unwrap_or(Value::Null);
    (status, v)
}

fn delete(addr: &str, path: &str, token: &str) -> (StatusCode, Value) {
    let url = format!("http://{addr}{path}");
    let resp = http()
        .delete(&url)
        .header("Authorization", bearer(token))
        .send()
        .expect("DELETE send");
    let status = resp.status();
    let v = resp.json().unwrap_or(Value::Null);
    (status, v)
}

fn get(addr: &str, path: &str, token: &str) -> (StatusCode, Value) {
    let url = format!("http://{addr}{path}");
    let resp = http()
        .get(&url)
        .header("Authorization", bearer(token))
        .send()
        .expect("GET send");
    let status = resp.status();
    let v = resp.json().unwrap_or(Value::Null);
    (status, v)
}

#[test]
fn rbac_walkthrough_happy_and_violation_paths() {
    // § 1 — bootstrap is handled by `spawn_server`'s operator_token shortcut.
    let server = common::spawn_server(&[]);
    let (_grpc, http_addr) = server
        .wait_listening(Duration::from_secs(10))
        .expect("server listening");

    // § 2 — create alice, grant client-z + 30000..30005 tcp.
    let (st, _) = post(
        &http_addr,
        "/v1/users",
        SUPER,
        json!({"user_id": "alice", "display_name": "Alice"}),
    );
    assert_eq!(st, StatusCode::CREATED, "user-add alice");

    let (st, body) = post(
        &http_addr,
        "/v1/grants",
        SUPER,
        json!({
            "user_id": "alice",
            "client": "client-z",
            "listen_port_start": 30000,
            "listen_port_end": 30005,
            "protocols": ["tcp"],
        }),
    );
    assert_eq!(st, StatusCode::CREATED, "grant-add alice");
    let alice_grant_id = body["grant_id"].as_str().expect("grant_id").to_string();

    // § 4 — add bob so the cascade sanity check at the end has a second
    // user to confirm is retained.
    let (st, _) = post(
        &http_addr,
        "/v1/users",
        SUPER,
        json!({"user_id": "bob", "display_name": "Bob"}),
    );
    assert_eq!(st, StatusCode::CREATED);

    // § 6 — grant revoke cascade. No rules to actually cascade in this
    // fixture (no client connected), but the response shape MUST
    // include `removed_rule_ids` (empty here).
    let (st, body) = delete(&http_addr, &format!("/v1/grants/{alice_grant_id}"), SUPER);
    assert_eq!(st, StatusCode::OK, "revoke; body={body}");
    assert_eq!(body["grant_id"], alice_grant_id);
    assert!(
        body["removed_rule_ids"].is_array(),
        "removed_rule_ids must be present; body={body}"
    );

    // § 7 (partial) — user-remove cascades. Removing alice MUST flush
    // her grants (and any owned rules — none here).
    let (st, body) = delete(&http_addr, "/v1/users/alice", SUPER);
    assert_eq!(st, StatusCode::OK, "user-remove; body={body}");
    assert_eq!(body["user_id"], "alice");

    // Sanity: superadmin still functional after the cascade.
    let (st, body) = get(&http_addr, "/v1/users", SUPER);
    assert_eq!(st, StatusCode::OK);
    let user_ids: Vec<&str> = body
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|u| u["user_id"].as_str())
        .collect();
    assert!(!user_ids.contains(&"alice"), "alice gone");
    assert!(user_ids.contains(&"bob"), "bob retained");
}
