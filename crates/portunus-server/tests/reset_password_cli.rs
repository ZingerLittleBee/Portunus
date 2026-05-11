//! Local recovery CLI tests for `reset-password`.

use std::io::Write;
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn server_bin() -> &'static str {
    env!("CARGO_BIN_EXE_portunus-server")
}

fn bootstrap(data: &TempDir) {
    let output = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("bootstrap-superadmin")
        .arg("--name")
        .arg("ops")
        .output()
        .expect("bootstrap-superadmin");
    assert!(
        output.status.success(),
        "bootstrap failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn scalar_i64(data: &TempDir, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open_with_flags(
        data.path().join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open sqlite");
    conn.query_row(sql, [], |row| row.get(0)).expect("query")
}

fn scalar_string(data: &TempDir, sql: &str) -> String {
    let conn = rusqlite::Connection::open_with_flags(
        data.path().join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open sqlite");
    conn.query_row(sql, [], |row| row.get(0)).expect("query")
}

#[test]
fn reset_password_refuses_missing_user() {
    let data = TempDir::new().expect("data tempdir");
    let output = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("reset-password")
        .arg("alice")
        .arg("--temporary")
        .output()
        .expect("reset-password");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(8));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("user_not_found"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reset_password_from_stdin_revokes_sessions_and_api_tokens_and_redacts_audit() {
    let data = TempDir::new().expect("data tempdir");
    bootstrap(&data);

    let mut child = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("reset-password")
        .arg("_superadmin")
        .arg("--password-stdin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn reset-password");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"changed correct horse battery staple\n")
        .expect("write password");
    let output = child.wait_with_output().expect("reset-password output");

    assert!(
        output.status.success(),
        "reset failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("password_reset=ok"), "stdout={stdout}");
    assert!(!stdout.contains("changed correct horse battery staple"));

    assert_eq!(
        scalar_i64(
            &data,
            "SELECT COUNT(*) FROM credentials WHERE user_id = '_superadmin' AND status = 'active'",
        ),
        0
    );
    let password_hash = scalar_string(
        &data,
        "SELECT password_hash FROM users WHERE user_id = '_superadmin'",
    );
    assert!(password_hash.starts_with("$argon2"));

    let audit_json = scalar_string(
        &data,
        "SELECT details_json FROM audit WHERE action = 'operator.password_reset' ORDER BY seq DESC LIMIT 1",
    );
    assert!(audit_json.contains("sessions_revoked"));
    assert!(audit_json.contains("api_tokens_revoked"));
    assert!(!audit_json.contains("changed correct horse battery staple"));
}

#[test]
fn reset_temporary_password_prints_secret_once_and_keeps_api_tokens_when_requested() {
    let data = TempDir::new().expect("data tempdir");
    bootstrap(&data);

    let output = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("reset-password")
        .arg("_superadmin")
        .arg("--temporary")
        .arg("--keep-api-tokens")
        .output()
        .expect("reset-password");

    assert!(
        output.status.success(),
        "reset failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let temporary_password = stdout
        .lines()
        .find_map(|line| line.strip_prefix("temporary_password="))
        .expect("temporary password line");
    assert!(temporary_password.len() >= 32);
    assert_eq!(stdout.matches("temporary_password=").count(), 1);
    assert_eq!(
        scalar_i64(
            &data,
            "SELECT COUNT(*) FROM credentials WHERE user_id = '_superadmin' AND status = 'active'",
        ),
        1
    );
    assert_eq!(
        scalar_i64(
            &data,
            "SELECT password_change_required FROM users WHERE user_id = '_superadmin'",
        ),
        1
    );
}
