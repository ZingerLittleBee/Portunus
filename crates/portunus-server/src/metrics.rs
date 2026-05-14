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
use portunus_core::{ClientName, RuleId, peek_histogram::PEEK_HISTOGRAM_BUCKETS_SECS};
use prometheus::{
    CounterVec, Encoder, GaugeVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Registry,
    TextEncoder, opts,
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
    /// 007-multi-target-failover T033: cumulative count of
    /// Healthy↔Failed transitions on multi-target rules. Always 0 for
    /// single-target rules (invariant I-3).
    #[serde(default)]
    pub target_failovers_total: u64,
    /// 007-multi-target-failover T033: per-target health + byte
    /// counter snapshots. Always empty for single-target rules (I-3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub per_target: Vec<PerTargetSnapshot>,
    pub updated_at: DateTime<Utc>,
}

/// 007-multi-target-failover T033: per-target snapshot. Shape mirrors
/// the proto `PerTargetStats` so wire ↔ JSON conversion is mechanical.
#[derive(Debug, Clone, Serialize)]
pub struct PerTargetSnapshot {
    pub index: u32,
    pub host: String,
    pub port: u32,
    pub priority: u32,
    /// 0 = Healthy, 1 = Failed (mirrors proto wire encoding).
    pub health: u32,
    pub consecutive_failures: u32,
    pub last_failure_at_unix_ms: u64,
    pub last_success_at_unix_ms: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub connections_accepted: u64,
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
    /// 007-multi-target-failover T035: per-rule cumulative count of
    /// target Healthy↔Failed transitions. EXACTLY one row per
    /// multi-target rule (single-target rules never emit because
    /// their delta stays 0 — observe() skips zero-delta writes —
    /// preserving the SC-006 cardinality budget).
    pub rule_target_failovers_total: IntCounterVec,
    /// 006-management-web-ui T009: cumulative count of audit-ring
    /// evictions. Bumped by `AuditRing::push` when the ring is at
    /// capacity and the oldest entry is dropped to make room.
    ///
    /// 008-sqlite-storage T031 reuses the same series for hand-off
    /// queue overflow on the durable audit writer (semantically
    /// identical: "we lost an audit entry due to backpressure"); see
    /// `contracts/operator-api.md` §Prometheus.
    pub audit_buffer_drops_total: IntCounter,
    /// 008-sqlite-storage T031: oldest hand-off-queue entry's age in
    /// seconds. 0 when the queue is empty / writer is idle. Useful
    /// for diagnosing burst saturation BEFORE drops happen.
    pub audit_durable_writer_lag_seconds: prometheus::Gauge,
    /// 008-sqlite-storage T031: cumulative count of `SQLITE_BUSY`
    /// occurrences mapped to `StoreError::Transient`. Should stay
    /// near zero in healthy deployments thanks to BEGIN IMMEDIATE.
    pub store_busy_total: IntCounter,
    /// 009-tls-sni-routing T079: per-rule SNI dispatch hits, labelled
    /// `(client, rule, owner, result)` where `result` is one of
    /// `exact`, `wildcard`, `fallback`. Cardinality budget per design
    /// observability section: 3 rows per SNI rule (one per result
    /// kind). Legacy plain-TCP rules NEVER emit a row because the
    /// listener's per-rule counters stay at 0.
    pub tls_sni_route_total: IntCounterVec,
    /// 009-tls-sni-routing T079: per-listener cumulative count of
    /// connections whose SNI didn't match any rule on the port AND
    /// no fallback was configured. Labelled `(client, port)`. One
    /// row per SNI listener.
    pub tls_sni_listener_miss_total: IntCounterVec,
    /// 009-tls-sni-routing T079: per-listener cumulative count of
    /// peeked bytes that failed to parse as a ClientHello (or
    /// timed out / hit size cap). Labelled `(client, port)`.
    pub tls_sni_listener_parse_failures_total: IntCounterVec,
    pub tls_client_hello_peek_duration_seconds_bucket: IntCounterVec,
    pub tls_client_hello_peek_duration_seconds_sum: CounterVec,
    pub tls_client_hello_peek_duration_seconds_count: IntCounterVec,
    /// 009-tls-sni-routing T081: gauge of active rules whose
    /// `sni_pattern.is_some()`. Refreshed each metrics tick from
    /// `ServerRuleStore`. Single global series — no labels — so
    /// `irate()` over time tells operators when SNI rule density
    /// changes meaningfully.
    pub tls_sni_routes_active: IntGauge,
    /// 011-rate-limiting-qos T023: per-rule cumulative count of
    /// connections (or UDP first-packets) rejected by a rate-limit
    /// cap. Labelled `(client, rule, owner, reason)`. `reason` is one
    /// of `conn_concurrent`, `conn_rate`, `udp_flow_rate`,
    /// `owner_concurrent`, `owner_conn_rate`, `owner_udp_flow_rate`.
    /// Cardinality budget: rules × 6 worst-case (almost always far
    /// fewer because reasons are sparse-emitted).
    pub rate_limit_reject_total: IntCounterVec,
    /// 011-rate-limiting-qos T023: per-rule cumulative wall-clock time
    /// the bandwidth cap blocked the read / write half-loop, in
    /// seconds. Labelled `(client, rule, owner, direction)` where
    /// `direction` ∈ {`in`, `out`}. Always 0 for rules with no
    /// bandwidth cap (proto3 default-strip means the field is absent).
    pub rate_limit_throttle_seconds_total: CounterVec,
    /// 011-rate-limiting-qos T023: per-rule live count of capped
    /// connections (TCP) or NAT-bound flows (UDP). Mirrors the
    /// limiter's `active_connections` atomic. Always 0 for rules
    /// with no concurrent cap.
    pub rate_limit_active_connections: GaugeVec,
    /// 013-traffic-quotas C3: current period bytes consumed
    /// per-(user, client). Labels `&["user", "client"]`. Reset on
    /// period rollover via the rollover tick.
    pub traffic_quota_bytes_used: IntGaugeVec,
    /// 013-traffic-quotas C3: configured monthly cap in bytes per
    /// quota. Stable across the lifetime of a quota row (changes
    /// only on operator PUT/PATCH).
    pub traffic_quota_bytes_limit: IntGaugeVec,
    /// 013-traffic-quotas C3: 1 when the quota is currently
    /// exhausted, else 0. Reset to 0 on period rollover.
    pub traffic_quota_exhausted: IntGaugeVec,
    /// 013-traffic-quotas C3: monotonic count of period boundary
    /// crossings (rollovers).
    pub traffic_quota_period_resets_total: IntCounterVec,
    /// 013-traffic-quotas C3: monotonic count of first-time
    /// exhaustions per period. One increment per (user, client) per
    /// period; subsequent over-usage within the same period does NOT
    /// increment.
    pub traffic_quota_exhausted_total: IntCounterVec,
}

impl Metrics {
    /// # Errors
    ///
    /// Returns the underlying `prometheus::Error` if collector registration
    /// fails — only happens for duplicate metric names, which would be a bug.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();
        let clients_connected =
            IntGauge::new("portunus_clients_connected", "Currently-connected clients")?;
        let auth_failures_total = IntCounterVec::new(
            opts!("portunus_auth_failures_total", "Auth failures by reason"),
            &["reason"],
        )?;
        let rule_bytes_in_total = CounterVec::new(
            opts!(
                "portunus_rule_bytes_in_total",
                "Cumulative bytes ingressing each rule"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_bytes_out_total = CounterVec::new(
            opts!(
                "portunus_rule_bytes_out_total",
                "Cumulative bytes egressing each rule"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_active_connections = GaugeVec::new(
            opts!(
                "portunus_rule_active_connections",
                "Active forwarded connections per rule"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_dns_failures_total = IntCounterVec::new(
            opts!(
                "portunus_rule_dns_failures_total",
                "Per-rule monotonic count of end-user connections refused due to DNS resolution failure (NXDOMAIN, SERVFAIL, timeout, full multi-A exhaustion)"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_active_flows = GaugeVec::new(
            opts!(
                "portunus_rule_active_flows",
                "Live UDP flows per rule (one row per rule, even for range rules; always 0 for TCP rules)"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_udp_datagrams_in_total = IntCounterVec::new(
            opts!(
                "portunus_rule_udp_datagrams_in_total",
                "Per-rule monotonic count of UDP datagrams received from end-users (always 0 for TCP rules)"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_udp_datagrams_out_total = IntCounterVec::new(
            opts!(
                "portunus_rule_udp_datagrams_out_total",
                "Per-rule monotonic count of UDP datagrams sent back to end-users (always 0 for TCP rules)"
            ),
            &["client", "rule", "owner"],
        )?;
        let rule_flows_dropped_overflow_total = IntCounterVec::new(
            opts!(
                "portunus_rule_flows_dropped_overflow_total",
                "Per-rule monotonic count of UDP first-datagrams dropped because the per-rule flow table hit `udp_max_flows_per_rule`"
            ),
            &["client", "rule", "owner"],
        )?;
        let operator_requests_total = IntCounterVec::new(
            opts!(
                "portunus_operator_requests_total",
                "Operator HTTP requests by outcome (allow|deny) and reason (`ok` on allow, RbacError code on deny)"
            ),
            &["outcome", "reason"],
        )?;
        let audit_buffer_drops_total = IntCounter::new(
            "portunus_audit_buffer_drops_total",
            "Cumulative count of audit-ring entries evicted because the buffer was at capacity (006-management-web-ui T009)",
        )?;
        let rule_target_failovers_total = IntCounterVec::new(
            opts!(
                "portunus_rule_target_failovers_total",
                "Cumulative count of target Healthy↔Failed transitions per multi-target rule (007-multi-target-failover T035)"
            ),
            &["client", "rule", "owner"],
        )?;
        let audit_durable_writer_lag_seconds = prometheus::Gauge::new(
            "portunus_audit_durable_writer_lag_seconds",
            "Age of the oldest entry currently sitting in the durable-audit hand-off queue (008-sqlite-storage T031)",
        )?;
        let store_busy_total = IntCounter::new(
            "portunus_store_busy_total",
            "Cumulative count of SQLITE_BUSY occurrences mapped to StoreError::Transient (008-sqlite-storage T031)",
        )?;
        // ----- 009-tls-sni-routing -----
        let tls_sni_route_total = IntCounterVec::new(
            opts!(
                "portunus_tls_sni_route_total",
                "Per-rule cumulative count of SNI-dispatched connections by match kind (009-tls-sni-routing T079). `result` is `exact`, `wildcard`, or `fallback`."
            ),
            &["client", "rule", "owner", "result"],
        )?;
        let tls_sni_listener_miss_total = IntCounterVec::new(
            opts!(
                "portunus_tls_sni_listener_miss_total",
                "Per-listener cumulative count of TLS connections whose SNI matched no rule and no fallback existed (009-tls-sni-routing T079)."
            ),
            &["client", "port"],
        )?;
        let tls_sni_listener_parse_failures_total = IntCounterVec::new(
            opts!(
                "portunus_tls_sni_listener_parse_failures_total",
                "Per-listener cumulative count of peeked bytes that failed to parse as a TLS ClientHello (009-tls-sni-routing T079)."
            ),
            &["client", "port"],
        )?;
        let tls_client_hello_peek_duration_seconds_bucket = IntCounterVec::new(
            opts!(
                "portunus_tls_client_hello_peek_duration_seconds_bucket",
                "Classic histogram bucket counters for SNI ClientHello peek duration by listener."
            ),
            &["client", "port", "le"],
        )?;
        let tls_client_hello_peek_duration_seconds_sum = CounterVec::new(
            opts!(
                "portunus_tls_client_hello_peek_duration_seconds_sum",
                "Cumulative sum of SNI ClientHello peek durations in seconds."
            ),
            &["client", "port"],
        )?;
        let tls_client_hello_peek_duration_seconds_count = IntCounterVec::new(
            opts!(
                "portunus_tls_client_hello_peek_duration_seconds_count",
                "Cumulative count of SNI ClientHello peek observations."
            ),
            &["client", "port"],
        )?;
        let tls_sni_routes_active = IntGauge::new(
            "portunus_tls_sni_routes_active",
            "Number of currently-active rules whose `sni_pattern` is non-empty (009-tls-sni-routing T081).",
        )?;
        // ----- 011-rate-limiting-qos T023 -----
        let rate_limit_reject_total = IntCounterVec::new(
            opts!(
                "portunus_rate_limit_reject_total",
                "Per-rule cumulative count of connections / UDP first-packets rejected by a rate-limit cap (011-rate-limiting-qos T023). `reason` ∈ {conn_concurrent, conn_rate, udp_flow_rate, owner_concurrent, owner_conn_rate, owner_udp_flow_rate}."
            ),
            &["client", "rule", "owner", "reason"],
        )?;
        let rate_limit_throttle_seconds_total = CounterVec::new(
            opts!(
                "portunus_rate_limit_throttle_seconds_total",
                "Per-rule cumulative wall-clock time a bandwidth cap blocked the copy half-loop, in seconds (011-rate-limiting-qos T023). `direction` ∈ {in, out}."
            ),
            &["client", "rule", "owner", "direction"],
        )?;
        let rate_limit_active_connections = GaugeVec::new(
            opts!(
                "portunus_rate_limit_active_connections",
                "Per-rule live count of capped connections (TCP) or NAT-bound flows (UDP) (011-rate-limiting-qos T023)."
            ),
            &["client", "rule", "owner"],
        )?;
        let traffic_quota_bytes_used = IntGaugeVec::new(
            opts!(
                "portunus_traffic_quota_bytes_used",
                "Per-(user, client) current-period cumulative bytes consumed (013-traffic-quotas)."
            ),
            &["user", "client"],
        )?;
        let traffic_quota_bytes_limit = IntGaugeVec::new(
            opts!(
                "portunus_traffic_quota_bytes_limit",
                "Per-(user, client) monthly byte budget (013-traffic-quotas)."
            ),
            &["user", "client"],
        )?;
        let traffic_quota_exhausted = IntGaugeVec::new(
            opts!(
                "portunus_traffic_quota_exhausted",
                "1 when the quota is currently exhausted, else 0 (013-traffic-quotas)."
            ),
            &["user", "client"],
        )?;
        let traffic_quota_period_resets_total = IntCounterVec::new(
            opts!(
                "portunus_traffic_quota_period_resets_total",
                "Per-(user, client) monotonic count of period boundary rollovers (013-traffic-quotas)."
            ),
            &["user", "client"],
        )?;
        let traffic_quota_exhausted_total = IntCounterVec::new(
            opts!(
                "portunus_traffic_quota_exhausted_total",
                "Per-(user, client) monotonic count of first-time period exhaustions (013-traffic-quotas)."
            ),
            &["user", "client"],
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
        registry.register(Box::new(rule_target_failovers_total.clone()))?;
        registry.register(Box::new(audit_durable_writer_lag_seconds.clone()))?;
        registry.register(Box::new(store_busy_total.clone()))?;
        registry.register(Box::new(tls_sni_route_total.clone()))?;
        registry.register(Box::new(tls_sni_listener_miss_total.clone()))?;
        registry.register(Box::new(tls_sni_listener_parse_failures_total.clone()))?;
        registry.register(Box::new(
            tls_client_hello_peek_duration_seconds_bucket.clone(),
        ))?;
        registry.register(Box::new(tls_client_hello_peek_duration_seconds_sum.clone()))?;
        registry.register(Box::new(
            tls_client_hello_peek_duration_seconds_count.clone(),
        ))?;
        registry.register(Box::new(tls_sni_routes_active.clone()))?;
        registry.register(Box::new(rate_limit_reject_total.clone()))?;
        registry.register(Box::new(rate_limit_throttle_seconds_total.clone()))?;
        registry.register(Box::new(rate_limit_active_connections.clone()))?;
        registry.register(Box::new(traffic_quota_bytes_used.clone()))?;
        registry.register(Box::new(traffic_quota_bytes_limit.clone()))?;
        registry.register(Box::new(traffic_quota_exhausted.clone()))?;
        registry.register(Box::new(traffic_quota_period_resets_total.clone()))?;
        registry.register(Box::new(traffic_quota_exhausted_total.clone()))?;

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
            rule_target_failovers_total,
            audit_buffer_drops_total,
            audit_durable_writer_lag_seconds,
            store_busy_total,
            tls_sni_route_total,
            tls_sni_listener_miss_total,
            tls_sni_listener_parse_failures_total,
            tls_client_hello_peek_duration_seconds_bucket,
            tls_client_hello_peek_duration_seconds_sum,
            tls_client_hello_peek_duration_seconds_count,
            tls_sni_routes_active,
            rate_limit_reject_total,
            rate_limit_throttle_seconds_total,
            rate_limit_active_connections,
            traffic_quota_bytes_used,
            traffic_quota_bytes_limit,
            traffic_quota_exhausted,
            traffic_quota_period_resets_total,
            traffic_quota_exhausted_total,
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

/// 009-tls-sni-routing T080: typedef for the per-listener delta map
/// (factors `clippy::type_complexity` out of `RuleStatsCache`).
type SniListenerPrevMap = HashMap<(String, u16), SniListenerPrevEntry>;

#[derive(Debug, Clone, Default)]
struct SniListenerPrevEntry {
    miss_total: u64,
    parse_failures_total: u64,
    peek_bucket_counts: Vec<u64>,
    peek_sum_micros: u64,
    peek_count: u64,
}

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
    /// 009-tls-sni-routing T080: per-listener delta cache keyed on
    /// `(client_name, listen_port)`. Tracks the previous tick's
    /// cumulative `(miss, parse_failures)` so the new tick can feed
    /// monotonic deltas into the per-listener Prometheus collectors.
    sni_listener_prev: Arc<RwLock<SniListenerPrevMap>>,
    /// 011-rate-limiting-qos T032: per-owner delta cache keyed on
    /// `(client_name, owner_id)`. Mirrors `prev_rate_limit_*` on the
    /// per-rule entry but lives at owner granularity so the
    /// cross-rule aggregation surfaced by the client's
    /// `OwnerRateLimitStatsRegistry` feeds into stable monotonic
    /// deltas in Prometheus.
    owner_rate_limit_prev: Arc<RwLock<HashMap<(ClientName, String), OwnerRateLimitPrevEntry>>>,
}

#[derive(Debug, Clone, Default)]
struct OwnerRateLimitPrevEntry {
    reject_by_reason: [u64; 6],
    throttle_micros_in: u64,
    throttle_micros_out: u64,
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
    /// 007-multi-target-failover T033/T035: previous failovers count
    /// for monotonic delta into the new collector.
    prev_target_failovers_total: u64,
    /// 009-tls-sni-routing T080: per-rule cumulative SNI hit
    /// counters from the previous tick. Same baseline-reset rule as
    /// the other deltas — `new < prev` is treated as a fresh window
    /// (monotonic counters never decrement).
    prev_sni_route_exact_total: u64,
    prev_sni_route_wildcard_total: u64,
    prev_sni_route_fallback_total: u64,
    /// 011-rate-limiting-qos T023: per-rule cumulative reject totals
    /// from the previous tick, indexed 1:1 with the `RejectReason`
    /// enum's six variants (ConnConcurrent, ConnRate, UdpFlowRate,
    /// OwnerConcurrent, OwnerConnRate, OwnerUdpFlowRate). Same
    /// baseline-reset rule as the other deltas.
    prev_rate_limit_reject_by_reason: [u64; 6],
    /// 011-rate-limiting-qos T023: per-rule cumulative throttle micros
    /// per direction from the previous tick. Converted to seconds on
    /// the wire.
    prev_rate_limit_throttle_micros_in: u64,
    prev_rate_limit_throttle_micros_out: u64,
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
        self.observe_with_targets(
            client_name,
            rule_id,
            owner,
            bytes_in,
            bytes_out,
            active_connections,
            dns_failures,
            datagrams_in,
            datagrams_out,
            active_flows,
            flows_dropped_overflow,
            0,
            Vec::new(),
            metrics,
        )
        .await;
    }

    /// 007-multi-target-failover (T033) extension to `observe`. Adds
    /// `target_failovers_total` (drives the new Prometheus collector)
    /// and `per_target` (cached for `?per_target=true` HTTP reads).
    /// Single-target rules pass `0 + Vec::new()` which yields a no-op
    /// delta — no extra series in `/metrics`, no per-target body in
    /// the JSON snapshot.
    #[allow(clippy::too_many_arguments)]
    pub async fn observe_with_targets(
        &self,
        client_name: &ClientName,
        rule_id: RuleId,
        owner: &str,
        bytes_in: u64,
        bytes_out: u64,
        active_connections: u32,
        dns_failures: u64,
        datagrams_in: u64,
        datagrams_out: u64,
        active_flows: u32,
        flows_dropped_overflow: u64,
        target_failovers_total: u64,
        per_target: Vec<PerTargetSnapshot>,
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
                target_failovers_total: 0,
                per_target: Vec::new(),
                updated_at: Utc::now(),
            },
            prev_bytes_in: 0,
            prev_bytes_out: 0,
            prev_dns_failures: 0,
            prev_datagrams_in: 0,
            prev_datagrams_out: 0,
            prev_flows_dropped_overflow: 0,
            prev_target_failovers_total: 0,
            prev_sni_route_exact_total: 0,
            prev_sni_route_wildcard_total: 0,
            prev_sni_route_fallback_total: 0,
            prev_rate_limit_reject_by_reason: [0; 6],
            prev_rate_limit_throttle_micros_in: 0,
            prev_rate_limit_throttle_micros_out: 0,
        });

        let rule_id_str = rule_id.0.to_string();
        let labels = [client_name.as_str(), rule_id_str.as_str(), owner];
        let in_delta = bytes_in.saturating_sub(entry.prev_bytes_in);
        let out_delta = bytes_out.saturating_sub(entry.prev_bytes_out);
        let dns_delta = dns_failures.saturating_sub(entry.prev_dns_failures);
        let dgin_delta = datagrams_in.saturating_sub(entry.prev_datagrams_in);
        let dgout_delta = datagrams_out.saturating_sub(entry.prev_datagrams_out);
        let drop_delta = flows_dropped_overflow.saturating_sub(entry.prev_flows_dropped_overflow);
        let failover_delta =
            target_failovers_total.saturating_sub(entry.prev_target_failovers_total);
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
        if failover_delta > 0 {
            metrics
                .rule_target_failovers_total
                .with_label_values(&labels)
                .inc_by(failover_delta);
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
        entry.prev_target_failovers_total = target_failovers_total;
        entry.snapshot.bytes_in = bytes_in;
        entry.snapshot.bytes_out = bytes_out;
        entry.snapshot.active_connections = active_connections;
        entry.snapshot.dns_failures = dns_failures;
        entry.snapshot.datagrams_in = datagrams_in;
        entry.snapshot.datagrams_out = datagrams_out;
        entry.snapshot.active_flows = active_flows;
        entry.snapshot.flows_dropped_overflow = flows_dropped_overflow;
        entry.snapshot.target_failovers_total = target_failovers_total;
        entry.snapshot.per_target = per_target;
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

    /// 009-tls-sni-routing T080: fold the per-rule SNI hit counters
    /// into the new `portunus_tls_sni_route_total` collector. Same
    /// monotonic-delta + baseline-reset semantics as `observe`. Each
    /// of the three counters lands on a distinct `result` label
    /// value (`exact` / `wildcard` / `fallback`).
    ///
    /// Called from the gRPC `handle_stats_report` for every
    /// `RuleStats` row whose three SNI counters are present (the
    /// proto3 default-strip means TCP-legacy / UDP rules don't even
    /// pay the load — their values stay 0 forever).
    #[allow(clippy::too_many_arguments)]
    pub async fn observe_sni_per_rule(
        &self,
        client_name: &ClientName,
        rule_id: RuleId,
        owner: &str,
        sni_route_exact_total: u64,
        sni_route_wildcard_total: u64,
        sni_route_fallback_total: u64,
        metrics: &Metrics,
    ) {
        let mut guard = self.inner.write().await;
        let Some(entry) = guard.get_mut(&rule_id) else {
            // observe() runs first in the gRPC handler, so the entry
            // must exist by the time we get here. If it doesn't,
            // the rule was just removed — drop silently.
            return;
        };
        let exact_delta = sni_route_exact_total.saturating_sub(entry.prev_sni_route_exact_total);
        let wild_delta =
            sni_route_wildcard_total.saturating_sub(entry.prev_sni_route_wildcard_total);
        let fb_delta = sni_route_fallback_total.saturating_sub(entry.prev_sni_route_fallback_total);
        let rule_id_str = rule_id.0.to_string();
        if exact_delta > 0 {
            metrics
                .tls_sni_route_total
                .with_label_values(&[client_name.as_str(), rule_id_str.as_str(), owner, "exact"])
                .inc_by(exact_delta);
        }
        if wild_delta > 0 {
            metrics
                .tls_sni_route_total
                .with_label_values(&[
                    client_name.as_str(),
                    rule_id_str.as_str(),
                    owner,
                    "wildcard",
                ])
                .inc_by(wild_delta);
        }
        if fb_delta > 0 {
            metrics
                .tls_sni_route_total
                .with_label_values(&[
                    client_name.as_str(),
                    rule_id_str.as_str(),
                    owner,
                    "fallback",
                ])
                .inc_by(fb_delta);
        }
        entry.prev_sni_route_exact_total = sni_route_exact_total;
        entry.prev_sni_route_wildcard_total = sni_route_wildcard_total;
        entry.prev_sni_route_fallback_total = sni_route_fallback_total;
    }

    /// 011-rate-limiting-qos T023: fold a per-rule `RateLimitStats`
    /// payload into the three new collectors (`reject_total`,
    /// `throttle_seconds_total`, `active_connections`). `reject_totals`
    /// is the dense 6-slot vector indexed by [`RejectReason`] — call
    /// sites in `service.rs` flatten the proto's sparse repeated
    /// `reject_total` into this shape so the cache can take direct
    /// deltas.
    ///
    /// Saturating-sub on the deltas mirrors the other observe paths
    /// — a client-side rebaseline (e.g. process restart) is treated
    /// as a fresh window and counters never decrement.
    #[allow(clippy::too_many_arguments)]
    pub async fn observe_rate_limit_per_rule(
        &self,
        client_name: &ClientName,
        rule_id: RuleId,
        owner: &str,
        reject_totals: [u64; 6],
        throttle_micros_in: u64,
        throttle_micros_out: u64,
        active_connections: u32,
        metrics: &Metrics,
    ) {
        let mut guard = self.inner.write().await;
        let Some(entry) = guard.get_mut(&rule_id) else {
            // Same race as observe_sni_per_rule: rule was just
            // removed between observe() and here. Drop silently.
            return;
        };
        let rule_id_str = rule_id.0.to_string();
        const REASON_LABELS: [&str; 6] = [
            "conn_concurrent",
            "conn_rate",
            "udp_flow_rate",
            "owner_concurrent",
            "owner_conn_rate",
            "owner_udp_flow_rate",
        ];
        for (idx, &total) in reject_totals.iter().enumerate() {
            let prev = entry.prev_rate_limit_reject_by_reason[idx];
            let delta = total.saturating_sub(prev);
            if delta > 0 {
                metrics
                    .rate_limit_reject_total
                    .with_label_values(&[
                        client_name.as_str(),
                        rule_id_str.as_str(),
                        owner,
                        REASON_LABELS[idx],
                    ])
                    .inc_by(delta);
            }
            entry.prev_rate_limit_reject_by_reason[idx] = total;
        }
        let in_delta = throttle_micros_in.saturating_sub(entry.prev_rate_limit_throttle_micros_in);
        if in_delta > 0 {
            metrics
                .rate_limit_throttle_seconds_total
                .with_label_values(&[client_name.as_str(), rule_id_str.as_str(), owner, "in"])
                .inc_by(precise_f64(in_delta) / 1_000_000.0);
        }
        let out_delta =
            throttle_micros_out.saturating_sub(entry.prev_rate_limit_throttle_micros_out);
        if out_delta > 0 {
            metrics
                .rate_limit_throttle_seconds_total
                .with_label_values(&[client_name.as_str(), rule_id_str.as_str(), owner, "out"])
                .inc_by(precise_f64(out_delta) / 1_000_000.0);
        }
        entry.prev_rate_limit_throttle_micros_in = throttle_micros_in;
        entry.prev_rate_limit_throttle_micros_out = throttle_micros_out;
        // Active-connections is a gauge — `set` not `inc_by`. The
        // limiter's atomic is the source of truth and may decrease as
        // connections close.
        metrics
            .rate_limit_active_connections
            .with_label_values(&[client_name.as_str(), rule_id_str.as_str(), owner])
            .set(f64::from(active_connections));
    }

    /// 011-rate-limiting-qos T032: fold per-owner cumulative counters
    /// into the existing `portunus_rate_limit_*` collectors using
    /// `rule=""` to denote owner-aggregated rows. Operators slice with
    /// `portunus_rate_limit_reject_total{rule="",owner="alice"}` for the
    /// owner aggregate or `rule!=""` for per-rule rows. Same monotonic
    /// delta semantics and baseline-reset rule as the per-rule path.
    #[allow(clippy::too_many_arguments)]
    pub async fn observe_rate_limit_per_owner(
        &self,
        client_name: &ClientName,
        owner_id: &str,
        reject_totals: [u64; 6],
        throttle_micros_in: u64,
        throttle_micros_out: u64,
        active_connections: u32,
        metrics: &Metrics,
    ) {
        const REASON_LABELS: [&str; 6] = [
            "conn_concurrent",
            "conn_rate",
            "udp_flow_rate",
            "owner_concurrent",
            "owner_conn_rate",
            "owner_udp_flow_rate",
        ];
        let key = (client_name.clone(), owner_id.to_string());
        let mut guard = self.owner_rate_limit_prev.write().await;
        let entry = guard.entry(key).or_default();
        for (idx, &total) in reject_totals.iter().enumerate() {
            let prev = entry.reject_by_reason[idx];
            let delta = total.saturating_sub(prev);
            if delta > 0 {
                metrics
                    .rate_limit_reject_total
                    .with_label_values(&[client_name.as_str(), "", owner_id, REASON_LABELS[idx]])
                    .inc_by(delta);
            }
            entry.reject_by_reason[idx] = total;
        }
        let in_delta = throttle_micros_in.saturating_sub(entry.throttle_micros_in);
        if in_delta > 0 {
            metrics
                .rate_limit_throttle_seconds_total
                .with_label_values(&[client_name.as_str(), "", owner_id, "in"])
                .inc_by(precise_f64(in_delta) / 1_000_000.0);
        }
        let out_delta = throttle_micros_out.saturating_sub(entry.throttle_micros_out);
        if out_delta > 0 {
            metrics
                .rate_limit_throttle_seconds_total
                .with_label_values(&[client_name.as_str(), "", owner_id, "out"])
                .inc_by(precise_f64(out_delta) / 1_000_000.0);
        }
        entry.throttle_micros_in = throttle_micros_in;
        entry.throttle_micros_out = throttle_micros_out;
        metrics
            .rate_limit_active_connections
            .with_label_values(&[client_name.as_str(), "", owner_id])
            .set(f64::from(active_connections));
    }

    /// 009-tls-sni-routing T080: fold per-listener counters into the
    /// new `portunus_tls_sni_listener_*` collectors.
    #[allow(clippy::too_many_arguments)]
    pub async fn observe_sni_listener(
        &self,
        client_name: &ClientName,
        port: u16,
        sni_route_miss_total: u64,
        client_hello_parse_failures_total: u64,
        client_hello_peek_bucket_counts: &[u64],
        client_hello_peek_sum_micros: u64,
        client_hello_peek_count: u64,
        metrics: &Metrics,
    ) {
        let key = (client_name.as_str().to_string(), port);
        let mut guard = self.sni_listener_prev.write().await;
        let prev = guard
            .entry(key.clone())
            .or_insert_with(|| SniListenerPrevEntry {
                peek_bucket_counts: vec![0; PEEK_HISTOGRAM_BUCKETS_SECS.len()],
                ..SniListenerPrevEntry::default()
            });
        let miss_delta = sni_route_miss_total.saturating_sub(prev.miss_total);
        let parse_delta =
            client_hello_parse_failures_total.saturating_sub(prev.parse_failures_total);
        let port_str = port.to_string();
        if miss_delta > 0 {
            metrics
                .tls_sni_listener_miss_total
                .with_label_values(&[client_name.as_str(), port_str.as_str()])
                .inc_by(miss_delta);
        }
        if parse_delta > 0 {
            metrics
                .tls_sni_listener_parse_failures_total
                .with_label_values(&[client_name.as_str(), port_str.as_str()])
                .inc_by(parse_delta);
        }
        for (idx, upper) in PEEK_HISTOGRAM_BUCKETS_SECS.iter().enumerate() {
            let next = client_hello_peek_bucket_counts
                .get(idx)
                .copied()
                .unwrap_or(0);
            let baseline = prev.peek_bucket_counts.get(idx).copied().unwrap_or(0);
            let delta = next.saturating_sub(baseline);
            if delta > 0 {
                let le = upper.to_string();
                metrics
                    .tls_client_hello_peek_duration_seconds_bucket
                    .with_label_values(&[client_name.as_str(), port_str.as_str(), le.as_str()])
                    .inc_by(delta);
            }
        }
        let sum_delta_micros = client_hello_peek_sum_micros.saturating_sub(prev.peek_sum_micros);
        if sum_delta_micros > 0 {
            metrics
                .tls_client_hello_peek_duration_seconds_sum
                .with_label_values(&[client_name.as_str(), port_str.as_str()])
                .inc_by(precise_f64(sum_delta_micros) / 1_000_000.0);
        }
        let count_delta = client_hello_peek_count.saturating_sub(prev.peek_count);
        if count_delta > 0 {
            metrics
                .tls_client_hello_peek_duration_seconds_bucket
                .with_label_values(&[client_name.as_str(), port_str.as_str(), "+Inf"])
                .inc_by(count_delta);
            metrics
                .tls_client_hello_peek_duration_seconds_count
                .with_label_values(&[client_name.as_str(), port_str.as_str()])
                .inc_by(count_delta);
        }
        prev.miss_total = sni_route_miss_total;
        prev.parse_failures_total = client_hello_parse_failures_total;
        prev.peek_bucket_counts = client_hello_peek_bucket_counts.to_vec();
        prev.peek_sum_micros = client_hello_peek_sum_micros;
        prev.peek_count = client_hello_peek_count;
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
            let _ = metrics
                .rule_target_failovers_total
                .remove_label_values(&labels);
            // 011-rate-limiting-qos T023: strip the rate-limit rows.
            // `reject_total` carries an extra `reason` label so each
            // of the six possibilities must be peeled off explicitly.
            // remove_label_values silently ignores absent rows, so
            // peeling all six is correct regardless of which actually
            // fired.
            let _ = metrics
                .rate_limit_active_connections
                .remove_label_values(&labels);
            for direction in ["in", "out"] {
                let labels_dir = [client_name.as_str(), rule_id_str.as_str(), owner, direction];
                let _ = metrics
                    .rate_limit_throttle_seconds_total
                    .remove_label_values(&labels_dir);
            }
            for reason in [
                "conn_concurrent",
                "conn_rate",
                "udp_flow_rate",
                "owner_concurrent",
                "owner_conn_rate",
                "owner_udp_flow_rate",
            ] {
                let labels_reason = [client_name.as_str(), rule_id_str.as_str(), owner, reason];
                let _ = metrics
                    .rate_limit_reject_total
                    .remove_label_values(&labels_reason);
            }
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
                "portunus_rule_bytes_in_total{client=\"edge-a\",owner=\"alice\",rule=\"7\"} 1500"
            ),
            "rendered metrics: {body}"
        );
        assert!(body.contains(
            "portunus_rule_bytes_out_total{client=\"edge-a\",owner=\"alice\",rule=\"7\"} 2100"
        ));
        assert!(body.contains(
            "portunus_rule_active_connections{client=\"edge-a\",owner=\"alice\",rule=\"7\"} 2"
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
                "portunus_rule_bytes_in_total{client=\"edge-a\",owner=\"alice\",rule=\"1\"} 5000"
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
                "portunus_rule_bytes_in_total{client=\"edge-a\",owner=\"alice\",rule=\"1\"} 5200"
            ),
            "rendered: {body}"
        );
    }

    /// T044 (US4): per-rule cardinality budget — exactly one
    /// `portunus_rule_dns_failures_total` row per `(client, rule)`
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
            .filter(|l| l.starts_with("portunus_rule_dns_failures_total{"))
            .count();
        assert_eq!(
            row_count as u64, N,
            "expected exactly N={N} rows, got {row_count}\n--- body ---\n{body}"
        );
        for i in 0..N {
            let pat = format!(
                "portunus_rule_dns_failures_total{{client=\"edge-a\",owner=\"alice\",rule=\"{i}\"}} 7"
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
            "portunus_rule_dns_failures_total{client=\"edge-a\",owner=\"alice\",rule=\"42\"} 5"
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
            "portunus_rule_active_flows{",
            "portunus_rule_udp_datagrams_in_total{",
            "portunus_rule_udp_datagrams_out_total{",
            "portunus_rule_flows_dropped_overflow_total{",
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
            "portunus_rule_active_flows{client=\"edge-a\",owner=\"alice\",rule=\"99\"} 7"
        ));
        assert!(body.contains(
            "portunus_rule_udp_datagrams_in_total{client=\"edge-a\",owner=\"alice\",rule=\"99\"} 100"
        ));

        cache
            .drop_rule(RuleId(99), &name("edge-a"), "alice", &metrics)
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        // Byte counters are kept per Prometheus convention (SC-002 budget
        // accepts unbounded retention there). UDP-specific gauges and
        // counters MUST be cleared.
        for collector in [
            "portunus_rule_active_flows{",
            "portunus_rule_udp_datagrams_in_total{",
            "portunus_rule_udp_datagrams_out_total{",
            "portunus_rule_flows_dropped_overflow_total{",
            "portunus_rule_active_connections{",
            "portunus_rule_dns_failures_total{",
        ] {
            assert!(
                !body.lines().any(|l| l.starts_with(collector)),
                "dropped rule row MUST disappear from {collector}: {body}"
            );
        }
    }

    // ----- 011-rate-limiting-qos T023: rate-limit fold tests -----

    #[tokio::test]
    async fn observe_rate_limit_emits_per_reason_rows() {
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();
        // Seed an entry via the standard observe path so the cached
        // record exists.
        cache
            .observe(
                &name("edge-a"),
                RuleId(11),
                "alice",
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
        // Two reasons fired; throttle micros are non-zero on both
        // directions; gauge says 4 active.
        cache
            .observe_rate_limit_per_rule(
                &name("edge-a"),
                RuleId(11),
                "alice",
                [3, 0, 0, 0, 1, 0],
                500_000,
                250_000,
                4,
                &metrics,
            )
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(
            body.contains(
                "portunus_rate_limit_reject_total{client=\"edge-a\",owner=\"alice\",reason=\"conn_concurrent\",rule=\"11\"} 3"
            ),
            "missing conn_concurrent row in: {body}"
        );
        assert!(
            body.contains(
                "portunus_rate_limit_reject_total{client=\"edge-a\",owner=\"alice\",reason=\"owner_conn_rate\",rule=\"11\"} 1"
            ),
            "missing owner_conn_rate row in: {body}"
        );
        // Untouched reason MUST NOT emit a row (cardinality budget).
        assert!(
            !body.contains("reason=\"udp_flow_rate\""),
            "unfired reason emitted a row: {body}"
        );
        assert!(body.contains(
            "portunus_rate_limit_throttle_seconds_total{client=\"edge-a\",direction=\"in\",owner=\"alice\",rule=\"11\"} 0.5"
        ));
        assert!(body.contains(
            "portunus_rate_limit_throttle_seconds_total{client=\"edge-a\",direction=\"out\",owner=\"alice\",rule=\"11\"} 0.25"
        ));
        assert!(body.contains(
            "portunus_rate_limit_active_connections{client=\"edge-a\",owner=\"alice\",rule=\"11\"} 4"
        ));
    }

    #[tokio::test]
    async fn observe_rate_limit_takes_monotonic_deltas() {
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();
        cache
            .observe(
                &name("edge-a"),
                RuleId(12),
                "bob",
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
            .observe_rate_limit_per_rule(
                &name("edge-a"),
                RuleId(12),
                "bob",
                [10, 0, 0, 0, 0, 0],
                100_000,
                0,
                2,
                &metrics,
            )
            .await;
        cache
            .observe_rate_limit_per_rule(
                &name("edge-a"),
                RuleId(12),
                "bob",
                [25, 0, 0, 0, 0, 0],
                150_000,
                0,
                3,
                &metrics,
            )
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        // Cumulative delta = 10 + 15 = 25 (monotonic).
        assert!(
            body.contains(
                "portunus_rate_limit_reject_total{client=\"edge-a\",owner=\"bob\",reason=\"conn_concurrent\",rule=\"12\"} 25"
            ),
            "expected cumulative 25, got: {body}"
        );
        assert!(body.contains(
            "portunus_rate_limit_throttle_seconds_total{client=\"edge-a\",direction=\"in\",owner=\"bob\",rule=\"12\"} 0.15"
        ));
        // Gauge tracks the latest value.
        assert!(body.contains(
            "portunus_rate_limit_active_connections{client=\"edge-a\",owner=\"bob\",rule=\"12\"} 3"
        ));
    }

    #[tokio::test]
    async fn drop_rule_removes_rate_limit_rows() {
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();
        cache
            .observe(
                &name("edge-a"),
                RuleId(13),
                "alice",
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
            .observe_rate_limit_per_rule(
                &name("edge-a"),
                RuleId(13),
                "alice",
                [1, 0, 0, 0, 1, 0],
                500_000,
                500_000,
                4,
                &metrics,
            )
            .await;
        // Sanity: rows exist before drop.
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(body.contains("rule=\"13\""));

        cache
            .drop_rule(RuleId(13), &name("edge-a"), "alice", &metrics)
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        for collector in [
            "portunus_rate_limit_reject_total{",
            "portunus_rate_limit_throttle_seconds_total{",
            "portunus_rate_limit_active_connections{",
        ] {
            assert!(
                !body.lines().any(|l| l.starts_with(collector)),
                "dropped rule row MUST disappear from {collector}: {body}"
            );
        }
    }

    #[tokio::test]
    async fn observe_rate_limit_silently_drops_when_rule_absent() {
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();
        // RuleId(99) was never observed; the call must be a no-op.
        cache
            .observe_rate_limit_per_rule(
                &name("edge-a"),
                RuleId(99),
                "alice",
                [5, 0, 0, 0, 0, 0],
                123,
                456,
                7,
                &metrics,
            )
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(
            !body.contains("rule=\"99\""),
            "absent-rule observe must not emit any rows: {body}"
        );
    }

    /// 011-rate-limiting-qos T032: per-owner observe writes
    /// owner-aggregated rows with `rule=""` so operators can slice
    /// owner totals separately from per-rule rows.
    #[tokio::test]
    async fn t032_observe_per_owner_emits_aggregated_rows_with_empty_rule() {
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();
        cache
            .observe_rate_limit_per_owner(
                &name("edge-a"),
                "alice",
                [0, 0, 0, 7, 0, 2], // owner_concurrent=7, owner_udp_flow_rate=2
                3_000_000,          // 3s throttle in
                0,
                15,
                &metrics,
            )
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(
            body.contains(
                "portunus_rate_limit_reject_total{client=\"edge-a\",owner=\"alice\",reason=\"owner_concurrent\",rule=\"\"} 7"
            ),
            "missing owner_concurrent owner-aggregate row in: {body}"
        );
        assert!(
            body.contains(
                "portunus_rate_limit_reject_total{client=\"edge-a\",owner=\"alice\",reason=\"owner_udp_flow_rate\",rule=\"\"} 2"
            ),
            "missing owner_udp_flow_rate owner-aggregate row in: {body}"
        );
        assert!(
            body.contains(
                "portunus_rate_limit_throttle_seconds_total{client=\"edge-a\",direction=\"in\",owner=\"alice\",rule=\"\"} 3"
            ),
            "missing throttle-in owner-aggregate row in: {body}"
        );
        assert!(
            body.contains(
                "portunus_rate_limit_active_connections{client=\"edge-a\",owner=\"alice\",rule=\"\"} 15"
            ),
            "missing active-connections owner-aggregate row in: {body}"
        );
    }

    /// 011-rate-limiting-qos T032: per-owner observe takes monotonic
    /// deltas across ticks (no double-counting on the second drain).
    #[tokio::test]
    async fn t032_observe_per_owner_takes_monotonic_deltas() {
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();
        cache
            .observe_rate_limit_per_owner(
                &name("edge-a"),
                "alice",
                [0, 0, 0, 5, 0, 0],
                1_000_000,
                0,
                3,
                &metrics,
            )
            .await;
        cache
            .observe_rate_limit_per_owner(
                &name("edge-a"),
                "alice",
                [0, 0, 0, 8, 0, 0], // +3
                2_500_000,          // +1.5s
                0,
                4,
                &metrics,
            )
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        // Counter is cumulative — total is 8 (5 from first tick +
        // delta 3 from second).
        assert!(
            body.contains(
                "portunus_rate_limit_reject_total{client=\"edge-a\",owner=\"alice\",reason=\"owner_concurrent\",rule=\"\"} 8"
            ),
            "monotonic counter must reach total=8 in: {body}"
        );
        assert!(
            body.contains(
                "portunus_rate_limit_throttle_seconds_total{client=\"edge-a\",direction=\"in\",owner=\"alice\",rule=\"\"} 2.5"
            ),
            "monotonic throttle counter must reach 2.5s in: {body}"
        );
        // Gauge reflects latest value, not delta.
        assert!(
            body.contains(
                "portunus_rate_limit_active_connections{client=\"edge-a\",owner=\"alice\",rule=\"\"} 4"
            ),
            "gauge must show latest value 4 in: {body}"
        );
    }

    /// 013-traffic-quotas C3: assert all five new families are
    /// registered and renderable. Cardinality is zero until the
    /// aggregator writes a value, so we exercise that path.
    #[test]
    fn traffic_quota_collectors_register_and_render() {
        let metrics = Metrics::new().unwrap();
        let labels = ["alice", "edge-01"];
        metrics
            .traffic_quota_bytes_used
            .with_label_values(&labels)
            .set(123);
        metrics
            .traffic_quota_bytes_limit
            .with_label_values(&labels)
            .set(1_000);
        metrics
            .traffic_quota_exhausted
            .with_label_values(&labels)
            .set(0);
        metrics
            .traffic_quota_period_resets_total
            .with_label_values(&labels)
            .inc();
        metrics
            .traffic_quota_exhausted_total
            .with_label_values(&labels)
            .inc();
        let body = String::from_utf8(metrics.render()).unwrap();
        for name in [
            "portunus_traffic_quota_bytes_used",
            "portunus_traffic_quota_bytes_limit",
            "portunus_traffic_quota_exhausted",
            "portunus_traffic_quota_period_resets_total",
            "portunus_traffic_quota_exhausted_total",
        ] {
            assert!(body.contains(name), "missing {name} in rendered metrics: {body}");
        }
        assert!(
            body.contains("portunus_traffic_quota_bytes_used{client=\"edge-01\",user=\"alice\"} 123"),
            "bytes_used must show 123 in: {body}"
        );
    }
}
