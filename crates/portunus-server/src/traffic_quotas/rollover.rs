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
    use crate::clients::ConnectedClients;
    use crate::store::Store;
    use crate::store::operator_store::SqliteOperatorStore;
    use crate::store::token_store::SqliteTokenStore;
    use crate::traffic_quotas::TrafficQuotaRow;
    use crate::traffic_quotas::cache::TrafficQuotaCache;
    use portunus_core::ClientName;
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    fn make_cache() -> (tempfile::TempDir, TrafficQuotaCache) {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).expect("open");
        (dir, TrafficQuotaCache::load(store).expect("cache"))
    }

    /// Build a full `AppState` backed by a temp SQLite store, mirroring
    /// the helper in `grpc/enrollment.rs`. `run_once` needs the real
    /// `AppState` (quota cache + connected-client registry + metrics).
    fn test_state() -> (tempfile::TempDir, AppState) {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&store)));
        let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&store)));
        operator_store
            .bootstrap_legacy_superadmin("test-token")
            .unwrap();
        let state = AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            None,
            7443,
            "deadbeef",
            include_str!("../advertised/testdata/san_fixture.pem"),
            16,
            store,
        )
        .unwrap();
        (dir, state)
    }

    /// Anchor at 2026-01-15 with `current_period_started_at == anchor` and
    /// `now` 32 days later: the row is due to advance one period.
    fn due_row(client_id: &str) -> (TrafficQuotaRow, DateTime<Utc>) {
        let anchor = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let now = anchor + chrono::Duration::days(32);
        let row = TrafficQuotaRow {
            user_id: "alice".into(),
            client_id: client_id.into(),
            client_name: "edge-display".into(),
            monthly_bytes: 1_000,
            billing_anchor: anchor.timestamp(),
            current_period_started_at: anchor.timestamp(),
            current_period_bytes_used: 750,
            exhausted_at: Some(anchor.timestamp()),
            created_at: anchor.timestamp(),
            updated_at: anchor.timestamp(),
        };
        (row, now)
    }

    /// Register a live client session and return its receiver so the test
    /// can observe (or drop) the pushed `TrafficQuotaUpdate`.
    async fn register_client(
        state: &AppState,
        client_id: ClientId,
    ) -> mpsc::Receiver<Result<portunus_proto::v1::ServerMessage, tonic::Status>> {
        let (tx, rx) = mpsc::channel(4);
        let name: ClientName = "edge-display".parse().unwrap();
        state
            .clients
            .register(
                client_id,
                name,
                None,
                CancellationToken::new(),
                tx,
                Arc::default(),
            )
            .await;
        rx
    }

    /// A due row whose live client is connected: `run_once` advances the
    /// period, zeroes usage, clears `exhausted_at`, and pushes a SET frame.
    #[tokio::test]
    async fn run_once_advances_due_row_and_pushes_to_live_client() {
        let (_d, state) = test_state();
        let client_id = ClientId::new();
        let (row, now) = due_row(&client_id.to_string());
        state.traffic_quotas.upsert(row).unwrap();
        let mut rx = register_client(&state, client_id).await;

        let advanced = run_once(&state, now).await.unwrap();
        assert_eq!(advanced, 1);

        // Cache row reflects the reset: usage zeroed, exhausted cleared,
        // period advanced to 2026-02-15.
        let after = state
            .traffic_quotas
            .get("alice", &client_id.to_string())
            .unwrap();
        assert_eq!(after.current_period_bytes_used, 0);
        assert!(after.exhausted_at.is_none());
        assert_eq!(
            after.current_period_started_at,
            Utc.with_ymd_and_hms(2026, 2, 15, 0, 0, 0)
                .unwrap()
                .timestamp()
        );

        // The live session received a TrafficQuotaUpdate SET frame.
        let pushed = rx.try_recv().expect("a quota frame was pushed");
        let msg = pushed.expect("frame is Ok");
        match msg.payload {
            Some(portunus_proto::v1::server_message::Payload::TrafficQuotaUpdate(u)) => {
                assert_eq!(u.action, portunus_proto::v1::TrafficQuotaAction::Set as i32);
                assert_eq!(u.user_id, "alice");
                assert_eq!(u.client_id, client_id.to_string());
            }
            other => panic!("expected TrafficQuotaUpdate, got {other:?}"),
        }
    }

    /// A due row with no live session still advances (push is a no-op when
    /// `handles` returns `None`).
    #[tokio::test]
    async fn run_once_advances_due_row_without_live_client() {
        let (_d, state) = test_state();
        let client_id = ClientId::new();
        let (row, now) = due_row(&client_id.to_string());
        state.traffic_quotas.upsert(row).unwrap();

        let advanced = run_once(&state, now).await.unwrap();
        assert_eq!(advanced, 1);
        let after = state
            .traffic_quotas
            .get("alice", &client_id.to_string())
            .unwrap();
        assert_eq!(after.current_period_bytes_used, 0);
    }

    /// A within-period row is skipped (no advance, no push).
    #[tokio::test]
    async fn run_once_skips_within_period_row() {
        let (_d, state) = test_state();
        let anchor = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let now = anchor + chrono::Duration::days(10);
        let row = TrafficQuotaRow {
            user_id: "alice".into(),
            client_id: "edge-01".into(),
            client_name: "edge-01".into(),
            monthly_bytes: 1_000,
            billing_anchor: anchor.timestamp(),
            current_period_started_at: anchor.timestamp(),
            current_period_bytes_used: 100,
            exhausted_at: None,
            created_at: 0,
            updated_at: 0,
        };
        state.traffic_quotas.upsert(row).unwrap();

        let advanced = run_once(&state, now).await.unwrap();
        assert_eq!(advanced, 0);
        // Untouched.
        let after = state.traffic_quotas.get("alice", "edge-01").unwrap();
        assert_eq!(after.current_period_bytes_used, 100);
    }

    /// An out-of-range `billing_anchor` cannot be mapped to a `DateTime`, so
    /// the row is skipped at the anchor guard.
    #[tokio::test]
    async fn run_once_skips_row_with_bad_billing_anchor() {
        let (_d, state) = test_state();
        let row = TrafficQuotaRow {
            user_id: "alice".into(),
            client_id: "edge-01".into(),
            client_name: "edge-01".into(),
            monthly_bytes: 1_000,
            billing_anchor: i64::MAX, // un-representable as a UTC instant
            current_period_started_at: 0,
            current_period_bytes_used: 10,
            exhausted_at: None,
            created_at: 0,
            updated_at: 0,
        };
        state.traffic_quotas.upsert(row).unwrap();
        let advanced = run_once(&state, Utc::now()).await.unwrap();
        assert_eq!(advanced, 0);
    }

    /// An out-of-range `current_period_started_at` is skipped at the start
    /// guard (the anchor parses, the start does not).
    #[tokio::test]
    async fn run_once_skips_row_with_bad_period_start() {
        let (_d, state) = test_state();
        let row = TrafficQuotaRow {
            user_id: "alice".into(),
            client_id: "edge-01".into(),
            client_name: "edge-01".into(),
            monthly_bytes: 1_000,
            billing_anchor: 0,
            current_period_started_at: i64::MAX,
            current_period_bytes_used: 10,
            exhausted_at: None,
            created_at: 0,
            updated_at: 0,
        };
        state.traffic_quotas.upsert(row).unwrap();
        let advanced = run_once(&state, Utc::now()).await.unwrap();
        assert_eq!(advanced, 0);
    }

    /// A due row present only in the in-memory cache (DB row deleted out
    /// from under it) hits the `reset_period -> None` branch: nothing is
    /// advanced.
    #[tokio::test]
    async fn run_once_skips_when_reset_period_returns_none() {
        let (_d, state) = test_state();
        let client_id = ClientId::new();
        let (row, now) = due_row(&client_id.to_string());
        state.traffic_quotas.upsert(row).unwrap();
        // Delete the persisted row directly, leaving the cache map stale.
        // The shared connection pool means this targets the same DB the
        // cache wrote through.
        crate::traffic_quotas::store::delete(&state.store, "alice", &client_id.to_string())
            .unwrap();

        let advanced = run_once(&state, now).await.unwrap();
        assert_eq!(advanced, 0);
    }

    /// `push_reset` early-returns when the row's `client_id` is not a valid
    /// ULID — no panic, no push.
    #[tokio::test]
    async fn push_reset_ignores_unparseable_client_id() {
        let (_d, state) = test_state();
        let row = TrafficQuotaRow {
            user_id: "alice".into(),
            client_id: "not-a-ulid".into(),
            client_name: "edge".into(),
            monthly_bytes: 1_000,
            billing_anchor: 0,
            current_period_started_at: 0,
            current_period_bytes_used: 0,
            exhausted_at: None,
            created_at: 0,
            updated_at: 0,
        };
        // Must not panic even though the id is unparseable.
        push_reset(&state, &row).await;
    }

    /// `push_reset` early-returns when the (valid) id has no live session.
    #[tokio::test]
    async fn push_reset_ignores_disconnected_client() {
        let (_d, state) = test_state();
        let client_id = ClientId::new();
        let (row, _now) = due_row(&client_id.to_string());
        // No `register` call: `handles` returns None, so nothing is sent.
        push_reset(&state, &row).await;
    }

    /// `push_reset` to a session whose receiver was dropped exercises the
    /// `send` error branch (the warn log) without panicking.
    #[tokio::test]
    async fn push_reset_handles_closed_receiver() {
        let (_d, state) = test_state();
        let client_id = ClientId::new();
        let (row, _now) = due_row(&client_id.to_string());
        let rx = register_client(&state, client_id).await;
        // Drop the receiver so the outbound channel is closed; the push
        // `send` then fails and takes the warn branch.
        drop(rx);
        push_reset(&state, &row).await;
    }

    /// A live client whose receiver is open receives the SET frame.
    #[tokio::test]
    async fn push_reset_delivers_to_live_client() {
        let (_d, state) = test_state();
        let client_id = ClientId::new();
        let (row, _now) = due_row(&client_id.to_string());
        let mut rx = register_client(&state, client_id).await;
        push_reset(&state, &row).await;
        let pushed = rx.try_recv().expect("frame delivered");
        assert!(pushed.is_ok());
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

    /// Exercise `run_forever`'s entry: spawn the long-lived task, let it
    /// be polled up to its first `sleep(TICK)`, then abort. We do not wait
    /// for `TICK` to elapse (that would be a 60s wall-clock dependence),
    /// so this only asserts the task starts cleanly and tears down on
    /// abort without panicking. A multi-thread runtime guarantees the
    /// spawned task is polled concurrently with the test body.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_forever_starts_and_aborts_cleanly() {
        let (_d, state) = test_state();
        let handle = tokio::spawn(run_forever(Arc::new(state)));
        // Yield so the spawned task is scheduled and reaches its first
        // `.await` point (registering the TICK timer).
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        handle.abort();
        let joined = handle.await;
        // Aborting a task surfaces as a cancelled `JoinError`, never a panic.
        assert!(joined.is_err());
        assert!(joined.unwrap_err().is_cancelled());
    }
}
