//! 015-client-stable-id T008 — wire contract: the additive `client_id` field
//! round-trips on the control-plane messages, and a message encoded WITHOUT
//! it (a pre-upgrade peer) still decodes (the field defaults to empty).

use portunus_proto::v1::{CredentialBundle, OwnerRateLimitUpdate, TrafficQuotaUpdate};
use prost::Message;

#[test]
fn credential_bundle_client_id_roundtrips() {
    let bundle = CredentialBundle {
        version: 1,
        client_name: "Acme Prod – East".into(),
        server_endpoint: "control.example:7443".into(),
        server_cert_sha256: "a".repeat(64),
        token: "tok".into(),
        client_id: "01HCLIENTID0000000000000000".into(),
    };
    let decoded = CredentialBundle::decode(bundle.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded.client_id, "01HCLIENTID0000000000000000");
    assert_eq!(decoded.client_name, "Acme Prod – East");
}

#[test]
fn legacy_credential_bundle_without_client_id_still_decodes() {
    // A pre-upgrade peer never set field 7. Re-create that by encoding the
    // older shape (fields 1-6 only) via the prost type with an empty
    // client_id — proto3 omits empty strings from the wire, so the bytes are
    // byte-identical to what an old client produced.
    let legacy = CredentialBundle {
        version: 1,
        client_name: "edge-01".into(),
        server_endpoint: "control.example:7443".into(),
        server_cert_sha256: "a".repeat(64),
        token: "tok".into(),
        client_id: String::new(), // not present on the wire
    };
    let bytes = legacy.encode_to_vec();
    let decoded = CredentialBundle::decode(bytes.as_slice()).unwrap();
    assert!(
        decoded.client_id.is_empty(),
        "absent client_id decodes to empty (legacy-tolerant)"
    );
    assert_eq!(decoded.token, "tok");
}

#[test]
fn owner_rate_limit_update_client_id_is_additive() {
    let u = OwnerRateLimitUpdate {
        client_name: "edge-01".into(),
        owner_id: "alice".into(),
        rate_limit: None,
        action: 0,
        client_id: "01HCLIENTID0000000000000000".into(),
    };
    let decoded = OwnerRateLimitUpdate::decode(u.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded.client_id, "01HCLIENTID0000000000000000");

    // Legacy: client_id empty -> not on wire -> decodes empty.
    let legacy = OwnerRateLimitUpdate {
        client_id: String::new(),
        ..u
    };
    let decoded = OwnerRateLimitUpdate::decode(legacy.encode_to_vec().as_slice()).unwrap();
    assert!(decoded.client_id.is_empty());
}

#[test]
fn traffic_quota_update_client_id_is_additive() {
    let u = TrafficQuotaUpdate {
        request_id: "r1".into(),
        user_id: "alice".into(),
        client_name: "edge-01".into(),
        action: 0,
        state: None,
        client_id: "01HCLIENTID0000000000000000".into(),
    };
    let decoded = TrafficQuotaUpdate::decode(u.encode_to_vec().as_slice()).unwrap();
    assert_eq!(decoded.client_id, "01HCLIENTID0000000000000000");
}
