//! Server-side Prometheus collectors and a tiny stats cache for the
//! operator `rule-stats` view (T061). Data flows in from `StatsReport`
//! messages on the gRPC bidi stream (T060) and gets:
//!
//! 1. Stored in `RuleStatsCache` so `rule-stats <id>` returns the latest
//!    snapshot synchronously.
//! 2. Folded into Prometheus counters so `/metrics` shows monotonic totals.
//!
//! Counters are computed as `delta = new - prev` per rule per report.
//! `prev` lives in the cache so a missed report just delays the increment;
//! a smaller-than-prev value (client restart) resets the baseline rather
//! than emitting a negative delta.
//!
//! Range rules (002-port-range-forward) deliberately reuse the
//! `(client, rule)` labels — per-port detail is tracked separately in
//! `operator::per_port_stats` and surfaced via the loopback HTTP API
//! when an operator passes `--per-port`. See SC-002 in
//! `specs/002-port-range-forward/contracts/operator-api.md`: the
//! Prometheus cardinality budget MUST stay invariant of range size,
//! otherwise a 1024-port range would burst the registry.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use forward_core::{ClientName, RuleId};
use prometheus::{
    CounterVec, Encoder, GaugeVec, IntCounter, IntCounterVec, IntGauge, Registry, TextEncoder, opts,
};
use serde::Serialize;
use tokio::sync::{RwLock, broadcast};

/// One client's report for one rule, plus the server-side wall-clock time
/// we last received it. Operators consume this via `rule-stats`.
#[derive(Debug, Clone, Serialize)]
pub struct RuleStatsSnapshot {
    pub rule_id: RuleId,
    pub client_name: ClientName,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
    /// 003-domain-name-forward T050: per-rule cumulative DNS-failure
    /// count, surfaced via `GET /v1/rules/{id}/stats` and
    /// `rule-stats <id>`. Always present (0 for IP-target rules) per
    /// `contracts/operator-api.md`.
    pub dns_failures: u64,
    /// 004-udp-forward T038: UDP-specific cumulative counters. All zero
    /// for TCP rules. Surfaced via `rule-stats <id>` JSON and the
    /// rendered `/metrics` collectors registered in T037.
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub active_flows: u32,
    pub flows_dropped_overflow: u64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Metrics {
    pub registry: Registry,
    pub clients_connected: IntGauge,
    pub auth_failures_total: IntCounterVec,
    pub rule_bytes_in_total: CounterVec,
    pub rule_bytes_out_total: CounterVec,
    pub rule_active_connections: GaugeVec,
    /// 003-domain-name-forward T049: per-rule monotonic DNS-failure
    /// counter, labelled `(client, rule, owner)`. Cardinality budget:
    /// strictly one row per rule, never per address / per attempt /
    /// per failure-mode reason (R-008 / SC-006). 005-multi-user-rbac
    /// T045 added the `owner` label so operators can slice failures by
    /// the user who owns the rule without re-grepping audit logs.
    pub rule_dns_failures_total: IntCounterVec,
    /// 004-udp-forward T037: per-rule live UDP flow gauge. One row per
    /// rule (NOT per port for range rules). Always 0 for TCP rules.
    pub rule_active_flows: GaugeVec,
    /// 004-udp-forward T037: per-rule cumulative UDP datagrams the
    /// listener received. One row per rule.
    pub rule_udp_datagrams_in_total: IntCounterVec,
    /// 004-udp-forward T037: per-rule cumulative UDP datagrams sent
    /// back to end-users. One row per rule.
    pub rule_udp_datagrams_out_total: IntCounterVec,
    /// 004-udp-forward T037: per-rule cumulative count of new-flow
    /// first-datagrams dropped because the per-rule UdpFlowTable was
    /// at `udp_max_flows_per_rule`. Always 0 for TCP rules.
    pub rule_flows_dropped_overflow_total: IntCounterVec,
    /// 005-multi-user-rbac T045: every operator HTTP request lands here
    /// once, labelled `{outcome=allow|deny, reason}`. `reason` is the
    /// `RbacError::code()` string on deny, or `"ok"` on allow. Bounded
    /// label set (≤ enum size + 1) keeps cardinality predictable
    /// regardless of traffic shape (R-009).
    pub operator_requests_total: IntCounterVec,
    /// 006-management-web-ui T009: cumulative count of audit-ring
    /// evictions. Bumped by `AuditRing::push` when the ring is at
    /// capacity and the oldest entry is dropped to make room.
    pub audit_buffer_drops_total: IntCounter,
}

impl Metrics {
    /// # Errors
    ///
    /// Returns the underlying `prometheus::Error` if collector registration
    /// fails — only happens for duplicate metric names, which would be a bug.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();
        let clients_connected =
            IntGauge::new("forward_clients_connected", "Currently-connected clients")?;
        let auth_failures_total = IntCounterVec::new(
            opts!("forward_auth_failures_total", "Auth failures by reason"),
            &["reason"],
        )?;
        let rule_bytes_in_total = CounterVec::new(
            opts!(
                "forward_rule_bytes_in_total",
                "Cumulative bytes ingressing each rule"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_bytes_out_total = CounterVec::new(
            opts!(
                "forward_rule_bytes_out_total",
                "Cumulative bytes egressing each rule"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_active_connections = GaugeVec::new(
            opts!(
                "forward_rule_active_connections",
                "Active forwarded connections per rule"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_dns_failures_total = IntCounterVec::new(
            opts!(
                "forward_rule_dns_failures_total",
                "Per-rule monotonic count of end-user connections refused due to DNS resolution failure (NXDOMAIN, SERVFAIL, timeout, full multi-A exhaustion)"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_active_flows = GaugeVec::new(
            opts!(
                "forward_rule_active_flows",
                "Live UDP flows per rule (one row per rule, even for range rules; always 0 for TCP rules)"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_udp_datagrams_in_total = IntCounterVec::new(
            opts!(
                "forward_rule_udp_datagrams_in_total",
                "Per-rule monotonic count of UDP datagrams received from end-users (always 0 for TCP rules)"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_udp_datagrams_out_total = IntCounterVec::new(
            opts!(
                "forward_rule_udp_datagrams_out_total",
                "Per-rule monotonic count of UDP datagrams sent back to end-users (always 0 for TCP rules)"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_flows_dropped_overflow_total = IntCounterVec::new(
            opts!(
                "forward_rule_flows_dropped_overflow_total",
                "Per-rule monotonic count of UDP first-datagrams dropped because the per-rule flow table hit `udp_max_flows_per_rule`"
            ),
            &["client", "rule", "owner"],
        )?;
        let operator_requests_total = IntCounterVec::new(
            opts!(
                "forward_operator_requests_total",
                "Operator HTTP requests by outcome (allow|deny) and reason (`ok` on allow, RbacError code on deny)"
            ),
            &["outcome", "reason"],
        )?;
        let audit_buffer_drops_total = IntCounter::new(
            "forward_audit_buffer_drops_total",
            "Cumulative count of audit-ring entries evicted because the buffer was at capacity (006-management-web-ui T009)",
        )?;
        registry.register(Box::new(clients_connected.clone()))?;
        registry.register(Box::new(auth_failures_total.clone()))?;
        registry.register(Box::new(rule_bytes_in_total.clone()))?;
        registry.register(Box::new(rule_bytes_out_total.clone()))?;
        registry.register(Box::new(rule_active_connections.clone()))?;
        registry.register(Box::new(rule_dns_failures_total.clone()))?;
        registry.register(Box::new(rule_active_flows.clone()))?;
        registry.register(Box::new(rule_udp_datagrams_in_total.clone()))?;
        registry.register(Box::new(rule_udp_datagrams_out_total.clone()))?;
        registry.register(Box::new(rule_flows_dropped_overflow_total.clone()))?;
        registry.register(Box::new(operator_requests_total.clone()))?;
        registry.register(Box::new(audit_buffer_drops_total.clone()))?;

        Ok(Self {
            registry,
            clients_connected,
            auth_failures_total,
            rule_bytes_in_total,
            rule_bytes_out_total,
            rule_active_connections,
            rule_dns_failures_total,
            rule_active_flows,
            rule_udp_datagrams_in_total,
            rule_udp_datagrams_out_total,
            rule_flows_dropped_overflow_total,
            operator_requests_total,
            audit_buffer_drops_total,
        })
    }

    /// Encode the registry into Prometheus text format for `/metrics`.
    #[must_use]
    pub fn render(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4096);
        let encoder = TextEncoder::new();
        let metrics = self.registry.gather();
        let _ = encoder.encode(&metrics, &mut out);
        out
    }
}

/// 006-management-web-ui T011: per-rule broadcast capacity. A new
/// subscriber gets the latest snapshot via `subscribe(...)`'s
/// initial-replay handling on the receiver side; runtime updates fan
/// out through this `tokio::sync::broadcast` channel. Capacity 16 is
/// generous for a 5-second cadence — slow consumers receive `Lagged`
/// errors and are logged, never blocking fast subscribers (R-008).
const STATS_BROADCAST_CAPACITY: usize = 16;

/// Cache the latest `StatsReport` per rule. Cheap to clone (`Arc` internal).
///
/// 006-management-web-ui T011: also fans out new snapshots over
/// `tokio::sync::broadcast` so the SSE endpoint can serve N concurrent
/// subscribers at O(rules) cost.
#[derive(Debug, Clone, Default)]
pub struct RuleStatsCache {
    inner: Arc<RwLock<HashMap<RuleId, CachedEntry>>>,
    /// Per-rule broadcast senders; lazy-initialized on first
    /// `subscribe(rule_id)`. Removed when `drop_rule` runs so a removed
    /// rule's broadcast resources don't accumulate.
    broadcasts: Arc<RwLock<HashMap<RuleId, broadcast::Sender<RuleStatsSnapshot>>>>,
}

#[derive(Debug, Clone)]
struct CachedEntry {
    snapshot: RuleStatsSnapshot,
    /// Last cumulative values seen; used to compute monotonic deltas for
    /// Prometheus counters in [`RuleStatsCache::observe`].
    prev_bytes_in: u64,
    prev_bytes_out: u64,
    /// 003-domain-name-forward T050: previous DNS-failure count for
    /// monotonic delta computation. Same baseline-reset rule as
    /// `prev_bytes_*`.
    prev_dns_failures: u64,
    /// 004-udp-forward T038: previous UDP cumulative readings used to
    /// compute monotonic deltas for the new collectors. Baseline-reset
    /// (new < prev) is treated as a fresh window — counters never
    /// decrement.
    prev_datagrams_in: u64,
    prev_datagrams_out: u64,
    prev_flows_dropped_overflow: u64,
}

impl RuleStatsCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one client-reported reading. Updates the cache and feeds deltas
    /// into the Prometheus collectors. A baseline reset (new < prev) is
    /// treated as a fresh window — counters are NOT decremented.
    #[allow(clippy::too_many_arguments)]
    pub async fn observe(
        &self,
        client_name: &ClientName,
        rule_id: RuleId,
        // 005-multi-user-rbac T045: owner user id, threaded as a third
        // Prometheus label on every per-rule collector. Passed in from
        // the gRPC StatsReport handler via `state.rules.get(rule_id)`;
        // stays bounded at one row per rule because owners are
        // immutable for the rule's lifetime (re-pushing transfers
        // ownership only via a brand-new rule id).
        owner: &str,
        bytes_in: u64,
        bytes_out: u64,
        active_connections: u32,
        dns_failures: u64,
        // 004-udp-forward T038: UDP-specific cumulative values from
        // the StatsReport. TCP rules pass zeros; the deltas land at 0
        // and the corresponding collectors stay quiet.
        datagrams_in: u64,
        datagrams_out: u64,
        active_flows: u32,
        flows_dropped_overflow: u64,
        metrics: &Metrics,
    ) {
        let mut guard = self.inner.write().await;
        let entry = guard.entry(rule_id).or_insert_with(|| CachedEntry {
            snapshot: RuleStatsSnapshot {
                rule_id,
                client_name: client_name.clone(),
                bytes_in: 0,
                bytes_out: 0,
                active_connections: 0,
                dns_failures: 0,
                datagrams_in: 0,
                datagrams_out: 0,
                active_flows: 0,
                flows_dropped_overflow: 0,
                updated_at: Utc::now(),
            },
            prev_bytes_in: 0,
            prev_bytes_out: 0,
            prev_dns_failures: 0,
            prev_datagrams_in: 0,
            prev_datagrams_out: 0,
            prev_flows_dropped_overflow: 0,
        });

        let rule_id_str = rule_id.0.to_string();
        let labels = [client_name.as_str(), rule_id_str.as_str(), owner];
        let in_delta = bytes_in.saturating_sub(entry.prev_bytes_in);
        let out_delta = bytes_out.saturating_sub(entry.prev_bytes_out);
        let dns_delta = dns_failures.saturating_sub(entry.prev_dns_failures);
        let dgin_delta = datagrams_in.saturating_sub(entry.prev_datagrams_in);
        let dgout_delta = datagrams_out.saturating_sub(entry.prev_datagrams_out);
        let drop_delta = flows_dropped_overflow.saturating_sub(entry.prev_flows_dropped_overflow);
        if in_delta > 0 {
            metrics
                .rule_bytes_in_total
                .with_label_values(&labels)
                .inc_by(precise_f64(in_delta));
        }
        if out_delta > 0 {
            metrics
                .rule_bytes_out_total
                .with_label_values(&labels)
                .inc_by(precise_f64(out_delta));
        }
        if dns_delta > 0 {
            metrics
                .rule_dns_failures_total
                .with_label_values(&labels)
                .inc_by(dns_delta);
        }
        if dgin_delta > 0 {
            metrics
                .rule_udp_datagrams_in_total
                .with_label_values(&labels)
                .inc_by(dgin_delta);
        }
        if dgout_delta > 0 {
            metrics
                .rule_udp_datagrams_out_total
                .with_label_values(&labels)
                .inc_by(dgout_delta);
        }
        if drop_delta > 0 {
            metrics
                .rule_flows_dropped_overflow_total
                .with_label_values(&labels)
                .inc_by(drop_delta);
        }
        metrics
            .rule_active_connections
            .with_label_values(&labels)
            .set(f64::from(active_connections));
        metrics
            .rule_active_flows
            .with_label_values(&labels)
            .set(f64::from(active_flows));

        entry.prev_bytes_in = bytes_in;
        entry.prev_bytes_out = bytes_out;
        entry.prev_dns_failures = dns_failures;
        entry.prev_datagrams_in = datagrams_in;
        entry.prev_datagrams_out = datagrams_out;
        entry.prev_flows_dropped_overflow = flows_dropped_overflow;
        entry.snapshot.bytes_in = bytes_in;
        entry.snapshot.bytes_out = bytes_out;
        entry.snapshot.active_connections = active_connections;
        entry.snapshot.dns_failures = dns_failures;
        entry.snapshot.datagrams_in = datagrams_in;
        entry.snapshot.datagrams_out = datagrams_out;
        entry.snapshot.active_flows = active_flows;
        entry.snapshot.flows_dropped_overflow = flows_dropped_overflow;
        entry.snapshot.updated_at = Utc::now();
        entry.snapshot.client_name = client_name.clone();
        let snap_clone = entry.snapshot.clone();
        drop(guard);
        // 006-management-web-ui T011: fan out the snapshot to any SSE
        // subscribers. Non-blocking: a slow consumer receives
        // `RecvError::Lagged` on its receiver side; the send itself
        // never awaits.
        let bcast_guard = self.broadcasts.read().await;
        if let Some(tx) = bcast_guard.get(&rule_id) {
            // `send` returns Err only when there are zero receivers;
            // ignoring is correct — no subscribers means no fan-out.
            let _ = tx.send(snap_clone);
        }
    }

    /// 006-management-web-ui T011: subscribe to live snapshots for a
    /// single rule. Lazily creates the broadcast sender on first call;
    /// returns a fresh receiver for each subscriber. The caller is
    /// responsible for the initial replay (the SSE handler reads
    /// `get(rule_id)` once after subscribing to seed the stream).
    pub async fn subscribe(&self, rule_id: RuleId) -> broadcast::Receiver<RuleStatsSnapshot> {
        let mut guard = self.broadcasts.write().await;
        let tx = guard
            .entry(rule_id)
            .or_insert_with(|| broadcast::channel(STATS_BROADCAST_CAPACITY).0);
        tx.subscribe()
    }

    /// 006-management-web-ui T011: drop a rule's broadcast sender. When
    /// the sender is dropped, every active receiver gets
    /// `RecvError::Closed` and the SSE handler terminates the stream
    /// naturally. Called from `drop_rule`.
    pub async fn drop_rule_broadcasts(&self, rule_id: RuleId) {
        let mut guard = self.broadcasts.write().await;
        guard.remove(&rule_id);
    }

    pub async fn get(&self, rule_id: RuleId) -> Option<RuleStatsSnapshot> {
        self.inner
            .read()
            .await
            .get(&rule_id)
            .map(|e| e.snapshot.clone())
    }

    pub async fn drop_rule(
        &self,
        rule_id: RuleId,
        client_name: &ClientName,
        // 005-multi-user-rbac T045: third label on per-rule collectors.
        // Caller passes the rule's `owner_user_id` so cleanup matches
        // the exact triple `observe()` recorded.
        owner: &str,
        metrics: &Metrics,
    ) {
        // 006-management-web-ui T011: drop the broadcast sender first
        // so any in-flight subscriber sees end-of-stream before the
        // cache slot disappears.
        self.drop_rule_broadcasts(rule_id).await;
        let mut guard = self.inner.write().await;
        if guard.remove(&rule_id).is_some() {
            // Strip the rule's labels from the gauges AND the
            // dns_failures counter (003-domain-name-forward T049 —
            // SC-006 cardinality budget: 1 row per live rule, no
            // accumulation of removed-rule rows). Byte counters are
            // kept per Prometheus convention; SC-002 already accepts
            // their unbounded retention. 004-udp-forward T038 extends
            // the cleanup to the four UDP-specific collectors so the
            // SC-004 cardinality budget holds for UDP rules as well.
            let rule_id_str = rule_id.0.to_string();
            let labels = [client_name.as_str(), rule_id_str.as_str(), owner];
            let _ = metrics.rule_active_connections.remove_label_values(&labels);
            let _ = metrics.rule_dns_failures_total.remove_label_values(&labels);
            let _ = metrics.rule_active_flows.remove_label_values(&labels);
            let _ = metrics
                .rule_udp_datagrams_in_total
                .remove_label_values(&labels);
            let _ = metrics
                .rule_udp_datagrams_out_total
                .remove_label_values(&labels);
            let _ = metrics
                .rule_flows_dropped_overflow_total
                .remove_label_values(&labels);
        }
    }
}

/// Convert a `u64` into the closest representable `f64`. The Prometheus
/// counter API takes f64; for byte counters at the scale we ship (well below
/// 2^53) the conversion is exact.
fn precise_f64(n: u64) -> f64 {
    // u64 → f64 is `clippy::cast_precision_loss`. Bytes per rule per
    // 5-second window won't approach 2^53; the conversion is exact in
    // practice.
    #[allow(clippy::cast_precision_loss)]
    let v = n as f64;
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn name(s: &str) -> ClientName {
        ClientName::from_str(s).unwrap()
    }

    #[tokio::test]
    async fn observe_then_get_roundtrip() {
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();
        cache
            .observe(
                &name("edge-a"),
                RuleId(7),
                "alice",
                1000,
                2000,
                3,
                0,
                0,
                0,
                0,
                0,
                &metrics,
            )
            .await;
        let snap = cache.get(RuleId(7)).await.unwrap();
        assert_eq!(snap.bytes_in, 1000);
        assert_eq!(snap.bytes_out, 2000);
        assert_eq!(snap.active_connections, 3);
        assert_eq!(snap.dns_failures, 0);
        assert_eq!(snap.client_name, name("edge-a"));

        // Second observation: counters take the delta.
        cache
            .observe(
                &name("edge-a"),
                RuleId(7),
                "alice",
                1500,
                2100,
                2,
                0,
                0,
                0,
                0,
                0,
                &metrics,
            )
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(
            body.contains(
                "forward_rule_bytes_in_total{client=\"edge-a\",owner=\"alice\",rule=\"7\"} 1500"
            ),
            "rendered metrics: {body}"
        );
        assert!(body.contains(
            "forward_rule_bytes_out_total{client=\"edge-a\",owner=\"alice\",rule=\"7\"} 2100"
        ));
        assert!(body.contains(
            "forward_rule_active_connections{client=\"edge-a\",owner=\"alice\",rule=\"7\"} 2"
        ));
    }

    #[tokio::test]
    async fn baseline_reset_does_not_decrement_counter() {
        // If the client restarts, its in-process counters reset to 0. The
        // Prometheus counter MUST NOT go backwards; we rebaseline silently.
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();
        cache
            .observe(
                &name("edge-a"),
                RuleId(1),
                "alice",
                5_000,
                5_000,
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
            .observe(
                &name("edge-a"),
                RuleId(1),
                "alice",
                100,
                100,
                0,
                0,
                0,
                0,
                0,
                0,
                &metrics,
            )
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        // Total stayed at 5000 (no negative delta); next observation will
        // accumulate from this new baseline.
        assert!(
            body.contains(
                "forward_rule_bytes_in_total{client=\"edge-a\",owner=\"alice\",rule=\"1\"} 5000"
            ),
            "rendered: {body}"
        );
        cache
            .observe(
                &name("edge-a"),
                RuleId(1),
                "alice",
                300,
                300,
                0,
                0,
                0,
                0,
                0,
                0,
                &metrics,
            )
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(
            body.contains(
                "forward_rule_bytes_in_total{client=\"edge-a\",owner=\"alice\",rule=\"1\"} 5200"
            ),
            "rendered: {body}"
        );
    }

    /// T044 (US4): per-rule cardinality budget — exactly one
    /// `forward_rule_dns_failures_total` row per `(client, rule)`
    /// pair, regardless of how many failures fold into it (SC-006 /
    /// R-008). This protects against accidental refactors that would
    /// add per-address or per-failure-reason labels and explode
    /// cardinality on a fleet of 10k rules.
    #[tokio::test]
    async fn dns_failures_cardinality_is_one_row_per_rule() {
        const N: u64 = 5;
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();

        // Simulate N rules, each emitting K=7 DNS failures across two
        // StatsReports (delta accumulates). Same `(client, rule)`
        // tuple MUST yield one row.
        for i in 0..N {
            cache
                .observe(
                    &name("edge-a"),
                    RuleId(i),
                    "alice",
                    0,
                    0,
                    0,
                    3,
                    0,
                    0,
                    0,
                    0,
                    &metrics,
                )
                .await;
            cache
                .observe(
                    &name("edge-a"),
                    RuleId(i),
                    "alice",
                    0,
                    0,
                    0,
                    7,
                    0,
                    0,
                    0,
                    0,
                    &metrics,
                )
                .await;
        }

        let body = String::from_utf8(metrics.render()).unwrap();
        let row_count = body
            .lines()
            .filter(|l| l.starts_with("forward_rule_dns_failures_total{"))
            .count();
        assert_eq!(
            row_count as u64, N,
            "expected exactly N={N} rows, got {row_count}\n--- body ---\n{body}"
        );
        for i in 0..N {
            let pat = format!(
                "forward_rule_dns_failures_total{{client=\"edge-a\",owner=\"alice\",rule=\"{i}\"}} 7"
            );
            assert!(body.contains(&pat), "missing {pat}\n--- body ---\n{body}");
        }
    }

    /// T044 part 2: dropping a rule removes its dns_failures row so a
    /// long-running server doesn't slowly accumulate stale rule rows.
    #[tokio::test]
    async fn drop_rule_removes_dns_failures_row() {
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();
        cache
            .observe(
                &name("edge-a"),
                RuleId(42),
                "alice",
                0,
                0,
                0,
                5,
                0,
                0,
                0,
                0,
                &metrics,
            )
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(body.contains(
            "forward_rule_dns_failures_total{client=\"edge-a\",owner=\"alice\",rule=\"42\"} 5"
        ));

        cache
            .drop_rule(RuleId(42), &name("edge-a"), "alice", &metrics)
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(
            !body.contains("rule=\"42\""),
            "dropped rule row MUST disappear from /metrics: {body}"
        );
    }

    // ---- 004-udp-forward T038 ----

    /// SC-004: per-rule UDP collectors emit exactly one row per rule
    /// regardless of how many flows / datagrams pass through. Same
    /// budget the v0.3 dns_failures collector enforces.
    #[tokio::test]
    async fn active_flows_cardinality_is_one_row_per_rule() {
        const N: u64 = 5;
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();

        // Drive each rule through three increasing observations to
        // simulate ramping flow counts. Cardinality MUST stay at N.
        for i in 0..N {
            for active_flows in [0_u32, 7, 13] {
                cache
                    .observe(
                        &name("edge-a"),
                        RuleId(i),
                        "alice",
                        100,
                        200,
                        0,
                        0,
                        50,
                        45,
                        active_flows,
                        2,
                        &metrics,
                    )
                    .await;
            }
        }

        let body = String::from_utf8(metrics.render()).unwrap();
        for collector in [
            "forward_rule_active_flows{",
            "forward_rule_udp_datagrams_in_total{",
            "forward_rule_udp_datagrams_out_total{",
            "forward_rule_flows_dropped_overflow_total{",
        ] {
            let row_count = body.lines().filter(|l| l.starts_with(collector)).count();
            assert_eq!(
                row_count as u64, N,
                "expected N={N} rows for {collector}, got {row_count}\n--- body ---\n{body}"
            );
        }
    }

    /// drop_rule removes UDP collector rows alongside the v0.3
    /// dns_failures cleanup.
    #[tokio::test]
    async fn drop_rule_removes_udp_rows() {
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();
        cache
            .observe(
                &name("edge-a"),
                RuleId(99),
                "alice",
                10,
                20,
                0,
                0,
                100,
                90,
                7,
                3,
                &metrics,
            )
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(body.contains(
            "forward_rule_active_flows{client=\"edge-a\",owner=\"alice\",rule=\"99\"} 7"
        ));
        assert!(body.contains(
            "forward_rule_udp_datagrams_in_total{client=\"edge-a\",owner=\"alice\",rule=\"99\"} 100"
        ));

        cache
            .drop_rule(RuleId(99), &name("edge-a"), "alice", &metrics)
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        // Byte counters are kept per Prometheus convention (SC-002 budget
        // accepts unbounded retention there). UDP-specific gauges and
        // counters MUST be cleared.
        for collector in [
            "forward_rule_active_flows{",
            "forward_rule_udp_datagrams_in_total{",
            "forward_rule_udp_datagrams_out_total{",
            "forward_rule_flows_dropped_overflow_total{",
            "forward_rule_active_connections{",
            "forward_rule_dns_failures_total{",
        ] {
            assert!(
                !body.lines().any(|l| l.starts_with(collector)),
                "dropped rule row MUST disappear from {collector}: {body}"
            );
        }
    }
}
