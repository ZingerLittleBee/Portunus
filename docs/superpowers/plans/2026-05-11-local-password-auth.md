# Local Password Auth Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship Portunus v1.1.0 local username/password Web UI login with first-run onboarding, server-side cookie sessions, CSRF protection, admin password reset, and local CLI break-glass recovery while keeping existing bearer API tokens working for CLI/automation.

**Architecture:** Keep the existing RBAC model (`users`, `credentials`, `grants`) and add password/session state around it. Add unauthenticated auth routes outside the current bearer middleware, then replace the bearer-only middleware with one auth seam that accepts Web session cookies or bearer API tokens. Update the Web UI to use cookie sessions and keep bearer credentials as explicit API tokens only.

**Tech Stack:** Rust 1.88, Axum 0.8, SQLite via rusqlite/refinery, `argon2` RustCrypto password hashing, existing `portunus_auth::token` high-entropy token generator for session/setup secrets, React 18, TanStack Query, i18next, Vitest/Playwright.

---

## Source Spec

- Design: `docs/superpowers/specs/2026-05-11-local-password-auth-design.md`
- Target release: `v1.1.0`

## Implementation Notes

- Run `cargo fmt`, never `cargo fmt --all`.
- For Rust changes, finish with `cargo clippy --all --benches --tests --examples --all-features`.
- Use frequent commits. Each task below ends with its own commit.
- Do not remove bearer API token support. Existing CLI scripts must keep working with `PORTUNUS_OPERATOR_TOKEN`.
- Do not add self-registration or email reset. Users are admin-created.
- Do not expose raw passwords, session secrets, setup tokens, or bearer tokens in logs.

## File Structure

Backend storage and primitives:

- Modify `Cargo.toml`: add RustCrypto `argon2` workspace dependency after dependency approval/research.
- Create `crates/portunus-server/src/operator/passwords.rs`: password policy, Argon2id hashing, PHC verification, password-change request structs.
- Create `crates/portunus-server/src/operator/sessions.rs`: Web session token generation, cookie constants, cookie parse/build helpers, session validation types.
- Create `crates/portunus-server/src/operator/setup_token.rs`: onboarding setup token generation, SQLite hash persistence, startup rotation, CLI rotation, and verification.
- Create `crates/portunus-server/src/operator/web_auth.rs`: HTTP handlers for status, onboarding, login, logout, self password change, admin password reset.
- Create `crates/portunus-server/src/operator/csrf.rs`: Origin/custom-header/content-type checks for cookie-authenticated writes.
- Modify `crates/portunus-core/src/config.rs`: add `operator_http_public_origin` so CSRF origin and cookie Secure policy are explicit instead of guessed from listen address.
- Modify `crates/portunus-server/src/operator/auth_layer.rs`: accept session-cookie auth and bearer auth through one seam; enforce CSRF for cookie writes.
- Modify `crates/portunus-server/src/operator/http.rs`: mount auth routes outside route-layer, then protect `/v1/*`.
- Modify `crates/portunus-server/src/operator/users.rs`: require/set initial password when creating local-password users.
- Modify `crates/portunus-server/src/operator/credentials.rs`: rename UI/API wording only where response labels or docs are affected; do not change bearer credential semantics.
- Modify `crates/portunus-server/src/operator/mod.rs`: export new modules.
- Modify `crates/portunus-server/src/state.rs`: store setup token state.
- Modify `crates/portunus-server/src/serve.rs`: generate and print setup token when no active superadmin exists.
- Modify `crates/portunus-server/src/main.rs`: add `reset-password <user_id>` and `onboarding-token` CLI commands.
- Create `crates/portunus-server/src/store/migrations/V006__add_local_password_auth.sql`: password/session/login-throttle schema.
- Modify `crates/portunus-server/src/store/operator_store.rs`: password/session/login-attempt persistence methods.
- Modify `crates/portunus-server/src/operator/audit.rs`: represent password-reset audit events without leaking secrets.
- Modify `crates/portunus-server/src/store/audit_writer.rs`: persist password-reset audit details in `details_json`.
- Modify `crates/portunus-server/tests/*`: add focused contract tests for onboarding, login/logout/session, CSRF, reset-password, migration, and compatibility.

Web UI:

- Modify `webui/src/api/client.ts`: use `credentials: "same-origin"`, add CSRF header on mutating requests, remove bearer injection for normal UI requests.
- Modify/remove `webui/src/auth/token-store.ts`: replace with migration helper that clears old `portunus.token`; stop storing login tokens.
- Modify `webui/src/auth/AuthGate.tsx`: session-cookie based identity loading, auth status handling.
- Modify `webui/src/auth/LoginPage.tsx`: username/password login.
- Create `webui/src/auth/OnboardingPage.tsx`: first-run admin creation with setup token.
- Create `webui/src/api/auth.ts`: typed auth endpoints.
- Modify `webui/src/App.tsx`: add onboarding route/status gate.
- Modify `webui/src/pages/UserCreate.tsx`: collect initial password and force-change option.
- Modify `webui/src/pages/UserDetail.tsx`: admin password reset and API-token wording.
- Modify `webui/src/components/Nav.tsx`: logout calls API instead of only clearing local storage.
- Modify `webui/src/i18n/en.json` and `webui/src/i18n/zh-CN.json`: login/onboarding/reset/API-token copy.
- Modify `webui/tests/e2e/fixtures/helpers.ts` and existing Playwright specs: replace bearer-login helpers with onboarding/session login helpers.

Docs:

- Modify `docs/content/docs/features/web-ui.mdx` and `docs/content/docs/zh/features/web-ui.mdx`.
- Modify `docs/content/docs/features/rbac.mdx` and `docs/content/docs/zh/features/rbac.mdx`.
- Modify `docs/content/docs/deployment/docker.mdx`, `docs/content/docs/zh/deployment/docker.mdx`, `docs/content/docs/deployment/systemd.mdx`, `docs/content/docs/zh/deployment/systemd.mdx`.
- Modify `docs/content/docs/operations/troubleshooting.mdx` and `docs/content/docs/zh/operations/troubleshooting.mdx`.
- Modify `docs/content/docs/cli/portunus-server.mdx` and `docs/content/docs/zh/cli/portunus-server.mdx`.
- Modify `docs/content/docs/api/operator-http.mdx` and `docs/content/docs/zh/api/operator-http.mdx`.

---

## Chunk 1: Backend Storage and Primitives

### Task 1: Add Password Hashing Dependency

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/portunus-server/Cargo.toml`

- [ ] **Step 1: Verify dependency choice**

Run:

```bash
cargo search argon2 --limit 3
```

Expected: RustCrypto `argon2` crate is current and maintained. If this result looks wrong, stop and confirm with the user before adding any dependency.

- [ ] **Step 2: Add workspace dependency**

In `Cargo.toml`, add:

```toml
argon2 = { version = "0.5", features = ["std"] }
```

In `crates/portunus-server/Cargo.toml`, add:

```toml
argon2 = { workspace = true }
```

- [ ] **Step 3: Verify metadata resolves**

Run:

```bash
cargo check -p portunus-server
```

Expected: PASS. No implementation uses `argon2` yet, but dependency resolution succeeds.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/portunus-server/Cargo.toml Cargo.lock
git commit -m "Add password hashing dependency"
```

### Task 2: Add Password Policy and Argon2 Helpers

**Files:**
- Create: `crates/portunus-server/src/operator/passwords.rs`
- Modify: `crates/portunus-server/src/operator/mod.rs`
- Test: `crates/portunus-server/src/operator/passwords.rs`

- [ ] **Step 1: Write failing unit tests**

Add tests to the bottom of `passwords.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_rejects_too_short_password() {
        assert_eq!(
            validate_password("short").unwrap_err(),
            PasswordError::TooShort
        );
    }

    #[test]
    fn policy_rejects_over_1024_utf8_bytes() {
        let pw = "a".repeat(1025);
        assert_eq!(validate_password(&pw).unwrap_err(), PasswordError::TooLong);
    }

    #[test]
    fn hash_round_trip_verifies() {
        let hash = hash_password("correct horse battery staple").expect("hash");
        verify_password("correct horse battery staple", &hash).expect("verify");
        assert_eq!(
            verify_password("wrong horse battery staple", &hash).unwrap_err(),
            PasswordError::Invalid
        );
        assert!(hash.starts_with("$argon2"));
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p portunus-server operator::passwords
```

Expected: FAIL because `passwords` module/functions do not exist.

- [ ] **Step 3: Implement password helper**

Create:

```rust
//! Local-password hashing and policy.

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use thiserror::Error;

pub const MIN_PASSWORD_CHARS: usize = 12;
pub const MAX_PASSWORD_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PasswordError {
    #[error("password_too_short")]
    TooShort,
    #[error("password_too_long")]
    TooLong,
    #[error("password_invalid")]
    Invalid,
    #[error("password_hash_failed")]
    HashFailed,
}

pub fn validate_password(password: &str) -> Result<(), PasswordError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(PasswordError::TooShort);
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(PasswordError::TooLong);
    }
    Ok(())
}

pub fn hash_password(password: &str) -> Result<String, PasswordError> {
    validate_password(password)?;
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| PasswordError::HashFailed)
}

pub fn verify_password(password: &str, encoded: &str) -> Result<(), PasswordError> {
    let parsed = PasswordHash::new(encoded).map_err(|_| PasswordError::Invalid)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| PasswordError::Invalid)
}
```

Modify `operator/mod.rs`:

```rust
pub mod passwords;
```

- [ ] **Step 4: Run tests to verify pass**

Run:

```bash
cargo test -p portunus-server operator::passwords
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/operator/passwords.rs crates/portunus-server/src/operator/mod.rs
git commit -m "Add local password hashing helpers"
```

### Task 3: Add SQLite Schema for Local Password Auth

**Files:**
- Create: `crates/portunus-server/src/store/migrations/V006__add_local_password_auth.sql`
- Modify: `crates/portunus-server/tests/store_schema_handshake.rs`

- [ ] **Step 1: Write failing schema test assertions**

In `store_schema_handshake.rs`, update the target-version assertion from 5 to 6 and add assertions that `users.password_hash`, `users.password_change_required`, `web_sessions`, and `login_attempts` exist.

Use this helper pattern:

```rust
fn column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("pragma");
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .expect("columns");
    rows.filter_map(Result::ok).any(|name| name == column)
}
```

- [ ] **Step 2: Run schema test to verify failure**

Run:

```bash
cargo test -p portunus-server store_schema_handshake::fresh_store_has_current_schema
```

Expected: FAIL because target schema is still 5 and V006 does not exist.

- [ ] **Step 3: Add V006 migration**

Create:

```sql
-- v1.1.0 local password auth.

ALTER TABLE users ADD COLUMN password_hash TEXT;
ALTER TABLE users ADD COLUMN password_change_required INTEGER NOT NULL DEFAULT 0 CHECK (password_change_required IN (0, 1));

CREATE TABLE web_sessions (
    session_hash         TEXT    PRIMARY KEY,
    user_id              TEXT    NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    created_at           TEXT    NOT NULL,
    last_seen_at         TEXT    NOT NULL,
    absolute_expires_at  TEXT    NOT NULL,
    revoked_at           TEXT,
    remote_addr          TEXT,
    user_agent           TEXT
) STRICT;

CREATE INDEX web_sessions_user_idx ON web_sessions(user_id);
CREATE INDEX web_sessions_expiry_idx ON web_sessions(absolute_expires_at, revoked_at);

CREATE TABLE login_attempts (
    subject        TEXT    NOT NULL,
    remote_addr    TEXT    NOT NULL,
    action         TEXT    NOT NULL CHECK (action IN ('login', 'onboarding', 'password_reset')),
    failures       INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0),
    first_failed_at TEXT,
    last_failed_at TEXT,
    locked_until   TEXT,
    PRIMARY KEY (subject, remote_addr, action)
) STRICT;

CREATE TABLE onboarding_setup (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    token_hash  TEXT    NOT NULL,
    issued_at   TEXT    NOT NULL,
    expires_at  TEXT    NOT NULL
) STRICT;
```

- [ ] **Step 4: Run schema test to verify pass**

Run:

```bash
cargo test -p portunus-server store_schema_handshake::fresh_store_has_current_schema
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/store/migrations/V006__add_local_password_auth.sql crates/portunus-server/tests/store_schema_handshake.rs
git commit -m "Add local password auth schema"
```

### Task 4: Add Password Persistence Methods

**Files:**
- Modify: `crates/portunus-server/src/store/operator_store.rs`
- Test: `crates/portunus-server/src/store/operator_store.rs`

- [ ] **Step 1: Write failing tests**

Add tests near existing store tests:

```rust
#[test]
fn password_hash_round_trips_without_exposing_in_user_view() {
    let (s, _dir) = test_store();
    let alice = UserId::from_str("alice").unwrap();
    s.add_user(User {
        id: alice.clone(),
        display_name: "Alice".into(),
        role: OperatorRole::User,
        created_at: Utc::now(),
        disabled: false,
    })
    .unwrap();

    s.set_password_hash(&alice, "$argon2id$fake", true).unwrap();
    let state = s.password_state(&alice).unwrap().unwrap();
    assert_eq!(state.hash, "$argon2id$fake");
    assert!(state.password_change_required);

    let public_user = s.get_user(&alice).unwrap();
    assert_eq!(public_user.id, alice);
}
```

- [ ] **Step 2: Run test to verify failure**

Run:

```bash
cargo test -p portunus-server store::operator_store::tests::password_hash_round_trips_without_exposing_in_user_view
```

Expected: FAIL because methods do not exist.

- [ ] **Step 3: Implement store methods**

Add a server-private type:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasswordState {
    pub hash: String,
    pub password_change_required: bool,
}
```

Add methods:

```rust
pub fn set_password_hash(
    &self,
    user_id: &UserId,
    hash: &str,
    password_change_required: bool,
) -> Result<(), IdentityStoreError> { /* UPDATE users ... */ }

pub fn password_state(&self, user_id: &UserId) -> Result<Option<PasswordState>, IdentityStoreError> { /* SELECT password_hash, password_change_required */ }
```

Map missing users to `IdentityStoreError::UserNotFound`.

- [ ] **Step 4: Run test to verify pass**

Run:

```bash
cargo test -p portunus-server store::operator_store::tests::password_hash_round_trips_without_exposing_in_user_view
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/store/operator_store.rs
git commit -m "Persist operator password state"
```

### Task 5: Add Web Session Store Methods

**Files:**
- Modify: `crates/portunus-server/src/store/operator_store.rs`
- Create: `crates/portunus-server/src/operator/sessions.rs`
- Modify: `crates/portunus-server/src/operator/mod.rs`
- Test: `crates/portunus-server/src/store/operator_store.rs`

- [ ] **Step 1: Write failing session tests**

Add tests:

```rust
#[test]
fn web_session_create_verify_revoke_round_trip() {
    let (s, _dir) = test_store();
    let alice = seed_user(&s, "alice");
    let raw = crate::operator::sessions::generate_session_secret();
    let hash = crate::operator::sessions::hash_session_secret(&raw);

    s.create_web_session(&hash, &alice, Utc::now(), Utc::now() + chrono::Duration::days(7), None, None)
        .unwrap();

    let session = s.verify_web_session(&hash, Utc::now()).unwrap().unwrap();
    assert_eq!(session.user_id, alice);

    s.revoke_web_session(&hash).unwrap();
    assert!(s.verify_web_session(&hash, Utc::now()).unwrap().is_none());
}
```

- [ ] **Step 2: Run test to verify failure**

Run:

```bash
cargo test -p portunus-server web_session_create_verify_revoke_round_trip
```

Expected: FAIL because session helpers/methods do not exist.

- [ ] **Step 3: Implement session primitives**

Create `sessions.rs`:

```rust
//! Web UI session cookie helpers.

use chrono::{DateTime, Duration, Utc};
use portunus_auth::token::{generate_token, hash_token};
use portunus_core::fingerprint;

pub const SESSION_COOKIE: &str = "portunus_session";
pub const IDLE_TIMEOUT: Duration = Duration::hours(8);
pub const ABSOLUTE_TIMEOUT: Duration = Duration::days(7);

#[must_use]
pub fn generate_session_secret() -> String {
    generate_token()
}

#[must_use]
pub fn hash_session_secret(secret: &str) -> String {
    fingerprint::hex(&hash_token(secret))
}

#[must_use]
pub fn session_is_expired(last_seen: DateTime<Utc>, absolute: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now > absolute || now > last_seen + IDLE_TIMEOUT
}
```

Add store methods:

```rust
pub fn create_web_session(...);
pub fn verify_web_session(...);
pub fn revoke_web_session(...);
pub fn revoke_web_sessions_for_user(...);
pub fn prune_expired_web_sessions(...);
```

Keep updates best-effort for `last_seen_at`; validation must reject revoked/expired sessions before cleanup.

- [ ] **Step 4: Run focused tests**

Run:

```bash
cargo test -p portunus-server web_session
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/operator/sessions.rs crates/portunus-server/src/operator/mod.rs crates/portunus-server/src/store/operator_store.rs
git commit -m "Persist operator web sessions"
```

### Task 6: Add Login Attempt Throttling

**Files:**
- Modify: `crates/portunus-server/src/store/operator_store.rs`
- Create: `crates/portunus-server/src/operator/throttle.rs`
- Modify: `crates/portunus-server/src/operator/mod.rs`
- Test: `crates/portunus-server/src/operator/throttle.rs`

- [ ] **Step 1: Write failing throttling tests**

```rust
#[test]
fn repeated_failures_trigger_bounded_lockout() {
    let mut state = ThrottleDecision::default();
    for _ in 0..5 {
        state.record_failure(Utc::now());
    }
    assert!(state.locked_until.is_some());
}
```

- [ ] **Step 2: Run test to verify failure**

Run:

```bash
cargo test -p portunus-server operator::throttle
```

Expected: FAIL because module does not exist.

- [ ] **Step 3: Implement throttle policy**

Use a simple bounded policy:

```rust
pub const LOCK_AFTER_FAILURES: u32 = 5;
pub const LOCKOUT_SECONDS: i64 = 60;
pub const MAX_LOCKOUT_SECONDS: i64 = 15 * 60;
```

Store failures in `login_attempts` by `(subject, remote_addr, action)`. Use generic subject `_unknown` for missing users to avoid enumeration.

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p portunus-server operator::throttle login_attempt
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/operator/throttle.rs crates/portunus-server/src/operator/mod.rs crates/portunus-server/src/store/operator_store.rs
git commit -m "Add auth attempt throttling"
```

---

## Chunk 2: HTTP Auth, Onboarding, and Recovery

### Task 7: Add Operator Public Origin Config

**Files:**
- Modify: `crates/portunus-core/src/config.rs`
- Modify: `crates/portunus-server/src/state.rs`
- Modify: `docs/content/docs/configuration/server.mdx`
- Modify: `docs/content/docs/zh/configuration/server.mdx`
- Test: `crates/portunus-core/src/config.rs`

- [ ] **Step 1: Write failing config tests**

Add tests that parse:

```toml
operator_http_public_origin = "https://ops.example.com"
```

Expected assertions:

```rust
assert_eq!(cfg.operator_http_public_origin.as_deref(), Some("https://ops.example.com"));
assert!(cfg.operator_http_cookie_secure());
assert_eq!(
    ServerConfig::default_for_data_dir(Path::new("/tmp")).operator_http_origin_for_csrf(),
    "http://127.0.0.1:7080"
);
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p portunus-core config::tests::server_config_public_origin
```

Expected: FAIL because config field/helpers do not exist.

- [ ] **Step 3: Implement config**

Add to `ServerConfig` and `ServerConfigToml`:

```rust
#[serde(default)]
pub operator_http_public_origin: Option<String>,
```

Add helpers:

```rust
pub fn operator_http_origin_for_csrf(&self) -> String {
    self.operator_http_public_origin.clone().unwrap_or_else(|| {
        format!("http://{}", self.operator_http_listen)
    })
}

pub fn operator_http_cookie_secure(&self) -> bool {
    self.operator_http_origin_for_csrf().starts_with("https://")
}
```

Validate configured origin:

- must start with `http://` or `https://`.
- must not end with `/`.
- must not contain path/query/fragment.

Thread this config into `AppState` as `operator_http_public_origin: String` and `operator_http_cookie_secure: bool`.

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p portunus-core config::tests::server_config_public_origin
cargo test -p portunus-server users_me_contract
```

Expected: PASS. Existing server tests should compile after `AppState::new` gets default origin values.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-core/src/config.rs crates/portunus-server/src/state.rs docs/content/docs/configuration/server.mdx docs/content/docs/zh/configuration/server.mdx
git commit -m "Add operator public origin config"
```

### Task 8: Add Setup Token State

**Files:**
- Create: `crates/portunus-server/src/operator/setup_token.rs`
- Modify: `crates/portunus-server/src/operator/mod.rs`
- Modify: `crates/portunus-server/src/serve.rs`
- Modify: `crates/portunus-server/src/store/operator_store.rs`
- Test: `crates/portunus-server/src/operator/setup_token.rs`

- [ ] **Step 1: Write failing setup-token tests**

```rust
#[test]
fn setup_token_verifies_then_expires() {
    let now = Utc::now();
    let (raw, record) = SetupTokenRecord::new(now, chrono::Duration::minutes(30));
    assert!(record.verify(&raw, now + chrono::Duration::minutes(1)));
    assert!(!record.verify(&raw, now + chrono::Duration::minutes(31)));
    assert!(!record.verify("wrong", now + chrono::Duration::minutes(1)));
}
```

- [ ] **Step 2: Run test to verify failure**

Run:

```bash
cargo test -p portunus-server operator::setup_token
```

Expected: FAIL because module does not exist.

- [ ] **Step 3: Implement setup token state**

Create:

```rust
pub struct SetupTokenRecord {
    hash_hex: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

impl SetupTokenRecord {
    pub fn new(now: DateTime<Utc>, ttl: Duration) -> (String, Self) { /* generate_token + hash */ }
    pub fn verify(&self, raw: &str, now: DateTime<Utc>) -> bool { /* constant-time */ }
    pub fn expires_at(&self) -> DateTime<Utc> { self.expires_at }
}
```

Add store methods:

```rust
pub fn rotate_onboarding_setup_token(&self, now: DateTime<Utc>) -> Result<String, IdentityStoreError>;
pub fn verify_onboarding_setup_token(&self, raw: &str, now: DateTime<Utc>) -> Result<bool, IdentityStoreError>;
pub fn clear_onboarding_setup_token(&self) -> Result<(), IdentityStoreError>;
```

`rotate_onboarding_setup_token` stores only the hash and expiry in SQLite. It returns the raw token once so `serve` or CLI can print it. Startup rotation overwrites any previous token, so tokens from a previous process are rejected after restart.

In `serve.rs`, after opening the store and before serving HTTP:

```rust
if !operator_store.has_any_superadmin() {
    let raw = operator_store.rotate_onboarding_setup_token(Utc::now())?;
    eprintln!("Portunus onboarding setup token: {raw}");
}
```

- [ ] **Step 4: Run setup-token tests**

Run:

```bash
cargo test -p portunus-server operator::setup_token
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/operator/setup_token.rs crates/portunus-server/src/operator/mod.rs crates/portunus-server/src/store/operator_store.rs crates/portunus-server/src/serve.rs
git commit -m "Persist onboarding setup token state"
```

### Task 9: Add Auth Status and Onboarding Endpoints

**Files:**
- Create: `crates/portunus-server/src/operator/web_auth.rs`
- Modify: `crates/portunus-server/src/operator/http.rs`
- Modify: `crates/portunus-server/src/operator/mod.rs`
- Test: `crates/portunus-server/tests/http_auth_onboarding_contract.rs`

- [ ] **Step 1: Write failing contract tests**

Create tests:

```rust
#[tokio::test]
async fn fresh_store_reports_onboarding_required() {
    let (router, _dir, _token) = build_router_without_superadmin();
    let resp = router.oneshot(req("GET", "/v1/auth/status", None, json!(null))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["onboarding_required"], true);
}

#[tokio::test]
async fn onboarding_requires_setup_token() {
    let (router, _dir, _token) = build_router_without_superadmin();
    let resp = router.oneshot(req("POST", "/v1/auth/onboarding", None, json!({
        "user_id": "admin",
        "display_name": "Admin",
        "password": "correct horse battery staple",
        "password_confirm": "correct horse battery staple",
        "setup_token": "wrong"
    }))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn onboarding_creates_first_superadmin_once() { /* expect 201, then 409/403 on repeat */ }
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p portunus-server --test http_auth_onboarding_contract
```

Expected: FAIL because routes do not exist.

- [ ] **Step 3: Implement routes**

In `web_auth.rs`, define:

```rust
#[derive(Serialize)]
pub struct AuthStatusResponse {
    pub onboarding_required: bool,
}

#[derive(Deserialize)]
pub struct OnboardingRequest {
    pub user_id: String,
    pub display_name: String,
    pub password: String,
    pub password_confirm: String,
    pub setup_token: String,
}
```

Onboarding behavior:

- reject if active superadmin exists.
- reject if setup token missing/invalid/expired.
- throttle failures by `(setup_token_present, remote_addr, onboarding)`.
- validate `user_id` via `UserId::from_str`.
- validate display name like `users::post_users`.
- validate password and confirmation.
- hash password.
- inside one store transaction, ensure no active superadmin and insert user + password hash.
- clear the stored setup token after successful first-admin creation.
- do not create API token.

Mount routes outside the protected route layer in `http.rs`:

```rust
.route("/v1/auth/status", get(web_auth::get_status))
.route("/v1/auth/onboarding", post(web_auth::post_onboarding))
```

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p portunus-server --test http_auth_onboarding_contract
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/operator/web_auth.rs crates/portunus-server/src/operator/http.rs crates/portunus-server/src/operator/mod.rs crates/portunus-server/tests/http_auth_onboarding_contract.rs
git commit -m "Add first-run onboarding API"
```

### Task 10: Add Login Endpoint

**Files:**
- Modify: `crates/portunus-server/src/operator/web_auth.rs`
- Modify: `crates/portunus-server/src/operator/http.rs`
- Modify: `crates/portunus-server/src/store/operator_store.rs`
- Test: `crates/portunus-server/tests/http_auth_session_contract.rs`

- [ ] **Step 1: Write failing login tests**

```rust
#[tokio::test]
async fn login_sets_http_only_session_cookie() {
    let (router, _dir) = build_router_with_password_user();
    let resp = router.oneshot(req("POST", "/v1/auth/login", None, json!({
        "user_id": "admin",
        "password": "correct horse battery staple"
    }))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(cookie.contains("portunus_session="));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p portunus-server --test http_auth_session_contract
```

Expected: FAIL because login route/session cookie is incomplete.

- [ ] **Step 3: Implement login**

Login:

- generic error for unknown user, missing password hash, bad password, disabled user.
- throttle failures.
- on success clear throttle, create session, return `204` with `Set-Cookie`.
- mount `POST /v1/auth/login` in `http.rs` outside the protected route layer.
- cookie format:

```text
portunus_session=<secret>; Path=/; HttpOnly; SameSite=Lax; Max-Age=604800
```

Set `Secure` when `state.operator_http_cookie_secure` is true. The current embedded operator listener is plain HTTP by default, so local Docker/systemd examples remain usable on `http://127.0.0.1:7080`; production docs must set `operator_http_public_origin = "https://..."` behind a TLS-terminating reverse proxy when exposing the UI beyond localhost.

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p portunus-server --test http_auth_session_contract
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/operator/web_auth.rs crates/portunus-server/src/operator/http.rs crates/portunus-server/src/store/operator_store.rs crates/portunus-server/tests/http_auth_session_contract.rs
git commit -m "Add web session login"
```

### Task 11: Add Session-or-Bearer Auth Seam, CSRF, and Logout

**Files:**
- Create: `crates/portunus-server/src/operator/csrf.rs`
- Modify: `crates/portunus-server/src/operator/auth_layer.rs`
- Modify: `crates/portunus-server/src/operator/http.rs`
- Modify: `crates/portunus-server/src/operator/mod.rs`
- Test: `crates/portunus-server/tests/http_auth_session_contract.rs`
- Test: `crates/portunus-server/tests/legacy_no_auth_rejected.rs`

- [ ] **Step 1: Write failing auth seam tests**

Add tests:

```rust
#[tokio::test]
async fn users_me_accepts_session_cookie() { /* login, then GET /v1/users/me with Cookie */ }

#[tokio::test]
async fn cookie_post_without_csrf_is_rejected() { /* login, POST /v1/users without X-Portunus-CSRF -> 403 */ }

#[tokio::test]
async fn bearer_post_does_not_need_csrf() { /* existing bearer POST /v1/users still 201 */ }

#[tokio::test]
async fn logout_requires_csrf_and_revokes_session() { /* login, logout without csrf -> 403, logout with csrf -> 204, users/me -> 401 */ }
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p portunus-server --test http_auth_session_contract --test legacy_no_auth_rejected
```

Expected: FAIL for new tests; existing bearer tests should still pass or reveal breakage.

- [ ] **Step 3: Implement auth seam**

In `auth_layer.rs`:

- keep bootstrap-required behavior for protected routes.
- authenticate session cookie first when present.
- fallback to bearer header.
- insert `OperatorIdentity` for both paths.
- update audit event with auth method but no secrets.
- when auth method is cookie and method is mutating, call `csrf::verify`.

In `csrf.rs`:

```rust
pub fn verify(req: &Request<Body>, operator_origin: &str) -> Result<(), RbacError> {
    // require Origin exact match, X-Portunus-CSRF: 1, JSON content-type for body methods
}
```

Use `state.operator_http_public_origin` from Task 7 as the exact allowed origin.

Add logout in `web_auth.rs` and mount it in `http.rs`:

- route: `POST /v1/auth/logout`.
- requires a valid session cookie.
- requires CSRF checks.
- revokes current session hash.
- returns expired cookie:

```text
portunus_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0
```

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p portunus-server --test http_auth_session_contract --test legacy_no_auth_rejected
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/operator/auth_layer.rs crates/portunus-server/src/operator/csrf.rs crates/portunus-server/src/operator/web_auth.rs crates/portunus-server/src/operator/http.rs crates/portunus-server/src/operator/mod.rs crates/portunus-server/tests/http_auth_session_contract.rs crates/portunus-server/tests/legacy_no_auth_rejected.rs
git commit -m "Accept session cookies on operator API"
```

### Task 12: Add Password Change and Admin Reset APIs

**Files:**
- Modify: `crates/portunus-server/src/operator/web_auth.rs`
- Modify: `crates/portunus-server/src/operator/http.rs`
- Modify: `crates/portunus-server/src/operator/audit.rs`
- Modify: `crates/portunus-server/src/store/audit_writer.rs`
- Modify: `crates/portunus-server/src/store/operator_store.rs`
- Test: `crates/portunus-server/tests/http_password_contract.rs`

- [ ] **Step 1: Write failing password contract tests**

```rust
#[tokio::test]
async fn self_password_change_requires_current_password() { /* wrong current -> 401 */ }

#[tokio::test]
async fn admin_reset_revokes_sessions_and_api_tokens_by_default() { /* reset, old session 401, old API token 401 */ }

#[tokio::test]
async fn admin_reset_can_keep_api_tokens_explicitly() { /* keep_api_tokens true */ }

#[tokio::test]
async fn admin_reset_writes_audit_event_without_password() { /* audit has operator.password_reset and no raw password */ }
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p portunus-server --test http_password_contract
```

Expected: FAIL because endpoints do not exist.

- [ ] **Step 3: Implement endpoints**

Routes:

```rust
.route("/v1/users/me/password", post(web_auth::post_self_password))
.route("/v1/users/{user_id}/password", post(web_auth::post_user_password))
```

Request bodies:

```rust
pub struct SelfPasswordRequest {
    pub current_password: String,
    pub new_password: String,
    pub new_password_confirm: String,
}

pub struct AdminPasswordResetRequest {
    pub new_password: Option<String>,
    pub temporary_password: Option<bool>,
    pub keep_api_tokens: Option<bool>,
}
```

Behavior:

- self-change verifies current password and clears `password_change_required`.
- admin reset requires superadmin role, revokes Web sessions, revokes API tokens unless `keep_api_tokens == Some(true)`.
- admin reset throttles repeated failed reset attempts by target user and remote address.
- generated temporary password is displayed once in response; never logged.
- write `operator.password_reset` audit event with actor, target user, outcome, `sessions_revoked`, and `api_tokens_revoked`.
- extend `AuditEntry`/durable audit mapping only enough to carry `action`, `resource_kind`, `resource_value`, and `details_json`; keep existing allow/deny rows backwards compatible with defaults.

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p portunus-server --test http_password_contract
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/operator/web_auth.rs crates/portunus-server/src/operator/http.rs crates/portunus-server/src/operator/audit.rs crates/portunus-server/src/store/audit_writer.rs crates/portunus-server/src/store/operator_store.rs crates/portunus-server/tests/http_password_contract.rs
git commit -m "Add password change and reset APIs"
```

### Task 13: Update User Creation for Initial Passwords

**Files:**
- Modify: `crates/portunus-server/src/operator/users.rs`
- Modify: `crates/portunus-server/tests/http_users_contract.rs`

- [ ] **Step 1: Write failing user-create test**

Add:

```rust
#[tokio::test]
async fn post_users_accepts_initial_password_without_issuing_api_token() {
    let (router, _d) = build_router();
    let resp = router.clone().oneshot(req("POST", "/v1/users", SUPERADMIN_TOKEN, json!({
        "user_id": "alice",
        "display_name": "Alice",
        "initial_password": "correct horse battery staple",
        "password_change_required": true
    }))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
}
```

- [ ] **Step 2: Run test to verify failure**

Run:

```bash
cargo test -p portunus-server --test http_users_contract post_users_accepts_initial_password_without_issuing_api_token
```

Expected: FAIL until body/schema supports initial password.

- [ ] **Step 3: Implement user create extension**

Extend `CreateUserBody`:

```rust
pub initial_password: Option<String>,
#[serde(default)]
pub password_change_required: bool,
```

After `add_user`, if `initial_password` exists, hash it and call `set_password_hash`. If hashing fails, rollback by adding a store method that inserts user+password in one transaction instead of two writes. Prefer atomic store method:

```rust
pub fn add_user_with_password(&self, user: User, password_hash: Option<&str>, change_required: bool)
```

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test -p portunus-server --test http_users_contract
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/operator/users.rs crates/portunus-server/src/store/operator_store.rs crates/portunus-server/tests/http_users_contract.rs
git commit -m "Support initial user passwords"
```

### Task 14: Add Local CLI Reset Password and Onboarding Token Rotation

**Files:**
- Modify: `Cargo.toml`
- Modify: `crates/portunus-server/Cargo.toml`
- Modify: `crates/portunus-server/src/main.rs`
- Create: `crates/portunus-server/src/operator/password_cli.rs`
- Modify: `crates/portunus-server/src/operator/mod.rs`
- Modify: `crates/portunus-server/src/store/operator_store.rs`
- Test: `crates/portunus-server/tests/reset_password_cli.rs`
- Test: `crates/portunus-server/tests/onboarding_token_cli.rs`

- [ ] **Step 1: Write failing CLI tests**

```rust
#[test]
fn reset_password_refuses_missing_user() {
    let data = tempfile::tempdir().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("portunus-server");
    let output = std::process::Command::new(bin)
        .arg("--data-dir").arg(data.path())
        .arg("reset-password")
        .arg("alice")
        .arg("--temporary")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("user_not_found"));
}

#[test]
fn onboarding_token_prints_new_token_for_unbootstrapped_store() {
    let data = tempfile::tempdir().unwrap();
    let bin = assert_cmd::cargo::cargo_bin("portunus-server");
    let output = std::process::Command::new(bin)
        .arg("--data-dir").arg(data.path())
        .arg("onboarding-token")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("setup_token="));
}
```

- [ ] **Step 2: Run test to verify failure**

Run:

```bash
cargo test -p portunus-server --test reset_password_cli
cargo test -p portunus-server --test onboarding_token_cli
```

Expected: FAIL because commands do not exist.

- [ ] **Step 3: Implement CLI**

Add dependency after confirming it resolves:

```toml
rpassword = "7"
```

Commands:

```rust
ResetPassword {
    user_id: String,
    #[arg(long)]
    password_stdin: bool,
    #[arg(long)]
    temporary: bool,
    #[arg(long)]
    keep_api_tokens: bool,
}

OnboardingToken
```

Behavior:

- open store directly.
- require user exists.
- if `--temporary`, generate a random password and print once.
- if not temporary, prompt twice without echo via `rpassword`; `--password-stdin` is allowed for automation tests and reads one line from stdin.
- set password hash.
- revoke Web sessions.
- revoke API tokens unless `--keep-api-tokens`.
- write `operator.password_reset` audit event with the same fields as the Web reset path.
- `onboarding-token` refuses when an active superadmin exists.
- `onboarding-token` rotates the stored setup token hash, expires it after 30 minutes, and prints the raw token exactly once.

- [ ] **Step 4: Run CLI tests**

Run:

```bash
cargo test -p portunus-server --test reset_password_cli
cargo test -p portunus-server --test onboarding_token_cli
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/portunus-server/Cargo.toml Cargo.lock crates/portunus-server/src/main.rs crates/portunus-server/src/operator/password_cli.rs crates/portunus-server/src/operator/mod.rs crates/portunus-server/src/store/operator_store.rs crates/portunus-server/tests/reset_password_cli.rs crates/portunus-server/tests/onboarding_token_cli.rs
git commit -m "Add local auth recovery CLIs"
```

---

## Chunk 3: Web UI

### Task 15: Switch API Client to Cookie Sessions and CSRF

**Files:**
- Modify: `webui/src/api/client.ts`
- Modify: `webui/src/auth/token-store.ts`
- Test: `webui/src/api/client.test.ts`

- [ ] **Step 1: Write failing Vitest tests**

```ts
import { describe, expect, it, vi } from "vitest";
import { apiFetch } from "@/api/client";

describe("apiFetch auth", () => {
  it("sends same-origin credentials and csrf header for POST", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    await apiFetch("/v1/users", { method: "POST", body: JSON.stringify({}) });
    const [, init] = fetchMock.mock.calls[0];
    expect(init.credentials).toBe("same-origin");
    expect(new Headers(init.headers).get("X-Portunus-CSRF")).toBe("1");
  });
});
```

- [ ] **Step 2: Run test to verify failure**

Run:

```bash
cd webui && pnpm test -- client.test.ts
```

Expected: FAIL because client uses bearer token and `credentials: "omit"`.

- [ ] **Step 3: Implement client changes**

Rules:

- remove normal bearer injection from `apiFetch`, `apiFetchText`, and `streamSse`.
- set `credentials: "same-origin"`.
- add `X-Portunus-CSRF: 1` for `POST`, `PUT`, `PATCH`, `DELETE`.
- keep `Content-Type: application/json` behavior.
- keep 401 event.
- change `token-store.ts` into:

```ts
const LEGACY_TOKEN_KEY = "portunus.token";

export function clearLegacyToken(): void {
  try {
    window.sessionStorage.removeItem(LEGACY_TOKEN_KEY);
  } catch {
    /* ignore */
  }
}
```

- [ ] **Step 4: Run tests**

Run:

```bash
cd webui && pnpm test -- client.test.ts
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add webui/src/api/client.ts webui/src/auth/token-store.ts webui/src/api/client.test.ts
git commit -m "Use cookie sessions in web API client"
```

### Task 16: Add Auth API Types

**Files:**
- Create: `webui/src/api/auth.ts`

- [ ] **Step 1: Add typed API wrapper**

Create:

```ts
import { apiFetch } from "@/api/client";

export interface AuthStatus {
  onboarding_required: boolean;
}

export interface LoginRequest {
  user_id: string;
  password: string;
}

export interface OnboardingRequest {
  user_id: string;
  display_name: string;
  password: string;
  password_confirm: string;
  setup_token: string;
}

export function getAuthStatus(): Promise<AuthStatus> {
  return apiFetch<AuthStatus>("/v1/auth/status");
}

export function login(body: LoginRequest): Promise<void> {
  return apiFetch<void>("/v1/auth/login", { method: "POST", body: JSON.stringify(body) });
}

export function logout(): Promise<void> {
  return apiFetch<void>("/v1/auth/logout", { method: "POST" });
}

export function onboard(body: OnboardingRequest): Promise<void> {
  return apiFetch<void>("/v1/auth/onboarding", { method: "POST", body: JSON.stringify(body) });
}
```

- [ ] **Step 2: Typecheck**

Run:

```bash
cd webui && pnpm build
```

Expected: PASS or fail only on unused exports if lint/build rules complain. If unused export fails, continue after wiring pages in next task before committing.

- [ ] **Step 3: Commit**

```bash
git add webui/src/api/auth.ts
git commit -m "Add web auth API client"
```

### Task 17: Replace Login and AuthGate

**Files:**
- Modify: `webui/src/auth/LoginPage.tsx`
- Modify: `webui/src/auth/AuthGate.tsx`
- Modify: `webui/src/App.tsx`
- Create: `webui/src/auth/OnboardingPage.tsx`
- Modify: `webui/src/i18n/en.json`
- Modify: `webui/src/i18n/zh-CN.json`
- Test: `webui/src/auth/AuthGate.test.tsx`

- [ ] **Step 1: Write failing UI auth tests**

Test expected behavior:

- no session -> navigate `/login`.
- login form has user ID and password, no bearer field.
- onboarding page appears when `getAuthStatus()` returns `onboarding_required: true`.

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cd webui && pnpm test -- AuthGate.test.tsx
```

Expected: FAIL because current UI expects bearer token.

- [ ] **Step 3: Implement session AuthGate**

AuthGate:

- call `fetchIdentity()` unconditionally for protected routes.
- no local token state.
- on 401 remove `ME_QUERY_KEY` and navigate login.
- call `clearLegacyToken()` once on app mount.

LoginPage:

- fields `user_id`, `password`.
- submit `login()` then fetch `/v1/users/me`.

OnboardingPage:

- fields `setup_token`, `user_id`, `display_name`, `password`, `password_confirm`.
- submit `onboard()`, then call `login()` with same password.

App:

- route `/onboarding`.
- status gate that redirects fresh stores to onboarding.

- [ ] **Step 4: Run UI tests**

Run:

```bash
cd webui && pnpm test -- AuthGate.test.tsx
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add webui/src/auth/LoginPage.tsx webui/src/auth/AuthGate.tsx webui/src/auth/OnboardingPage.tsx webui/src/App.tsx webui/src/i18n/en.json webui/src/i18n/zh-CN.json webui/src/auth/AuthGate.test.tsx
git commit -m "Replace bearer login with local password UI"
```

### Task 18: Update User Management UI

**Files:**
- Modify: `webui/src/pages/UserCreate.tsx`
- Modify: `webui/src/pages/UserDetail.tsx`
- Modify: `webui/src/components/Nav.tsx`
- Modify: `webui/src/components/TokenRevealModal.tsx`
- Modify: `webui/src/api/users.ts`
- Modify: `webui/src/i18n/en.json`
- Modify: `webui/src/i18n/zh-CN.json`
- Modify: `webui/tests/e2e/fixtures/helpers.ts`
- Modify: `webui/tests/e2e/token-leak-audit.spec.ts`
- Modify: `webui/tests/e2e/quickstart-walkthrough.spec.ts`
- Modify: `webui/tests/e2e/us1-superadmin.spec.ts`
- Modify: `webui/tests/e2e/us2-tenant-isolation.spec.ts`
- Modify: `webui/tests/e2e/us3-audit-and-metrics.spec.ts`

- [ ] **Step 1: Write/update UI tests**

Add or update tests to assert:

- new-user form includes initial password.
- reset password action exists for superadmin.
- API-token modal says API token, not login token.
- sign out calls `POST /v1/auth/logout`.
- Playwright helpers log in with user ID/password and assert no bearer token is stored in `sessionStorage`.

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cd webui && pnpm test
```

Expected: FAIL on tests for new UI behavior.

- [ ] **Step 3: Implement UI changes**

User create:

- fields: user ID, display name, initial password, force change.
- send `initial_password` and `password_change_required`.

User detail:

- add reset password dialog.
- default checkbox: revoke API tokens checked.
- show one-time temporary password if generated.

Nav:

- call `logout()` then clear query cache and navigate `/login`.

Token reveal:

- change copy to "API token" in i18n.

Playwright:

- replace `#bearer` helpers with onboarding/session login helpers.
- update token-leak audit to assert API tokens appear only in one-time reveal/API-token flows, not as the Web login secret.
- keep bearer API token assertions for CLI/API compatibility where relevant.

- [ ] **Step 4: Run tests**

Run:

```bash
cd webui && pnpm test
cd webui && pnpm test:e2e
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add webui/src/pages/UserCreate.tsx webui/src/pages/UserDetail.tsx webui/src/components/Nav.tsx webui/src/components/TokenRevealModal.tsx webui/src/api/users.ts webui/src/i18n/en.json webui/src/i18n/zh-CN.json webui/tests/e2e
git commit -m "Add password management UI"
```

---

## Chunk 4: Docs and End-to-End Verification

### Task 19: Update User-Facing Docs

**Files:**
- Modify: `docs/content/docs/features/web-ui.mdx`
- Modify: `docs/content/docs/zh/features/web-ui.mdx`
- Modify: `docs/content/docs/features/rbac.mdx`
- Modify: `docs/content/docs/zh/features/rbac.mdx`
- Modify: `docs/content/docs/deployment/docker.mdx`
- Modify: `docs/content/docs/zh/deployment/docker.mdx`
- Modify: `docs/content/docs/deployment/systemd.mdx`
- Modify: `docs/content/docs/zh/deployment/systemd.mdx`
- Modify: `docs/content/docs/operations/troubleshooting.mdx`
- Modify: `docs/content/docs/zh/operations/troubleshooting.mdx`
- Modify: `docs/content/docs/cli/portunus-server.mdx`
- Modify: `docs/content/docs/zh/cli/portunus-server.mdx`
- Modify: `docs/content/docs/api/operator-http.mdx`
- Modify: `docs/content/docs/zh/api/operator-http.mdx`

- [ ] **Step 1: Update docs**

Document:

- first-run onboarding.
- setup token source, 30-minute expiry, restart invalidates old token.
- Web login uses user ID/password.
- admin-created users only.
- API tokens are separate from Web login.
- normal user password reset by superadmin.
- final superadmin recovery:

```bash
portunus-server --data-dir /var/lib/portunus reset-password _superadmin --temporary
```

- no remote reset path for final admin.

- [ ] **Step 2: Search for stale bearer-login wording**

Run:

```bash
rg -n 'Paste your operator bearer token|sessionStorage|login token|粘贴你的 operator bearer token|仅存于 `sessionStorage`' docs/content webui/src
```

Expected: no stale Web login wording remains. API token docs may still mention bearer tokens.

- [ ] **Step 3: Build docs**

Run:

```bash
cd docs && pnpm build
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add docs/content webui/src/i18n
git commit -m "Document local password authentication"
```

### Task 20: Run Backend Verification

**Files:**
- No direct edits unless failures require fixes.

- [ ] **Step 1: Format**

Run:

```bash
cargo fmt
```

Expected: no unrelated submodule formatting.

- [ ] **Step 2: Run focused backend tests**

Run:

```bash
cargo test -p portunus-server --test http_auth_onboarding_contract
cargo test -p portunus-server --test http_auth_session_contract
cargo test -p portunus-server --test http_password_contract
cargo test -p portunus-server --test reset_password_cli
cargo test -p portunus-server store_schema_handshake
```

Expected: PASS.

- [ ] **Step 3: Run compatibility tests**

Run:

```bash
cargo test -p portunus-server --test legacy_no_auth_rejected
cargo test -p portunus-server --test http_users_contract
cargo test -p portunus-server --test http_credentials_contract
cargo test -p portunus-server --test users_me_contract
```

Expected: PASS. This proves bearer API tokens still work.

- [ ] **Step 4: Run clippy**

Run:

```bash
cargo clippy --all --benches --tests --examples --all-features
```

Expected: PASS with no warnings.

- [ ] **Step 5: Commit any fixes**

If verification required changes:

```bash
git add <fixed-files>
git commit -m "Fix local auth verification issues"
```

### Task 21: Run Web UI Verification

**Files:**
- No direct edits unless failures require fixes.

- [ ] **Step 1: Run UI tests**

Run:

```bash
cd webui && pnpm test
cd webui && pnpm test:e2e
```

Expected: PASS.

- [ ] **Step 2: Run UI lint/build**

Run:

```bash
cd webui && pnpm lint
cd webui && pnpm build
```

Expected: PASS.

- [ ] **Step 3: Run browser smoke**

Start server with a fresh data dir and open the Web UI:

```bash
cargo run -p portunus-server -- --data-dir ./tmp-local-auth serve --operator-http-listen 127.0.0.1:7080
```

In a browser:

- visit `http://127.0.0.1:7080/`.
- verify onboarding appears.
- use printed setup token.
- create admin.
- log out.
- log in with password.
- create a normal user.
- reset that user's password.
- issue an API token and verify CLI bearer path still works.

Expected: all flows work.

- [ ] **Step 4: Commit any fixes**

If verification required changes:

```bash
git add <fixed-files>
git commit -m "Fix local auth web verification issues"
```

### Task 22: Final Repo Verification

**Files:**
- No direct edits unless failures require fixes.

- [ ] **Step 1: Check git status**

Run:

```bash
git status --short
```

Expected: clean, or only intentional uncommitted fixes.

- [ ] **Step 2: Run final diff check**

Run:

```bash
git diff --check
```

Expected: PASS, no output.

- [ ] **Step 3: Summarize implementation**

Prepare handoff with:

- commits made.
- tests run.
- docs updated.
- any remaining risks or follow-ups.
