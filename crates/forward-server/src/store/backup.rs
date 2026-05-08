//! 008-sqlite-storage T060..T064 — backup / restore / reset CLI plumbing.
//!
//! Implementation per `specs/008-sqlite-storage/research.md` R-007:
//! `rusqlite::backup::Backup::run(-1)` produces a clean single-file
//! artefact regardless of WAL state. Restore copies the artefact into
//! the data-dir then re-runs the schema-version handshake. Reset
//! removes `state.db` (+ -wal / -shm sidecars) after a magic-number
//! signature check (R-011 — protect against typo'd `--data-dir`).

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, backup::Backup};
use thiserror::Error;
use tracing::{info, warn};

use crate::store::{DATA_FILE_NAME, Store};

/// SQLite header magic — the first 16 bytes of any well-formed v3 DB.
const SQLITE_HEADER: &[u8] = b"SQLite format 3\0";

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("io: {0}")]
    Io(String),
    #[error("source not found: {0}")]
    SourceMissing(PathBuf),
    #[error("destination exists: {0}")]
    DestinationExists(PathBuf),
    #[error("destination not empty: {0}")]
    DestinationNonEmpty(PathBuf),
    #[error("not a sqlite file: {0}")]
    NotSqlite(PathBuf),
    #[error("rusqlite: {0}")]
    Sqlite(String),
    #[error("schema_too_new: backup at version {found} > supported {target}")]
    SchemaTooNew { found: u32, target: u32 },
    #[error("migration_failed: {0}")]
    MigrationFailed(String),
}

impl BackupError {
    /// CLI exit code per `contracts/cli.md`. 0 = success.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::SourceMissing(_) => 5, // not_found
            Self::DestinationExists(_) | Self::DestinationNonEmpty(_) => 6, // would_overwrite
            Self::NotSqlite(_) => 7,     // signature_check_failed
            Self::SchemaTooNew { .. } => 78, // SchemaTooNew (matches startup exit)
            Self::MigrationFailed(_) => 70,
            Self::Io(_) | Self::Sqlite(_) => 1,
        }
    }
}

fn map_io(e: std::io::Error) -> BackupError {
    BackupError::Io(e.to_string())
}

fn map_sqlite(e: rusqlite::Error) -> BackupError {
    BackupError::Sqlite(e.to_string())
}

/// Resolve `--out` to a concrete file path. If `out` is an existing
/// directory, append `forward-state-<RFC3339>.db`. Otherwise treat as
/// the literal target file.
pub fn resolve_backup_destination(out: &Path) -> PathBuf {
    if out.is_dir() {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        out.join(format!("forward-state-{stamp}.db"))
    } else {
        out.to_path_buf()
    }
}

/// `forward-server backup --out <PATH>`. Refuses to overwrite an
/// existing file. Uses the SQLite Online Backup API on a read-only
/// handle so concurrent writers (e.g., the audit_writer) don't see a
/// torn snapshot.
pub fn run_backup(data_dir: &Path, out: &Path) -> Result<PathBuf, BackupError> {
    let src = data_dir.join(DATA_FILE_NAME);
    if !src.exists() {
        return Err(BackupError::SourceMissing(src));
    }
    let dst = resolve_backup_destination(out);
    if dst.exists() {
        return Err(BackupError::DestinationExists(dst));
    }
    if let Some(parent) = dst.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(map_io)?;
    }

    let src_conn = Connection::open_with_flags(
        &src,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(map_sqlite)?;
    let mut dst_conn = Connection::open(&dst).map_err(map_sqlite)?;

    {
        let backup = Backup::new(&src_conn, &mut dst_conn).map_err(map_sqlite)?;
        backup
            .run_to_completion(100, std::time::Duration::from_millis(0), None)
            .map_err(map_sqlite)?;
    }
    drop(dst_conn);
    drop(src_conn);

    info!(
        event = "cli.backup_complete",
        src = %src.display(),
        dst = %dst.display(),
        outcome = "ok",
    );
    Ok(dst)
}

/// `forward-server restore --in <PATH> [--force]`. Validates the
/// artefact is a SQLite file, refuses to clobber a non-empty data-dir
/// without `--force`, copies it in, then re-opens the Store so the
/// schema-version handshake runs (refuses if the artefact is newer than
/// this binary's target).
pub fn run_restore(in_path: &Path, data_dir: &Path, force: bool) -> Result<(), BackupError> {
    if !in_path.exists() {
        return Err(BackupError::SourceMissing(in_path.to_path_buf()));
    }
    verify_sqlite_signature(in_path)?;

    let target = data_dir.join(DATA_FILE_NAME);
    if target.exists() && !force {
        return Err(BackupError::DestinationNonEmpty(target));
    }
    fs::create_dir_all(data_dir).map_err(map_io)?;
    // Best-effort cleanup of WAL sidecars when --force.
    if force {
        for sidecar in ["state.db-wal", "state.db-shm", "state.db.lock"] {
            let p = data_dir.join(sidecar);
            if p.exists() {
                let _ = fs::remove_file(&p);
            }
        }
        if target.exists() {
            fs::remove_file(&target).map_err(map_io)?;
        }
    }
    fs::copy(in_path, &target).map_err(map_io)?;

    // Run the schema handshake by opening the store; this catches
    // schema_too_new and migration failures.
    match Store::open(data_dir) {
        Ok(_) => {
            info!(
                event = "cli.restore_complete",
                src = %in_path.display(),
                dst = %target.display(),
                outcome = "ok",
            );
            Ok(())
        }
        Err(e) => {
            // Roll back the half-written destination so the next start
            // doesn't see a corrupt file.
            let _ = fs::remove_file(&target);
            for sidecar in ["state.db-wal", "state.db-shm"] {
                let _ = fs::remove_file(data_dir.join(sidecar));
            }
            warn!(
                event = "cli.restore_failed",
                src = %in_path.display(),
                error = ?e,
            );
            Err(match e {
                crate::store::BootError::SchemaTooNew { on_disk, target } => {
                    BackupError::SchemaTooNew {
                        found: on_disk,
                        target,
                    }
                }
                other => BackupError::MigrationFailed(format!("{other:?}")),
            })
        }
    }
}

/// `forward-server reset --confirm`. Removes `state.db` + sidecars
/// after verifying the file actually looks like a SQLite database
/// (R-011 — guard against operators typo'ing `--data-dir`).
pub fn run_reset(data_dir: &Path) -> Result<(), BackupError> {
    let target = data_dir.join(DATA_FILE_NAME);
    if !target.exists() {
        // Idempotent no-op.
        info!(
            event = "cli.reset_complete",
            data_dir = %data_dir.display(),
            outcome = "noop",
        );
        return Ok(());
    }
    verify_sqlite_signature(&target)?;
    fs::remove_file(&target).map_err(map_io)?;
    for sidecar in ["state.db-wal", "state.db-shm", "state.db.lock"] {
        let p = data_dir.join(sidecar);
        if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }
    info!(
        event = "cli.reset_complete",
        data_dir = %data_dir.display(),
        outcome = "ok",
    );
    Ok(())
}

fn verify_sqlite_signature(path: &Path) -> Result<(), BackupError> {
    let mut header = [0u8; 16];
    use std::io::Read;
    let mut f = fs::File::open(path).map_err(map_io)?;
    // A short read also fails the signature check — short / empty files
    // are NOT sqlite, so we don't surface an io error to the operator.
    if f.read_exact(&mut header).is_err() {
        return Err(BackupError::NotSqlite(path.to_path_buf()));
    }
    if header != SQLITE_HEADER {
        return Err(BackupError::NotSqlite(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn seed_store(dir: &Path) {
        let store = Store::open(dir).unwrap();
        store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO users (user_id, role, display_name, created_at) \
                     VALUES ('alice','user','Alice','2026-05-08T00:00:00Z')",
                    [],
                )
                .map_err(crate::store::map_rusqlite)?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn backup_round_trip_preserves_rows() {
        let src_dir = tempdir().unwrap();
        seed_store(src_dir.path());
        let dst = tempdir().unwrap();
        let dst_file = dst.path().join("snap.db");
        let written = run_backup(src_dir.path(), &dst_file).unwrap();
        assert_eq!(written, dst_file);
        // Restore into a fresh dir and assert the row survives.
        let restore_dir = tempdir().unwrap();
        run_restore(&dst_file, restore_dir.path(), false).unwrap();
        let restored = Store::open(restore_dir.path()).unwrap();
        let n: i64 = restored
            .with_conn(|c| {
                c.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
                    .map_err(crate::store::map_rusqlite)
            })
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn backup_refuses_to_overwrite_existing_destination() {
        let src_dir = tempdir().unwrap();
        seed_store(src_dir.path());
        let dst = tempdir().unwrap();
        let dst_file = dst.path().join("snap.db");
        // Pre-create the destination file.
        fs::write(&dst_file, b"").unwrap();
        let err = run_backup(src_dir.path(), &dst_file).unwrap_err();
        assert!(matches!(err, BackupError::DestinationExists(_)));
    }

    #[test]
    fn restore_refuses_non_empty_data_dir_without_force() {
        let src_dir = tempdir().unwrap();
        seed_store(src_dir.path());
        let dst = tempdir().unwrap();
        let snap = dst.path().join("snap.db");
        run_backup(src_dir.path(), &snap).unwrap();

        let target = tempdir().unwrap();
        seed_store(target.path()); // already populated.
        let err = run_restore(&snap, target.path(), false).unwrap_err();
        assert!(matches!(err, BackupError::DestinationNonEmpty(_)));

        // With --force, replaces successfully.
        run_restore(&snap, target.path(), true).unwrap();
    }

    #[test]
    fn restore_rejects_non_sqlite_file() {
        let bad = tempdir().unwrap();
        let p = bad.path().join("not-a-db.bin");
        fs::write(&p, b"this is not sqlite").unwrap();
        let target = tempdir().unwrap();
        let err = run_restore(&p, target.path(), false).unwrap_err();
        assert!(matches!(err, BackupError::NotSqlite(_)));
    }

    #[test]
    fn reset_removes_db_and_sidecars() {
        let dir = tempdir().unwrap();
        seed_store(dir.path());
        // Write fake sidecars so we know they get cleaned up.
        fs::write(dir.path().join("state.db-wal"), b"x").unwrap();
        fs::write(dir.path().join("state.db-shm"), b"x").unwrap();
        run_reset(dir.path()).unwrap();
        assert!(!dir.path().join("state.db").exists());
        assert!(!dir.path().join("state.db-wal").exists());
        assert!(!dir.path().join("state.db-shm").exists());
    }

    #[test]
    fn reset_refuses_non_sqlite_target() {
        let dir = tempdir().unwrap();
        // Plant a non-SQLite file at state.db.
        fs::write(dir.path().join("state.db"), b"definitely not sqlite").unwrap();
        let err = run_reset(dir.path()).unwrap_err();
        assert!(matches!(err, BackupError::NotSqlite(_)));
    }

    #[test]
    fn reset_on_empty_dir_is_noop() {
        let dir = tempdir().unwrap();
        run_reset(dir.path()).unwrap();
    }

    #[test]
    fn resolve_destination_appends_filename_for_directory() {
        let dir = tempdir().unwrap();
        let resolved = resolve_backup_destination(dir.path());
        let name = resolved.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("forward-state-")
                && std::path::Path::new(&name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("db"))
        );
    }
}
