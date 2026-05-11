//! 008-sqlite-storage T071 — audit query scaling.
//!
//! Seeds 100 000 audit rows and asserts each of three representative
//! envelope queries (no filter, outcome filter, since-until-cursor)
//! runs under 2 s on a developer-class machine (SC-005).
//!
//! Marked `#[ignore]` by default — CI runs it on a nightly job. Run
//! locally with: `cargo test -p portunus-server --test
//! audit_query_scaling -- --ignored --nocapture`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use portunus_server::operator::audit::{AuditEntry, AuditOutcome};
use portunus_server::store::audit_query::AuditQuery;
use portunus_server::store::{Store, audit_writer};
use prometheus::{Gauge, IntCounter};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

const ROWS: usize = 100_000;
const BUDGET: Duration = Duration::from_secs(2);

#[tokio::test(flavor = "multi_thread")]
#[ignore = "perf — runs in CI nightly only"]
async fn envelope_queries_under_two_seconds_at_100k_rows() {
    let dir = tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).expect("open"));
    let drops = IntCounter::new("scaling_drops", "test").unwrap();
    let lag = Gauge::new("scaling_lag", "test").unwrap();
    let cancel = CancellationToken::new();
    let handle = audit_writer::spawn(Arc::clone(&store), drops, lag, cancel.clone());

    // Seed `ROWS` entries striped across an 8-day window so the time
    // index has work to do.
    let base = Utc::now() - chrono::Duration::days(8);
    for i in 0..ROWS {
        // Spread across ~8 days; one row every ~7 seconds.
        let ts = base + chrono::Duration::seconds(i64::try_from(i * 7).unwrap_or(0));
        let outcome = if i % 3 == 0 {
            AuditOutcome::Deny
        } else {
            AuditOutcome::Allow
        };
        handle.record(AuditEntry {
            timestamp: ts,
            actor: format!("u-{}", i % 64),
            role: None,
            method: "GET".into(),
            path: "/v1/users".into(),
            outcome,
            reason: None,
            action: None,
            resource_kind: None,
            resource_value: None,
            details: None,
        });
    }
    // Allow the durable writer to flush every batch.
    drop(handle);
    tokio::time::sleep(Duration::from_secs(2)).await;
    cancel.cancel();
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Sanity: every row landed on disk.
    let conn = rusqlite::Connection::open_with_flags(
        dir.path().join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM audit", [], |r| r.get(0))
        .unwrap();
    assert!(
        count >= i64::try_from(ROWS).unwrap() - 1024,
        "expected ~{ROWS} rows persisted; got {count}"
    );

    // 1) No filter — newest 100 rows.
    let q1 = AuditQuery {
        limit: 100,
        outcome: None,
        since: None,
        until: None,
        before_seq: None,
    };
    let t = Instant::now();
    let p1 = store.query_audit_envelope(&q1).expect("q1");
    let e1 = t.elapsed();
    assert!(p1.rows.len() == 100);
    assert!(
        e1 < BUDGET,
        "no-filter envelope took {e1:?}; budget {BUDGET:?}"
    );

    // 2) Outcome filter — newest 100 deny rows (uses
    // `audit_outcome_ts_idx`).
    let q2 = AuditQuery {
        limit: 100,
        outcome: Some(AuditOutcome::Deny),
        since: None,
        until: None,
        before_seq: None,
    };
    let t = Instant::now();
    let p2 = store.query_audit_envelope(&q2).expect("q2");
    let e2 = t.elapsed();
    assert!(p2.rows.len() == 100);
    for r in &p2.rows {
        assert_eq!(r.outcome, AuditOutcome::Deny);
    }
    assert!(
        e2 < BUDGET,
        "outcome-filter envelope took {e2:?}; budget {BUDGET:?}"
    );

    // 3) since/until + cursor — narrow to a 24-hour window in the
    // middle of the seeded range, paginate one page deep.
    let mid = base + chrono::Duration::days(4);
    let time_window_query = AuditQuery {
        limit: 100,
        outcome: None,
        since: Some(mid),
        until: Some(mid + chrono::Duration::days(1)),
        before_seq: None,
    };
    let t = Instant::now();
    let time_window_page = store
        .query_audit_envelope(&time_window_query)
        .expect("time_window_query");
    let time_window_elapsed = t.elapsed();
    assert!(
        time_window_elapsed < BUDGET,
        "since-until envelope took {time_window_elapsed:?}; budget {BUDGET:?}"
    );
    if let Some(_cursor) = time_window_page.next_cursor.clone() {
        let second_page_query = AuditQuery {
            limit: 100,
            outcome: None,
            since: Some(mid),
            until: Some(mid + chrono::Duration::days(1)),
            before_seq: time_window_page.last_seq,
        };
        let t = Instant::now();
        let _second_page = store
            .query_audit_envelope(&second_page_query)
            .expect("second_page_query");
        let second_page_elapsed = t.elapsed();
        assert!(
            second_page_elapsed < BUDGET,
            "since-until-cursor envelope took {second_page_elapsed:?}; budget {BUDGET:?}"
        );
    }
}
