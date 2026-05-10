//! 007-multi-target-failover T032 — `portunus_rule_target_failovers_total`
//! cardinality budget (SC-006). The collector MUST stay at zero rows
//! for single-target rules and gain exactly one row per
//! `(client, rule, owner)` triple that actually experiences a
//! failover. Verified by driving `RuleStatsCache::observe_with_targets`
//! directly — that's the single seam the gRPC StatsReport path funnels
//! through, so cardinality there is cardinality on the wire.

use std::str::FromStr;

use portunus_core::{ClientName, RuleId};
use portunus_server::metrics::{Metrics, RuleStatsCache};

/// Single-target rules contribute zero series to
/// `portunus_rule_target_failovers_total`. The collector renders
/// nothing — not even a header — until a multi-target rule with a
/// non-zero failover delta lands.
#[tokio::test]
async fn single_target_rules_emit_zero_failover_series() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str("edge-a").expect("client");

    // Three single-target rules, observed multiple times each. Each
    // observation passes `target_failovers_total: 0` (legacy `observe`
    // path forwards to `observe_with_targets` with that default).
    for rid in [RuleId(10), RuleId(11), RuleId(12)] {
        for (b_in, b_out) in [(100_u64, 200), (300, 400), (500, 600)] {
            cache
                .observe(
                    &client, rid, "alice", b_in, b_out, 1, 0, 0, 0, 0, 0, &metrics,
                )
                .await;
        }
    }

    let body = String::from_utf8(metrics.render()).expect("utf8 metrics");
    let row_count = body
        .lines()
        .filter(|l| l.starts_with("portunus_rule_target_failovers_total{"))
        .count();
    assert_eq!(
        row_count, 0,
        "single-target rules MUST NOT create failover series — got {row_count}\n--- body ---\n{body}"
    );
}

/// Multi-target rules with actual failovers produce exactly one row
/// per `(client, rule, owner)` triple. Repeated observations of the
/// same monotonic counter MUST NOT double-count.
#[tokio::test]
async fn multi_target_failovers_emit_one_row_per_triple() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str("edge-b").expect("client");

    // Two multi-target rules, three monotonic observations each.
    // Final cumulative `target_failovers_total` per rule = 5.
    let rules = [(RuleId(20), "alice"), (RuleId(21), "bob")];
    for (rid, owner) in rules {
        for total in [1_u64, 3, 5] {
            cache
                .observe_with_targets(
                    &client,
                    rid,
                    owner,
                    100,
                    200,
                    1,
                    0,
                    0,
                    0,
                    0,
                    0,
                    total,
                    Vec::new(),
                    &metrics,
                )
                .await;
        }
    }

    let body = String::from_utf8(metrics.render()).expect("utf8 metrics");
    let row_count = body
        .lines()
        .filter(|l| l.starts_with("portunus_rule_target_failovers_total{"))
        .count();
    assert_eq!(
        row_count, 2,
        "expected 2 rows (one per rule), got {row_count}\n--- body ---\n{body}"
    );

    // Final cumulative value = 5 per rule. Counter rolls forward via
    // the monotonic delta path — never via cumulative double-count.
    for (rid, owner) in rules {
        let pat = format!(
            "portunus_rule_target_failovers_total{{client=\"edge-b\",owner=\"{owner}\",rule=\"{}\"}} 5",
            rid.0
        );
        assert!(body.contains(&pat), "missing {pat}\n--- body ---\n{body}");
    }
}

/// Mixed fleet: single-target rules contribute zero failover series,
/// only the multi-target rules show up. Confirms the single-target
/// hot-path stays cardinality-free even when sharing the cache with
/// multi-target rules.
#[tokio::test]
async fn mixed_fleet_only_failover_rules_emit_series() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str("edge-c").expect("client");

    // 4 single-target rules + 1 multi-target rule (with failovers).
    for rid in [RuleId(30), RuleId(31), RuleId(32), RuleId(33)] {
        cache
            .observe(&client, rid, "alice", 100, 200, 1, 0, 0, 0, 0, 0, &metrics)
            .await;
    }
    cache
        .observe_with_targets(
            &client,
            RuleId(34),
            "bob",
            100,
            200,
            1,
            0,
            0,
            0,
            0,
            0,
            7,
            Vec::new(),
            &metrics,
        )
        .await;

    let body = String::from_utf8(metrics.render()).expect("utf8 metrics");
    let row_count = body
        .lines()
        .filter(|l| l.starts_with("portunus_rule_target_failovers_total{"))
        .count();
    assert_eq!(
        row_count, 1,
        "expected 1 row (only the multi-target rule), got {row_count}\n--- body ---\n{body}"
    );
    assert!(
        body.contains(
            "portunus_rule_target_failovers_total{client=\"edge-c\",owner=\"bob\",rule=\"34\"} 7"
        ),
        "missing multi-target row\n--- body ---\n{body}"
    );
}
