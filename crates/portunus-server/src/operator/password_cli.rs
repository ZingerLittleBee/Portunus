//! Local break-glass password and onboarding-token CLI commands.

use std::io;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use chrono::Utc;
use portunus_auth::{IdentityStoreError, OperatorRole, UserId};

use crate::operator::audit::{AuditEntry, AuditOutcome};
use crate::operator::passwords::{PasswordError, hash_password};
use crate::store::Store;
use crate::store::operator_store::SqliteOperatorStore;

pub fn reset_password(
    data_dir: &Path,
    raw_user_id: &str,
    password_stdin: bool,
    temporary: bool,
) -> Result<(), u8> {
    let user_id = parse_user_id(raw_user_id)?;
    let operator_store = open_operator_store(data_dir)?;
    if operator_store.get_user(&user_id).is_none() {
        eprintln!("error: user_not_found: {}", user_id.as_str());
        return Err(8);
    }

    let password = if temporary {
        portunus_auth::token::generate_token()
    } else if password_stdin {
        read_password_stdin()?
    } else {
        read_password_prompted()?
    };
    let password_change_required = temporary;
    let password_hash = hash_password(&password).map_err(password_error_exit)?;
    let summary = operator_store
        .reset_password_state(&user_id, &password_hash, password_change_required, true)
        .map_err(identity_error_exit)?;

    let audit_result = operator_store.insert_audit_entry(&AuditEntry {
        timestamp: Utc::now(),
        actor: "_local_cli".into(),
        role: Some(OperatorRole::Superadmin),
        method: "CLI".into(),
        path: format!("reset-password {}", user_id.as_str()),
        outcome: AuditOutcome::Allow,
        reason: None,
        action: Some("operator.password_reset".into()),
        resource_kind: Some("user".into()),
        resource_value: Some(user_id.as_str().to_string()),
        details: Some(serde_json::json!({
            "sessions_revoked": summary.sessions_revoked,
            "temporary_password_generated": temporary,
            "password_change_required": password_change_required,
            "source": "local_cli",
        })),
    });
    if let Err(e) = audit_result {
        eprintln!("warning: audit_write_failed: {e}");
    }

    println!(
        "password_reset=ok user_id={} sessions_revoked={}",
        user_id.as_str(),
        summary.sessions_revoked
    );
    if temporary {
        println!("temporary_password={password}");
    }
    Ok(())
}

pub fn onboarding_token(data_dir: &Path) -> Result<(), u8> {
    let operator_store = open_operator_store(data_dir)?;
    if operator_store
        .has_active_superadmin()
        .map_err(identity_error_exit)?
    {
        eprintln!("error: already_bootstrapped (a superadmin already exists)");
        return Err(2);
    }
    let raw = operator_store
        .rotate_onboarding_setup_token(Utc::now())
        .map_err(identity_error_exit)?;
    println!("setup_token={raw}");
    Ok(())
}

fn open_operator_store(data_dir: &Path) -> Result<SqliteOperatorStore, u8> {
    std::fs::create_dir_all(data_dir).map_err(|e| {
        eprintln!("error: data_dir: {e}");
        1
    })?;
    let store = Store::open(data_dir).map_err(|e| {
        eprintln!("error: open_store: {e}");
        1
    })?;
    Ok(SqliteOperatorStore::new(Arc::new(store)))
}

fn parse_user_id(raw: &str) -> Result<UserId, u8> {
    if raw.starts_with('_') {
        Ok(UserId::reserved(raw))
    } else {
        UserId::from_str(raw).map_err(|e| {
            eprintln!("error: {}", e.code());
            3
        })
    }
}

fn read_password_stdin() -> Result<String, u8> {
    let mut line = String::new();
    io::stdin().read_line(&mut line).map_err(|e| {
        eprintln!("error: read_password_stdin: {e}");
        1
    })?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn read_password_prompted() -> Result<String, u8> {
    let first = rpassword::prompt_password("New password: ").map_err(|e| {
        eprintln!("error: read_password_tty: {e}");
        1
    })?;
    let second = rpassword::prompt_password("Confirm password: ").map_err(|e| {
        eprintln!("error: read_password_tty: {e}");
        1
    })?;
    if first != second {
        eprintln!("error: password_mismatch");
        return Err(3);
    }
    Ok(first)
}

fn password_error_exit(error: PasswordError) -> u8 {
    eprintln!("error: {error}");
    match error {
        PasswordError::TooShort | PasswordError::TooLong | PasswordError::Invalid => 3,
        PasswordError::HashFailed => 1,
    }
}

fn identity_error_exit(error: IdentityStoreError) -> u8 {
    match &error {
        IdentityStoreError::UserNotFound(user_id) => {
            eprintln!("error: user_not_found: {}", user_id.as_str());
            8
        }
        IdentityStoreError::UserAlreadyExists(user_id) => {
            eprintln!("error: user_already_exists: {}", user_id.as_str());
            2
        }
        _ => {
            eprintln!("error: {error}");
            1
        }
    }
}
