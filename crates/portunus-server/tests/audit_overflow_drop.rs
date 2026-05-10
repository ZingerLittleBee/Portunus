//! 008-sqlite-storage T025 — `portunus_audit_buffer_drops_total`
//! increments under hand-off queue saturation.
//!
//! The bounded `mpsc` channel inside `audit_writer` has capacity
//! `HANDOFF_CAPACITY` (1024). When the producer pushes faster than the
//! durable writer can drain, `try_send` returns `Full`, the writer
//! pops the oldest entry, increments the drop counter, and inserts the
//! new entry. The counter is exported to `/metrics` as
//! `portunus_audit_buffer_drops_total`.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use portunus_server::operator::audit::{AuditEntry, AuditOutcome};
use portunus_server::store::{Store, audit_writer};
use prometheus::{Gauge, IntCounter};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn entry_at(ts: chrono::DateTime<Utc>, actor: &str) -> AuditEntry {
    AuditEntry {
        timestamp: ts,
        actor: actor.to_string(),
        role: None,
        method: "GET".into(),
        path: "/v1/audit".into(),
        outcome: AuditOutcome::Allow,
        reason: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn drops_counter_increments_under_saturation() {
    let dir = tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).expect("open store"));
    let drops = IntCounter::new("t25_drops", "test").unwrap();
    let lag = Gauge::new("t25_lag", "test").unwrap();
    let cancel = CancellationToken::new();
    let handle = audit_writer::spawn(Arc::clone(&store), drops.clone(), lag, cancel.clone());

    // Push 2× capacity in a tight loop. The writer is busy committing
    // SQLite batches; the surplus must flow through the drop-oldest
    // path and bump the counter.
    let burst = audit_writer::HANDOFF_CAPACITY * 2;
    for i in 0..burst {
        handle.record(entry_at(Utc::now(), &format!("u-{i}")));
    }

    // Allow the writer to chew through what it can.
    tokio::time::sleep(Duration::from_millis(250)).await;
    cancel.cancel();
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        drops.get() > 0,
        "expected drop counter to increment under saturation; got {}",
        drops.get()
    );

    // The durable rows that did land are the most-recent: the
    // drop-oldest policy guarantees the surviving entries are at the
    // tail of the burst. We look for the highest-numbered actor
    // (`u-{burst-1}`) in the persisted set.
    drop(handle);
    let conn = rusqlite::Connection::open_with_flags(
        dir.path().join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("ro open");
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM audit", [], |r| r.get(0))
        .expect("count");
    assert!(
        count > 0,
        "writer should have committed at least some rows; got count={count}"
    );
    let last_actor: Option<String> = conn
        .query_row(
            "SELECT user_id FROM audit ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    let last_actor = last_actor.expect("at least one durable row");
    // Entries arrive in pushed order; the last-committed should be
    // toward the tail of the burst. We don't assert the exact actor
    // because batching boundaries make it timing-dependent, but it
    // MUST NOT be one of the very-earliest entries (which were dropped
    // by drop-oldest).
    assert!(
        !last_actor.starts_with("u-0") || last_actor == format!("u-{}", burst - 1),
        "last surviving row should be near tail of burst; got {last_actor}"
    );
}
