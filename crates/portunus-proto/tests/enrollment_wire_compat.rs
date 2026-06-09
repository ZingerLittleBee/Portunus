//! Wire contract for one-shot client enrollment.
//!
//! Enrollment is additive: it introduces a separate unauthenticated RPC on the
//! existing TLS control listener. Existing `Control.Channel` bytes remain
//! untouched; these tests pin the new message tags so future edits do not
//! casually reshuffle the onboarding contract.

use portunus_proto::v1::{CredentialBundle, EnrollClientRequest};
use prost::Message;

#[test]
fn enroll_client_request_roundtrips() {
    let req = EnrollClientRequest {
        code: "join-code".into(),
    };

    let bytes = req.encode_to_vec();
    assert!(
        bytes.contains(&0x0a),
        "field 1 tag for code must be present: {bytes:?}"
    );

    let decoded = EnrollClientRequest::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, req);
}

#[test]
fn credential_bundle_roundtrips() {
    let bundle = CredentialBundle {
        version: 1,
        client_name: "edge-01".into(),
        server_endpoint: "control.example.com:7443".into(),
        server_cert_sha256: "a".repeat(64),
        token: "client-token".into(),
        client_id: "01HCLIENTID0000000000000000".into(),
    };

    let bytes = bundle.encode_to_vec();
    for (tag, field) in [
        (0x08_u8, "version"),
        (0x12, "client_name"),
        (0x1a, "server_endpoint"),
        (0x22, "server_cert_sha256"),
        (0x32, "token"),
    ] {
        assert!(
            bytes.contains(&tag),
            "field tag for {field} must be present: {bytes:?}"
        );
    }

    // field 5 (server_cert_pem) is reserved after the pin-only migration; the
    // cert PEM must never reach the wire. tag = (5 << 3) | 2 = 0x2a.
    assert!(
        !bytes.contains(&0x2a),
        "field 5 (server_cert_pem, tag 0x2a) must be absent from the wire after pin-only migration"
    );

    let decoded = CredentialBundle::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, bundle);
}
