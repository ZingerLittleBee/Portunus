//! T032 (005-multi-user-rbac, US2) — bootstrap & gen-token CLI helpers.
//!
//! Two distinct entry points operators reach for to seed an empty
//! `identity.json`:
//!
//! 1. `bootstrap-superadmin --name <display>` — interactive single-shot
//!    creation of the canonical `_superadmin` user + an Active credential.
//!    The freshly minted bearer token is printed to stdout EXACTLY ONCE
//!    and never logged. Exits 2 if a superadmin already exists.
//!
//! 2. `gen-token` — pure helper that prints a fresh
//!    [`forward_auth::token::generate_token`] result to stdout. Useful
//!    for generating an `operator_token` value to paste into `server.toml`
//!    out-of-band, before first start.
//!
//! The `operator_token` server.toml shortcut itself lives in
//! `serve.rs::run` (it bootstraps in-process at startup), not here —
//! this file only owns the explicit operator-invoked paths.

use std::path::Path;

use chrono::Utc;
use forward_auth::OperatorAuthenticator;
use forward_auth::operator_store::{FileOperatorStore, IdentityStoreError};
use forward_auth::{
    Credential, CredentialId, CredentialStatus, OperatorRole, User, UserId,
    token::{generate_token, hash_token},
};
use tracing::{info, warn};

/// Exit code surfaced by the binary when an operator action is rejected.
/// Mirrors the values frozen in `contracts/operator-api.md` § "CLI Exit Codes".
pub const EXIT_OK: u8 = 0;
pub const EXIT_GENERIC: u8 = 1;
pub const EXIT_ALREADY_BOOTSTRAPPED: u8 = 2;
pub const EXIT_VALIDATION: u8 = 3;

/// Implementation behind `forward-server bootstrap-superadmin --name <…>`.
///
/// On success, prints `superadmin user_id=_superadmin token=<43-char>` to
/// stdout and returns `EXIT_OK`. On already-bootstrapped, emits a clear
/// error to stderr and returns `EXIT_ALREADY_BOOTSTRAPPED`.
///
/// The raw token NEVER reaches `tracing` — only stdout. We only emit a
/// single INFO audit line `event = "operator.bootstrap"` carrying the
/// post-creation user_id and credential id (no token field).
pub fn bootstrap_superadmin(identity_path: &Path, display_name: &str) -> u8 {
    let store = match FileOperatorStore::open(identity_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: open operator store: {e}");
            return EXIT_GENERIC;
        }
    };

    if store.has_any_superadmin() {
        eprintln!("error: already_bootstrapped (a superadmin already exists)");
        return EXIT_ALREADY_BOOTSTRAPPED;
    }

    let display_name = display_name.trim();
    if display_name.is_empty() || display_name.len() > 64 {
        eprintln!("error: invalid_display_name (length must be 1..=64 chars)");
        return EXIT_VALIDATION;
    }

    let raw = generate_token();
    let user = User {
        id: UserId::superadmin(),
        display_name: display_name.to_string(),
        role: OperatorRole::Superadmin,
        created_at: Utc::now(),
        disabled: false,
    };
    let cred = Credential {
        id: CredentialId::new(),
        user_id: user.id.clone(),
        token_hash: hash_token(&raw),
        label: Some("bootstrap".to_string()),
        created_at: Utc::now(),
        last_used_at: None,
        status: CredentialStatus::active(),
    };

    if let Err(e) = persist_pair(&store, user.clone(), cred.clone()) {
        eprintln!("error: persist superadmin: {e}");
        return EXIT_GENERIC;
    }

    info!(
        event = "operator.bootstrap",
        actor = "_anonymous",
        new_user = %user.id,
        new_credential = %cred.id,
        outcome = "ok",
    );

    println!("superadmin user_id={} token={}", user.id, raw);
    EXIT_OK
}

/// Implementation behind `forward-server gen-token`.
///
/// Prints a fresh URL-safe-base64 token (43 chars) to stdout, followed
/// by a newline. Always succeeds; useful for seeding `operator_token`
/// in `server.toml` out-of-band.
#[must_use]
pub fn gen_token() -> u8 {
    println!("{}", generate_token());
    EXIT_OK
}

/// Atomic helper: insert user + credential in a single store mutation.
/// `FileOperatorStore` doesn't expose a combined "create user + credential
/// with raw token" path (the regular `issue_credential` always generates
/// a random token), so we mirror what the
/// [`FileOperatorStore::bootstrap_legacy_superadmin`] path does for the
/// `_legacy` user — but with the canonical `_superadmin` id.
///
/// Implementation note: rather than duplicate atomic-write logic here,
/// we reuse `bootstrap_legacy_superadmin`-style sequencing — we have to
/// poke at the store via its public surface. The cleanest path is two
/// public methods, both of which already commit atomically:
/// `add_user` then a synthetic `issue_credential` — but the latter
/// generates a random token. So we drop down to a direct atomic-write
/// helper exposed by the store (see [`bootstrap_pair`]).
fn persist_pair(
    store: &FileOperatorStore,
    user: User,
    cred: Credential,
) -> Result<(), IdentityStoreError> {
    store.bootstrap_pair(user, cred)
}

/// Emit a structured WARN line when the `operator_token` shortcut is
/// silently ignored on a non-empty store. Called from `serve.rs` when
/// the store already has a superadmin — see `data-model.md` edge case
/// "operator_token shortcut on already-bootstrapped store".
#[allow(dead_code)]
pub fn log_config_token_ignored() {
    warn!(
        event = "operator.config_token_ignored",
        reason = "superadmin_already_exists",
    );
}
