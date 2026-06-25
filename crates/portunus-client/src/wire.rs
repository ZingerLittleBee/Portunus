//! Wire-translation functions: convert proto-free snapshot types from
//! portunus-forwarder into the proto wire types used on the bidi gRPC
//! stream.
//!
//! Spec: docs/superpowers/specs/2026-05-14-standalone-forwarder-design.md
//! §3.4 and §5.2. Keep these conversions byte-identical to the previous
//! inline constructions in control.rs — `*_wire_compat` tests enforce that.
//!
//! # Orphan-rule note
//!
//! Both the snapshot types (`portunus_forwarder::*`) and the proto types
//! (`portunus_proto::v1::*`) are foreign to this crate, so `From` impls are
//! prohibited by Rust's orphan rule (E0117). Free functions provide the
//! same conversion logic without violating the rule.

use portunus_forwarder::{
    OwnerRateLimitStatsSnapshot, PerPortStatsSnapshot, PerTargetStatsSnapshot,
    RateLimitRejectReason, RateLimitStatsSnapshot, RuleStatsSnapshot, SniListenerStatsSnapshot,
};
use portunus_proto::v1 as proto;

/// Translate a [`RateLimitRejectReason`] snapshot value to its proto enum.
pub fn reject_reason_to_proto(r: RateLimitRejectReason) -> proto::RateLimitRejectReason {
    match r {
        RateLimitRejectReason::Unspecified => proto::RateLimitRejectReason::Unspecified,
        RateLimitRejectReason::ConnConcurrent => proto::RateLimitRejectReason::ConnConcurrent,
        RateLimitRejectReason::ConnRate => proto::RateLimitRejectReason::ConnRate,
        RateLimitRejectReason::UdpFlowRate => proto::RateLimitRejectReason::UdpFlowRate,
        RateLimitRejectReason::OwnerConcurrent => proto::RateLimitRejectReason::OwnerConcurrent,
        RateLimitRejectReason::OwnerConnRate => proto::RateLimitRejectReason::OwnerConnRate,
        RateLimitRejectReason::OwnerUdpFlowRate => proto::RateLimitRejectReason::OwnerUdpFlowRate,
    }
}

/// Translate a [`RateLimitStatsSnapshot`] into its proto wire representation.
pub fn rate_limit_stats_to_proto(s: RateLimitStatsSnapshot) -> proto::RateLimitStats {
    proto::RateLimitStats {
        reject_total: s
            .reject_total
            .into_iter()
            .map(|(reason, total)| proto::RateLimitRejectCount {
                reason: reject_reason_to_proto(reason) as i32,
                total,
            })
            .collect(),
        throttle_micros_in: s.throttle_micros_in,
        throttle_micros_out: s.throttle_micros_out,
        active_connections: s.active_connections,
    }
}

/// Translate an [`OwnerRateLimitStatsSnapshot`] into its proto wire representation.
pub fn owner_rate_limit_stats_to_proto(
    s: OwnerRateLimitStatsSnapshot,
) -> proto::OwnerRateLimitStats {
    proto::OwnerRateLimitStats {
        owner_id: s.owner_id,
        stats: Some(rate_limit_stats_to_proto(s.stats)),
    }
}

/// Translate a [`PerPortStatsSnapshot`] into its proto wire representation.
pub fn per_port_stats_to_proto(s: PerPortStatsSnapshot) -> proto::PerPortStats {
    proto::PerPortStats {
        listen_port: u32::from(s.listen_port),
        bytes_in: s.bytes_in,
        bytes_out: s.bytes_out,
        active_connections: s.active_connections,
        datagrams_in: s.datagrams_in,
        datagrams_out: s.datagrams_out,
    }
}

/// Translate a [`PerTargetStatsSnapshot`] into its proto wire representation.
pub fn per_target_stats_to_proto(s: PerTargetStatsSnapshot) -> proto::PerTargetStats {
    proto::PerTargetStats {
        index: s.index,
        host: s.host,
        port: u32::from(s.port),
        priority: s.priority,
        health: s.health.as_wire(),
        consecutive_failures: s.consecutive_failures,
        last_failure_at_unix_ms: s.last_failure_at_unix_ms,
        last_success_at_unix_ms: s.last_success_at_unix_ms,
        bytes_in: s.bytes_in,
        bytes_out: s.bytes_out,
        connections_accepted: s.connections_accepted,
    }
}

/// Translate a [`SniListenerStatsSnapshot`] into its proto wire representation.
pub fn sni_listener_stats_to_proto(s: SniListenerStatsSnapshot) -> proto::SniListenerStats {
    proto::SniListenerStats {
        listen_port: u32::from(s.listen_port),
        sni_route_miss_total: s.sni_route_miss_total,
        client_hello_parse_failures_total: s.client_hello_parse_failures_total,
        client_hello_peek_bucket_counts: s.client_hello_peek_bucket_counts,
        client_hello_peek_sum_micros: s.client_hello_peek_sum_micros,
        client_hello_peek_count: s.client_hello_peek_count,
    }
}

/// Translate a [`RuleStatsSnapshot`] into its proto wire representation.
pub fn rule_stats_to_proto(s: RuleStatsSnapshot) -> proto::RuleStats {
    proto::RuleStats {
        rule_id: s.rule_id.0,
        bytes_in: s.bytes_in,
        bytes_out: s.bytes_out,
        active_connections: s.active_connections,
        per_port: s
            .per_port
            .into_iter()
            .map(per_port_stats_to_proto)
            .collect(),
        dns_failures: s.dns_failures,
        datagrams_in: s.datagrams_in,
        datagrams_out: s.datagrams_out,
        active_flows: s.active_flows,
        flows_dropped_overflow: s.flows_dropped_overflow,
        target_failovers_total: s.target_failovers_total,
        per_target: s
            .per_target
            .into_iter()
            .map(per_target_stats_to_proto)
            .collect(),
        sni_route_exact_total: s.sni_route_exact_total,
        sni_route_wildcard_total: s.sni_route_wildcard_total,
        sni_route_fallback_total: s.sni_route_fallback_total,
        rate_limit: s.rate_limit.map(rate_limit_stats_to_proto),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portunus_forwarder::TargetHealth;

    #[test]
    fn empty_rule_stats_round_trip_byte_identical() {
        let snap = RuleStatsSnapshot {
            rule_id: portunus_core::RuleId(0),
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
            sni_route_exact_total: 0,
            sni_route_wildcard_total: 0,
            sni_route_fallback_total: 0,
            rate_limit: None,
        };
        let p = rule_stats_to_proto(snap);
        assert_eq!(p, proto::RuleStats::default());
    }

    #[test]
    fn reject_reason_maps_every_variant_in_order() {
        // The snapshot enum and proto enum must agree variant-for-variant so
        // the `as i32` cast on the wire stays byte-identical.
        let cases = [
            (
                RateLimitRejectReason::Unspecified,
                proto::RateLimitRejectReason::Unspecified,
            ),
            (
                RateLimitRejectReason::ConnConcurrent,
                proto::RateLimitRejectReason::ConnConcurrent,
            ),
            (
                RateLimitRejectReason::ConnRate,
                proto::RateLimitRejectReason::ConnRate,
            ),
            (
                RateLimitRejectReason::UdpFlowRate,
                proto::RateLimitRejectReason::UdpFlowRate,
            ),
            (
                RateLimitRejectReason::OwnerConcurrent,
                proto::RateLimitRejectReason::OwnerConcurrent,
            ),
            (
                RateLimitRejectReason::OwnerConnRate,
                proto::RateLimitRejectReason::OwnerConnRate,
            ),
            (
                RateLimitRejectReason::OwnerUdpFlowRate,
                proto::RateLimitRejectReason::OwnerUdpFlowRate,
            ),
        ];
        for (snap, expected) in cases {
            assert_eq!(reject_reason_to_proto(snap), expected);
        }
    }

    #[test]
    fn rate_limit_stats_maps_all_fields_and_reject_totals() {
        let snap = RateLimitStatsSnapshot {
            reject_total: vec![
                (RateLimitRejectReason::ConnRate, 7),
                (RateLimitRejectReason::OwnerUdpFlowRate, 11),
            ],
            throttle_micros_in: 100,
            throttle_micros_out: 200,
            active_connections: 5,
        };
        let p = rate_limit_stats_to_proto(snap);

        assert_eq!(p.throttle_micros_in, 100);
        assert_eq!(p.throttle_micros_out, 200);
        assert_eq!(p.active_connections, 5);
        assert_eq!(
            p.reject_total,
            vec![
                proto::RateLimitRejectCount {
                    reason: proto::RateLimitRejectReason::ConnRate as i32,
                    total: 7,
                },
                proto::RateLimitRejectCount {
                    reason: proto::RateLimitRejectReason::OwnerUdpFlowRate as i32,
                    total: 11,
                },
            ],
        );
    }

    #[test]
    fn owner_rate_limit_stats_wraps_inner_stats() {
        let snap = OwnerRateLimitStatsSnapshot {
            owner_id: "owner-42".to_string(),
            stats: RateLimitStatsSnapshot {
                reject_total: vec![(RateLimitRejectReason::OwnerConcurrent, 3)],
                throttle_micros_in: 9,
                throttle_micros_out: 13,
                active_connections: 2,
            },
        };
        let p = owner_rate_limit_stats_to_proto(snap.clone());

        assert_eq!(p.owner_id, "owner-42");
        assert_eq!(p.stats, Some(rate_limit_stats_to_proto(snap.stats)));
    }

    #[test]
    fn per_port_stats_widens_port_and_copies_counters() {
        let snap = PerPortStatsSnapshot {
            listen_port: 8443,
            bytes_in: 1_000,
            bytes_out: 2_000,
            active_connections: 4,
            datagrams_in: 30,
            datagrams_out: 40,
        };
        let p = per_port_stats_to_proto(snap);

        // u16 listen_port is widened to u32 on the wire.
        assert_eq!(p.listen_port, 8443u32);
        assert_eq!(p.bytes_in, 1_000);
        assert_eq!(p.bytes_out, 2_000);
        assert_eq!(p.active_connections, 4);
        assert_eq!(p.datagrams_in, 30);
        assert_eq!(p.datagrams_out, 40);
    }

    #[test]
    fn per_target_stats_encodes_health_failed_as_wire() {
        let snap = PerTargetStatsSnapshot {
            index: 1,
            host: "backend.example".to_string(),
            port: 443,
            priority: 10,
            health: TargetHealth::Failed,
            consecutive_failures: 6,
            last_failure_at_unix_ms: 111,
            last_success_at_unix_ms: 222,
            bytes_in: 333,
            bytes_out: 444,
            connections_accepted: 55,
        };
        let p = per_target_stats_to_proto(snap);

        assert_eq!(p.index, 1);
        assert_eq!(p.host, "backend.example");
        assert_eq!(p.port, 443u32);
        assert_eq!(p.priority, 10);
        // Failed health maps to wire value 1.
        assert_eq!(p.health, 1u32);
        assert_eq!(p.consecutive_failures, 6);
        assert_eq!(p.last_failure_at_unix_ms, 111);
        assert_eq!(p.last_success_at_unix_ms, 222);
        assert_eq!(p.bytes_in, 333);
        assert_eq!(p.bytes_out, 444);
        assert_eq!(p.connections_accepted, 55);
    }

    #[test]
    fn per_target_stats_encodes_health_healthy_as_wire() {
        let snap = PerTargetStatsSnapshot {
            health: TargetHealth::Healthy,
            ..Default::default()
        };
        let p = per_target_stats_to_proto(snap);

        // Healthy health maps to wire value 0.
        assert_eq!(p.health, 0u32);
    }

    #[test]
    fn sni_listener_stats_widens_port_and_copies_histogram() {
        let snap = SniListenerStatsSnapshot {
            listen_port: 8080,
            sni_route_miss_total: 5,
            client_hello_parse_failures_total: 2,
            client_hello_peek_bucket_counts: vec![1, 2, 3, 4],
            client_hello_peek_sum_micros: 999,
            client_hello_peek_count: 8,
        };
        let p = sni_listener_stats_to_proto(snap);

        assert_eq!(p.listen_port, 8080u32);
        assert_eq!(p.sni_route_miss_total, 5);
        assert_eq!(p.client_hello_parse_failures_total, 2);
        assert_eq!(p.client_hello_peek_bucket_counts, vec![1, 2, 3, 4]);
        assert_eq!(p.client_hello_peek_sum_micros, 999);
        assert_eq!(p.client_hello_peek_count, 8);
    }

    #[test]
    fn rule_stats_maps_nested_collections_and_rate_limit() {
        let snap = RuleStatsSnapshot {
            rule_id: portunus_core::RuleId(99),
            bytes_in: 10,
            bytes_out: 20,
            active_connections: 3,
            per_port: vec![PerPortStatsSnapshot {
                listen_port: 1234,
                bytes_in: 1,
                bytes_out: 2,
                active_connections: 1,
                datagrams_in: 0,
                datagrams_out: 0,
            }],
            dns_failures: 4,
            datagrams_in: 5,
            datagrams_out: 6,
            active_flows: 7,
            flows_dropped_overflow: 8,
            target_failovers_total: 9,
            per_target: vec![PerTargetStatsSnapshot {
                index: 0,
                host: "t0".to_string(),
                port: 9000,
                priority: 1,
                health: TargetHealth::Healthy,
                consecutive_failures: 0,
                last_failure_at_unix_ms: 0,
                last_success_at_unix_ms: 1,
                bytes_in: 2,
                bytes_out: 3,
                connections_accepted: 4,
            }],
            sni_route_exact_total: 11,
            sni_route_wildcard_total: 12,
            sni_route_fallback_total: 13,
            rate_limit: Some(RateLimitStatsSnapshot {
                reject_total: vec![(RateLimitRejectReason::ConnConcurrent, 1)],
                throttle_micros_in: 14,
                throttle_micros_out: 15,
                active_connections: 2,
            }),
        };
        let p = rule_stats_to_proto(snap.clone());

        assert_eq!(p.rule_id, 99);
        assert_eq!(p.bytes_in, 10);
        assert_eq!(p.bytes_out, 20);
        assert_eq!(p.active_connections, 3);
        assert_eq!(p.dns_failures, 4);
        assert_eq!(p.datagrams_in, 5);
        assert_eq!(p.datagrams_out, 6);
        assert_eq!(p.active_flows, 7);
        assert_eq!(p.flows_dropped_overflow, 8);
        assert_eq!(p.target_failovers_total, 9);
        assert_eq!(p.sni_route_exact_total, 11);
        assert_eq!(p.sni_route_wildcard_total, 12);
        assert_eq!(p.sni_route_fallback_total, 13);

        // Nested collections are translated element-by-element.
        assert_eq!(p.per_port.len(), 1);
        assert_eq!(
            p.per_port[0],
            per_port_stats_to_proto(snap.per_port[0].clone()),
        );
        assert_eq!(p.per_target.len(), 1);
        assert_eq!(
            p.per_target[0],
            per_target_stats_to_proto(snap.per_target[0].clone()),
        );
        // Some(rate_limit) maps through the inner translator.
        assert_eq!(
            p.rate_limit,
            Some(rate_limit_stats_to_proto(snap.rate_limit.unwrap())),
        );
    }
}
