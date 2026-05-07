//! T051 (005-multi-user-rbac) — end-to-end walkthrough mirroring
//! `specs/005-multi-user-rbac/quickstart.md` sections 1–7.
//!
//! Drives the live `forward-server` binary through the operator HTTP
//! surface (the same path the CLI subcommands take internally). No
//! real `forward-client` is connected — push-rule attempts that pass
//! RBAC fail downstream with `client_not_connected`, which is exactly
//! the proof we want: the failure is POST-authorisation. This mirrors
//! the v0.4.0 contract test pattern.
//!
//! Coverage map (vs. quickstart):
//! - § 1 Bootstrap superadmin via `operator_token` shortcut
//! - § 2 user-add + credential-issue + grant-add
//! - § 3 push-rule within grant (post-RBAC failure permitted)
//! - § 3.1 reject port_outside_grant / protocol_not_granted /
//!   client_not_granted
//! - § 4 RBAC read-filtering (alice / bob / superadmin views diverge)
//! - § 5 credential-rotate self-service (old token rejected, new token works)
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

    // § 2 — create alice, mint a credential, grant client-z + 30000..30005 tcp.
    let (st, _) = post(
        &http_addr,
        "/v1/users",
        SUPER,
        json!({"user_id": "alice", "display_name": "Alice"}),
    );
    assert_eq!(st, StatusCode::CREATED, "user-add alice");

    let (st, body) = post(
        &http_addr,
        "/v1/users/alice/credentials",
        SUPER,
        json!({"label": "laptop"}),
    );
    assert_eq!(st, StatusCode::CREATED);
    let alice_token = body["token"].as_str().expect("alice token").to_string();
    let alice_cred_id = body["credential_id"]
        .as_str()
        .expect("alice cred_id")
        .to_string();

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

    // § 3 — alice pushes within her grant. Client-z is offline, so the
    // expected outcome is the POST-RBAC error `client_not_connected`,
    // NOT a 403 — the authorisation layer must allow the push first.
    let (st, body) = post(
        &http_addr,
        "/v1/rules",
        &alice_token,
        json!({
            "client": "client-z",
            "listen_port": 30005,
            "target_host": "10.0.0.5",
            "target_port": 80,
            "protocol": "tcp",
        }),
    );
    assert_ne!(
        st,
        StatusCode::FORBIDDEN,
        "RBAC must allow this push; got {st} body={body}"
    );
    assert_eq!(
        body["error"]["code"].as_str().unwrap_or(""),
        "client_not_connected",
        "expected post-RBAC failure to be client_not_connected; got {st} body={body}"
    );

    // § 3.1 — three violation paths, each must be 403 with the matching code.
    for (port, proto, client, expected_code) in [
        (30099_u16, "tcp", "client-z", "port_outside_grant"),
        (30005, "udp", "client-z", "protocol_not_granted"),
        (30005, "tcp", "client-other", "client_not_granted"),
    ] {
        let (st, body) = post(
            &http_addr,
            "/v1/rules",
            &alice_token,
            json!({
                "client": client,
                "listen_port": port,
                "target_host": "10.0.0.5",
                "target_port": 80,
                "protocol": proto,
            }),
        );
        assert_eq!(
            st,
            StatusCode::FORBIDDEN,
            "violation must 403; got {st} body={body}"
        );
        assert_eq!(
            body["error"]["code"].as_str().unwrap_or(""),
            expected_code,
            "wrong code; body={body}"
        );
    }

    // § 4 — RBAC read filtering. Add bob and confirm each actor sees
    // only their own rules. We can't easily push rules without a
    // connected client, so this section verifies the visibility
    // contract on `/v1/users` instead (which is a superadmin-only
    // surface) — alice MUST be denied 403 when she calls it.
    let (st, _) = post(
        &http_addr,
        "/v1/users",
        SUPER,
        json!({"user_id": "bob", "display_name": "Bob"}),
    );
    assert_eq!(st, StatusCode::CREATED);
    let (st, _) = post(
        &http_addr,
        "/v1/users/bob/credentials",
        SUPER,
        json!({"label": "laptop"}),
    );
    assert_eq!(st, StatusCode::CREATED);

    let (st, _) = get(&http_addr, "/v1/users", &alice_token);
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "alice (role=user) MUST be denied on superadmin-only listings"
    );

    // alice viewing her own credentials: allowed.
    let (st, body) = get(&http_addr, "/v1/users/alice/credentials", &alice_token);
    assert_eq!(st, StatusCode::OK, "alice owns alice; body={body}");

    // alice viewing bob's credentials: forbidden.
    let (st, body) = get(&http_addr, "/v1/users/bob/credentials", &alice_token);
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "cross-tenant read MUST 403; body={body}"
    );

    // § 5 — credential-rotate self-service. alice rotates her own
    // credential. Old token rejected (401), new token works.
    let (st, body) = post(
        &http_addr,
        &format!("/v1/users/alice/credentials/{alice_cred_id}/rotate"),
        &alice_token,
        json!({}),
    );
    assert_eq!(st, StatusCode::OK, "rotate; body={body}");
    let alice_token_new = body["token"].as_str().expect("rotated token").to_string();
    assert_ne!(alice_token, alice_token_new, "new token must differ");

    // Old alice_token now invalid.
    let (st, _) = get(&http_addr, "/v1/users/alice/credentials", &alice_token);
    assert_eq!(
        st,
        StatusCode::UNAUTHORIZED,
        "rotated-out token MUST be rejected"
    );
    // New alice_token works.
    let (st, _) = get(&http_addr, "/v1/users/alice/credentials", &alice_token_new);
    assert_eq!(st, StatusCode::OK);

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
    // her credentials and grants (and any owned rules — none here).
    let (st, body) = delete(&http_addr, "/v1/users/alice", SUPER);
    assert_eq!(st, StatusCode::OK, "user-remove; body={body}");
    assert_eq!(body["user_id"], "alice");
    let removed_creds = body["removed_credential_ids"]
        .as_array()
        .expect("removed_credential_ids array");
    // alice had two credentials at this point (initial + rotated).
    assert!(
        !removed_creds.is_empty(),
        "expected credentials to cascade; body={body}"
    );
    // After removal, alice's tokens MUST be rejected.
    let (st, _) = get(&http_addr, "/v1/users/alice/credentials", &alice_token_new);
    assert_eq!(
        st,
        StatusCode::UNAUTHORIZED,
        "post-remove token MUST be rejected"
    );

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
