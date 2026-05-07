//! Per-port detail cache for range rules (002-port-range-forward, US3).
//!
//! Fed by the client's `StatsReport.per_port` slot on the existing gRPC
//! bidi stream; read on demand when an operator passes
//! `rule-stats <id> --per-port` (HTTP `?per_port=true`). Crucially this
//! cache is **never** re-exported as Prometheus series — that's the
//! cardinality guarantee in SC-002. The per-port detail is for
//! interactive operator use only.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use forward_core::RuleId;
use serde::Serialize;
use tokio::sync::RwLock;

/// One per-port reading, tagged with the wall-clock instant the server
/// last received it. Mirrors the proto `PerPortStats` shape one-for-one.
///
/// 004-udp-forward T053/T055: `datagrams_in/out` carry per-port UDP
/// counters for range UDP rules. TCP entries leave them at 0; the JSON
/// serialization is unconditional so generic operator tooling can rely
/// on the field's presence (mirrors the protocol/datagrams_* fields on
/// the rule-level stats body).
#[derive(Debug, Clone, Serialize)]
pub struct PerPortSnapshot {
    pub listen_port: u16,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
    #[serde(default)]
    pub datagrams_in: u64,
    #[serde(default)]
    pub datagrams_out: u64,
    pub updated_at: DateTime<Utc>,
}

/// Per-rule per-port detail cache. Cheap to clone (`Arc` internal).
#[derive(Debug, Clone, Default)]
pub struct PerPortStatsCache {
    inner: Arc<RwLock<HashMap<RuleId, BTreeMap<u16, PerPortSnapshot>>>>,
}

impl PerPortStatsCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the entire per-port snapshot for `rule_id`. Each call
    /// represents one `StatsReport.per_port` arrival; keys absent from
    /// `snapshots` are dropped (the client owns the canonical view of
    /// "which ports this rule covers").
    pub async fn update(&self, rule_id: RuleId, snapshots: Vec<PerPortSnapshot>) {
        let mut guard = self.inner.write().await;
        let map = snapshots
            .into_iter()
            .map(|s| (s.listen_port, s))
            .collect::<BTreeMap<_, _>>();
        if map.is_empty() {
            guard.remove(&rule_id);
        } else {
            guard.insert(rule_id, map);
        }
    }

    /// Snapshot every per-port reading for a rule, ordered by port.
    pub async fn get(&self, rule_id: RuleId) -> Option<Vec<PerPortSnapshot>> {
        let guard = self.inner.read().await;
        guard
            .get(&rule_id)
            .map(|m| m.values().cloned().collect::<Vec<_>>())
    }

    /// Drop any cache entry for `rule_id` (called from the rule-removal
    /// path so a removed rule's per-port detail doesn't linger).
    pub async fn drop_rule(&self, rule_id: RuleId) {
        self.inner.write().await.remove(&rule_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(port: u16, in_b: u64, out_b: u64) -> PerPortSnapshot {
        PerPortSnapshot {
            listen_port: port,
            bytes_in: in_b,
            bytes_out: out_b,
            active_connections: 0,
            datagrams_in: 0,
            datagrams_out: 0,
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn update_then_get_returns_ordered_ports() {
        let cache = PerPortStatsCache::new();
        cache
            .update(
                RuleId(7),
                vec![
                    snap(30002, 200, 0),
                    snap(30000, 100, 0),
                    snap(30001, 150, 0),
                ],
            )
            .await;
        let got = cache.get(RuleId(7)).await.unwrap();
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].listen_port, 30000);
        assert_eq!(got[1].listen_port, 30001);
        assert_eq!(got[2].listen_port, 30002);
    }

    #[tokio::test]
    async fn empty_update_clears_entry() {
        let cache = PerPortStatsCache::new();
        cache.update(RuleId(1), vec![snap(30000, 1, 1)]).await;
        cache.update(RuleId(1), vec![]).await;
        assert!(cache.get(RuleId(1)).await.is_none());
    }

    #[tokio::test]
    async fn drop_rule_removes_entry() {
        let cache = PerPortStatsCache::new();
        cache.update(RuleId(1), vec![snap(30000, 1, 1)]).await;
        cache.drop_rule(RuleId(1)).await;
        assert!(cache.get(RuleId(1)).await.is_none());
    }
}
