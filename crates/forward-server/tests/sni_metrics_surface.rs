//! 009-tls-sni-routing T074 — server-side `/metrics` surface test.
//!
//! Drives `RuleStatsCache::{observe, observe_sni_per_rule,
//! observe_sni_listener}` with synthesised counter values, then
//! scrapes `Metrics::render` (the same body served by
//! `GET /metrics` per `serve.rs::render_metrics`) and asserts the
//! expected `forward_tls_sni_*` lines are present with the right
//! labels and values.
//!
//! Companion to T070/T071 (per-rule + per-listener counter emission
//! at the client) and T080 (server-side fold-in). Closes the
//! observability loop end-to-end without spinning up gRPC.

use std::str::FromStr;

use forward_auth::UserId;
use forward_core::{ClientName, RuleId};
use forward_server::metrics::{Metrics, RuleStatsCache};

const CLIENT: &str = "edge-metrics-test";
const OWNER: &str = "u-7";

fn render_text(metrics: &Metrics) -> String {
    let body = metrics.render();
    String::from_utf8(body).expect("metrics body is UTF-8")
}

/// Convenience: assert that `body` contains a line that exactly
/// matches `prefix … {labels…} <value>` (Prometheus encodes a single
/// space between the labelled name and the numeric value).
#[track_caller]
fn assert_metric_line(body: &str, expected_substring: &str) {
    assert!(
        body.lines().any(|l| l.contains(expected_substring)),
        "expected metrics body to contain `{expected_substring}` — full body:\n{body}"
    );
}

#[track_caller]
fn assert_no_metric_line(body: &str, forbidden_substring: &str) {
    assert!(
        !body.lines().any(|l| l.contains(forbidden_substring)),
        "metrics body must NOT contain `{forbidden_substring}` — full body:\n{body}"
    );
}

#[tokio::test]
async fn per_rule_sni_counters_render_with_correct_labels() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str(CLIENT).expect("client name");
    let rule_id = RuleId(42);

    // observe() seeds the per-rule cache entry so observe_sni_per_rule
    // can find it. Counters at zero — the SNI fold-in will be the
    // first signal on the wire.
    cache
        .observe(
            &client, rule_id, OWNER, 0, // bytes_in
            0, // bytes_out
            0, // active_connections
            0, // dns_failures
            0, // datagrams_in
            0, // datagrams_out
            0, // active_flows
            0, // flows_dropped_overflow
            &metrics,
        )
        .await;

    // Drive 10 exact + 5 wildcard + 3 fallback hits.
    cache
        .observe_sni_per_rule(&client, rule_id, OWNER, 10, 5, 3, &metrics)
        .await;

    let body = render_text(&metrics);
    assert_metric_line(
        &body,
        "forward_tls_sni_route_total{client=\"edge-metrics-test\",owner=\"u-7\",result=\"exact\",rule=\"42\"} 10",
    );
    assert_metric_line(
        &body,
        "forward_tls_sni_route_total{client=\"edge-metrics-test\",owner=\"u-7\",result=\"wildcard\",rule=\"42\"} 5",
    );
    assert_metric_line(
        &body,
        "forward_tls_sni_route_total{client=\"edge-metrics-test\",owner=\"u-7\",result=\"fallback\",rule=\"42\"} 3",
    );
}

#[tokio::test]
async fn per_rule_sni_counters_apply_monotonic_delta() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str(CLIENT).expect("client");
    let rule_id = RuleId(7);

    cache
        .observe(&client, rule_id, OWNER, 0, 0, 0, 0, 0, 0, 0, 0, &metrics)
        .await;

    // First StatsReport: 4 exact hits.
    cache
        .observe_sni_per_rule(&client, rule_id, OWNER, 4, 0, 0, &metrics)
        .await;
    // Second StatsReport: cumulative 6 exact (delta = +2).
    cache
        .observe_sni_per_rule(&client, rule_id, OWNER, 6, 0, 0, &metrics)
        .await;

    let body = render_text(&metrics);
    assert_metric_line(
        &body,
        "forward_tls_sni_route_total{client=\"edge-metrics-test\",owner=\"u-7\",result=\"exact\",rule=\"7\"} 6",
    );
}

#[tokio::test]
async fn per_rule_sni_counters_handle_baseline_reset() {
    // Client process restart: cumulative counters reset to a smaller
    // value. The Prometheus collector must NOT decrement; the new
    // value rebaselines and subsequent deltas accumulate from there.
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str(CLIENT).expect("client");
    let rule_id = RuleId(11);

    cache
        .observe(&client, rule_id, OWNER, 0, 0, 0, 0, 0, 0, 0, 0, &metrics)
        .await;

    cache
        .observe_sni_per_rule(&client, rule_id, OWNER, 100, 0, 0, &metrics)
        .await;
    // Baseline reset → 5 < 100. The collector stays at 100; the cache
    // rebaselines to 5 so the next observe's delta computes from there.
    cache
        .observe_sni_per_rule(&client, rule_id, OWNER, 5, 0, 0, &metrics)
        .await;
    cache
        .observe_sni_per_rule(&client, rule_id, OWNER, 9, 0, 0, &metrics)
        .await;

    let body = render_text(&metrics);
    // 100 (initial) + (9 - 5) (post-rebaseline delta) = 104.
    assert_metric_line(
        &body,
        "forward_tls_sni_route_total{client=\"edge-metrics-test\",owner=\"u-7\",result=\"exact\",rule=\"11\"} 104",
    );
}

#[tokio::test]
async fn listener_counters_render_with_correct_labels() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str(CLIENT).expect("client");

    cache
        .observe_sni_listener(&client, 443, 4, 3, &[], 0, 0, &metrics)
        .await;

    let body = render_text(&metrics);
    assert_metric_line(
        &body,
        "forward_tls_sni_listener_miss_total{client=\"edge-metrics-test\",port=\"443\"} 4",
    );
    assert_metric_line(
        &body,
        "forward_tls_sni_listener_parse_failures_total{client=\"edge-metrics-test\",port=\"443\"} 3",
    );
}

#[tokio::test]
async fn listener_counters_keep_separate_state_per_port() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str(CLIENT).expect("client");

    cache
        .observe_sni_listener(&client, 443, 10, 0, &[], 0, 0, &metrics)
        .await;
    cache
        .observe_sni_listener(&client, 8443, 7, 0, &[], 0, 0, &metrics)
        .await;
    // Bumps to port 443 don't leak into port 8443's prev-state.
    cache
        .observe_sni_listener(&client, 443, 12, 0, &[], 0, 0, &metrics)
        .await;

    let body = render_text(&metrics);
    assert_metric_line(
        &body,
        "forward_tls_sni_listener_miss_total{client=\"edge-metrics-test\",port=\"443\"} 12",
    );
    assert_metric_line(
        &body,
        "forward_tls_sni_listener_miss_total{client=\"edge-metrics-test\",port=\"8443\"} 7",
    );
}

#[tokio::test]
async fn quiet_listener_does_not_emit_collector_lines() {
    // Per the implementation: bumps with delta == 0 don't touch the
    // collector. A first-seen listener with all-zero counters should
    // not produce any series rows in /metrics.
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str(CLIENT).expect("client");

    cache
        .observe_sni_listener(&client, 443, 0, 0, &[], 0, 0, &metrics)
        .await;

    let body = render_text(&metrics);
    assert_no_metric_line(
        &body,
        "forward_tls_sni_listener_miss_total{client=\"edge-metrics-test\",port=\"443\"}",
    );
    assert_no_metric_line(
        &body,
        "forward_tls_sni_listener_parse_failures_total{client=\"edge-metrics-test\",port=\"443\"}",
    );
}

#[tokio::test]
async fn metrics_help_lines_describe_sni_collectors() {
    // The Prometheus exposition format prefixes every metric with
    // `# HELP <name> <description>`. Smoke check that the four new
    // 009-tls-sni-routing collectors are registered (catch any future
    // accidental drop from `Metrics::new`).
    //
    // CounterVecs only emit HELP/TYPE once at least one labelset has
    // been instantiated; drive each one with a non-zero observation
    // first so the surface check exercises real registry output.
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str(CLIENT).expect("client");
    let rule_id = RuleId(1);

    cache
        .observe(&client, rule_id, OWNER, 0, 0, 0, 0, 0, 0, 0, 0, &metrics)
        .await;
    cache
        .observe_sni_per_rule(&client, rule_id, OWNER, 1, 1, 1, &metrics)
        .await;
    cache
        .observe_sni_listener(&client, 443, 1, 1, &[1, 1, 1], 3_000, 1, &metrics)
        .await;

    let body = render_text(&metrics);
    for name in [
        "forward_tls_sni_route_total",
        "forward_tls_sni_listener_miss_total",
        "forward_tls_sni_listener_parse_failures_total",
        "forward_tls_client_hello_peek_duration_seconds_bucket",
        "forward_tls_client_hello_peek_duration_seconds_sum",
        "forward_tls_client_hello_peek_duration_seconds_count",
        "forward_tls_sni_routes_active",
    ] {
        assert_metric_line(&body, &format!("# HELP {name} "));
        assert_metric_line(&body, &format!("# TYPE {name} "));
    }
}

#[tokio::test]
async fn listener_peek_histogram_renders_bucket_sum_and_count() {
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str(CLIENT).expect("client");
    let buckets = vec![1, 1, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 4];

    cache
        .observe_sni_listener(&client, 443, 0, 0, &buckets, 3_750, 4, &metrics)
        .await;

    let body = render_text(&metrics);
    assert_metric_line(
        &body,
        "forward_tls_client_hello_peek_duration_seconds_bucket{client=\"edge-metrics-test\",le=\"0.0001\",port=\"443\"} 1",
    );
    assert_metric_line(
        &body,
        "forward_tls_client_hello_peek_duration_seconds_bucket{client=\"edge-metrics-test\",le=\"3\",port=\"443\"} 4",
    );
    assert_metric_line(
        &body,
        "forward_tls_client_hello_peek_duration_seconds_bucket{client=\"edge-metrics-test\",le=\"+Inf\",port=\"443\"} 4",
    );
    assert_metric_line(
        &body,
        "forward_tls_client_hello_peek_duration_seconds_sum{client=\"edge-metrics-test\",port=\"443\"} 0.00375",
    );
    assert_metric_line(
        &body,
        "forward_tls_client_hello_peek_duration_seconds_count{client=\"edge-metrics-test\",port=\"443\"} 4",
    );
}

#[tokio::test]
async fn owner_label_uses_string_form_of_user_id() {
    // 005-multi-user-rbac T045: per-rule collectors carry `owner` —
    // a UserId. Smoke check that the value matches `UserId::to_string()`.
    let metrics = Metrics::new().expect("metrics");
    let cache = RuleStatsCache::new();
    let client = ClientName::from_str(CLIENT).expect("client");
    let rule_id = RuleId(99);
    let owner = UserId::from_str("alice").expect("user id");

    cache
        .observe(
            &client,
            rule_id,
            owner.to_string().as_str(),
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            &metrics,
        )
        .await;
    cache
        .observe_sni_per_rule(
            &client,
            rule_id,
            owner.to_string().as_str(),
            1,
            0,
            0,
            &metrics,
        )
        .await;

    let body = render_text(&metrics);
    assert_metric_line(
        &body,
        "forward_tls_sni_route_total{client=\"edge-metrics-test\",owner=\"alice\",result=\"exact\",rule=\"99\"} 1",
    );
}
