//! 008-sqlite-storage T023 — SC-001 verification.
//!
//! After a clean restart of `forward-server`, an operator can retrieve
//! every audit event recorded before the restart, with no events lost
//! and no events reordered.
//!
//! This test does not stand up the full HTTP stack — that surface is
//! covered by `audit_persists_e2e` in `crates/forward-e2e/tests/`.
//! Here we exercise the Store + audit_writer round-trip directly:
//!
//! 1. Open Store at a tempdir.
//! 2. Spawn the audit writer.
//! 3. Record N entries, give the writer time to flush.
//! 4. Cancel the writer (clean shutdown), drop the Store.
//! 5. Re-open Store at the same path; query newest-first; verify
//!    every entry is present in the right order.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use forward_auth::OperatorRole;
use forward_server::operator::audit::{AuditEntry, AuditOutcome};
use forward_server::store::Store;
use forward_server::store::audit_writer;
use prometheus::{Gauge, IntCounter};
use tempfile::tempdir;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

fn entry(actor: &str, outcome: AuditOutcome, path: &str) -> AuditEntry {
    AuditEntry {
        timestamp: Utc::now(),
        actor: actor.into(),
        role: Some(OperatorRole::Superadmin),
        method: "GET".into(),
        path: path.into(),
        outcome,
        reason: match outcome {
            AuditOutcome::Allow => None,
            AuditOutcome::Deny => Some("port_outside_grant".into()),
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_entries_survive_clean_restart() {
    let dir = tempdir().unwrap();

    // ---- First boot: write 50 entries through the durable writer ----
    {
        let store = Arc::new(Store::open(dir.path()).expect("open #1"));
        let drops = IntCounter::new("t_drops", "test").unwrap();
        let lag = Gauge::new("t_lag", "test").unwrap();
        let cancel = CancellationToken::new();
        let handle = audit_writer::spawn(
            Arc::clone(&store),
            drops.clone(),
            lag,
            cancel.clone(),
        );

        for i in 0..50 {
            let outcome = if i % 5 == 0 {
                AuditOutcome::Deny
            } else {
                AuditOutcome::Allow
            };
            handle.record(entry(&format!("alice-{i}"), outcome, "/v1/users"));
        }

        // Allow the batch to flush.
        sleep(Duration::from_millis(250)).await;

        // Trigger a clean shutdown of the writer; this drains the
        // pending batch and commits before the task exits.
        cancel.cancel();
        sleep(Duration::from_millis(120)).await;

        assert_eq!(drops.get(), 0, "no drops during clean run");
        // Drop the store so the file lock is released for the second
        // boot below.
        drop(store);
    }

    // ---- Second boot: re-open and verify every entry is recoverable ----
    let store = Store::open(dir.path()).expect("open #2");
    let rows = store
        .query_audit_recent(1000, None)
        .expect("read after restart");

    assert_eq!(rows.len(), 50, "every pre-restart entry recoverable");

    // Newest-first. Insertion order was alice-0 .. alice-49; the
    // newest-first read returns alice-49 first.
    assert_eq!(rows[0].actor, "alice-49");
    assert_eq!(rows[49].actor, "alice-0");

    // Outcome filter still works on the durable read.
    let denies = store
        .query_audit_recent(100, Some(AuditOutcome::Deny))
        .unwrap();
    let expected_denies = (0..50).filter(|i| i % 5 == 0).count();
    assert_eq!(
        denies.len(),
        expected_denies,
        "every deny entry recoverable through the outcome filter"
    );
    assert!(denies.iter().all(|e| e.outcome == AuditOutcome::Deny));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn audit_entries_survive_repeated_open_close_cycles() {
    // Soak: 5 sequential boot cycles, each adding 10 entries. After
    // the last cycle, all 50 must be recoverable.
    let dir = tempdir().unwrap();

    for cycle in 0..5 {
        let store = Arc::new(Store::open(dir.path()).unwrap_or_else(|e| panic!("open cycle {cycle}: {e}")));
        let drops = IntCounter::new(format!("t_drops_{cycle}"), "test").unwrap();
        let lag = Gauge::new(format!("t_lag_{cycle}"), "test").unwrap();
        let cancel = CancellationToken::new();
        let handle = audit_writer::spawn(Arc::clone(&store), drops, lag, cancel.clone());

        for i in 0..10 {
            handle.record(entry(
                &format!("c{cycle}-{i}"),
                AuditOutcome::Allow,
                "/v1/rules",
            ));
        }
        sleep(Duration::from_millis(250)).await;
        cancel.cancel();
        sleep(Duration::from_millis(120)).await;
        drop(store);
    }

    let store = Store::open(dir.path()).unwrap();
    let rows = store.query_audit_recent(1000, None).unwrap();
    assert_eq!(rows.len(), 50, "5 cycles × 10 entries each");
}
