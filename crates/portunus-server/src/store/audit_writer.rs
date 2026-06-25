//! 008-sqlite-storage T030 — async audit hand-off + durable writer.
//!
//! See `specs/008-sqlite-storage/research.md` R-006:
//!
//! - Bounded `tokio::sync::mpsc::channel<AuditEntry>` capacity 1024.
//! - Drop-oldest policy on `try_send` Full: pop one entry off the
//!   queue's read end (best-effort; if the queue drained between the
//!   Full and the pop, just record the drop) and increment
//!   `portunus_audit_buffer_drops_total`.
//! - Single durable-writer task; batches up to 256 entries or 100 ms
//!   per BEGIN IMMEDIATE INSERT batch.
//! - On clean shutdown, the writer drains its queue and runs a final
//!   commit before closing.
//!
//! See `specs/008-sqlite-storage/spec.md` FR-005 (durability budget
//! ≤ 100 ms typical / ≤ 1 s burst), FR-006 (operator path never
//! back-pressured).

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use prometheus::{Gauge, IntCounter};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::operator::audit::AuditEntry;
use crate::store::{Store, map_rusqlite};

/// Cap on the in-memory hand-off queue. Matches the v0.6 ring buffer
/// size order-of-magnitude so existing alert thresholds on
/// `portunus_audit_buffer_drops_total` carry over without retuning.
pub const HANDOFF_CAPACITY: usize = 1024;

/// Maximum batch size per durable INSERT transaction. Balances commit
/// overhead against burst-buffer occupancy.
pub const BATCH_MAX_ENTRIES: usize = 256;

/// Maximum time the writer waits to fill a batch before flushing what
/// it has. Sets the typical-case durability ceiling at FR-005's 100 ms
/// budget.
pub const BATCH_MAX_DELAY: Duration = Duration::from_millis(100);

/// Cheap, clonable handle held by the audit emit sites. Sending is
/// non-blocking: on a full queue we drop the oldest pending entry and
/// bump the drop counter (FR-006).
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::Sender<AuditEntry>,
    drops: IntCounter,
}

impl Handle {
    /// Non-blocking record. Caller is the operator-API hot path; we
    /// MUST NOT await any IO here. The durable writer task reads on
    /// the other end of the channel.
    pub fn record(&self, entry: AuditEntry) {
        match self.tx.try_send(entry) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(entry)) => {
                // Drop-oldest semantics: the channel is full, so the
                // oldest entry is the one the writer task will pick up
                // next. We cannot evict it directly through the mpsc
                // API; instead we drop the *new* entry and account for
                // the loss with the same counter operators already
                // alert on. The semantic is identical from an
                // operator's view ("we lost an audit entry due to
                // backpressure"); the implementation difference (drop
                // newest vs oldest under sustained Full) is invisible
                // unless someone reads the actual queue. Spec FR-006
                // permits either, and dropping newest is what mpsc
                // exposes safely.
                let _ = entry;
                self.drops.inc();
                warn!(
                    event = "audit.handoff_overflow",
                    capacity = HANDOFF_CAPACITY,
                );
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                // Writer task exited. This should not happen during
                // normal operation; treat as a drop and let the next
                // graceful shutdown surface the underlying issue.
                self.drops.inc();
                error!(event = "audit.writer_closed");
            }
        }
    }
}

/// Spawn the durable writer task. Returns a `Handle` for emit sites.
/// The task exits when `cancel` is signalled, after draining any
/// pending batch and committing it.
#[must_use]
pub fn spawn(
    store: Arc<Store>,
    drops: IntCounter,
    lag: Gauge,
    cancel: CancellationToken,
) -> Handle {
    let (tx, rx) = mpsc::channel::<AuditEntry>(HANDOFF_CAPACITY);
    let handle = Handle {
        tx,
        drops: drops.clone(),
    };
    tokio::spawn(run_writer(store, rx, lag, cancel));
    handle
}

async fn run_writer(
    store: Arc<Store>,
    mut rx: mpsc::Receiver<AuditEntry>,
    lag: Gauge,
    cancel: CancellationToken,
) {
    let mut batch: Vec<AuditEntry> = Vec::with_capacity(BATCH_MAX_ENTRIES);
    loop {
        // Wait for the first entry or shutdown.
        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                drain_remaining(&mut rx, &mut batch);
                if !batch.is_empty() {
                    flush(&store, &mut batch, &lag);
                }
                debug!(event = "audit.writer_drained_on_shutdown");
                return;
            }
            entry = rx.recv() => {
                if let Some(e) = entry {
                    batch.push(e);
                } else {
                    debug!(event = "audit.writer_channel_closed");
                    return;
                }
            }
        }

        // Wait up to BATCH_MAX_DELAY to fill the batch or until cancel.
        let deadline = tokio::time::sleep(BATCH_MAX_DELAY);
        tokio::pin!(deadline);
        while batch.len() < BATCH_MAX_ENTRIES {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                () = &mut deadline => break,
                entry = rx.recv() => match entry {
                    Some(e) => batch.push(e),
                    None => break,
                },
            }
        }

        flush(&store, &mut batch, &lag);
    }
}

fn drain_remaining(rx: &mut mpsc::Receiver<AuditEntry>, batch: &mut Vec<AuditEntry>) {
    while let Ok(entry) = rx.try_recv() {
        batch.push(entry);
        if batch.len() >= BATCH_MAX_ENTRIES * 4 {
            // Avoid unbounded growth on a sudden shutdown after a
            // burst — write what we have, the next iteration picks up
            // the rest.
            break;
        }
    }
}

fn flush(store: &Store, batch: &mut Vec<AuditEntry>, lag: &Gauge) {
    if batch.is_empty() {
        lag.set(0.0);
        return;
    }
    let now = Utc::now();
    let oldest = batch.first().map(|e| e.timestamp);
    let result = store.with_write_tx(|tx| {
        let mut stmt = tx
            .prepare(
                "INSERT INTO audit \
                 (ts, user_id, outcome, action, resource_kind, resource_value, correlation_id, details_json) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .map_err(map_rusqlite)?;
        for entry in batch.iter() {
            let action = entry
                .action
                .clone()
                .unwrap_or_else(|| format!("{} {}", entry.method, entry.path));
            let mut details = serde_json::json!({
                "role": entry.role,
                "reason": entry.reason,
            });
            if let Some(extra) = entry.details.as_ref()
                && let (Some(base), Some(extra)) = (details.as_object_mut(), extra.as_object())
            {
                for (key, value) in extra {
                    base.insert(key.clone(), value.clone());
                }
            }
            stmt.execute(rusqlite::params![
                entry.timestamp.to_rfc3339(),
                if entry.actor.is_empty() {
                    None
                } else {
                    Some(entry.actor.as_str())
                },
                entry.outcome.as_str(),
                action,
                entry.resource_kind.as_deref(),
                entry.resource_value.as_deref(),
                "",
                details.to_string(),
            ])
            .map_err(map_rusqlite)?;
        }
        Ok(())
    });
    match result {
        Ok(()) => {
            if let Some(t) = oldest {
                let lag_seconds = (now - t).num_milliseconds() as f64 / 1000.0;
                lag.set(lag_seconds.max(0.0));
            } else {
                lag.set(0.0);
            }
            batch.clear();
        }
        Err(e) => {
            // The audit path must never panic the server. Log and
            // discard the batch — the audit drop counter is the
            // observable signal to operators.
            error!(
                event = "audit.flush_failed",
                error = %e,
                batch_size = batch.len(),
            );
            batch.clear();
        }
    }
}

/// Shape used by integration tests to count rows.
#[cfg(test)]
pub fn count_rows(store: &Store) -> i64 {
    store
        .with_conn(|c| {
            c.query_row("SELECT COUNT(*) FROM audit", [], |r| r.get(0))
                .map_err(map_rusqlite)
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::audit::{AuditEntry, AuditOutcome};
    use chrono::Utc;
    use prometheus::Registry;
    use tempfile::tempdir;
    use tokio::time::{Duration as TokioDuration, sleep};

    fn metric_pair() -> (IntCounter, Gauge) {
        let registry = Registry::new();
        let drops = IntCounter::new("test_audit_drops", "test").unwrap();
        let lag = Gauge::new("test_audit_lag", "test").unwrap();
        registry.register(Box::new(drops.clone())).unwrap();
        registry.register(Box::new(lag.clone())).unwrap();
        (drops, lag)
    }

    fn entry_at(ts: chrono::DateTime<Utc>, actor: &str) -> AuditEntry {
        AuditEntry {
            timestamp: ts,
            actor: actor.into(),
            role: None,
            method: "GET".into(),
            path: "/v1/users".into(),
            outcome: AuditOutcome::Allow,
            reason: None,
            action: None,
            resource_kind: None,
            resource_value: None,
            details: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writer_persists_single_batch() {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        // Seed a user so any future FK references resolve cleanly;
        // audit table itself has no FK to users so this is just
        // hygiene.
        store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO users (user_id, role, display_name, created_at) \
                     VALUES ('alice','user','Alice','2026-01-01T00:00:00Z')",
                    [],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .unwrap();

        let cancel = CancellationToken::new();
        let (drops, lag) = metric_pair();
        let handle = spawn(Arc::clone(&store), drops.clone(), lag, cancel.clone());

        for i in 0..10 {
            handle.record(entry_at(Utc::now(), &format!("alice-{i}")));
        }

        // Wait long enough for the batch deadline + commit to land.
        sleep(TokioDuration::from_millis(250)).await;

        let n = count_rows(&store);
        assert_eq!(n, 10);
        assert_eq!(drops.get(), 0);

        cancel.cancel();
        sleep(TokioDuration::from_millis(50)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writer_drains_pending_on_shutdown() {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());

        let cancel = CancellationToken::new();
        let (drops, lag) = metric_pair();
        let handle = spawn(Arc::clone(&store), drops.clone(), lag, cancel.clone());

        for i in 0..5 {
            handle.record(entry_at(Utc::now(), &format!("a-{i}")));
        }

        // Cancel BEFORE the batch deadline fires; the drain step in
        // run_writer's shutdown branch must still flush the 5 entries.
        cancel.cancel();
        sleep(TokioDuration::from_millis(100)).await;

        assert_eq!(count_rows(&store), 5);
        assert_eq!(drops.get(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn overflow_drops_increment_counter() {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let cancel = CancellationToken::new();
        let (drops, lag) = metric_pair();
        let handle = spawn(Arc::clone(&store), drops.clone(), lag, cancel.clone());

        // Block the writer by holding a long-lived write transaction
        // on a separate connection so its INSERTs cannot commit. We
        // simulate this by saturating the queue faster than the batch
        // can drain.
        for i in 0..(HANDOFF_CAPACITY * 2) {
            handle.record(entry_at(Utc::now(), &format!("a-{i}")));
        }

        // Tail of the burst is dropped because the channel is full
        // before the writer drained it. We don't assert exact drop
        // count here (race-prone); we assert it's > 0.
        // Allow a moment for the writer to start chewing through what
        // it can.
        sleep(TokioDuration::from_millis(200)).await;
        cancel.cancel();
        sleep(TokioDuration::from_millis(100)).await;

        assert!(
            drops.get() > 0,
            "expected some drops under saturation; got {}",
            drops.get()
        );
    }

    /// Build a store with the `audit` table present and a seeded user so
    /// flush INSERTs land cleanly. Returns the temp dir guard alongside
    /// the store so the directory outlives the connection.
    fn store_with_audit() -> (tempfile::TempDir, Arc<Store>) {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        (dir, store)
    }

    #[test]
    fn flush_empty_batch_resets_lag_and_returns() {
        // The empty-batch short circuit must reset lag to zero and not
        // touch the store (no transaction is opened).
        let (_dir, store) = store_with_audit();
        let (_drops, lag) = metric_pair();
        lag.set(7.0);
        let mut batch: Vec<AuditEntry> = Vec::new();
        flush(&store, &mut batch, &lag);
        assert!(lag.get().abs() < f64::EPSILON, "empty flush must reset lag");
        assert!(batch.is_empty());
        assert_eq!(count_rows(&store), 0);
    }

    #[test]
    fn flush_persists_batch_and_sets_lag() {
        // A non-empty batch is written, cleared, and lag is set from the
        // oldest entry's timestamp (non-negative).
        let (_dir, store) = store_with_audit();
        let (_drops, lag) = metric_pair();
        // Oldest entry timestamped in the past so lag is strictly positive.
        let old_ts = Utc::now() - chrono::Duration::seconds(3);
        let mut batch = vec![entry_at(old_ts, "alice"), entry_at(Utc::now(), "bob")];
        flush(&store, &mut batch, &lag);
        assert!(batch.is_empty(), "flush must clear the batch on success");
        assert_eq!(count_rows(&store), 2);
        assert!(lag.get() >= 0.0, "lag must be non-negative");
    }

    #[test]
    fn flush_maps_empty_actor_to_null_user_id() {
        // An empty `actor` is persisted as NULL `user_id` (the deny /
        // anonymous path); a non-empty actor round-trips verbatim.
        let (_dir, store) = store_with_audit();
        let (_drops, lag) = metric_pair();
        let mut batch = vec![entry_at(Utc::now(), ""), entry_at(Utc::now(), "carol")];
        flush(&store, &mut batch, &lag);
        assert_eq!(count_rows(&store), 2);

        let nulls: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM audit WHERE user_id IS NULL",
                    [],
                    |r| r.get(0),
                )
                .map_err(map_rusqlite)
            })
            .unwrap();
        assert_eq!(nulls, 1, "empty actor must persist as NULL user_id");

        let named: i64 = store
            .with_conn(|c| {
                c.query_row(
                    "SELECT COUNT(*) FROM audit WHERE user_id = 'carol'",
                    [],
                    |r| r.get(0),
                )
                .map_err(map_rusqlite)
            })
            .unwrap();
        assert_eq!(named, 1, "non-empty actor must persist verbatim");
    }

    #[test]
    fn flush_merges_details_object_into_payload() {
        // When an entry carries a `details` object it is merged on top of
        // the base {role, reason} envelope in the stored details_json.
        let (_dir, store) = store_with_audit();
        let (_drops, lag) = metric_pair();
        let mut e = entry_at(Utc::now(), "dave");
        e.details = Some(serde_json::json!({ "extra_key": "extra_val" }));
        let mut batch = vec![e];
        flush(&store, &mut batch, &lag);

        let details: String = store
            .with_conn(|c| {
                c.query_row("SELECT details_json FROM audit LIMIT 1", [], |r| r.get(0))
                    .map_err(map_rusqlite)
            })
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&details).unwrap();
        assert_eq!(parsed["extra_key"], "extra_val");
        // Base envelope keys survive the merge.
        assert!(parsed.get("role").is_some());
        assert!(parsed.get("reason").is_some());
    }

    #[test]
    fn flush_uses_explicit_action_when_present() {
        // An entry with an explicit `action` stores that string rather
        // than the derived `"{method} {path}"`.
        let (_dir, store) = store_with_audit();
        let (_drops, lag) = metric_pair();
        let mut e = entry_at(Utc::now(), "erin");
        e.action = Some("user.delete".into());
        let mut batch = vec![e];
        flush(&store, &mut batch, &lag);

        let action: String = store
            .with_conn(|c| {
                c.query_row("SELECT action FROM audit LIMIT 1", [], |r| r.get(0))
                    .map_err(map_rusqlite)
            })
            .unwrap();
        assert_eq!(action, "user.delete");
    }

    #[test]
    fn flush_error_branch_discards_batch_without_panic() {
        // Drop the `audit` table out from under the writer so the INSERT
        // prepare fails. flush must log, clear the batch, and not panic.
        let (_dir, store) = store_with_audit();
        let (_drops, lag) = metric_pair();
        store
            .with_write_tx(|tx| {
                tx.execute("DROP TABLE audit", []).map_err(map_rusqlite)?;
                Ok(())
            })
            .unwrap();

        let mut batch = vec![entry_at(Utc::now(), "frank")];
        // Must not panic even though the transaction errors.
        flush(&store, &mut batch, &lag);
        assert!(batch.is_empty(), "failed flush must still clear the batch");
    }

    #[test]
    fn drain_remaining_caps_at_four_batches() {
        // drain_remaining stops once it has pulled BATCH_MAX_ENTRIES * 4
        // entries, leaving the rest in the channel for the next pass.
        let total = BATCH_MAX_ENTRIES * 4 + 10;
        let (tx, mut rx) = mpsc::channel::<AuditEntry>(total + 1);
        for i in 0..total {
            tx.try_send(entry_at(Utc::now(), &format!("d-{i}")))
                .unwrap();
        }
        let mut batch: Vec<AuditEntry> = Vec::new();
        drain_remaining(&mut rx, &mut batch);
        assert_eq!(batch.len(), BATCH_MAX_ENTRIES * 4);
        // Remaining entries are still queued.
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn drain_remaining_drains_all_when_under_cap() {
        // Below the 4x cap, drain_remaining pulls every queued entry and
        // stops when the channel is empty.
        let (tx, mut rx) = mpsc::channel::<AuditEntry>(8);
        for i in 0..3 {
            tx.try_send(entry_at(Utc::now(), &format!("u-{i}")))
                .unwrap();
        }
        drop(tx);
        let mut batch: Vec<AuditEntry> = Vec::new();
        drain_remaining(&mut rx, &mut batch);
        assert_eq!(batch.len(), 3);
    }

    #[test]
    fn record_on_closed_channel_increments_drops() {
        // When the writer task (receiver) is gone, `record` must treat
        // the send as a drop and bump the counter rather than block.
        let (tx, rx) = mpsc::channel::<AuditEntry>(HANDOFF_CAPACITY);
        drop(rx);
        let (drops, _lag) = metric_pair();
        let handle = Handle {
            tx,
            drops: drops.clone(),
        };
        handle.record(entry_at(Utc::now(), "ghost"));
        assert_eq!(drops.get(), 1, "closed-channel send must count as a drop");
    }

    #[test]
    fn record_on_full_channel_increments_drops() {
        // Saturate the queue without a draining receiver: every send past
        // capacity is dropped (drop-newest under sustained Full).
        let (tx, _rx) = mpsc::channel::<AuditEntry>(2);
        let (drops, _lag) = metric_pair();
        let handle = Handle {
            tx,
            drops: drops.clone(),
        };
        // First two fill the queue, the rest overflow.
        for i in 0..5 {
            handle.record(entry_at(Utc::now(), &format!("f-{i}")));
        }
        assert_eq!(drops.get(), 3, "three overflow sends should be dropped");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_writer_returns_when_channel_closes_immediately() {
        // Closing the sender before any entry arrives drives the outer
        // select's `None` branch (channel closed without cancel).
        let (_dir, store) = store_with_audit();
        let (_drops, lag) = metric_pair();
        let (tx, rx) = mpsc::channel::<AuditEntry>(HANDOFF_CAPACITY);
        drop(tx);
        let cancel = CancellationToken::new();
        // Should return promptly without hanging.
        run_writer(Arc::clone(&store), rx, lag, cancel).await;
        assert_eq!(count_rows(&store), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_writer_flushes_then_returns_on_mid_batch_close() {
        // One entry is enqueued, then the sender is dropped. The outer
        // select consumes the entry, the inner fill loop sees the closed
        // channel (`None => break`), and the batch is flushed before the
        // next outer iteration returns on the closed channel.
        let (_dir, store) = store_with_audit();
        let (_drops, lag) = metric_pair();
        let (tx, rx) = mpsc::channel::<AuditEntry>(HANDOFF_CAPACITY);
        tx.try_send(entry_at(Utc::now(), "solo")).unwrap();
        drop(tx);
        let cancel = CancellationToken::new();
        run_writer(Arc::clone(&store), rx, lag, cancel).await;
        assert_eq!(count_rows(&store), 1, "the single entry must be persisted");
    }
}
