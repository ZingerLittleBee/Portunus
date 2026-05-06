//! T026 + T027 — US1 happy path coverage.
//!
//! Spins up `forward-server`, provisions `edge-01`, starts `forward-client`
//! against the issued bundle, and asserts the connected-state shows up via
//! the loopback operator HTTP API within 5 s (acceptance scenario #1).

mod common;

use std::time::Duration;

use serde_json::Value;

#[test]
fn test_list_clients_after_connect() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server should log listening event within 5s");

    let bundle = common::provision_client_http(&http, "edge-01");
    let client = common::spawn_client(&bundle, &[]);

    // Acceptance scenario #1: client appears as connected within 5 s.
    let view = common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        let edge = arr
            .as_array()?
            .iter()
            .find(|v| v.get("client_name").and_then(|n| n.as_str()) == Some("edge-01"))?;
        if edge.get("connected")?.as_bool()? {
            Some(edge.clone())
        } else {
            None
        }
    });
    if view.is_none() {
        eprintln!("--- server stderr ---");
        for l in server.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
        eprintln!("--- client stderr ---");
        for l in client.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
    }
    let edge = view.expect("edge-01 should be reported connected within 5s");
    assert_eq!(edge["client_name"], "edge-01");
    assert_eq!(edge["connected"], true);
    assert!(
        edge.get("remote_addr").is_some(),
        "remote_addr field present"
    );
    assert!(
        edge.get("connected_at").is_some(),
        "connected_at field present"
    );
}

/// Walks the four US1 acceptance scenarios in one run:
/// 1. Provision + connect → appears connected.
/// 2. Bad token → never appears connected, server logs `auth_failure`.
/// 3. Revoked token → server logs reason `token_revoked`, client never appears.
/// 4. Pin mismatch → client refuses TLS, exits with `bundle pin mismatch`.
#[test]
#[allow(clippy::too_many_lines)] // intentional: one test walks all 4 US1 scenarios end-to-end
fn test_user_story_1_acceptance() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server should be listening");

    // ---- Scenario 1: happy path ----
    let bundle = common::provision_client_http(&http, "edge-01");
    let good_client = common::spawn_client(&bundle, &[]);
    let connected = common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("edge-01")
                    && v.get("connected")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
            })
            .cloned()
    });
    if connected.is_none() {
        eprintln!("--- server stderr ---");
        for l in server.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
        eprintln!("--- client stderr ---");
        for l in good_client.stderr_lines.lock().unwrap().iter() {
            eprintln!("{l}");
        }
    }
    assert!(
        connected.is_some(),
        "scenario 1: edge-01 should appear connected"
    );

    // ---- Scenario 2: bad token (provisioned client, mutated token) ----
    let bad_bundle_path = server.config_dir.path().join("bad.bundle.json");
    let mut bad: Value = serde_json::from_str(&std::fs::read_to_string(&bundle).unwrap()).unwrap();
    // Replace the token with garbage that has the same length shape.
    bad["client_name"] = Value::String("bogus".into());
    bad["token"] = Value::String("Aaaa-bbbb-cccc-dddd-eeee-ffff-gggg-hhhh-iii".into());
    std::fs::write(&bad_bundle_path, serde_json::to_vec_pretty(&bad).unwrap()).unwrap();
    let _bad_client = common::spawn_client(&bad_bundle_path, &[]);
    // Wait briefly for the auth failure to materialise on the server side.
    let auth_failure_seen = common::wait_for(Duration::from_secs(5), || {
        // Look for the audit / auth_failure structured event in stderr.
        let lines = server.stderr_lines.lock().unwrap();
        lines
            .iter()
            .any(|l| l.contains("auth.failure"))
            .then_some(())
    });
    assert!(
        auth_failure_seen.is_some(),
        "scenario 2: server should log auth_failure for bogus token"
    );
    // bogus client never appears connected.
    let arr = common::list_clients_http(&http);
    let bogus_present = arr
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.get("client_name").and_then(|n| n.as_str()) == Some("bogus"));
    assert!(
        !bogus_present,
        "scenario 2: 'bogus' was never provisioned, must not appear"
    );

    // ---- Scenario 3: revoked token ----
    let edge2_bundle = common::provision_client_http(&http, "edge-02");
    let revoke_status = common::revoke_http(&http, "edge-02");
    assert!(
        revoke_status.is_success(),
        "revoke should succeed: {revoke_status}"
    );
    let _revoked_client = common::spawn_client(&edge2_bundle, &[]);
    let revoke_event_seen = common::wait_for(Duration::from_secs(5), || {
        let lines = server.stderr_lines.lock().unwrap();
        lines
            .iter()
            .any(|l| l.contains("token_revoked"))
            .then_some(())
    });
    assert!(
        revoke_event_seen.is_some(),
        "scenario 3: server stderr should contain 'token_revoked'"
    );

    // ---- Scenario 4: pin mismatch ----
    let pin_mismatch_path = server.config_dir.path().join("pin-mismatch.bundle.json");
    let mut tampered: Value =
        serde_json::from_str(&std::fs::read_to_string(&bundle).unwrap()).unwrap();
    // Flip one byte of the fingerprint hex to force the bundle's pin check
    // (CredentialBundle::verify_pin_consistency) to fire — the client refuses
    // to dial out at all.
    let original = tampered["server_cert_sha256"].as_str().unwrap().to_string();
    let mut chars: Vec<char> = original.chars().collect();
    chars[0] = if chars[0] == '0' { 'f' } else { '0' };
    tampered["server_cert_sha256"] = Value::String(chars.into_iter().collect());
    std::fs::write(
        &pin_mismatch_path,
        serde_json::to_vec_pretty(&tampered).unwrap(),
    )
    .unwrap();
    let mut bad_pin_client = common::spawn_client(&pin_mismatch_path, &[]);
    // Client should exit non-zero quickly because the bundle fails pin check
    // at load time.
    let exit = common::wait_for(Duration::from_secs(5), || {
        bad_pin_client.child.try_wait().ok().flatten()
    });
    let status = exit.expect("client must exit on pin mismatch within 5s");
    assert!(
        !status.success(),
        "scenario 4: client must exit non-zero on pin mismatch, got {status:?}"
    );
    assert!(
        bad_pin_client.stderr_contains("bundle_load_failed")
            || bad_pin_client.stderr_contains("pin mismatch"),
        "scenario 4: client stderr should report pin mismatch / bundle_load_failed"
    );
}
