//! 007-multi-target-failover T031 — per-target stats serialisation
//! contract. Asserts that:
//!
//! 1. Single-target rules produce a snapshot whose `per_target` is
//!    empty (`#[serde(skip_serializing_if = "Vec::is_empty")]` keeps
//!    the field off the wire entirely — preserves byte-identical
//!    v0.6.0 JSON shape per Constitution Principle II).
//! 2. Single-target snapshots carry `target_failovers_total: 0` even
//!    after many observations (no series, no double-count).
//! 3. Multi-target snapshots round-trip the per-target body verbatim,
//!    including health flag, consecutive_failures, the last-failure /
//!    last-success unix-ms timestamps, and per-target byte/connection
//!    counters.
//! 4. Cumulative `target_failovers_total` advances monotonically and
//!    matches the most-recent observation.
//!
//! Drives `RuleStatsCache::observe_with_targets` directly — the same
//! seam the gRPC StatsReport handler funnels through, so JSON shape
//! here is JSON shape on the wire.

use std::str::FromStr;

use portunus_core::{ClientName, RuleId};
use portunus_server::metrics::{Metrics, PerTargetSnapshot, RuleStatsCache};

#[tokio::test]
async fn single_target_snapshot_omits_per_target_field() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str("edge-a").expect("client");

    cache
        .observe(
            &client,
            RuleId(10),
            "alice",
            500,
            600,
            2,
            0,
            0,
            0,
            0,
            0,
            &metrics,
        )
        .await;

    let snap = cache.get(RuleId(10)).await.expect("snapshot present");
    assert_eq!(snap.target_failovers_total, 0, "expected 0 failovers");
    assert!(
        snap.per_target.is_empty(),
        "single-target rules MUST have empty per_target, got {:?}",
        snap.per_target
    );

    let body = serde_json::to_string(&snap).expect("serialise");
    assert!(
        !body.contains("per_target"),
        "skip_serializing_if MUST strip the field — body: {body}"
    );
    assert!(
        body.contains("\"target_failovers_total\":0"),
        "target_failovers_total field MUST always be present (default 0): {body}"
    );
}

#[tokio::test]
async fn multi_target_snapshot_round_trips_per_target_body() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str("edge-b").expect("client");

    let per_target = vec![
        PerTargetSnapshot {
            index: 0,
            host: "primary.example.com".to_string(),
            port: 443,
            priority: 0,
            health: 1, // Failed
            consecutive_failures: 3,
            last_failure_at_unix_ms: 1_700_000_000_000,
            last_success_at_unix_ms: 1_699_999_990_000,
            bytes_in: 12_345,
            bytes_out: 23_456,
            connections_accepted: 7,
        },
        PerTargetSnapshot {
            index: 1,
            host: "secondary.example.com".to_string(),
            port: 443,
            priority: 1,
            health: 0, // Healthy
            consecutive_failures: 0,
            last_failure_at_unix_ms: 0,
            last_success_at_unix_ms: 1_700_000_005_000,
            bytes_in: 5_000,
            bytes_out: 6_000,
            connections_accepted: 4,
        },
    ];

    cache
        .observe_with_targets(
            &client,
            RuleId(20),
            "bob",
            17_345,
            29_456,
            1,
            0,
            0,
            0,
            0,
            0,
            2, // 2 failovers
            per_target.clone(),
            &metrics,
        )
        .await;

    let snap = cache.get(RuleId(20)).await.expect("snapshot present");
    assert_eq!(snap.target_failovers_total, 2);
    assert_eq!(snap.per_target.len(), 2);

    let primary = &snap.per_target[0];
    assert_eq!(primary.index, 0);
    assert_eq!(primary.host, "primary.example.com");
    assert_eq!(primary.health, 1);
    assert_eq!(primary.consecutive_failures, 3);
    assert_eq!(primary.bytes_in, 12_345);
    assert_eq!(primary.bytes_out, 23_456);
    assert_eq!(primary.connections_accepted, 7);

    let secondary = &snap.per_target[1];
    assert_eq!(secondary.index, 1);
    assert_eq!(secondary.host, "secondary.example.com");
    assert_eq!(secondary.health, 0);
    assert_eq!(secondary.consecutive_failures, 0);

    // Wire-shape spot check: the JSON serialisation MUST include
    // `per_target` for multi-target rules, with all the per-target
    // fields exposed under their canonical names.
    let body = serde_json::to_string(&snap).expect("serialise");
    assert!(
        body.contains("\"per_target\""),
        "per_target field missing from multi-target snapshot: {body}"
    );
    assert!(
        body.contains("\"target_failovers_total\":2"),
        "missing failover count: {body}"
    );
    for needle in [
        "\"primary.example.com\"",
        "\"secondary.example.com\"",
        "\"consecutive_failures\":3",
        "\"connections_accepted\":7",
        "\"last_failure_at_unix_ms\":1700000000000",
    ] {
        assert!(body.contains(needle), "missing {needle} in body: {body}");
    }
}

#[tokio::test]
async fn target_failovers_total_advances_monotonically() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str("edge-c").expect("client");

    for total in [1_u64, 4, 7, 7, 9] {
        cache
            .observe_with_targets(
                &client,
                RuleId(30),
                "alice",
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

    let snap = cache.get(RuleId(30)).await.expect("snapshot present");
    assert_eq!(
        snap.target_failovers_total, 9,
        "snapshot MUST mirror most-recent absolute value, not sum of deltas"
    );
}
