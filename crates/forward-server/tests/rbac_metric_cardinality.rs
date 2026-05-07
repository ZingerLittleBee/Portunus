//! T042 (005-multi-user-rbac, US3) — `forward_rule_*` per-rule
//! Prometheus collectors must carry the `owner` label and stay at one
//! row per `(client, rule, owner)` triple even when many users own
//! many rules. R-009 cardinality budget: ≤ N_rules rows total per
//! collector, regardless of how many StatsReport observations land.
//!
//! Drives `RuleStatsCache::observe` directly because spinning up the
//! gRPC stream from inside an in-process unit test is more wiring than
//! the test needs — the cache is the single seam the gRPC handler funnels
//! through, so cardinality there is cardinality on the wire.

use std::str::FromStr;

use forward_core::{ClientName, RuleId};
use forward_server::metrics::{Metrics, RuleStatsCache};

#[tokio::test]
async fn per_rule_collectors_are_owner_labelled_and_one_row_per_triple() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str("edge-a").expect("client");

    // 5 rules across 3 owners, each observed twice (delta path) plus
    // a third observation that ramps active_connections to verify the
    // gauge label set matches the counter label set.
    let rules = [
        (RuleId(10), "alice"),
        (RuleId(11), "alice"),
        (RuleId(12), "bob"),
        (RuleId(13), "bob"),
        (RuleId(14), "carol"),
    ];
    for (rid, owner) in rules {
        for (b_in, b_out, conns) in [(100_u64, 200, 1_u32), (300, 400, 2), (500, 600, 3)] {
            cache
                .observe(
                    &client, rid, owner, b_in, b_out, conns, 0, 0, 0, 0, 0, &metrics,
                )
                .await;
        }
    }

    let body = String::from_utf8(metrics.render()).expect("utf8 metrics");

    // For every per-rule collector that we DO write to in this test,
    // assert exactly one row per (client, rule, owner) triple — i.e.
    // 5 rows for a 5-rule fixture, regardless of how many observations
    // landed. (DNS / UDP collectors stay at zero deltas and never emit
    // a row, by design — the SC-006/SC-004 quiet-collector budget.)
    for collector in [
        "forward_rule_bytes_in_total{",
        "forward_rule_bytes_out_total{",
        "forward_rule_active_connections{",
    ] {
        let row_count = body.lines().filter(|l| l.starts_with(collector)).count();
        assert_eq!(
            row_count, 5,
            "{collector}: expected 5 rows (one per rule), got {row_count}\n--- body ---\n{body}"
        );
    }

    // Spot-check label ordering and presence of all three owners.
    for owner in ["alice", "bob", "carol"] {
        assert!(
            body.contains(&format!("owner=\"{owner}\"")),
            "owner=\"{owner}\" missing\n--- body ---\n{body}"
        );
    }

    // Counter values must roll forward across observations. Final
    // bytes_in for each rule is 500 (last absolute value, NOT
    // 100+300+500 — `observe` records cumulative readings).
    for (rid, owner) in rules {
        let pat = format!(
            "forward_rule_bytes_in_total{{client=\"edge-a\",owner=\"{owner}\",rule=\"{}\"}} 500",
            rid.0
        );
        assert!(body.contains(&pat), "missing {pat}\n--- body ---\n{body}");
    }
}

/// T045 second half: `forward_operator_requests_total{outcome,reason}`
/// is registered with bounded label set (≤ 2 outcomes × small reason
/// enum). The collector renders zero rows until something is observed,
/// confirming registration succeeded without exploding cardinality.
#[tokio::test]
async fn operator_requests_counter_registered_and_renderable() {
    let metrics = Metrics::new().expect("metrics");
    metrics
        .operator_requests_total
        .with_label_values(&["allow", "ok"])
        .inc();
    metrics
        .operator_requests_total
        .with_label_values(&["deny", "client_not_granted"])
        .inc_by(2);
    let body = String::from_utf8(metrics.render()).expect("utf8");
    assert!(
        body.contains("forward_operator_requests_total{outcome=\"allow\",reason=\"ok\"} 1"),
        "missing allow row\n--- body ---\n{body}"
    );
    assert!(
        body.contains(
            "forward_operator_requests_total{outcome=\"deny\",reason=\"client_not_granted\"} 2"
        ),
        "missing deny row\n--- body ---\n{body}"
    );
}
