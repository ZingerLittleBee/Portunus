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
    RateLimitRejectReason, RateLimitStatsSnapshot, RuleStatsSnapshot,
    SniListenerStatsSnapshot,
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
        per_port: s.per_port.into_iter().map(per_port_stats_to_proto).collect(),
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
}
