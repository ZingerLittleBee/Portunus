//! Wire-compatibility tests for the additive rate-limiting fields
//! (011-rate-limiting-qos T004, T005).
//!
//! Pins the v0.11 wire delta documented in
//! `specs/011-rate-limiting-qos/contracts/wire.md`:
//!
//! - `Rule.rate_limit = 12` (optional message)
//! - `RuleStats.rate_limit = 16` (optional message)
//! - `StatsReport.owner_rate_limit_stats = 4` (repeated)
//! - new top-level messages: `RateLimit`, `RateLimitStats`,
//!   `RateLimitRejectCount`, `OwnerRateLimitStats`,
//!   `OwnerRateLimitUpdate`
//! - new enums: `RateLimitRejectReason`, `OwnerRateLimitAction`
//!
//! Constitution II gate (SC-004 wire-side): a v0.10-shaped `Rule`
//! / `RuleStats` / `StatsReport` MUST emit byte-identical bytes
//! before and after v0.11. The new fields are all proto3
//! `optional` / `repeated` / message-typed, so absent on the
//! sender side ⇒ no bytes for the new tag — proto3
//! default-stripping is the load-bearing invariant.

#![allow(
    clippy::manual_let_else,
    clippy::cast_possible_truncation,
    clippy::wildcard_imports
)]

use forward_proto::v1::{
    OwnerRateLimitAction, OwnerRateLimitStats, OwnerRateLimitUpdate, PerTargetStats, Protocol,
    RateLimit, RateLimitRejectCount, RateLimitRejectReason, RateLimitStats, Rule, RuleStats,
    SniListenerStats, StatsReport, Target,
};
use prost::Message;

// ----- helpers --------------------------------------------------------------

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
                let (_, m) = decode_varint(&bytes[i..]).expect("malformed varint payload");
                i += m;
            }
            1 => i += 8,
            2 => {
                let (len, m) = decode_varint(&bytes[i..]).expect("malformed length prefix");
                i += m + len as usize;
            }
            5 => i += 4,
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

fn empty_rate_limit() -> RateLimit {
    RateLimit {
        bandwidth_in_bps: None,
        bandwidth_out_bps: None,
        new_connections_per_sec: None,
        concurrent_connections: None,
        bandwidth_in_burst: None,
        bandwidth_out_burst: None,
        new_connections_burst: None,
    }
}

fn full_rate_limit() -> RateLimit {
    RateLimit {
        bandwidth_in_bps: Some(1_048_576),
        bandwidth_out_bps: Some(2_097_152),
        new_connections_per_sec: Some(50),
        concurrent_connections: Some(200),
        bandwidth_in_burst: Some(2_097_152),
        bandwidth_out_burst: Some(4_194_304),
        new_connections_burst: Some(100),
    }
}

// ----- T004: Rule.rate_limit = 12 -------------------------------------------

#[test]
fn t004_rule_rate_limit_roundtrip_when_present() {
    let r = Rule {
        rule_id: 1,
        listen_port: 443,
        target_host: String::from("upstream"),
        target_port: 8443,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: None,
        targets: vec![],
        health_check_interval_secs: 0,
        sni_pattern: None,
        rate_limit: Some(full_rate_limit()),
        owner_id: None,
    };
    let bytes = r.encode_to_vec();
    let decoded = Rule::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, r);
    assert!(
        has_top_level_field(&bytes, 12),
        "Rule with non-empty rate_limit must emit field 12"
    );
}

#[test]
fn t004_rule_rate_limit_partial_caps_roundtrip() {
    // Only `bandwidth_in_bps` is set; every other dimension is None.
    // The wire encoding still emits field 12 (the message exists),
    // but the inner RateLimit only emits field 1.
    let r = Rule {
        rule_id: 7,
        listen_port: 80,
        target_host: String::from("backend"),
        target_port: 8080,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: None,
        targets: vec![],
        health_check_interval_secs: 0,
        sni_pattern: None,
        rate_limit: Some(RateLimit {
            bandwidth_in_bps: Some(1_000_000),
            ..empty_rate_limit()
        }),
        owner_id: None,
    };
    let bytes = r.encode_to_vec();
    let decoded = Rule::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, r);
}

// ----- T005: byte-stability gate --------------------------------------------

#[test]
fn t005_v010_rule_byte_identical_when_rate_limit_absent() {
    // A v0.10-shaped Rule (no rate_limit field set on the sender)
    // MUST encode to the same bytes as a Rule constructed without
    // any awareness of v0.11. proto3 default-stripping requires the
    // optional message to omit all bytes when None.
    let r = Rule {
        rule_id: 42,
        listen_port: 443,
        target_host: String::from("upstream"),
        target_port: 8443,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: None,
        targets: vec![Target {
            host: String::from("10.0.0.1"),
            port: 8443,
            priority: 0,
            proxy_protocol: None,
        }],
        health_check_interval_secs: 0,
        sni_pattern: Some(String::from("api.example.com")),
        rate_limit: None,
        owner_id: None,
    };
    let bytes = r.encode_to_vec();
    assert!(
        !has_top_level_field(&bytes, 12),
        "Rule with rate_limit=None MUST NOT emit field 12 — byte-stability gate"
    );
    let decoded = Rule::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded.rate_limit, None);
}

#[test]
fn t005_v010_rule_stats_byte_identical_when_rate_limit_absent() {
    let s = RuleStats {
        rule_id: 7,
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
        sni_route_exact_total: 5,
        sni_route_wildcard_total: 1,
        sni_route_fallback_total: 2,
        rate_limit: None,
    };
    let bytes = s.encode_to_vec();
    assert!(
        !has_top_level_field(&bytes, 16),
        "RuleStats with rate_limit=None MUST NOT emit field 16 — byte-stability gate"
    );
}

#[test]
fn t005_v010_stats_report_byte_identical_when_owner_stats_empty() {
    // StatsReport.owner_rate_limit_stats is a `repeated` field, so
    // an empty vec emits zero bytes for tag 4 (proto3 default-strips
    // empty repeated fields).
    let report = StatsReport {
        sent_at_unix_ms: 1_700_000_000,
        stats: vec![],
        sni_listener_stats: vec![SniListenerStats {
            listen_port: 443,
            sni_route_miss_total: 0,
            client_hello_parse_failures_total: 0,
            client_hello_peek_bucket_counts: vec![],
            client_hello_peek_sum_micros: 0,
            client_hello_peek_count: 0,
        }],
        owner_rate_limit_stats: Vec::new(),
    };
    let bytes = report.encode_to_vec();
    assert!(
        !has_top_level_field(&bytes, 4),
        "StatsReport with empty owner_rate_limit_stats MUST NOT emit field 4 — byte-stability gate"
    );
}

// ----- New-message round-trips ----------------------------------------------

#[test]
fn t004_rate_limit_stats_roundtrip() {
    let s = RateLimitStats {
        reject_total: vec![
            RateLimitRejectCount {
                reason: RateLimitRejectReason::ConnConcurrent as i32,
                total: 4,
            },
            RateLimitRejectCount {
                reason: RateLimitRejectReason::OwnerConnRate as i32,
                total: 1,
            },
        ],
        throttle_micros_in: 1_500_000,
        throttle_micros_out: 0,
        active_connections: 25,
    };
    let bytes = s.encode_to_vec();
    let decoded = RateLimitStats::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, s);
}

#[test]
fn t004_owner_rate_limit_stats_roundtrip() {
    let s = OwnerRateLimitStats {
        owner_id: String::from("alice"),
        stats: Some(RateLimitStats {
            reject_total: vec![],
            throttle_micros_in: 999,
            throttle_micros_out: 999,
            active_connections: 7,
        }),
    };
    let bytes = s.encode_to_vec();
    let decoded = OwnerRateLimitStats::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, s);
}

#[test]
fn t004_owner_rate_limit_update_set_roundtrip() {
    let u = OwnerRateLimitUpdate {
        client_name: String::from("edge-01"),
        owner_id: String::from("alice"),
        rate_limit: Some(full_rate_limit()),
        action: OwnerRateLimitAction::Set as i32,
    };
    let bytes = u.encode_to_vec();
    let decoded = OwnerRateLimitUpdate::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, u);
}

#[test]
fn t004_owner_rate_limit_update_remove_roundtrip() {
    let u = OwnerRateLimitUpdate {
        client_name: String::from("edge-01"),
        owner_id: String::from("alice"),
        rate_limit: None,
        action: OwnerRateLimitAction::Remove as i32,
    };
    let bytes = u.encode_to_vec();
    let decoded = OwnerRateLimitUpdate::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, u);
}

// ----- T005: rule with all v0.10 features set + rate_limit absent ----------

#[test]
fn t005_v010_full_feature_rule_byte_identical_when_rate_limit_absent() {
    // A rule that exercises every v0.10 feature (multi-target,
    // proxy_protocol per target, SNI, prefer_ipv6) MUST emit
    // byte-identical bytes regardless of v0.11's awareness — i.e.
    // sender that knows about field 12 but leaves it None must not
    // change the wire.
    let r = Rule {
        rule_id: 99,
        listen_port: 443,
        target_host: String::new(), // multi-target rule: target_host is empty
        target_port: 0,
        protocol: Protocol::Tcp as i32,
        listen_port_end: 0,
        target_port_end: 0,
        prefer_ipv6: Some(true),
        targets: vec![
            Target {
                host: String::from("10.0.0.1"),
                port: 8443,
                priority: 0,
                proxy_protocol: Some(forward_proto::v1::ProxyProtocolVersion::V1 as i32),
            },
            Target {
                host: String::from("10.0.0.2"),
                port: 8443,
                priority: 1,
                proxy_protocol: Some(forward_proto::v1::ProxyProtocolVersion::V2 as i32),
            },
        ],
        health_check_interval_secs: 30,
        sni_pattern: Some(String::from("api.example.com")),
        rate_limit: None,
        owner_id: None,
    };
    let bytes = r.encode_to_vec();
    assert!(
        !has_top_level_field(&bytes, 12),
        "v0.10 feature-complete Rule must NOT emit field 12 when rate_limit=None"
    );
    // Round-trip sanity.
    let decoded = Rule::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, r);
}

#[test]
fn t005_per_target_stats_unchanged_by_v011() {
    // PerTargetStats and the v0.7 stats fields (tags 11/12) must
    // continue to round-trip unchanged. v0.11 added no fields to
    // PerTargetStats — this is a regression sentinel.
    let pt = PerTargetStats {
        index: 0,
        host: String::from("10.0.0.1"),
        port: 8443,
        priority: 0,
        health: 0,
        consecutive_failures: 0,
        last_failure_at_unix_ms: 0,
        last_success_at_unix_ms: 0,
        bytes_in: 1_000,
        bytes_out: 2_000,
        connections_accepted: 5,
    };
    let bytes = pt.encode_to_vec();
    let decoded = PerTargetStats::decode(bytes.as_slice()).expect("decodes");
    assert_eq!(decoded, pt);
}

// ----- Reject-reason enum coverage ------------------------------------------

#[test]
fn t004_all_reject_reasons_round_trip() {
    let reasons = [
        RateLimitRejectReason::Unspecified,
        RateLimitRejectReason::ConnConcurrent,
        RateLimitRejectReason::ConnRate,
        RateLimitRejectReason::UdpFlowRate,
        RateLimitRejectReason::OwnerConcurrent,
        RateLimitRejectReason::OwnerConnRate,
        RateLimitRejectReason::OwnerUdpFlowRate,
    ];
    for r in reasons {
        let c = RateLimitRejectCount {
            reason: r as i32,
            total: 17,
        };
        let bytes = c.encode_to_vec();
        let decoded = RateLimitRejectCount::decode(bytes.as_slice()).expect("decodes");
        assert_eq!(decoded.reason, r as i32);
        assert_eq!(decoded.total, 17);
    }
}
