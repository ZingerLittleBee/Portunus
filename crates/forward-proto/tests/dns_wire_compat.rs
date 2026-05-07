//! Wire-compatibility test for the additive DNS-target fields (T010).
//!
//! Asserts:
//!   * a v0.2.0-shaped `Rule` (no `prefer_ipv6` set) encodes to a byte
//!     string that contains NONE of the new field-8 tag bytes — proves
//!     the wire is unchanged for v0.2.0 IP-target rules (FR-010);
//!   * a `Rule` carrying `prefer_ipv6: Some(true)` round-trips and the
//!     decoded value matches;
//!   * a v0.2.0-shaped `RuleStats` (no `dns_failures`, i.e. value 0)
//!     encodes to a byte string with NO field-6 tag — proves StatsReport
//!     bytes are unchanged for v0.2.0 clients;
//!   * a `RuleStats` with `dns_failures = 7` round-trips and the
//!     decoded value matches.
//!
//! Constitution Principle III gate: byte-identical encoding of a
//! v0.2.0-shaped Rule before and after this change MUST hold.

use forward_proto::v1::{Protocol, Rule, RuleStats};
use prost::Message;

#[test]
fn v0_2_0_rule_byte_compatible_when_prefer_ipv6_absent() {
    // Single-byte target host so we can byte-scan for new tags
    // without false positives on the host string itself (0x68 == 'h').
    let r = Rule {
        rule_id: 9,
        listen_port: 18080,
        target_host: String::from("h"),
        target_port: 8080,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: None,
    };
    let bytes = r.encode_to_vec();

    // Field 8 wire-type 0 (varint) = (8 << 3) | 0 = 0x40. The optional
    // proto3 bool MUST emit no bytes when its value is None.
    assert!(
        !bytes.contains(&0x40),
        "tag for prefer_ipv6 must be absent when None — got bytes {bytes:?}"
    );

    let decoded = Rule::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, r);
    assert_eq!(decoded.prefer_ipv6, None);
}

#[test]
fn rule_with_prefer_ipv6_true_roundtrips() {
    let r = Rule {
        rule_id: 10,
        listen_port: 8080,
        target_host: "api.example.com".into(),
        target_port: 443,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: Some(true),
    };
    let bytes = r.encode_to_vec();
    let decoded = Rule::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, r);
    assert_eq!(decoded.prefer_ipv6, Some(true));
}

#[test]
fn rule_with_prefer_ipv6_false_roundtrips() {
    // `Some(false)` must NOT collapse to `None` — the operator
    // explicitly opted in to IPv4-first; we keep the distinction for
    // round-tripping even though the runtime semantics are identical.
    let r = Rule {
        rule_id: 11,
        listen_port: 8081,
        target_host: "api.example.com".into(),
        target_port: 443,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: Some(false),
    };
    let bytes = r.encode_to_vec();
    let decoded = Rule::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded.prefer_ipv6, Some(false));
}

#[test]
fn v0_2_0_rule_stats_byte_compatible_when_dns_failures_zero() {
    let s = RuleStats {
        rule_id: 9,
        bytes_in: 1234,
        bytes_out: 5678,
        active_connections: 3,
        per_port: vec![],
        dns_failures: 0,
    };
    let bytes = s.encode_to_vec();

    // Field 6 wire-type 0 (varint) = (6 << 3) | 0 = 0x30. uint64 with
    // value 0 is the proto3 default and MUST be stripped on encode.
    assert!(
        !bytes.contains(&0x30),
        "tag for dns_failures must be absent when value is 0 — got bytes {bytes:?}"
    );

    let decoded = RuleStats::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, s);
    assert_eq!(decoded.dns_failures, 0);
}

#[test]
fn rule_stats_with_dns_failures_roundtrips() {
    let s = RuleStats {
        rule_id: 10,
        bytes_in: 9000,
        bytes_out: 9000,
        active_connections: 2,
        per_port: vec![],
        dns_failures: 7,
    };
    let bytes = s.encode_to_vec();
    let decoded = RuleStats::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, s);
    assert_eq!(decoded.dns_failures, 7);
}
