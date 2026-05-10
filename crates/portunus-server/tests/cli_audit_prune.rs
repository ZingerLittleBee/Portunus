//! 008-sqlite-storage T072 — CLI smoke for `portunus-server audit prune`.
//!
//! Covers happy path (delete-old) + `--dry-run` no-mutation + invalid
//! RFC3339 input.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use portunus_server::operator::audit::{AuditEntry, AuditOutcome};
use portunus_server::store::{Store, audit_writer};
use prometheus::{Gauge, IntCounter};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_portunus-server")
}

fn bootstrap(data: &TempDir) {
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("bootstrap-superadmin")
        .arg("--name")
        .arg("ops")
        .output()
        .expect("bootstrap");
    assert!(
        out.status.success(),
        "bootstrap failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Seed `n` audit rows whose timestamps span `[base - n .. base)` seconds,
/// flushed through the durable writer (matches production write path).
async fn seed_rows(data: &TempDir, base: chrono::DateTime<chrono::Utc>, n: usize) {
    let store = Arc::new(Store::open(data.path()).expect("open store"));
    let cancel = CancellationToken::new();
    let drops = IntCounter::new("prune_drops", "test").unwrap();
    let lag = Gauge::new("prune_lag", "test").unwrap();
    let handle = audit_writer::spawn(store.clone(), drops, lag, cancel.clone());
    for i in 0..n {
        let ts = base - chrono::Duration::seconds(i64::try_from(n - i).unwrap());
        handle.record(AuditEntry {
            timestamp: ts,
            actor: format!("u-{i}"),
            role: None,
            method: "GET".into(),
            path: "/v1/audit".into(),
            outcome: AuditOutcome::Allow,
            reason: None,
        });
    }
    // Flush window — durable writer batches every ~250ms.
    tokio::time::sleep(Duration::from_millis(500)).await;
    cancel.cancel();
    drop(handle);
    drop(store);
    // Give the writer task a moment to drain.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

fn run_prune(data: &TempDir, before: &str, dry_run: bool) -> std::process::Output {
    let mut cmd = Command::new(server_bin());
    cmd.arg("--data-dir")
        .arg(data.path())
        .arg("audit")
        .arg("prune")
        .arg("--before")
        .arg(before);
    if dry_run {
        cmd.arg("--dry-run");
    }
    cmd.output().expect("prune cmd")
}

fn count_audit_rows(data: &TempDir) -> i64 {
    let conn = rusqlite::Connection::open_with_flags(
        data.path().join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open ro");
    conn.query_row("SELECT COUNT(*) FROM audit", [], |r| r.get::<_, i64>(0))
        .expect("count audit")
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_does_not_mutate() {
    let data = TempDir::new().unwrap();
    bootstrap(&data);
    let now = chrono::Utc::now();
    seed_rows(&data, now, 5).await;
    let before_count = count_audit_rows(&data);
    assert!(before_count >= 5, "seed should have inserted ≥5 rows");

    let cutoff = now.to_rfc3339();
    let out = run_prune(&data, &cutoff, true);
    assert!(
        out.status.success(),
        "dry-run failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("dry_run=true"),
        "expected dry_run report, got: {stdout}"
    );
    let after_count = count_audit_rows(&data);
    assert_eq!(before_count, after_count, "dry-run must not mutate");
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_deletes_only_matching_rows() {
    let data = TempDir::new().unwrap();
    bootstrap(&data);
    let now = chrono::Utc::now();
    // 5 old rows (ts < now - 10s) and 3 fresh rows (ts ≥ now - 10s).
    seed_rows(&data, now - chrono::Duration::seconds(20), 5).await;
    seed_rows(&data, now, 3).await;

    let total_before = count_audit_rows(&data);
    let cutoff = (now - chrono::Duration::seconds(15)).to_rfc3339();
    let out = run_prune(&data, &cutoff, false);
    assert!(
        out.status.success(),
        "prune failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("deleted="),
        "expected deleted report, got: {stdout}"
    );

    let total_after = count_audit_rows(&data);
    assert!(
        total_after < total_before,
        "rows should have been deleted: {total_before} → {total_after}"
    );
    // Fresh rows (≥ now-10s) must survive — at least the 3 we seeded after `now`.
    assert!(
        total_after >= 3,
        "fresh rows should survive: surviving={total_after}"
    );
}

#[test]
fn invalid_rfc3339_rejected() {
    let data = TempDir::new().unwrap();
    bootstrap(&data);
    let out = run_prune(&data, "not-a-timestamp", false);
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("RFC3339") || stderr.contains("rfc3339"),
        "expected RFC3339 error, got: {stderr}"
    );
}
