//! 008-sqlite-storage T054..T059 — CLI smoke for backup/restore/reset.

use std::process::Command;

use tempfile::TempDir;

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_forward-server")
}

fn bootstrap(data: &TempDir, cfg: &TempDir) {
    let out = Command::new(server_bin())
        .arg("--config-dir")
        .arg(cfg.path())
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

#[test]
fn backup_then_restore_roundtrip() {
    let cfg = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    bootstrap(&data, &cfg);

    // backup → file
    let snap_dir = TempDir::new().unwrap();
    let snap_file = snap_dir.path().join("snap.db");
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("backup")
        .arg("--out")
        .arg(&snap_file)
        .output()
        .expect("backup");
    assert!(
        out.status.success(),
        "backup failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(snap_file.exists());

    // Restore into a fresh data dir.
    let cfg2 = TempDir::new().unwrap();
    let data2 = TempDir::new().unwrap();
    let out = Command::new(server_bin())
        .arg("--config-dir")
        .arg(cfg2.path())
        .arg("--data-dir")
        .arg(data2.path())
        .arg("restore")
        .arg("--in")
        .arg(&snap_file)
        .output()
        .expect("restore");
    assert!(
        out.status.success(),
        "restore failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(data2.path().join("state.db").exists());
}

#[test]
fn backup_refuses_overwrite_without_replace() {
    let cfg = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    bootstrap(&data, &cfg);
    let snap_dir = TempDir::new().unwrap();
    let snap_file = snap_dir.path().join("snap.db");
    std::fs::write(&snap_file, b"placeholder").unwrap();
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("backup")
        .arg("--out")
        .arg(&snap_file)
        .output()
        .expect("backup");
    assert!(!out.status.success(), "should refuse to overwrite");
    assert_eq!(out.status.code(), Some(6));
}

#[test]
fn restore_refuses_non_empty_data_dir_without_force() {
    let cfg = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    bootstrap(&data, &cfg);
    let snap_dir = TempDir::new().unwrap();
    let snap_file = snap_dir.path().join("snap.db");
    Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("backup")
        .arg("--out")
        .arg(&snap_file)
        .output()
        .expect("backup");
    // Now try to restore into the same (already populated) data-dir.
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("restore")
        .arg("--in")
        .arg(&snap_file)
        .output()
        .expect("restore");
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(6));
    // With --force, succeeds.
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("restore")
        .arg("--in")
        .arg(&snap_file)
        .arg("--force")
        .output()
        .expect("restore --force");
    assert!(
        out.status.success(),
        "restore --force should succeed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn restore_rejects_garbage_file() {
    let bad = TempDir::new().unwrap();
    let p = bad.path().join("not-a-db.bin");
    std::fs::write(&p, b"not sqlite").unwrap();
    let data = TempDir::new().unwrap();
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("restore")
        .arg("--in")
        .arg(&p)
        .output()
        .expect("restore");
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn reset_dry_run_prints_path_and_keeps_db() {
    let cfg = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    bootstrap(&data, &cfg);
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("reset")
        .output()
        .expect("reset dry-run");
    assert!(out.status.success(), "dry-run must succeed");
    assert!(data.path().join("state.db").exists(), "dry-run must keep DB");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("dry-run"));
}

#[test]
fn reset_confirmed_removes_db() {
    let cfg = TempDir::new().unwrap();
    let data = TempDir::new().unwrap();
    bootstrap(&data, &cfg);
    assert!(data.path().join("state.db").exists());
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("reset")
        .arg("--confirm")
        .output()
        .expect("reset confirm");
    assert!(out.status.success());
    assert!(!data.path().join("state.db").exists());
}

#[test]
fn reset_refuses_typo_data_dir() {
    let bad = TempDir::new().unwrap();
    // Plant a non-SQLite file at state.db.
    std::fs::write(bad.path().join("state.db"), b"not sqlite").unwrap();
    let out = Command::new(server_bin())
        .arg("--data-dir")
        .arg(bad.path())
        .arg("reset")
        .arg("--confirm")
        .output()
        .expect("reset");
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(7));
    // The bogus file must still be there — reset refused.
    assert!(bad.path().join("state.db").exists());
}
