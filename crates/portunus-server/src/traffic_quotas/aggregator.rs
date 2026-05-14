//! 013-traffic-quotas: server-side byte aggregator hooked into the
//! gRPC `StatsReport` path. Reads cumulative per-rule readings,
//! computes deltas locally (independent of `RuleStatsCache`'s prev
//! map so we don't perturb its locking), and:
//!   (a) UPSERTs the current-minute sample row (always, for any pair
//!       with a non-empty owner)
//!   (b) accumulates into the active quota row (if present) and emits
//!       one `QuotaExhaustedEvent` per pair the first time bytes_used
//!       crosses monthly_bytes.

use crate::metrics::Metrics;
use crate::store::Store;
use crate::traffic_quotas::cache::TrafficQuotaCache;
use crate::traffic_quotas::samples;
use portunus_core::RuleId;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, warn};

/// Event the aggregator emits when a quota crosses from "not exhausted"
/// to "exhausted" as a result of an accumulated delta. The gRPC push
/// task consumes these and pushes `TrafficQuotaUpdate{exhausted=true}`
/// to the relevant client session.
#[derive(Debug, Clone)]
pub struct QuotaExhaustedEvent {
    pub user_id: String,
    pub client_name: String,
}

#[derive(Default, Debug)]
struct PrevSnapshot {
    bytes_in: u64,
    bytes_out: u64,
}

/// Internal prev map keyed by rule_id. The aggregator owns its own
/// delta computation so it remains decoupled from `RuleStatsCache`.
type PrevMap = HashMap<RuleId, PrevSnapshot>;

#[derive(Clone)]
pub struct TrafficAggregator {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    cache: TrafficQuotaCache,
    exhaust_tx: mpsc::Sender<QuotaExhaustedEvent>,
    prev: Mutex<PrevMap>,
    metrics: Option<Arc<Metrics>>,
}

impl TrafficAggregator {
    #[must_use]
    pub fn new(
        store: Store,
        cache: TrafficQuotaCache,
        exhaust_tx: mpsc::Sender<QuotaExhaustedEvent>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                cache,
                exhaust_tx,
                prev: Mutex::new(PrevMap::default()),
                metrics: None,
            }),
        }
    }

    #[must_use]
    pub fn with_metrics(
        store: Store,
        cache: TrafficQuotaCache,
        exhaust_tx: mpsc::Sender<QuotaExhaustedEvent>,
        metrics: Arc<Metrics>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                cache,
                exhaust_tx,
                prev: Mutex::new(PrevMap::default()),
                metrics: Some(metrics),
            }),
        }
    }

    /// Called from the gRPC `StatsReport` handler after the existing
    /// `RuleStatsCache::observe_with_targets` for one rule entry.
    /// Cumulative readings come straight from the wire; the aggregator
    /// computes its own deltas with a saturating_sub baseline-reset
    /// guard (mirrors `RuleStatsCache` rebaseline semantics).
    pub async fn record(
        &self,
        client_name: &str,
        rule_id: RuleId,
        owner_user_id: &str,
        cum_bytes_in: u64,
        cum_bytes_out: u64,
        now_unix_sec: i64,
    ) {
        // Skip legacy / unowned rules.
        if owner_user_id.is_empty() || owner_user_id == "_unknown" {
            return;
        }

        let (delta_in, delta_out) = {
            let mut prev = self.inner.prev.lock().await;
            let snap = prev.entry(rule_id).or_default();
            // saturating_sub: a client restart that resets cumulative
            // to 0 yields delta=0 (no negative counters), preserving
            // the same rebaseline guarantee `RuleStatsCache` uses.
            let din = cum_bytes_in.saturating_sub(snap.bytes_in);
            let dout = cum_bytes_out.saturating_sub(snap.bytes_out);
            snap.bytes_in = cum_bytes_in;
            snap.bytes_out = cum_bytes_out;
            (din, dout)
        };
        if delta_in == 0 && delta_out == 0 {
            return;
        }

        // (a) Always write into 1m samples — coverage is "all pairs",
        // not "quota'd pairs only".
        let ts_minute = samples::SampleBucket::M1.align(now_unix_sec);
        if let Err(e) = samples::upsert_1m_delta(
            &self.inner.store,
            owner_user_id,
            client_name,
            ts_minute,
            i64::try_from(delta_in).unwrap_or(i64::MAX),
            i64::try_from(delta_out).unwrap_or(i64::MAX),
        ) {
            error!(
                event = "traffic_aggregator.sample_write_failed",
                error = %e,
                client = client_name,
                user = owner_user_id,
            );
        }

        // (b) If a quota row exists for this pair, accumulate + check
        // first-time exhausted.
        if self.inner.cache.get(owner_user_id, client_name).is_none() {
            return;
        }
        let delta_total = i64::try_from(delta_in.saturating_add(delta_out)).unwrap_or(i64::MAX);
        match self
            .inner
            .cache
            .accumulate(owner_user_id, client_name, delta_total, now_unix_sec)
        {
            Ok(Some((row, just_exhausted))) => {
                if let Some(metrics) = self.inner.metrics.as_ref() {
                    let labels = [row.user_id.as_str(), row.client_name.as_str()];
                    metrics
                        .traffic_quota_bytes_used
                        .with_label_values(&labels)
                        .set(row.current_period_bytes_used);
                    metrics
                        .traffic_quota_bytes_limit
                        .with_label_values(&labels)
                        .set(row.monthly_bytes);
                    metrics
                        .traffic_quota_exhausted
                        .with_label_values(&labels)
                        .set(i64::from(row.is_exhausted()));
                    if just_exhausted {
                        metrics
                            .traffic_quota_exhausted_total
                            .with_label_values(&labels)
                            .inc();
                    }
                }
                if just_exhausted {
                    // First-time exhausted. Emit one event; downstream
                    // pushes TrafficQuotaUpdate{exhausted=true} to client.
                    if self
                        .inner
                        .exhaust_tx
                        .send(QuotaExhaustedEvent {
                            user_id: row.user_id.clone(),
                            client_name: row.client_name.clone(),
                        })
                        .await
                        .is_err()
                    {
                        warn!(
                            event = "traffic_aggregator.exhaust_channel_closed",
                            client = client_name,
                            user = owner_user_id,
                        );
                    }
                }
                debug!(
                    event = "traffic_aggregator.accumulated",
                    user = %row.user_id,
                    client = %row.client_name,
                    delta_total,
                    used = row.current_period_bytes_used,
                    monthly = row.monthly_bytes,
                );
            }
            Ok(None) => {
                debug!(
                    event = "traffic_aggregator.no_row",
                    client = client_name,
                    user = owner_user_id,
                );
            }
            Err(e) => {
                error!(
                    event = "traffic_aggregator.accumulate_failed",
                    error = %e,
                );
            }
        }
    }

    /// Drop the prev entry for a rule (called when a rule is removed
    /// so the prev map doesn't accumulate dead entries).
    pub async fn drop_rule(&self, rule_id: RuleId) {
        let mut prev = self.inner.prev.lock().await;
        prev.remove(&rule_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use crate::traffic_quotas::TrafficQuotaRow;
    use crate::traffic_quotas::store as quota_store;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    fn build_agg() -> (
        tempfile::TempDir,
        Store,
        TrafficAggregator,
        mpsc::Receiver<QuotaExhaustedEvent>,
    ) {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).expect("open");
        let cache = TrafficQuotaCache::load(store.clone()).expect("cache");
        let (tx, rx) = mpsc::channel(16);
        let agg = TrafficAggregator::new(store.clone(), cache, tx);
        (dir, store, agg, rx)
    }

    fn sample_quota(monthly: i64) -> TrafficQuotaRow {
        TrafficQuotaRow {
            user_id: "alice".into(),
            client_name: "edge-01".into(),
            monthly_bytes: monthly,
            billing_anchor: 0,
            current_period_started_at: 0,
            current_period_bytes_used: 0,
            exhausted_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[tokio::test]
    async fn record_skips_empty_owner() {
        let (_d, _store, agg, mut rx) = build_agg();
        agg.record("edge-01", RuleId(1), "", 100, 200, 60).await;
        // No 1m row written.
        let rows = samples::query_samples(
            &agg.inner.store,
            samples::SampleBucket::M1,
            None,
            None,
            0,
            120,
        )
        .unwrap();
        assert!(rows.is_empty());
        // No exhaust event.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn record_writes_minute_sample_with_delta() {
        let (_d, _store, agg, _rx) = build_agg();
        // First tick — delta = cumulative.
        agg.record("edge-01", RuleId(1), "alice", 100, 200, 60)
            .await;
        // Second tick — delta = (new - prev).
        agg.record("edge-01", RuleId(1), "alice", 150, 250, 60)
            .await;
        let rows = samples::query_samples(
            &agg.inner.store,
            samples::SampleBucket::M1,
            Some("alice"),
            Some("edge-01"),
            0,
            120,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        // Cumulative deltas: in = 100 + 50 = 150; out = 200 + 50 = 250
        assert_eq!(rows[0].bytes_in, 150);
        assert_eq!(rows[0].bytes_out, 250);
    }

    #[tokio::test]
    async fn record_handles_client_rebaseline() {
        let (_d, _store, agg, _rx) = build_agg();
        agg.record("edge-01", RuleId(1), "alice", 1_000, 2_000, 60)
            .await;
        // Client restart: cumulative drops back to small numbers.
        agg.record("edge-01", RuleId(1), "alice", 50, 100, 60).await;
        let rows = samples::query_samples(
            &agg.inner.store,
            samples::SampleBucket::M1,
            Some("alice"),
            Some("edge-01"),
            0,
            120,
        )
        .unwrap();
        // First tick wrote 1_000/2_000; second tick yields delta=0 due to
        // saturating_sub.
        assert_eq!(rows[0].bytes_in, 1_000);
        assert_eq!(rows[0].bytes_out, 2_000);
    }

    #[tokio::test]
    async fn record_accumulates_when_quota_present_and_emits_exhausted_once() {
        let (_d, store, _agg, _rx) = build_agg();
        // Pre-create a 1_000-byte quota.
        quota_store::insert_or_replace(&store, &sample_quota(1_000)).unwrap();
        // Reload cache to pick up the row.
        let cache = TrafficQuotaCache::load(store.clone()).unwrap();
        let (tx, mut rx2) = mpsc::channel(16);
        let agg = TrafficAggregator::new(store.clone(), cache, tx);

        // Tick 1: 400 bytes total -> under quota.
        agg.record("edge-01", RuleId(1), "alice", 200, 200, 60)
            .await;
        assert!(rx2.try_recv().is_err());

        // Tick 2: cumulative jumps to 1_200 total -> crosses quota.
        agg.record("edge-01", RuleId(1), "alice", 600, 600, 60)
            .await;
        let evt = rx2.recv().await.unwrap();
        assert_eq!(evt.user_id, "alice");
        assert_eq!(evt.client_name, "edge-01");

        // Tick 3: more bytes after exhausted -> no second event.
        agg.record("edge-01", RuleId(1), "alice", 1000, 1000, 60)
            .await;
        assert!(rx2.try_recv().is_err());
    }

    #[tokio::test]
    async fn record_no_op_when_zero_delta() {
        let (_d, _store, agg, _rx) = build_agg();
        agg.record("edge-01", RuleId(1), "alice", 0, 0, 60).await;
        let rows = samples::query_samples(
            &agg.inner.store,
            samples::SampleBucket::M1,
            None,
            None,
            0,
            120,
        )
        .unwrap();
        assert!(rows.is_empty());
    }
}
