//! 013-traffic-quotas C4 — period rollover background tick.
//!
//! Runs every 60s. For every cached quota row, computes whether the
//! current period has elapsed (calendar-month anchor math from the
//! parent module's `advance_period_if_due`) and, if so:
//!   - resets `current_period_bytes_used = 0`, clears `exhausted_at`
//!     and advances `current_period_started_at`
//!   - pushes `TrafficQuotaUpdate{SET}` to the connected client so the
//!     `QuotaHandle.remaining` is reseeded (recovering exhausted state)
//!   - increments `traffic_quota_period_resets_total`
//!
//! `run_once` is the unit-testable core; `run_forever` is the
//! long-lived tokio task.

use crate::state::AppState;
use crate::traffic_quotas::{TrafficQuotaRow, advance_period_if_due, make_traffic_quota_set_msg};
use chrono::{DateTime, TimeZone, Utc};
use portunus_core::ClientId;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info, warn};

const TICK: Duration = Duration::from_secs(60);

pub async fn run_forever(state: Arc<AppState>) {
    loop {
        sleep(TICK).await;
        match run_once(&state, Utc::now()).await {
            Ok(advanced) if advanced > 0 => info!(event = "traffic_rollover.tick", advanced,),
            Ok(_) => {}
            Err(e) => error!(event = "traffic_rollover.tick_failed", error = %e),
        }
    }
}

pub async fn run_once(
    state: &AppState,
    now: DateTime<Utc>,
) -> Result<usize, crate::store::StoreError> {
    let now_ts = now.timestamp();
    let rows = state.traffic_quotas.list_all();
    let mut advanced = 0usize;
    for r in rows {
        let Some(anchor) = Utc.timestamp_opt(r.billing_anchor, 0).single() else {
            continue;
        };
        let Some(start) = Utc.timestamp_opt(r.current_period_started_at, 0).single() else {
            continue;
        };
        let Some(new_start) = advance_period_if_due(anchor, start, now) else {
            continue;
        };
        let Some(updated) = state.traffic_quotas.reset_period(
            &r.user_id,
            &r.client_id,
            new_start.timestamp(),
            now_ts,
        )?
        else {
            continue;
        };
        info!(
            event = "traffic_quota.period_rolled",
            user = %updated.user_id,
            client = %updated.client_name,
            new_start = updated.current_period_started_at,
        );
        let labels = [updated.user_id.as_str(), updated.client_name.as_str()];
        state
            .metrics
            .traffic_quota_period_resets_total
            .with_label_values(&labels)
            .inc();
        state
            .metrics
            .traffic_quota_bytes_used
            .with_label_values(&labels)
            .set(0);
        state
            .metrics
            .traffic_quota_exhausted
            .with_label_values(&labels)
            .set(0);
        push_reset(state, &updated).await;
        advanced += 1;
    }
    Ok(advanced)
}

async fn push_reset(state: &AppState, row: &TrafficQuotaRow) {
    // 015-client-stable-id: address the live session by the stable id so
    // a renamed client still receives its period-reset push.
    let Ok(client_id) = ClientId::from_str(&row.client_id) else {
        return;
    };
    let Some((outbound, _waiters)) = state.clients.handles(&client_id).await else {
        return;
    };
    let msg = make_traffic_quota_set_msg(row, format!("quota-rollover-{}", ulid::Ulid::new()));
    if outbound.send(Ok(msg)).await.is_err() {
        warn!(
            event = "traffic_quota.rollover_push_failed",
            user = %row.user_id,
            client_id = %client_id,
        );
    }
}

#[cfg(test)]
mod tests {
    //! `run_once` builds + uses a full `AppState`, which is too
    //! heavyweight to fabricate. We instead unit-test the period-math
    //! decision (advance_period_if_due) plus the cache mutation
    //! (`reset_period`) — the same observable state changes `run_once`
    //! performs. Push delivery is exercised by `quota_http`'s push
    //! helpers; metric increments are covered by C3's smoke test.
    use super::*;
    use crate::store::Store;
    use crate::traffic_quotas::TrafficQuotaRow;
    use crate::traffic_quotas::cache::TrafficQuotaCache;
    use tempfile::tempdir;

    fn make_cache() -> (tempfile::TempDir, TrafficQuotaCache) {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).expect("open");
        (dir, TrafficQuotaCache::load(store).expect("cache"))
    }

    /// 32-day-old anchor with `current_period_started_at == anchor`
    /// should advance one period.
    #[test]
    fn advance_due_returns_new_start() {
        let anchor = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let now = anchor + chrono::Duration::days(32);
        let advanced = advance_period_if_due(anchor, anchor, now).expect("advance");
        assert_eq!(
            advanced,
            Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0).unwrap()
        );
    }

    /// Within-period (29 days in) should NOT advance.
    #[test]
    fn no_advance_within_period() {
        let anchor = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let now = anchor + chrono::Duration::days(29);
        assert!(advance_period_if_due(anchor, anchor, now).is_none());
    }

    /// Cache.reset_period zeros usage + clears exhausted_at; this is
    /// the mutation `run_once` performs after computing `new_start`.
    #[test]
    fn cache_reset_period_zeros_usage_and_clears_exhausted() {
        let (_d, cache) = make_cache();
        let r = TrafficQuotaRow {
            user_id: "alice".into(),
            client_id: "edge-01".into(),
            client_name: "edge-01".into(),
            monthly_bytes: 100,
            billing_anchor: 0,
            current_period_started_at: 0,
            current_period_bytes_used: 200,
            exhausted_at: Some(1),
            created_at: 0,
            updated_at: 0,
        };
        cache.upsert(r).unwrap();
        let after = cache
            .reset_period("alice", "edge-01", 2_592_000, 2_592_010)
            .unwrap()
            .unwrap();
        assert_eq!(after.current_period_started_at, 2_592_000);
        assert_eq!(after.current_period_bytes_used, 0);
        assert!(after.exhausted_at.is_none());
        // Read-back through cache.get mirrors the post-reset row.
        let mirror = cache.get("alice", "edge-01").unwrap();
        assert_eq!(mirror.current_period_bytes_used, 0);
    }
}
