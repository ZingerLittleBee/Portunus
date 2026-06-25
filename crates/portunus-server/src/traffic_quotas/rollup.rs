//! 013-traffic-quotas: hourly rollup background task.
//!
//! Every hour at H+1 minute, rolls up any closed-hour windows from
//! `traffic_samples_1m` into `traffic_samples_1h` (idempotent — re-runs
//! produce the same row), advances the `traffic_rollup_state.last_rolled_up_hour`
//! watermark, then prunes:
//!   - 1m samples older than 7 days
//!   - 1h samples older than 90 days
//!
//! `run_once` is the unit-testable core; `run_forever` is the long-lived
//! tokio task that sleeps until the next H+1m boundary.

use crate::store::{Store, StoreError};
use crate::traffic_quotas::samples;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info};

pub(crate) const HOUR: i64 = 3600;
pub(crate) const RETENTION_1M_SECS: i64 = 7 * 24 * HOUR;
pub(crate) const RETENTION_1H_SECS: i64 = 90 * 24 * HOUR;

/// Per-tick stats returned by `run_once` (for logging + tests).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RollupStats {
    pub rolled_up_hours: usize,
    pub deleted_1m: usize,
    pub deleted_1h: usize,
}

/// Long-lived background task: sleep until the next H+1 minute boundary,
/// invoke `run_once`, log the stats, repeat. Never returns under normal
/// operation (the server's shutdown signal aborts the spawned task).
pub async fn run_forever(store: Store) {
    loop {
        let now = now_unix_sec();
        let into_hour = now.rem_euclid(HOUR);
        let sleep_secs: u64 = if into_hour < 60 {
            u64::try_from(60 - into_hour).unwrap_or(60)
        } else {
            u64::try_from(HOUR - into_hour + 60).unwrap_or(60)
        };
        sleep(Duration::from_secs(sleep_secs)).await;

        match run_once(&store, now_unix_sec()) {
            Ok(stats) => info!(
                event = "traffic_rollup.tick",
                rolled_up = stats.rolled_up_hours,
                deleted_1m = stats.deleted_1m,
                deleted_1h = stats.deleted_1h,
            ),
            Err(e) => error!(event = "traffic_rollup.tick_failed", error = %e),
        }
    }
}

/// Roll up every closed hour since `last_rolled_up_hour` and prune old
/// rows. `now_unix_sec` is parameterized so tests can pin time.
///
/// On a fresh DB (watermark = 0) we start at `now_hour - 1h` so we
/// don't try to backfill all of history; older rows would be pruned by
/// the retention sweep anyway.
#[allow(
    clippy::similar_names,
    reason = "deleted_1m / deleted_1h are intentional parallel naming"
)]
pub fn run_once(store: &Store, now_unix_sec: i64) -> Result<RollupStats, StoreError> {
    let now_hour = now_unix_sec - now_unix_sec.rem_euclid(HOUR);

    let last = samples::get_last_rolled_up_hour(store)?;
    let mut next = if last == 0 {
        now_hour - HOUR
    } else {
        last + HOUR
    };
    let mut rolled = 0usize;
    while next < now_hour {
        samples::rollup_hour(store, next)?;
        samples::set_last_rolled_up_hour(store, next)?;
        rolled += 1;
        next += HOUR;
        // Sanity bound: never roll more than 90 days of hours in one
        // tick. Anything older than that gets pruned anyway.
        if rolled > 24 * 90 {
            break;
        }
    }

    let deleted_1m = samples::delete_1m_older_than(store, now_unix_sec - RETENTION_1M_SECS)?;
    let deleted_1h = samples::delete_1h_older_than(store, now_unix_sec - RETENTION_1H_SECS)?;
    Ok(RollupStats {
        rolled_up_hours: rolled,
        deleted_1m,
        deleted_1h,
    })
}

fn now_unix_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use tempfile::tempdir;

    fn make_store() -> (tempfile::TempDir, Store) {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).expect("open");
        (dir, store)
    }

    fn insert_minute(
        store: &Store,
        user: &str,
        client: &str,
        ts_minute: i64,
        b_in: i64,
        b_out: i64,
    ) {
        samples::upsert_1m_delta(store, user, client, client, ts_minute, b_in, b_out).unwrap();
    }

    #[test]
    fn run_once_rolls_up_pending_hours() {
        let (_d, store) = make_store();
        // now = 2026-06-15 04:30:00 UTC == 1781325000
        let now: i64 = 1_781_325_000;
        let now_hour = now - now.rem_euclid(HOUR);
        // Insert 1m rows in hours H-3, H-2, H-1.
        for h in 1..=3 {
            let hour_ts = now_hour - i64::from(h) * HOUR;
            for m in 0..3 {
                insert_minute(&store, "alice", "edge-01", hour_ts + m * 60, 10, 20);
            }
        }
        // Set watermark so the loop starts at H-3.
        samples::set_last_rolled_up_hour(&store, now_hour - 4 * HOUR).unwrap();

        let stats = run_once(&store, now).unwrap();
        assert_eq!(stats.rolled_up_hours, 3);

        for h in 1..=3 {
            let hour_ts = now_hour - i64::from(h) * HOUR;
            let rows = samples::query_samples(
                &store,
                samples::SampleBucket::H1,
                Some("alice"),
                Some("edge-01"),
                hour_ts,
                hour_ts + 1,
            )
            .unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].bytes_in, 30);
            assert_eq!(rows[0].bytes_out, 60);
        }

        assert_eq!(
            samples::get_last_rolled_up_hour(&store).unwrap(),
            now_hour - HOUR
        );
    }

    #[test]
    fn run_once_is_idempotent() {
        let (_d, store) = make_store();
        let now: i64 = 1_781_325_000;
        let now_hour = now - now.rem_euclid(HOUR);
        insert_minute(&store, "alice", "edge-01", now_hour - HOUR, 100, 200);
        samples::set_last_rolled_up_hour(&store, now_hour - 2 * HOUR).unwrap();

        let first = run_once(&store, now).unwrap();
        assert_eq!(first.rolled_up_hours, 1);

        let second = run_once(&store, now).unwrap();
        assert_eq!(second.rolled_up_hours, 0);
    }

    #[test]
    fn run_once_handles_fresh_db_starts_one_hour_back() {
        let (_d, store) = make_store();
        let now: i64 = 1_781_325_000;
        let now_hour = now - now.rem_euclid(HOUR);
        // Watermark = 0 (default). Insert a row in H-1 so rollup picks it up.
        insert_minute(&store, "alice", "edge-01", now_hour - HOUR, 5, 7);

        let stats = run_once(&store, now).unwrap();
        assert_eq!(stats.rolled_up_hours, 1);
        assert_eq!(
            samples::get_last_rolled_up_hour(&store).unwrap(),
            now_hour - HOUR
        );
    }

    #[test]
    fn retention_prunes_old_rows() {
        let (_d, store) = make_store();
        let now: i64 = 2_000_000_000;
        // Old 1m row: 8 days back.
        let old_1m = now - 8 * 24 * HOUR;
        insert_minute(&store, "alice", "edge-01", old_1m, 1, 1);
        // Fresh 1m row: 1 day back.
        let fresh_1m = now - 24 * HOUR;
        insert_minute(&store, "alice", "edge-01", fresh_1m, 1, 1);
        // Old 1h row: 91 days back — write via rollup of a synthetic minute.
        let old_hour = now - 91 * 24 * HOUR;
        let old_hour = old_hour - old_hour.rem_euclid(HOUR);
        insert_minute(&store, "alice", "edge-01", old_hour, 1, 1);
        samples::rollup_hour(&store, old_hour).unwrap();

        // Make sure rollup doesn't try to backfill: set watermark to current.
        let now_hour = now - now.rem_euclid(HOUR);
        samples::set_last_rolled_up_hour(&store, now_hour).unwrap();

        let stats = run_once(&store, now).unwrap();
        // The synthetic 1m at `old_hour` is well past the 7d window, so it
        // gets pruned along with the explicit old 1m row.
        assert!(stats.deleted_1m >= 2);
        assert!(stats.deleted_1h >= 1);

        // Fresh minute survives.
        let fresh = samples::query_samples(
            &store,
            samples::SampleBucket::M1,
            Some("alice"),
            Some("edge-01"),
            fresh_1m,
            fresh_1m + 1,
        )
        .unwrap();
        assert_eq!(fresh.len(), 1);
    }

    /// Run a raw DDL/DML statement against the live connection. Used by the
    /// error-path tests below to break specific tables so the `?` operators
    /// in `run_once` propagate a `StoreError`.
    fn exec_sql(store: &Store, sql: &str) {
        store
            .with_conn(|c| {
                c.execute_batch(sql).map_err(crate::store::map_rusqlite)?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn run_once_errors_when_rollup_state_table_missing() {
        let (_d, store) = make_store();
        // Drop the watermark table so `get_last_rolled_up_hour` fails.
        exec_sql(&store, "DROP TABLE traffic_rollup_state");
        let Err(e) = run_once(&store, 1_781_325_000) else {
            panic!("expected error when traffic_rollup_state is missing");
        };
        // SQLite reports a missing table; the exact variant is an Internal
        // mapping — assert only that it surfaces as an error display.
        assert!(!e.to_string().is_empty());
    }

    #[test]
    fn run_once_errors_when_rollup_source_table_missing() {
        let (_d, store) = make_store();
        let now: i64 = 1_781_325_000;
        let now_hour = now - now.rem_euclid(HOUR);
        // Watermark one hour back so the loop runs exactly once and reaches
        // `rollup_hour` (which SELECTs from traffic_samples_1m).
        samples::set_last_rolled_up_hour(&store, now_hour - 2 * HOUR).unwrap();
        exec_sql(&store, "DROP TABLE traffic_samples_1m");
        let Err(e) = run_once(&store, now) else {
            panic!("expected error when traffic_samples_1m is missing");
        };
        assert!(!e.to_string().is_empty());
    }

    #[test]
    fn run_once_errors_when_setting_watermark_fails() {
        let (_d, store) = make_store();
        let now: i64 = 1_781_325_000;
        let now_hour = now - now.rem_euclid(HOUR);
        // Loop runs once: `rollup_hour` succeeds, then `set_last_rolled_up_hour`
        // UPDATE is blocked by a trigger so the `?` on that call fires.
        samples::set_last_rolled_up_hour(&store, now_hour - 2 * HOUR).unwrap();
        exec_sql(
            &store,
            "CREATE TRIGGER block_rollup_update BEFORE UPDATE ON traffic_rollup_state \
             BEGIN SELECT RAISE(ABORT, 'blocked'); END",
        );
        let Err(e) = run_once(&store, now) else {
            panic!("expected error when set_last_rolled_up_hour is blocked");
        };
        assert!(!e.to_string().is_empty());
    }

    #[test]
    fn run_once_errors_when_delete_1m_table_missing() {
        let (_d, store) = make_store();
        let now: i64 = 1_781_325_000;
        let now_hour = now - now.rem_euclid(HOUR);
        // Watermark at current hour so the rollup loop is skipped; the failure
        // surfaces at the `delete_1m_older_than` prune step.
        samples::set_last_rolled_up_hour(&store, now_hour).unwrap();
        exec_sql(&store, "DROP TABLE traffic_samples_1m");
        let Err(e) = run_once(&store, now) else {
            panic!("expected error when traffic_samples_1m prune target is missing");
        };
        assert!(!e.to_string().is_empty());
    }

    #[test]
    fn run_once_errors_when_delete_1h_table_missing() {
        let (_d, store) = make_store();
        let now: i64 = 1_781_325_000;
        let now_hour = now - now.rem_euclid(HOUR);
        // Skip the loop; keep traffic_samples_1m so the 1m prune succeeds, then
        // drop traffic_samples_1h so the 1h prune `?` propagates.
        samples::set_last_rolled_up_hour(&store, now_hour).unwrap();
        exec_sql(&store, "DROP TABLE traffic_samples_1h");
        let Err(e) = run_once(&store, now) else {
            panic!("expected error when traffic_samples_1h prune target is missing");
        };
        assert!(!e.to_string().is_empty());
    }

    #[test]
    fn run_once_caps_rollup_at_sanity_bound() {
        let (_d, store) = make_store();
        let now: i64 = 1_781_325_000;
        let now_hour = now - now.rem_euclid(HOUR);
        // Watermark far enough back that the loop would roll more than the
        // 24*90 = 2160 hour sanity bound, forcing the `break`.
        samples::set_last_rolled_up_hour(&store, now_hour - 2_200 * HOUR).unwrap();
        let stats = run_once(&store, now).unwrap();
        // The loop increments `rolled`, then breaks once it exceeds 2160, so
        // exactly 2161 hours are rolled before bailing out.
        assert_eq!(stats.rolled_up_hours, 2161);
        // Watermark advanced to the last hour rolled before the break.
        assert_eq!(
            samples::get_last_rolled_up_hour(&store).unwrap(),
            now_hour - 2_200 * HOUR + 2_161 * HOUR
        );
    }

    #[test]
    fn now_unix_sec_returns_positive_recent_timestamp() {
        let now = now_unix_sec();
        // Sanity: well after 2020-01-01 (1_577_836_800) and a real i64.
        assert!(now > 1_577_836_800);
    }

    #[tokio::test(start_paused = true)]
    async fn run_forever_ticks_then_aborts_on_success() {
        let (_d, store) = make_store();
        // Seed an H-1 row so the first tick actually rolls something up; the
        // store stays valid so the `Ok(stats)` arm logs the tick.
        let now = now_unix_sec();
        let now_hour = now - now.rem_euclid(HOUR);
        insert_minute(&store, "alice", "edge-01", now_hour - HOUR, 5, 7);

        let handle = tokio::spawn(run_forever(store));
        // Virtual time auto-advances while the task sleeps; this drives at
        // least one full sleep -> run_once -> log cycle before we abort.
        sleep(Duration::from_secs(7_200)).await;
        handle.abort();
        let joined = handle.await;
        assert!(joined.is_err(), "aborted task resolves to a JoinError");
    }

    #[tokio::test(start_paused = true)]
    async fn run_forever_logs_error_arm_when_run_once_fails() {
        let (_d, store) = make_store();
        // Break the store so every `run_once` call returns an error, exercising
        // the `Err(e)` logging arm of the loop.
        exec_sql(&store, "DROP TABLE traffic_rollup_state");

        let handle = tokio::spawn(run_forever(store));
        sleep(Duration::from_secs(7_200)).await;
        handle.abort();
        let joined = handle.await;
        assert!(joined.is_err(), "aborted task resolves to a JoinError");
    }
}
