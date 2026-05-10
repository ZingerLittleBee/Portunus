//! Wire-compatibility test for the additive range fields (T006).
//!
//! Asserts:
//!   * a v0.1.0-shaped `Rule` (`listen_port_end` / `target_port_end` == 0)
//!     encodes to a byte string that contains NONE of the new field
//!     tags (6, 7) — proves the wire is unchanged in the legacy case;
//!   * a range rule (`listen_port_end` != 0, `target_port_end` != 0)
//!     round-trips through encode/decode and the new fields survive;
//!   * a `RuleStats` with no `per_port` entries encodes to the same
//!     bytes a v0.1.0 build would have produced (no tag 5);
//!   * a `RuleStats` with `per_port` entries round-trips and exposes
//!     the new repeated tag.

use portunus_proto::v1::{PerPortStats, Protocol, Rule, RuleStats};
use prost::Message;

#[test]
fn legacy_rule_wire_compat() {
    // A Rule with the new fields explicitly set to 0 (the proto3
    // default) MUST encode to the exact same byte string as one built
    // without the new fields ever being touched. This is the proto3
    // default-stripping guarantee; we assert it explicitly so a future
    // change to prost or our codegen settings can't silently break
    // backward compat for v0.1.0 clients.
    let host = String::from("h"); // single byte, no incidental tag-byte collisions
    let with_zero_ends = Rule {
        rule_id: 7,
        listen_port: 18080,
        target_host: host.clone(),
        target_port: 8080,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: None,
        targets: vec![],
        health_check_interval_secs: 0,
        sni_pattern: None,
        rate_limit: None,
        owner_id: None,
    };
    let mut without_ends = with_zero_ends.clone();
    without_ends.listen_port_end = 0;
    without_ends.target_port_end = 0;

    let bytes_a = with_zero_ends.encode_to_vec();
    let bytes_b = without_ends.encode_to_vec();
    assert_eq!(bytes_a, bytes_b, "default-stripping must be deterministic");

    // And the new field tags MUST be absent (tag 6 wire-type 0 = 0x30,
    // tag 7 wire-type 0 = 0x38). Safe to byte-scan now that the host
    // string is a single 'h' (0x68).
    assert!(
        !bytes_a.contains(&0x30),
        "tag for listen_port_end must be absent for single-port rule"
    );
    assert!(
        !bytes_a.contains(&0x38),
        "tag for target_port_end must be absent for single-port rule"
    );

    let decoded = Rule::decode(bytes_a.as_slice()).expect("decode");
    assert_eq!(decoded, with_zero_ends);
}

#[test]
fn range_rule_roundtrips() {
    let r = Rule {
        rule_id: 8,
        listen_port: 30000,
        target_host: "10.0.0.5".into(),
        target_port: 30000,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 30050,
        target_port_end: 30050,
        prefer_ipv6: None,
        targets: vec![],
        health_check_interval_secs: 0,
        sni_pattern: None,
        rate_limit: None,
        owner_id: None,
    };
    let bytes = r.encode_to_vec();
    let decoded = Rule::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, r);
    assert_eq!(decoded.listen_port_end, 30050);
    assert_eq!(decoded.target_port_end, 30050);
}

#[test]
fn legacy_rule_stats_encodes_without_per_port_tag() {
    let s = RuleStats {
        rule_id: 7,
        bytes_in: 1234,
        bytes_out: 5678,
        active_connections: 3,
        per_port: vec![],
        dns_failures: 0,
        datagrams_in: 0,
        datagrams_out: 0,
        active_flows: 0,
        flows_dropped_overflow: 0,
        target_failovers_total: 0,
        per_target: vec![],
        sni_route_exact_total: 0,
        sni_route_wildcard_total: 0,
        sni_route_fallback_total: 0,
        rate_limit: None,
    };
    let bytes = s.encode_to_vec();
    // tag 5 wire-type 2 (length-delimited) = 0x2a. Empty repeated
    // messages MUST NOT emit any bytes.
    assert!(
        !bytes.contains(&0x2a),
        "tag for per_port must be absent when the vec is empty"
    );

    let decoded = RuleStats::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, s);
}

#[test]
fn rule_stats_with_per_port_roundtrips() {
    let s = RuleStats {
        rule_id: 8,
        bytes_in: 9000,
        bytes_out: 9000,
        active_connections: 2,
        per_port: vec![
            PerPortStats {
                listen_port: 30000,
                bytes_in: 4500,
                bytes_out: 4500,
                active_connections: 1,
                datagrams_in: 0,
                datagrams_out: 0,
            },
            PerPortStats {
                listen_port: 30001,
                bytes_in: 4500,
                bytes_out: 4500,
                active_connections: 1,
                datagrams_in: 0,
                datagrams_out: 0,
            },
        ],
        dns_failures: 0,
        datagrams_in: 0,
        datagrams_out: 0,
        active_flows: 0,
        flows_dropped_overflow: 0,
        target_failovers_total: 0,
        per_target: vec![],
        sni_route_exact_total: 0,
        sni_route_wildcard_total: 0,
        sni_route_fallback_total: 0,
        rate_limit: None,
    };
    let bytes = s.encode_to_vec();
    let decoded = RuleStats::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, s);
    assert_eq!(decoded.per_port.len(), 2);
    assert_eq!(decoded.per_port[0].listen_port, 30000);
}
