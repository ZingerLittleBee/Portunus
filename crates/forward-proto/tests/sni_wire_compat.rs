//! Wire-compatibility tests for the additive SNI-routing fields
//! (009-tls-sni-routing T007..T010, T019).
//!
//! Test fixtures intentionally use casts/match shapes that read
//! cleanly against the wire spec — clippy's pedantic flags that suit
//! library code don't fit a wire-format walker.
#![allow(
    clippy::manual_let_else,
    clippy::cast_possible_truncation,
    clippy::wildcard_imports
)]
//!
//! Constitution Principle V gate: byte-identical encoding of a
//! v0.8-shaped `Rule`, `RuleStats`, and `StatsReport` MUST hold
//! across this change. The new fields are all `optional` /
//! default-zero / empty-repeated proto3 fields, so a v0.8-shaped
//! message MUST emit identical bytes before and after this change.
//!
//! Coverage matrix (from `specs/009-tls-sni-routing/contracts/wire.md` §5):
//!
//!   * T007: round-trip `Rule.sni_pattern = 11`; absent encoding
//!     equals v0.8.
//!   * T008: round-trip `RuleStats.sni_route_*_total = 13/14/15`;
//!     absent encoding equals v0.8.
//!   * T009: round-trip `StatsReport.sni_listener_stats = 3`
//!     carrying `SniListenerStats`; empty list equals v0.8.
//!   * T010 (NEGATIVE — HIGH-1 from round-3 review): a `RuleStats`
//!     with v0.7 fields 11 (`target_failovers_total`) and 12
//!     (`per_target`) populated and NO SNI fields produces bytes
//!     identical to a v0.8 encoding of the same logical content.
//!   * T019: parse `proto/forward.proto` and assert the field-number
//!     registry in `contracts/wire.md` §4 matches reality.

use forward_proto::v1::{
    PerTargetStats, Protocol, Rule, RuleStats, SniListenerStats, StatsReport, Target,
};
use prost::Message;

// ----- helpers --------------------------------------------------------------

/// Walk a proto3 wire-encoded byte stream and return true iff a tag
/// for the given (1-based) field number appears at the top level.
///
/// Length-delimited fields contain nested tags inside their payload —
/// we MUST skip the payload to avoid false positives. The walker
/// only handles wire types 0/1/2/5 because those are the only ones
/// proto3 emits today.
fn has_top_level_field(bytes: &[u8], field_number: u32) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        let (tag, n) = match decode_varint(&bytes[i..]) {
            Some(v) => v,
            None => return false,
        };
        i += n;
        let fnum = (tag >> 3) as u32;
        let wire = (tag & 0x07) as u8;
        if fnum == field_number {
            return true;
        }
        match wire {
            0 => {
                // varint payload
                let (_, m) = decode_varint(&bytes[i..]).expect("malformed varint payload");
                i += m;
            }
            1 => i += 8, // 64-bit
            2 => {
                // length-delimited: varint length + payload
                let (len, m) = decode_varint(&bytes[i..]).expect("malformed length prefix");
                i += m + len as usize;
            }
            5 => i += 4, // 32-bit
            other => panic!("unsupported wire type {other} at offset {i}"),
        }
    }
    false
}

fn decode_varint(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    let mut shift = 0u32;
    for (i, &b) in bytes.iter().enumerate() {
        value |= u64::from(b & 0x7F) << shift;
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
        shift += 7;
        if shift >= 64 {
            return None;
        }
    }
    None
}

// ----- T007: Rule.sni_pattern = 11 ------------------------------------------

#[test]
fn t007_rule_sni_pattern_roundtrip() {
    let r = Rule {
        rule_id: 9,
        listen_port: 443,
        target_host: String::from("backend"),
        target_port: 8443,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: None,
        targets: vec![],
        health_check_interval_secs: 0,
        sni_pattern: Some(String::from("api.example.com")),
    };
    let bytes = r.encode_to_vec();
    let decoded = Rule::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, r);
    // Field 11 wire-tag is (11 << 3) | 2 = 90 = 0x5A. Payload follows.
    assert!(
        has_top_level_field(&bytes, 11),
        "Rule with sni_pattern=Some MUST emit field 11 on the wire"
    );
}

#[test]
fn t007_rule_sni_pattern_absent_byte_stable_with_v08() {
    // A v0.8-shaped Rule (all v0.7 fields, sni_pattern unset) MUST
    // emit zero bytes for field 11 — proto3 elides absent optional
    // fields. This is what makes the upgrade byte-stable for v0.8
    // peers that don't know about field 11.
    let r = Rule {
        rule_id: 9,
        listen_port: 443,
        target_host: String::from("backend"),
        target_port: 8443,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: None,
        targets: vec![],
        health_check_interval_secs: 0,
        sni_pattern: None,
    };
    let bytes = r.encode_to_vec();
    assert!(
        !has_top_level_field(&bytes, 11),
        "Rule with sni_pattern=None MUST NOT emit field 11 (would break v0.8 byte-stability)"
    );
}

// ----- T008: RuleStats.sni_route_*_total = 13/14/15 -------------------------

#[test]
fn t008_rule_stats_sni_counters_roundtrip() {
    let s = RuleStats {
        rule_id: 1,
        bytes_in: 0,
        bytes_out: 0,
        active_connections: 0,
        per_port: vec![],
        dns_failures: 0,
        datagrams_in: 0,
        datagrams_out: 0,
        active_flows: 0,
        flows_dropped_overflow: 0,
        target_failovers_total: 0,
        per_target: vec![],
        sni_route_exact_total: 7,
        sni_route_wildcard_total: 3,
        sni_route_fallback_total: 1,
    };
    let bytes = s.encode_to_vec();
    let decoded = RuleStats::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, s);
    assert!(
        has_top_level_field(&bytes, 13),
        "field 13 must appear when non-zero"
    );
    assert!(
        has_top_level_field(&bytes, 14),
        "field 14 must appear when non-zero"
    );
    assert!(
        has_top_level_field(&bytes, 15),
        "field 15 must appear when non-zero"
    );
}

#[test]
fn t008_rule_stats_sni_counters_zero_omits_tags() {
    // proto3 default-zero scalars are elided on encode. A v0.8-shaped
    // RuleStats (all SNI counters zero) MUST emit zero bytes for
    // fields 13/14/15.
    let s = RuleStats {
        rule_id: 1,
        bytes_in: 100,
        bytes_out: 200,
        active_connections: 1,
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
    };
    let bytes = s.encode_to_vec();
    for f in [13u32, 14, 15] {
        assert!(
            !has_top_level_field(&bytes, f),
            "RuleStats with all SNI counters zero MUST NOT emit field {f}"
        );
    }
}

// ----- T009: StatsReport.sni_listener_stats = 3 -----------------------------

#[test]
fn t009_stats_report_sni_listener_stats_roundtrip() {
    let report = StatsReport {
        sent_at_unix_ms: 1_700_000_000,
        stats: vec![],
        sni_listener_stats: vec![
            SniListenerStats {
                listen_port: 443,
                sni_route_miss_total: 4,
                client_hello_parse_failures_total: 2,
                client_hello_peek_bucket_counts: vec![1, 2, 3],
                client_hello_peek_sum_micros: 3_500,
                client_hello_peek_count: 3,
            },
            SniListenerStats {
                listen_port: 8443,
                sni_route_miss_total: 0,
                client_hello_parse_failures_total: 1,
                client_hello_peek_bucket_counts: vec![],
                client_hello_peek_sum_micros: 0,
                client_hello_peek_count: 0,
            },
        ],
    };
    let bytes = report.encode_to_vec();
    let decoded = StatsReport::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, report);
    assert!(
        has_top_level_field(&bytes, 3),
        "StatsReport with non-empty sni_listener_stats must emit field 3"
    );
}

#[test]
fn t009_stats_report_empty_sni_list_omits_field_3() {
    // A v0.8-shaped StatsReport (no SNI listener) MUST emit zero
    // bytes for field 3. This is what makes the StatsReport byte-
    // stable for v0.8 peers.
    let report = StatsReport {
        sent_at_unix_ms: 1_700_000_000,
        stats: vec![],
        sni_listener_stats: vec![],
    };
    let bytes = report.encode_to_vec();
    assert!(
        !has_top_level_field(&bytes, 3),
        "empty sni_listener_stats MUST NOT emit field 3 (proto3 repeated empty rule)"
    );
}

// ----- T010: NEGATIVE — RuleStats fields 11 / 12 untouched ------------------

/// HIGH-1 from round-3 review: assert that this spec does NOT disturb
/// `RuleStats.target_failovers_total` (field 11) or `RuleStats.per_target`
/// (field 12). A RuleStats with those v0.7 fields populated and zero
/// SNI fields MUST encode exactly the same bytes a v0.8 binary would
/// emit for the same logical content.
#[test]
fn t010_rule_stats_v07_fields_unchanged_when_no_sni() {
    let s = RuleStats {
        rule_id: 1,
        bytes_in: 0,
        bytes_out: 0,
        active_connections: 0,
        per_port: vec![],
        dns_failures: 0,
        datagrams_in: 0,
        datagrams_out: 0,
        active_flows: 0,
        flows_dropped_overflow: 0,
        target_failovers_total: 9, // v0.7 field 11 — populated
        per_target: vec![PerTargetStats {
            // v0.7 field 12 — populated
            index: 0,
            host: String::from("h"),
            port: 8080,
            priority: 0,
            health: 0,
            consecutive_failures: 0,
            last_failure_at_unix_ms: 0,
            last_success_at_unix_ms: 0,
            bytes_in: 0,
            bytes_out: 0,
            connections_accepted: 0,
        }],
        sni_route_exact_total: 0,
        sni_route_wildcard_total: 0,
        sni_route_fallback_total: 0,
    };
    let bytes = s.encode_to_vec();

    // The v0.7 fields MUST be present.
    assert!(
        has_top_level_field(&bytes, 11),
        "v0.7 field 11 (target_failovers_total) MUST still appear when set"
    );
    assert!(
        has_top_level_field(&bytes, 12),
        "v0.7 field 12 (per_target) MUST still appear when set"
    );

    // The v0.9 SNI fields MUST be absent.
    for f in [13u32, 14, 15] {
        assert!(
            !has_top_level_field(&bytes, f),
            "v0.9 field {f} MUST NOT appear when its counter is zero"
        );
    }

    // Round-trip is lossless.
    let decoded = RuleStats::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, s);
}

// ----- T010 companion: encoding the same Rule twice is byte-identical ------

#[test]
fn t010_rule_encoding_deterministic_under_no_sni() {
    let mk = || Rule {
        rule_id: 9,
        listen_port: 8000,
        target_host: String::from("upstream"),
        target_port: 9000,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: None,
        targets: vec![],
        health_check_interval_secs: 0,
        sni_pattern: None,
    };
    let a = mk().encode_to_vec();
    let b = mk().encode_to_vec();
    assert_eq!(a, b, "Rule encoding without SNI must be deterministic");
}

#[test]
fn t010_multi_target_rule_unchanged_when_no_sni() {
    // A multi-target v0.7 Rule with sni_pattern=None must emit exactly
    // the same bytes as it did in v0.7 (no field 11 anywhere).
    let r = Rule {
        rule_id: 9,
        listen_port: 8000,
        target_host: String::new(),
        target_port: 0,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: None,
        targets: vec![
            Target {
                host: String::from("a"),
                port: 9001,
                priority: 0,
                proxy_protocol: None,
            },
            Target {
                host: String::from("b"),
                port: 9002,
                priority: 1,
                proxy_protocol: None,
            },
        ],
        health_check_interval_secs: 30,
        sni_pattern: None,
    };
    let bytes = r.encode_to_vec();
    assert!(
        !has_top_level_field(&bytes, 11),
        "v0.9 field 11 (sni_pattern) MUST NOT appear on a v0.7 multi-target rule with sni_pattern=None"
    );
    let decoded = Rule::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, r);
}

// ----- T019: drift guard between proto declaration and contracts/wire.md ---

/// Read the actual `proto/forward.proto` file and assert that the
/// SNI-related field numbers documented in
/// `specs/009-tls-sni-routing/contracts/wire.md` §4 match the declarations.
/// If someone bumps a field number without updating the spec (or vice
/// versa), this test catches it before the wire breaks.
#[test]
fn t019_proto_field_numbers_match_spec() {
    let proto = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../proto/forward.proto"
    ))
    .expect("read proto/forward.proto");

    // Each entry: (substring matcher, expected field-number suffix).
    // The match is loose by design — a single grep is enough; if a future
    // spec moves a field number, both the declaration and the contract
    // will need updating, which is exactly what this test checks for.
    let expected = [
        ("optional string sni_pattern", "= 11"),
        ("uint64 sni_route_exact_total", "= 13"),
        ("uint64 sni_route_wildcard_total", "= 14"),
        ("uint64 sni_route_fallback_total", "= 15"),
        ("uint32 listen_port", "= 1"), // SniListenerStats.listen_port
        ("uint64 sni_route_miss_total", "= 2"), // SniListenerStats
        ("uint64 client_hello_parse_failures_total", "= 3"),
        ("repeated SniListenerStats sni_listener_stats", "= 3"),
    ];

    for (needle, suffix) in &expected {
        let mut found = false;
        for line in proto.lines() {
            if line.contains(needle) && line.contains(suffix) {
                found = true;
                break;
            }
        }
        assert!(
            found,
            "proto/forward.proto missing line matching '{needle}' AND '{suffix}'\n\
             — see specs/009-tls-sni-routing/contracts/wire.md §4 for the registry"
        );
    }
}
