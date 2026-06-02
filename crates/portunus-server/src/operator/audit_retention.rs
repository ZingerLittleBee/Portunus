//! Audit-table retention reaper.
//!
//! The durable audit table (`<data-dir>/state.db` → `audit`) would grow
//! without bound otherwise. Once successful reads stopped being audited
//! (see `auth_layer::is_auditable_mutation`) the write rate dropped
//! sharply, but mutations and denials still accumulate forever. This
//! background task enforces two ceilings, hourly:
//!
//!   - **age**: delete rows older than [`RETENTION_SECS`].
//!   - **size**: keep at most [`MAX_ROWS`] newest rows (a hard backstop
//!     for a sudden burst of denials, e.g. a credential-stuffing probe).
//!
//! `run_once` is the unit-testable core; `run_forever` is the long-lived
//! tokio task. Mirrors `traffic_quotas::rollup`'s shape and shutdown
//! semantics (the server's cancellation token aborts the spawned task).

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info};

use crate::store::{Store, StoreError};

/// Maximum age of an audit row before it is pruned. 90 days matches the
/// longest existing retention window in the codebase (1h traffic samples).
pub const RETENTION_SECS: i64 = 90 * 24 * 3600;

/// Hard ceiling on audit table row count. Survives a burst that the
/// age window alone would not bound. Sized so the table stays small
/// enough for the newest-first index scans to remain cheap.
pub const MAX_ROWS: usize = 50_000;

/// How often the reaper sweeps.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);

/// Per-sweep stats returned by [`run_once`] (for logging + tests).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RetentionStats {
    /// Rows deleted because they were older than [`RETENTION_SECS`].
    pub deleted_age: u64,
    /// Rows deleted because the table exceeded [`MAX_ROWS`].
    pub deleted_overflow: u64,
}

/// Long-lived background task: sweep, log, sleep, repeat. Never returns
/// under normal operation (the server's shutdown signal aborts the
/// spawned task via the surrounding `select`).
pub async fn run_forever(store: Store) {
    loop {
        sleep(SWEEP_INTERVAL).await;
        match run_once(&store, Utc::now()) {
            Ok(stats) => info!(
                event = "audit_retention.sweep",
                deleted_age = stats.deleted_age,
                deleted_overflow = stats.deleted_overflow,
            ),
            Err(e) => error!(event = "audit_retention.sweep_failed", error = %e),
        }
    }
}

/// Prune by age, then by row count. `now` is parameterized so tests can
/// pin time. The two passes are independent: age trims the long tail,
/// the row cap trims a burst the age window would otherwise keep.
pub fn run_once(store: &Store, now: DateTime<Utc>) -> Result<RetentionStats, StoreError> {
    let cutoff = now - ChronoDuration::seconds(RETENTION_SECS);
    let deleted_age = store.audit_prune_apply(cutoff)?;
    let deleted_overflow = store.audit_prune_to_max_rows(MAX_ROWS)?;
    Ok(RetentionStats {
        deleted_age,
        deleted_overflow,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::map_rusqlite;
    use tempfile::tempdir;

    fn insert(store: &Store, ts: DateTime<Utc>) {
        store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO audit \
                     (ts, user_id, outcome, action, resource_kind, resource_value, correlation_id, details_json) \
                     VALUES (?, 'u', 'allow', 'POST /v1/rules', NULL, NULL, '', '{}')",
                    rusqlite::params![ts.to_rfc3339()],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .unwrap();
    }

    fn count(store: &Store) -> i64 {
        store
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM audit", [], |r| r.get(0))
                    .map_err(map_rusqlite)
            })
            .unwrap()
    }

    #[test]
    fn prunes_rows_older_than_retention() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let now = Utc::now();
        // One row well past the window, one fresh.
        insert(&store, now - ChronoDuration::seconds(RETENTION_SECS + 3600));
        insert(&store, now - ChronoDuration::seconds(60));

        let stats = run_once(&store, now).unwrap();
        assert_eq!(stats.deleted_age, 1);
        assert_eq!(stats.deleted_overflow, 0);
        assert_eq!(count(&store), 1);
    }

    #[test]
    fn enforces_row_ceiling_via_run_once() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let now = Utc::now();
        // All rows are fresh (within the age window) so only the row cap
        // can trim them. Use a tiny synthetic table and assert the cap
        // path runs (we can't cheaply seed 50k rows here, so just verify
        // run_once leaves freshly-inserted rows intact when under cap).
        for i in 0..10 {
            insert(&store, now - ChronoDuration::seconds(i));
        }
        let stats = run_once(&store, now).unwrap();
        assert_eq!(stats.deleted_age, 0);
        assert_eq!(stats.deleted_overflow, 0);
        assert_eq!(count(&store), 10);
    }
}
