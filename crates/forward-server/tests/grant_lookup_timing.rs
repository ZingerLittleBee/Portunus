//! 008-sqlite-storage T053 — Constitution Principle V verification.
//!
//! `OperatorAuthenticator::grants_for` must be timing-independent w.r.t.
//! whether the named user has 0 vs N grants on file. The SQLite-backed
//! impl runs an indexed `WHERE user_id = ?` so absent users return early
//! at the index seek; but they should not return MEASURABLY faster than
//! "user with 0 grants but row exists" or "user with M grants".
//!
//! We use a coarse 5x ratio threshold rather than a tight comparison —
//! we're catching obvious leaks (factor-of-100), not microbenchmarking.
//! Marked `#[ignore]` by default so CI without performance counters
//! doesn't go red on heavily-loaded runners.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use forward_auth::{
    Grant, GrantId, OperatorAuthenticator, OperatorRole, ProtocolSet, User, UserId,
};
use forward_server::store::Store;
use forward_server::store::operator_store::SqliteOperatorStore;
use tempfile::tempdir;

const ITERATIONS: usize = 5_000;
const RATIO_LIMIT: f64 = 5.0; // a clear leak would be ~100x

fn build_store() -> (tempfile::TempDir, SqliteOperatorStore) {
    let dir = tempdir().unwrap();
    let store = Arc::new(Store::open(dir.path()).unwrap());
    let s = SqliteOperatorStore::new(store);
    // Seed two users — one with 5 grants, one with 0 — plus one
    // sentinel that doesn't exist at all.
    s.add_user(User {
        id: UserId::from_str("with-grants").unwrap(),
        display_name: "with grants".into(),
        role: OperatorRole::User,
        created_at: chrono::Utc::now(),
        disabled: false,
    })
    .unwrap();
    s.add_user(User {
        id: UserId::from_str("zero-grants").unwrap(),
        display_name: "no grants".into(),
        role: OperatorRole::User,
        created_at: chrono::Utc::now(),
        disabled: false,
    })
    .unwrap();
    for i in 0..5 {
        s.add_grant(Grant {
            id: GrantId::new(),
            user_id: UserId::from_str("with-grants").unwrap(),
            client: forward_auth::ClientScope::Any,
            listen_port_start: 30000 + i,
            listen_port_end: 30000 + i,
            protocols: ProtocolSet::TCP,
            note: None,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    }
    (dir, s)
}

fn time_lookup(store: &SqliteOperatorStore, user: &str) -> u128 {
    let id = UserId::from_str(user).unwrap();
    // Warm cache.
    for _ in 0..50 {
        let _ = store.grants_for(&id);
    }
    let start = Instant::now();
    for _ in 0..ITERATIONS {
        let _ = std::hint::black_box(store.grants_for(&id));
    }
    start.elapsed().as_nanos()
}

#[test]
#[ignore = "perf-sensitive — run via `cargo test --test grant_lookup_timing -- --ignored`"]
fn grants_for_latency_independent_of_user_presence() {
    let (_d, store) = build_store();
    // User that does not exist.
    let absent = time_lookup(&store, "ghost-user");
    // User present with 0 grants.
    let zero = time_lookup(&store, "zero-grants");
    // User present with 5 grants.
    let five = time_lookup(&store, "with-grants");

    #[allow(clippy::cast_precision_loss)]
    let max = absent.max(zero).max(five) as f64;
    #[allow(clippy::cast_precision_loss)]
    let min = absent.min(zero).min(five).max(1) as f64;
    let ratio = max / min;
    assert!(
        ratio < RATIO_LIMIT,
        "Constitution V regression: grants_for timing ratio absent={absent} zero={zero} \
         five={five} (max/min ratio = {ratio:.1}x, limit = {RATIO_LIMIT}x)",
    );
}
