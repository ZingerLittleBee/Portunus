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
    CounterVec, Encoder, GaugeVec, IntCounterVec, IntGauge, Registry, TextEncoder, opts,
};
use serde::Serialize;
use tokio::sync::RwLock;

/// One client's report for one rule, plus the server-side wall-clock time
/// we last received it. Operators consume this via `rule-stats`.
#[derive(Debug, Clone, Serialize)]
pub struct RuleStatsSnapshot {
    pub rule_id: RuleId,
    pub client_name: ClientName,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
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
            &["client", "rule"],
        )?;
        let rule_bytes_out_total = CounterVec::new(
            opts!(
                "forward_rule_bytes_out_total",
                "Cumulative bytes egressing each rule"
            ),
            &["client", "rule"],
        )?;
        let rule_active_connections = GaugeVec::new(
            opts!(
                "forward_rule_active_connections",
                "Active forwarded connections per rule"
            ),
            &["client", "rule"],
        )?;
        registry.register(Box::new(clients_connected.clone()))?;
        registry.register(Box::new(auth_failures_total.clone()))?;
        registry.register(Box::new(rule_bytes_in_total.clone()))?;
        registry.register(Box::new(rule_bytes_out_total.clone()))?;
        registry.register(Box::new(rule_active_connections.clone()))?;

        Ok(Self {
            registry,
            clients_connected,
            auth_failures_total,
            rule_bytes_in_total,
            rule_bytes_out_total,
            rule_active_connections,
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

/// Cache the latest `StatsReport` per rule. Cheap to clone (`Arc` internal).
#[derive(Debug, Clone, Default)]
pub struct RuleStatsCache {
    inner: Arc<RwLock<HashMap<RuleId, CachedEntry>>>,
}

#[derive(Debug, Clone)]
struct CachedEntry {
    snapshot: RuleStatsSnapshot,
    /// Last cumulative values seen; used to compute monotonic deltas for
    /// Prometheus counters in [`RuleStatsCache::observe`].
    prev_bytes_in: u64,
    prev_bytes_out: u64,
}

impl RuleStatsCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one client-reported reading. Updates the cache and feeds deltas
    /// into the Prometheus collectors. A baseline reset (new < prev) is
    /// treated as a fresh window — counters are NOT decremented.
    pub async fn observe(
        &self,
        client_name: &ClientName,
        rule_id: RuleId,
        bytes_in: u64,
        bytes_out: u64,
        active_connections: u32,
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
                updated_at: Utc::now(),
            },
            prev_bytes_in: 0,
            prev_bytes_out: 0,
        });

        let labels = [client_name.as_str(), &rule_id.0.to_string()];
        let in_delta = bytes_in.saturating_sub(entry.prev_bytes_in);
        let out_delta = bytes_out.saturating_sub(entry.prev_bytes_out);
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
        metrics
            .rule_active_connections
            .with_label_values(&labels)
            .set(f64::from(active_connections));

        entry.prev_bytes_in = bytes_in;
        entry.prev_bytes_out = bytes_out;
        entry.snapshot.bytes_in = bytes_in;
        entry.snapshot.bytes_out = bytes_out;
        entry.snapshot.active_connections = active_connections;
        entry.snapshot.updated_at = Utc::now();
        entry.snapshot.client_name = client_name.clone();
    }

    pub async fn get(&self, rule_id: RuleId) -> Option<RuleStatsSnapshot> {
        self.inner
            .read()
            .await
            .get(&rule_id)
            .map(|e| e.snapshot.clone())
    }

    pub async fn drop_rule(&self, rule_id: RuleId, client_name: &ClientName, metrics: &Metrics) {
        let mut guard = self.inner.write().await;
        if guard.remove(&rule_id).is_some() {
            // Strip the rule's labels from the gauges so a stale entry doesn't
            // hang around in `/metrics` after the rule is removed. Counters
            // are kept per Prometheus convention (counters never disappear
            // mid-process).
            let _ = metrics
                .rule_active_connections
                .remove_label_values(&[client_name.as_str(), &rule_id.0.to_string()]);
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
            .observe(&name("edge-a"), RuleId(7), 1000, 2000, 3, &metrics)
            .await;
        let snap = cache.get(RuleId(7)).await.unwrap();
        assert_eq!(snap.bytes_in, 1000);
        assert_eq!(snap.bytes_out, 2000);
        assert_eq!(snap.active_connections, 3);
        assert_eq!(snap.client_name, name("edge-a"));

        // Second observation: counters take the delta.
        cache
            .observe(&name("edge-a"), RuleId(7), 1500, 2100, 2, &metrics)
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(
            body.contains("forward_rule_bytes_in_total{client=\"edge-a\",rule=\"7\"} 1500"),
            "rendered metrics: {body}"
        );
        assert!(body.contains("forward_rule_bytes_out_total{client=\"edge-a\",rule=\"7\"} 2100"));
        assert!(body.contains("forward_rule_active_connections{client=\"edge-a\",rule=\"7\"} 2"));
    }

    #[tokio::test]
    async fn baseline_reset_does_not_decrement_counter() {
        // If the client restarts, its in-process counters reset to 0. The
        // Prometheus counter MUST NOT go backwards; we rebaseline silently.
        let metrics = Metrics::new().unwrap();
        let cache = RuleStatsCache::new();
        cache
            .observe(&name("edge-a"), RuleId(1), 5_000, 5_000, 0, &metrics)
            .await;
        cache
            .observe(&name("edge-a"), RuleId(1), 100, 100, 0, &metrics)
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        // Total stayed at 5000 (no negative delta); next observation will
        // accumulate from this new baseline.
        assert!(
            body.contains("forward_rule_bytes_in_total{client=\"edge-a\",rule=\"1\"} 5000"),
            "rendered: {body}"
        );
        cache
            .observe(&name("edge-a"), RuleId(1), 300, 300, 0, &metrics)
            .await;
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(
            body.contains("forward_rule_bytes_in_total{client=\"edge-a\",rule=\"1\"} 5200"),
            "rendered: {body}"
        );
    }
}
