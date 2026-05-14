//! 008-sqlite-storage T015..T018 — persistent SQLite store.
//!
//! Owns the connection pool, PRAGMA contract, transaction wrappers,
//! schema-version handshake, and the audit-writer plumbing.
//!
//! See `specs/008-sqlite-storage/contracts/persistence.md` for the
//! authoritative on-disk + boot-handshake protocol.
//!
//! Module layout:
//! - this file: `Store`, pool, PRAGMA `with_init`, transaction helpers
//! - `audit_writer.rs`: bounded mpsc → durable batch writer (T030).
//! - `backup.rs`: Online Backup API wrapper (T060..T061).
//! - `migrations/V###__*.sql`: refinery embedded migrations.

// rusqlite returns i64 for every column type, so the SQLite store
// surface unavoidably casts between i64 and usize/u32/u16/u8 when
// projecting into domain types. Pedantic cast lints are not actionable
// in this seam — the upstream constraint is the SQLite type system.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    clippy::cast_precision_loss,
    clippy::must_use_candidate
)]

pub mod audit_query;
pub mod audit_writer;
pub mod backup;
pub mod error;
pub mod operator_store;
pub mod owner_cap_store;
pub mod rule_store;
pub mod token_store;

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OpenFlags};
use thiserror::Error;
use tracing::{info, warn};

pub use error::{StoreError, map_rusqlite};

// Refinery migration set — V001 ships the entire schema; later
// versions add tables / indexes only via additive migrations.
mod embedded {
    refinery::embed_migrations!("src/store/migrations");
}

/// Standard data file name inside the resolved `--data-dir`.
pub const DATA_FILE_NAME: &str = "state.db";

/// Conn pool size cap. SQLite serialises writers internally; the pool
/// primarily helps reads. Capping at 8 avoids paying the per-connection
/// WAL frame-cache cost on small deployments. See research R-004.
pub const MAX_POOL_SIZE: u32 = 8;

/// Wait this long for SQLITE_BUSY before returning to the caller. Paired
/// with `BEGIN IMMEDIATE` for write transactions, this should rarely
/// trigger; when it does, callers see `StoreError::Transient` (R-015).
pub const BUSY_TIMEOUT: Duration = Duration::from_millis(5_000);

/// Persistent store handle. Cheap to clone (Arc internally).
#[derive(Clone)]
pub struct Store {
    inner: Arc<StoreInner>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("db_path", &self.inner.db_path)
            .finish_non_exhaustive()
    }
}

struct StoreInner {
    pool: Pool<SqliteConnectionManager>,
    /// Path to the live `state.db`; held so backup / reset paths can
    /// re-open with different flags without re-resolving the data-dir.
    db_path: PathBuf,
    /// Exclusive file lock on `state.db`. Stored as an option so the
    /// non-Unix branch (currently a no-op) can hold `None`. The lock
    /// is released when the underlying File is dropped — i.e. when
    /// the last `Arc<StoreInner>` clone goes away. See T017.
    #[cfg(unix)]
    _lock: nix::fcntl::Flock<std::fs::File>,
    #[cfg(not(unix))]
    _lock: std::fs::File,
}

/// Errors surfaced by the store boot path. Runtime errors map through
/// `error::map_rusqlite` into `StoreError`; this enum only covers boot
/// pre-conditions (file-class, file-lock, schema-handshake).
#[derive(Debug, Error)]
pub enum BootError {
    #[error("io: {0}")]
    Io(#[from] io::Error),

    #[error("store_in_use: {0}")]
    InUse(PathBuf),

    #[error("store_corrupt: {path} ({source})")]
    Corrupt {
        path: PathBuf,
        source: rusqlite::Error,
    },

    #[error("schema_version_too_new: on_disk={on_disk} binary_supports={target}")]
    SchemaTooNew { on_disk: u32, target: u32 },

    #[error("migration_failed: {0}")]
    MigrationFailed(String),

    #[error("pool: {0}")]
    Pool(#[from] r2d2::Error),
}

impl Store {
    /// Open or initialise the store at `<data_dir>/state.db`. Performs:
    ///
    /// 1. Acquire an exclusive lock on the file (`flock(LOCK_EX | LOCK_NB)`).
    ///    A second process holding the lock fails with `BootError::InUse`.
    /// 2. Run `refinery` migrations forward; abort on failure.
    /// 3. Refuse if the on-disk `schema_migrations` head is newer than
    ///    the binary's compiled-in target (`BootError::SchemaTooNew`).
    /// 4. Build the connection pool with the standard PRAGMA set.
    pub fn open(data_dir: &Path) -> Result<Self, BootError> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join(DATA_FILE_NAME);
        // We lock a sentinel file alongside `state.db` rather than
        // `state.db` itself — SQLite acquires its own POSIX advisory
        // locks on the database file, and an exclusive flock on the
        // same fd would deadlock SQLite's own opens. The sentinel
        // file is empty; its only purpose is to carry the flock that
        // distinguishes "another portunus-server is running here" from
        // "we just crashed and a `*-shm` is leftover".
        let lock_path = data_dir.join(format!("{DATA_FILE_NAME}.lock"));

        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        let locked = match acquire_exclusive_lock(lock_file) {
            Ok(f) => f,
            Err(LockError::WouldBlock) => return Err(BootError::InUse(db_path)),
            Err(LockError::Io(e)) => return Err(BootError::Io(e)),
        };

        // Run migrations on a single, dedicated connection. We open this
        // by hand (not via the pool) to avoid race conditions where the
        // pool-init PRAGMAs fight refinery's own SQL.
        let mut conn = Connection::open_with_flags(
            &db_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )
        .map_err(|e| classify_open_error(&db_path, e))?;
        configure_connection(&conn).map_err(BootError::MigrationFailed)?;

        let target_version = embedded::migrations::runner()
            .get_migrations()
            .iter()
            .map(refinery::Migration::version)
            .max()
            .unwrap_or(0);
        let head_before = read_head_version(&conn).unwrap_or(0);
        if head_before > target_version {
            return Err(BootError::SchemaTooNew {
                on_disk: head_before,
                target: target_version,
            });
        }

        embedded::migrations::runner()
            .set_migration_table_name("schema_migrations")
            .run(&mut conn)
            .map_err(|e| BootError::MigrationFailed(e.to_string()))?;

        let head_after = read_head_version(&conn).unwrap_or(0);
        info!(
            event = "store.opened",
            path = %db_path.display(),
            schema_version = head_after,
            target_version,
        );
        drop(conn);

        // Build the runtime pool. PRAGMAs are applied on every checkout
        // via `with_init` so a connection reset (rare) re-arms them.
        let manager =
            SqliteConnectionManager::file(&db_path).with_init(|c| configure_connection_rusqlite(c));
        let pool_size = num_cpus::get().min(MAX_POOL_SIZE as usize) as u32;
        let pool = Pool::builder()
            .max_size(pool_size)
            .build(manager)
            .map_err(BootError::Pool)?;

        Ok(Store {
            inner: Arc::new(StoreInner {
                pool,
                db_path,
                _lock: locked,
            }),
        })
    }

    /// Borrow a connection from the pool. Panics only if the pool is
    /// poisoned (e.g., a previous panic while holding the lock).
    pub fn with_conn<F, R>(&self, f: F) -> Result<R, StoreError>
    where
        F: FnOnce(&mut Connection) -> Result<R, StoreError>,
    {
        let mut conn = self.checkout()?;
        f(&mut conn)
    }

    /// Run `f` inside a `BEGIN IMMEDIATE` transaction (research R-014).
    /// Acquires the writer lock up front so a contention failure
    /// surfaces before any statement runs.
    pub fn with_write_tx<F, R>(&self, f: F) -> Result<R, StoreError>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<R, StoreError>,
    {
        let mut conn = self.checkout()?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(map_rusqlite)?;
        let out = f(&tx)?;
        tx.commit().map_err(map_rusqlite)?;
        Ok(out)
    }

    /// Issue `PRAGMA wal_checkpoint(TRUNCATE)` on a fresh connection.
    /// Called from the graceful-shutdown drain so an orderly stop leaves
    /// a quiesced WAL on disk (T022).
    pub fn checkpoint_for_clean_shutdown(&self) -> Result<(), StoreError> {
        let conn = self.checkout()?;
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .map_err(map_rusqlite)?;
        Ok(())
    }

    /// Path to the live `state.db`; backup / reset use this.
    pub fn db_path(&self) -> &Path {
        &self.inner.db_path
    }

    /// Currently-applied schema version. Used by the boot sequence and
    /// by the backup/restore handshake.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        let conn = self.checkout()?;
        Ok(read_head_version(&conn).unwrap_or(0))
    }

    /// Compile-time-baked target schema version (the highest version in
    /// the refinery migration set).
    #[must_use]
    pub fn target_schema_version() -> u32 {
        embedded::migrations::runner()
            .get_migrations()
            .iter()
            .map(refinery::Migration::version)
            .max()
            .unwrap_or(0)
    }

    fn checkout(&self) -> Result<PooledConnection<SqliteConnectionManager>, StoreError> {
        self.inner.pool.get().map_err(|e| {
            warn!(event = "store.pool_exhausted", error = %e);
            StoreError::Internal {
                message: format!("pool: {e}"),
            }
        })
    }
}

/// The exact PRAGMA contract from `contracts/persistence.md` §5.
/// Every connection (pool init + the boot-time migration handle) runs
/// this. Tests assert each PRAGMA via `PRAGMA x;` round-trip.
fn configure_connection(c: &Connection) -> Result<(), String> {
    configure_connection_rusqlite(c).map_err(|e| e.to_string())
}

/// Same contract as `configure_connection`, but returns the underlying
/// `rusqlite::Error` so it slots into `r2d2_sqlite::with_init`'s
/// signature without a wrapping conversion.
fn configure_connection_rusqlite(c: &Connection) -> Result<(), rusqlite::Error> {
    c.pragma_update(None, "journal_mode", "WAL")?;
    c.pragma_update(None, "synchronous", "NORMAL")?;
    c.pragma_update(None, "foreign_keys", "ON")?;
    c.busy_timeout(BUSY_TIMEOUT)?;
    c.pragma_update(None, "temp_store", "MEMORY")?;
    c.pragma_update(None, "cache_size", -8000_i64)?;
    Ok(())
}

fn read_head_version(conn: &Connection) -> Option<u32> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_migrations')",
            [],
            |r| r.get::<_, i64>(0).map(|v| v != 0),
        )
        .unwrap_or(false);
    if !exists {
        return None;
    }
    conn.query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
        r.get::<_, Option<i64>>(0)
    })
    .ok()
    .flatten()
    .map(|v| v as u32)
}

fn classify_open_error(path: &Path, e: rusqlite::Error) -> BootError {
    use rusqlite::ErrorCode;
    match &e {
        rusqlite::Error::SqliteFailure(err, _)
            if matches!(
                err.code,
                ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt
            ) =>
        {
            BootError::Corrupt {
                path: path.to_path_buf(),
                source: e,
            }
        }
        _ => BootError::Io(io::Error::other(e.to_string())),
    }
}

// --------------------------------------------------------------------
// Cross-platform exclusive file lock (T017)
// --------------------------------------------------------------------

#[derive(Debug)]
enum LockError {
    WouldBlock,
    Io(io::Error),
}

#[cfg(unix)]
type LockedFile = nix::fcntl::Flock<std::fs::File>;
#[cfg(not(unix))]
type LockedFile = std::fs::File;

#[cfg(unix)]
fn acquire_exclusive_lock(file: std::fs::File) -> Result<LockedFile, LockError> {
    // Use nix's safe Flock wrapper so the boot path stays
    // `unsafe_code = "forbid"`-clean. The lock is held for the lifetime
    // of the returned `Flock<File>` and released on Drop.
    use nix::fcntl::{Flock, FlockArg};
    Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_file, errno)| match errno {
        nix::errno::Errno::EWOULDBLOCK => LockError::WouldBlock,
        e => LockError::Io(io::Error::from(e)),
    })
}

#[cfg(not(unix))]
fn acquire_exclusive_lock(file: std::fs::File) -> Result<LockedFile, LockError> {
    // Windows is out of scope for v0.1+; this branch exists only so the
    // crate compiles on macOS / Linux dev hosts and CI.
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_fresh_dir_initialises_schema() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).expect("open fresh");
        let v = store.schema_version().unwrap();
        assert_eq!(v, Store::target_schema_version());
        assert!(v >= 1, "schema migrations must include V001");
    }

    #[test]
    fn v008_creates_traffic_quota_tables() {
        // 013-traffic-quotas: V008 introduces traffic_quotas + sample tables.
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).expect("open fresh");
        assert!(store.schema_version().unwrap() >= 8);
        store
            .with_conn(|c| {
                let names = [
                    "traffic_quotas",
                    "traffic_samples_1m",
                    "traffic_samples_1h",
                    "traffic_rollup_state",
                ];
                for n in names {
                    let cnt: i64 = c
                        .query_row(
                            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                            [n],
                            |r| r.get(0),
                        )
                        .map_err(map_rusqlite)?;
                    assert_eq!(cnt, 1, "table {n} should exist after V008");
                }
                // traffic_rollup_state should have its singleton row.
                let last: i64 = c
                    .query_row(
                        "SELECT last_rolled_up_hour FROM traffic_rollup_state WHERE id = 1",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(map_rusqlite)?;
                assert_eq!(last, 0);
                Ok(())
            })
            .expect("inspect tables");
    }

    #[test]
    fn pragma_contract_holds_on_pool_connections() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        store
            .with_conn(|c| {
                let journal: String = c
                    .pragma_query_value(None, "journal_mode", |r| r.get(0))
                    .map_err(map_rusqlite)?;
                assert_eq!(journal.to_lowercase(), "wal");

                let sync: i64 = c
                    .pragma_query_value(None, "synchronous", |r| r.get(0))
                    .map_err(map_rusqlite)?;
                // synchronous=NORMAL maps to 1
                assert_eq!(sync, 1);

                let fk: i64 = c
                    .pragma_query_value(None, "foreign_keys", |r| r.get(0))
                    .map_err(map_rusqlite)?;
                assert_eq!(fk, 1);

                let temp_store: i64 = c
                    .pragma_query_value(None, "temp_store", |r| r.get(0))
                    .map_err(map_rusqlite)?;
                // temp_store=MEMORY maps to 2
                assert_eq!(temp_store, 2);

                let cache: i64 = c
                    .pragma_query_value(None, "cache_size", |r| r.get(0))
                    .map_err(map_rusqlite)?;
                assert_eq!(cache, -8000);

                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn second_open_blocked_by_file_lock() {
        let dir = tempdir().unwrap();
        let _first = Store::open(dir.path()).unwrap();
        let err = Store::open(dir.path()).unwrap_err();
        assert!(matches!(err, BootError::InUse(_)), "got {err:?}");
    }

    #[test]
    fn write_tx_commits_atomically() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO users (user_id, role, display_name, created_at) \
                     VALUES (?, 'superadmin', ?, datetime('now'))",
                    rusqlite::params!["root-admin", "root admin"],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .unwrap();

        let n: i64 = store
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
                    .map_err(map_rusqlite)
            })
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn write_tx_rolls_back_on_error() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();

        let _: Result<(), StoreError> = store.with_write_tx(|tx| {
            tx.execute(
                "INSERT INTO users (user_id, role, display_name, created_at) \
                 VALUES (?, 'superadmin', ?, datetime('now'))",
                rusqlite::params!["abort-me", "abort me"],
            )
            .map_err(map_rusqlite)?;
            Err(StoreError::Internal {
                message: "deliberate".into(),
            })
        });

        let n: i64 = store
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
                    .map_err(map_rusqlite)
            })
            .unwrap();
        assert_eq!(n, 0, "transaction must roll back on error");
    }

    #[test]
    fn schema_too_new_refuses_open() {
        // Hand-craft a state.db that claims schema version target+1.
        let dir = tempdir().unwrap();
        let path = dir.path().join(DATA_FILE_NAME);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, name TEXT, applied_on TEXT, checksum TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO schema_migrations VALUES (?, 'phantom', '2026-05-08T00:00:00Z', 'x')",
            rusqlite::params![Store::target_schema_version() as i64 + 1],
        )
        .unwrap();
        drop(conn);

        let err = Store::open(dir.path()).unwrap_err();
        assert!(matches!(err, BootError::SchemaTooNew { .. }), "got {err:?}");
    }

    #[test]
    fn cascade_delete_removes_credentials_and_grants() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO users (user_id, role, display_name, created_at) \
                     VALUES ('alice','user','Alice','2026-01-01T00:00:00Z')",
                    [],
                )
                .map_err(map_rusqlite)?;
                tx.execute(
                    "INSERT INTO credentials \
                     (credential_id, user_id, hash, status, issued_at) \
                     VALUES ('c1','alice','h','active','2026-01-01T00:00:00Z')",
                    [],
                )
                .map_err(map_rusqlite)?;
                tx.execute(
                    "INSERT INTO grants \
                     (grant_id, user_id, client, listen_port_start, listen_port_end, protocols, created_at) \
                     VALUES ('g1','alice','*',30000,30010,1,'2026-01-01T00:00:00Z')",
                    [],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .unwrap();

        store
            .with_write_tx(|tx| {
                tx.execute("DELETE FROM users WHERE user_id='alice'", [])
                    .map_err(map_rusqlite)?;
                Ok(())
            })
            .unwrap();

        let n_creds: i64 = store
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM credentials", [], |r| r.get(0))
                    .map_err(map_rusqlite)
            })
            .unwrap();
        let n_grants: i64 = store
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM grants", [], |r| r.get(0))
                    .map_err(map_rusqlite)
            })
            .unwrap();
        assert_eq!(n_creds, 0, "credentials must cascade-delete");
        assert_eq!(n_grants, 0, "grants must cascade-delete");
    }
}
