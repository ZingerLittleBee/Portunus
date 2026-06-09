//! T022 / T023 / T024 — gRPC auth contract checks.
//!
//! These exercise specific failure modes individually so a regression points
//! to the right cause:
//! - T022: missing bearer metadata → `UNAUTHENTICATED` reason `missing_token`.
//! - T023: revoked-token client → `UNAUTHENTICATED` reason `token_revoked`.
//! - T024: pin mismatch → the pinned TLS handshake fails, so the client
//!   never reaches a session and never sends its bearer token.

mod common;

use std::time::Duration;

use serde_json::Value;

#[test]
fn test_missing_token_rejected() {
    // We assert at the structured-log level: when no bearer is supplied,
    // the auth interceptor logs an `auth_failure` with reason `missing_token`.
    // Driving this via a raw gRPC client would pull tonic into the test
    // crate; instead we check the easier observable surface — start a client
    // with an empty `token` field and confirm the server's auth_failure
    // event names `missing_token`.

    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server listening");

    let bundle_path = common::provision_client_http(&http, "edge-01");
    let mut tampered: Value =
        serde_json::from_str(&std::fs::read_to_string(&bundle_path).unwrap()).unwrap();
    // Empty token → interceptor on server side sees `Bearer ` and treats as
    // malformed/missing.
    tampered["token"] = Value::String(String::new());
    let bad_path = server.data_dir.path().join("empty.bundle.json");
    std::fs::write(&bad_path, serde_json::to_vec_pretty(&tampered).unwrap()).unwrap();
    let _client = common::spawn_client(&bad_path, &[]);

    let seen = common::wait_for(Duration::from_secs(5), || {
        let lines = server.stderr_lines.lock().unwrap();
        lines
            .iter()
            .any(|l| {
                l.contains("auth.failure")
                    && (l.contains("missing_token") || l.contains("malformed_token"))
            })
            .then_some(())
    });
    assert!(
        seen.is_some(),
        "server should log auth.failure with missing_token / malformed_token reason"
    );
}

#[test]
fn test_revoked_token_rejected() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server listening");

    let bundle = common::provision_client_http(&http, "edge-01");
    let revoke_status = common::revoke_http(&http, "edge-01");
    assert!(
        revoke_status.is_success(),
        "revoke should succeed: {revoke_status}"
    );

    let _client = common::spawn_client(&bundle, &[]);

    let seen = common::wait_for(Duration::from_secs(5), || {
        let lines = server.stderr_lines.lock().unwrap();
        lines
            .iter()
            .any(|l| l.contains("token_revoked"))
            .then_some(())
    });
    assert!(
        seen.is_some(),
        "server should log token_revoked when revoked client dials"
    );
}

#[test]
fn test_pin_mismatch_rejected() {
    // Pin-only model: the bundle carries only the SHA-256 pin (no cert
    // PEM), so a tampered-but-still-hex fingerprint loads fine. The TLS
    // handshake then rejects the server's real certificate (whose
    // fingerprint != the tampered pin), so the client never reaches a
    // connected session — and therefore never sends its bearer token.

    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server listening");

    let bundle = common::provision_client_http(&http, "edge-01");
    let mut tampered: Value =
        serde_json::from_str(&std::fs::read_to_string(&bundle).unwrap()).unwrap();
    let original = tampered["server_cert_sha256"].as_str().unwrap().to_string();
    let mut chars: Vec<char> = original.chars().collect();
    chars[0] = if chars[0] == '0' { 'f' } else { '0' };
    tampered["server_cert_sha256"] = Value::String(chars.into_iter().collect());
    let bad_path = server.data_dir.path().join("pin.bundle.json");
    std::fs::write(&bad_path, serde_json::to_vec_pretty(&tampered).unwrap()).unwrap();

    let mut client = common::spawn_client(&bad_path, &[]);

    // The pinned handshake must fail; the client logs it and retries (it
    // does not exit, since a transport failure is non-terminal).
    let saw_failure = common::wait_for(Duration::from_secs(8), || {
        client
            .stderr_contains("control.connect_failed")
            .then_some(())
    });

    // It must never have reached a live session: no Welcome, so the bearer
    // token was never transmitted to the server.
    let reached_session = client.stderr_contains("control.connected");
    let clients = common::list_clients_http(&http);
    let server_sees_connected = clients.as_array().is_some_and(|arr| {
        arr.iter()
            .any(|c| c["client_name"] == "edge-01" && c["connected"] == Value::Bool(true))
    });

    // The control.connect_failed line must name the SPECIFIC cause — a TLS
    // certificate fingerprint (pin) mismatch — not just the generic
    // connect-failed event (which fires for any non-terminal transport
    // failure). The PinnedCertVerifier surfaces this stable marker string
    // through the rustls -> tonic transport error chain.
    let saw_pin_cause =
        client.stderr_contains("server certificate fingerprint does not match the pinned value");

    client.child.kill().ok();
    let _ = client.child.wait();

    assert!(
        saw_failure.is_some(),
        "client should log control.connect_failed under a mismatched pin"
    );
    assert!(
        saw_pin_cause,
        "control.connect_failed must name the pin/fingerprint mismatch cause"
    );
    assert!(
        !reached_session,
        "client must never reach control.connected under a mismatched pin"
    );
    assert!(
        !server_sees_connected,
        "server must not see the client as connected under a mismatched pin"
    );
}
