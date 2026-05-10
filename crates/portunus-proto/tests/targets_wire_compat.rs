//! Wire-compatibility tests for the additive multi-target fields (T004).
//!
//! Constitution Principle III gate: byte-identical encoding of a v0.6.0-shaped
//! `Rule` and `RuleStats` MUST hold across this change. The new
//! `Rule.targets` (field 9) and `Rule.health_check_interval_secs` (field 10),
//! and the new `RuleStats.target_failovers_total` (field 11) and
//! `RuleStats.per_target` (field 12), are all additive proto3 fields with
//! default-zero (or empty-repeated) values, so a v0.6.0-shaped message MUST
//! emit identical bytes before and after this change.
//!
//! Coverage matrix (W-1..W-6 from contracts/proto-rule-extension.md §5):
//!
//!   * W-1 + W-4 + W-6: a v0.6.0-shaped Rule / RuleStats (no new fields set)
//!     emits no new field tag bytes — the single-target hot path is bytes-on-
//!     wire identical.
//!   * W-2: encoded multi-target wire (only `targets` populated, `target_host`
//!     empty / `target_port` 0) is invalid for any v0.6.0 reader's downstream
//!     validation (we don't have a v0.6.0 reader in-tree, so the test asserts
//!     the wire representation a v0.6.0 reader would reject).
//!   * W-3: dual-shape rejection lives at the HTTP layer (T013a in
//!     portunus-server tests), not at the proto layer.
//!   * W-5: a v0.7 RuleStats with both new fields populated round-trips; a
//!     v0.6.0-shaped RuleStats (defaults) decodes identically.

use portunus_proto::v1::{PerTargetStats, Protocol, ProxyProtocolVersion, Rule, RuleStats, Target};
use prost::Message;

// ----- W-1 / W-4 / W-6: single-target byte-identity ------------------------

#[test]
fn v06_single_target_rule_emits_no_new_field_tags() {
    // Single-byte target host so a byte-scan for new tags can't match the
    // host string itself (0x68 == 'h').
    let r = Rule {
        rule_id: 9,
        listen_port: 18080,
        target_host: String::from("h"),
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
    let bytes = r.encode_to_vec();

    // Field 9 wire-type 2 (length-delimited) = (9 << 3) | 2 = 0x4A.
    // Field 10 wire-type 0 (varint)         = (10 << 3) | 0 = 0x50.
    // proto3 elides default-empty repeated and default-zero scalar fields.
    assert!(
        !bytes.contains(&0x4A),
        "tag for targets must be absent for v0.6.0-shaped Rule — got {bytes:?}"
    );
    assert!(
        !bytes.contains(&0x50),
        "tag for health_check_interval_secs must be absent when 0 — got {bytes:?}"
    );

    let decoded = Rule::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, r);
    assert!(decoded.targets.is_empty());
    assert_eq!(decoded.health_check_interval_secs, 0);
}

#[test]
fn v06_single_target_rule_stats_emits_no_new_field_tags() {
    let s = RuleStats {
        rule_id: 9,
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

    // Field 11 wire-type 0 (varint)         = (11 << 3) | 0 = 0x58.
    // Field 12 wire-type 2 (length-delimited) = (12 << 3) | 2 = 0x62.
    assert!(
        !bytes.contains(&0x58),
        "tag for target_failovers_total must be absent when 0 — got {bytes:?}"
    );
    assert!(
        !bytes.contains(&0x62),
        "tag for per_target must be absent when empty — got {bytes:?}"
    );

    let decoded = RuleStats::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, s);
    assert_eq!(decoded.target_failovers_total, 0);
    assert!(decoded.per_target.is_empty());
}

// ----- Round-trip: multi-target rule ---------------------------------------

#[test]
fn multi_target_rule_roundtrips() {
    let r = Rule {
        rule_id: 42,
        listen_port: 8080,
        // back-compat encoding: legacy fields cleared on multi-target rules.
        target_host: String::new(),
        target_port: 0,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: None,
        targets: vec![
            Target {
                host: "primary.example.com".into(),
                port: 80,
                priority: 0,
                proxy_protocol: None,
            },
            Target {
                host: "secondary.example.com".into(),
                port: 80,
                priority: 1,
                proxy_protocol: None,
            },
        ],
        health_check_interval_secs: 30,
        sni_pattern: None,
        rate_limit: None,
        owner_id: None,
    };
    let bytes = r.encode_to_vec();
    let decoded = Rule::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, r);
    assert_eq!(decoded.targets.len(), 2);
    assert_eq!(decoded.targets[0].host, "primary.example.com");
    assert_eq!(decoded.targets[1].priority, 1);
    assert_eq!(decoded.health_check_interval_secs, 30);
}

#[test]
fn multi_target_rule_with_legacy_fields_clear_keeps_back_compat_shape() {
    // W-2: multi-target wire MUST NOT populate the legacy target_host/
    // target_port fields. A v0.6.0 reader decoding this would see an empty
    // target_host (which v0.6.0 validation rejects) and would never
    // accidentally treat the first target as the canonical upstream.
    let r = Rule {
        rule_id: 100,
        listen_port: 9000,
        target_host: String::new(),
        target_port: 0,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: None,
        targets: vec![Target {
            host: "a.test".into(),
            port: 80,
            priority: 0,
            proxy_protocol: None,
        }],
        health_check_interval_secs: 0,
        sni_pattern: None,
        rate_limit: None,
        owner_id: None,
    };
    let bytes = r.encode_to_vec();

    // Field 3 (target_host, length-delimited) tag = (3 << 3) | 2 = 0x1A.
    // Field 4 (target_port, varint)           tag = (4 << 3) | 0 = 0x20.
    // Both must be absent on a multi-target rule whose legacy fields are
    // proto3-default (empty / zero).
    assert!(
        !bytes.contains(&0x1A),
        "legacy target_host tag must be absent on multi-target rule — got {bytes:?}"
    );
    assert!(
        !bytes.contains(&0x20),
        "legacy target_port tag must be absent on multi-target rule — got {bytes:?}"
    );

    let decoded = Rule::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded.targets.len(), 1);
    assert!(decoded.target_host.is_empty());
    assert_eq!(decoded.target_port, 0);
}

// ----- Round-trip: stats with per-target ----------------------------------

#[test]
fn rule_stats_per_target_roundtrips() {
    let s = RuleStats {
        rule_id: 42,
        bytes_in: 1_000_000,
        bytes_out: 800_000,
        active_connections: 10,
        per_port: vec![],
        dns_failures: 0,
        datagrams_in: 0,
        datagrams_out: 0,
        active_flows: 0,
        flows_dropped_overflow: 0,
        target_failovers_total: 3,
        per_target: vec![
            PerTargetStats {
                index: 0,
                host: "primary.example.com".into(),
                port: 80,
                priority: 0,
                health: 0, // Healthy
                consecutive_failures: 0,
                last_failure_at_unix_ms: 1_700_000_000_000,
                last_success_at_unix_ms: 1_700_000_010_000,
                bytes_in: 800_000,
                bytes_out: 600_000,
                connections_accepted: 8,
            },
            PerTargetStats {
                index: 1,
                host: "secondary.example.com".into(),
                port: 80,
                priority: 1,
                health: 1, // Failed
                consecutive_failures: 5,
                last_failure_at_unix_ms: 1_700_000_005_000,
                last_success_at_unix_ms: 0,
                bytes_in: 200_000,
                bytes_out: 200_000,
                connections_accepted: 2,
            },
        ],
        sni_route_exact_total: 0,
        sni_route_wildcard_total: 0,
        sni_route_fallback_total: 0,
        rate_limit: None,
    };
    let bytes = s.encode_to_vec();
    let decoded = RuleStats::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, s);
    assert_eq!(decoded.target_failovers_total, 3);
    assert_eq!(decoded.per_target.len(), 2);
    assert_eq!(decoded.per_target[0].host, "primary.example.com");
    assert_eq!(decoded.per_target[1].health, 1);
}

#[test]
fn target_message_roundtrips_with_priority_zero() {
    // Defensive check: a Target with priority 0 (the default proto3 value)
    // round-trips correctly. priority=0 means "highest priority", so its
    // value must be preserved across encode/decode even when the encoded
    // varint is elided as a default.
    let t = Target {
        host: "primary.example.com".into(),
        port: 80,
        priority: 0,
        proxy_protocol: None,
    };
    let bytes = t.encode_to_vec();
    let decoded = Target::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, t);
    assert_eq!(decoded.priority, 0);
}

#[test]
fn target_message_roundtrips_with_proxy_protocol() {
    let t = Target {
        host: "proxy.example.com".into(),
        port: 443,
        priority: 0,
        proxy_protocol: Some(ProxyProtocolVersion::V2 as i32),
    };
    let bytes = t.encode_to_vec();
    let decoded = Target::decode(bytes.as_slice()).expect("decode");
    assert_eq!(decoded, t);
    assert_eq!(
        ProxyProtocolVersion::try_from(decoded.proxy_protocol.expect("field present")).ok(),
        Some(ProxyProtocolVersion::V2)
    );
}
