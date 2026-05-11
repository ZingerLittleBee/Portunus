//! Local recovery CLI tests for `onboarding-token`.

use std::process::Command;

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

fn token_hash(data: &TempDir) -> String {
    let conn = rusqlite::Connection::open_with_flags(
        data.path().join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open sqlite");
    conn.query_row(
        "SELECT token_hash FROM onboarding_setup WHERE id = 1",
        [],
        |row| row.get(0),
    )
    .expect("query token hash")
}

fn expires_delta_seconds(data: &TempDir) -> i64 {
    let conn = rusqlite::Connection::open_with_flags(
        data.path().join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("open sqlite");
    let (issued_at, expires_at): (String, String) = conn
        .query_row(
            "SELECT issued_at, expires_at FROM onboarding_setup WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query ttl");
    let issued_at = chrono::DateTime::parse_from_rfc3339(&issued_at)
        .expect("issued_at rfc3339")
        .with_timezone(&chrono::Utc);
    let expires_at = chrono::DateTime::parse_from_rfc3339(&expires_at)
        .expect("expires_at rfc3339")
        .with_timezone(&chrono::Utc);
    (expires_at - issued_at).num_seconds()
}

#[test]
fn onboarding_token_prints_new_token_for_unbootstrapped_store() {
    let data = TempDir::new().expect("data tempdir");
    let output = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("onboarding-token")
        .output()
        .expect("onboarding-token");

    assert!(
        output.status.success(),
        "onboarding-token failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let token = stdout
        .trim()
        .strip_prefix("setup_token=")
        .expect("setup_token output");
    assert!(token.len() >= 32);
    assert!(!token_hash(&data).contains(token));
    assert_eq!(expires_delta_seconds(&data), 30 * 60);
}

#[test]
fn onboarding_token_rotates_existing_token() {
    let data = TempDir::new().expect("data tempdir");
    let first = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("onboarding-token")
        .output()
        .expect("first onboarding-token");
    assert!(first.status.success());
    let first_stdout = String::from_utf8_lossy(&first.stdout).trim().to_string();
    let first_hash = token_hash(&data);

    let second = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("onboarding-token")
        .output()
        .expect("second onboarding-token");
    assert!(second.status.success());
    let second_stdout = String::from_utf8_lossy(&second.stdout).trim().to_string();
    let second_hash = token_hash(&data);

    assert_ne!(first_stdout, second_stdout);
    assert_ne!(first_hash, second_hash);
}

#[test]
fn onboarding_token_refuses_when_active_superadmin_exists() {
    let data = TempDir::new().expect("data tempdir");
    bootstrap(&data);

    let output = Command::new(server_bin())
        .arg("--data-dir")
        .arg(data.path())
        .arg("onboarding-token")
        .output()
        .expect("onboarding-token");

    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("already_bootstrapped"),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}
