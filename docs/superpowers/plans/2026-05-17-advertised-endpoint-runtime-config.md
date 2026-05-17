# Advertised Endpoint Runtime-Config — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the credential-bundle advertised endpoint runtime-configurable via SQLite + a Web UI field, resolved once at enrollment creation and replayed at redeem, with cert-SAN validation.

**Architecture:** New `server_settings` SQLite singleton holds an operator override. A new `advertised` module resolves `host:port` at enrollment-creation time via a tiered, SAN-filtered, fail-closed resolver and persists the result on the `client_enrollments` row; `redeem()` replays that frozen value. Explicit config (SQLite/CLI/env) that is malformed or not SAN-covered is a hard error; implicit tiers (request-Host derive, loopback) skip-and-fall-through.

**Tech Stack:** Rust 2024, refinery SQLite migrations, rusqlite, axum operator HTTP, tonic gRPC, `x509-parser` (new workspace dep) for SAN extraction, React+Vite Web UI.

**Spec:** `docs/superpowers/specs/2026-05-17-advertised-endpoint-runtime-config-design.md`

**House rules:** Every Rust test/build command is prefixed `PORTUNUS_SKIP_WEBUI=1`. Commit after every green step. Run `cargo fmt --all` before each commit. Workspace lints gate on `-D warnings` (`clippy::pedantic`).

---

## File Structure

**Create:**
- `crates/portunus-server/src/store/migrations/V010__add_server_settings.sql` — schema.
- `crates/portunus-server/src/store/settings_store.rs` — `SqliteSettingsStore` get/set.
- `crates/portunus-server/src/advertised/mod.rs` — public types: `ResolvedAdvertisedEndpoint`, `EndpointSource`, `ResolveEndpointError`; re-exports.
- `crates/portunus-server/src/advertised/grammar.rs` — `validate_authority`.
- `crates/portunus-server/src/advertised/host_header.rs` — `host_from_header`.
- `crates/portunus-server/src/advertised/san.rs` — `CertSanSet` (parse PEM, `covers`).
- `crates/portunus-server/src/advertised/resolve.rs` — `resolve_advertised_endpoint`.

**Modify:**
- `Cargo.toml` (workspace dep), `crates/portunus-server/Cargo.toml`.
- `crates/portunus-server/src/lib.rs` (or `main.rs` mod tree) — declare `advertised` module + `settings_store`.
- `crates/portunus-server/src/state.rs` — replace `server_endpoint` field; extend `AppState::new`.
- `crates/portunus-server/src/serve.rs` — build SAN set + seed + control port.
- `crates/portunus-server/src/main.rs` — env fallback for seed; `build_offline_state` signature.
- `crates/portunus-server/src/store/enrollment_store.rs` — persist + return endpoint.
- `crates/portunus-server/src/operator/cli.rs` — resolve at create; `enrollment_uri`; `OperatorError` variants + `code()`/`exit_code()`.
- `crates/portunus-server/src/operator/http.rs` — Host extraction; settings routes/handlers; `ApiError` status mapping.
- `crates/portunus-server/src/grpc/enrollment.rs` — replay persisted; legacy-NULL resolve.
- `crates/portunus-server/tests/store_schema_handshake.rs` — v10.
- `crates/portunus-server/tests/http_client_enrollments_contract.rs` — `AppState::new` call update.
- `crates/portunus-server/tests/rules_crud_sqlite.rs`, `benches/operator_api.rs`, `src/grpc/service.rs` test — `AppState::new` call update.
- `webui/src/pages/Settings.tsx`, `webui/src/api/` (new `settings.ts`), `webui/src/i18n/en.json`, `webui/src/i18n/zh-CN.json`.

---

## Phase 0 — Dependency

### Task 0: Add `x509-parser` workspace dependency

**Files:**
- Modify: `Cargo.toml` (workspace `[workspace.dependencies]`)
- Modify: `crates/portunus-server/Cargo.toml`

Rationale: `rcgen` only *generates* certs; the operator may drop a custom `<data-dir>/server.crt`. We must parse arbitrary PEM to read SAN GeneralNames. `x509-parser` is a pure-Rust, widely-used parser. Webpki name-match semantics are then implemented explicitly in `san.rs` with exhaustive tests pinning parity (rejected alternative: building a full `rustls-webpki` `EndEntityCert` just to call `verify_is_valid_for_subject_name` — heavier ceremony, needs DER plumbing, and still no chain check, so no correctness gain over a tested matcher).

- [ ] **Step 1: Add workspace dep**

In `Cargo.toml`, under `[workspace.dependencies]` near the TLS block, add:

```toml
x509-parser = "0.16"
```

- [ ] **Step 2: Reference it from portunus-server**

In `crates/portunus-server/Cargo.toml`, in `[dependencies]` near `rcgen = { workspace = true }`, add:

```toml
x509-parser = { workspace = true }
```

- [ ] **Step 3: Verify it resolves**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server`
Expected: builds (no code uses it yet; just dependency resolution).

- [ ] **Step 4: Commit**

```bash
cargo fmt --all
git add Cargo.toml Cargo.lock crates/portunus-server/Cargo.toml
git commit -m "build: add x509-parser for cert SAN parsing"
```

---

## Phase 1 — Storage layer

### Task 1: V010 migration

**Files:**
- Create: `crates/portunus-server/src/store/migrations/V010__add_server_settings.sql`
- Test: `crates/portunus-server/tests/store_schema_handshake.rs:38-61`

- [ ] **Step 1: Update the schema-handshake test to expect v10 (failing)**

In `crates/portunus-server/tests/store_schema_handshake.rs`, change the assertion block (currently lines 38-61):

```rust
    let v = store.schema_version().expect("read schema version");
    assert_eq!(
        v, 10,
        "current target schema is 10 (V001 + V002 + V003 + V004 + V005 + V006 + V007 + V008 + V009 + V010)"
    );
    assert_eq!(v, Store::target_schema_version());

    store
        .with_conn(|conn| {
            assert!(column_exists(conn, "users", "password_hash"));
            assert!(column_exists(conn, "users", "password_change_required"));
            assert!(table_exists(conn, "web_sessions"));
            assert!(table_exists(conn, "login_attempts"));
            assert!(table_exists(conn, "onboarding_setup"));
            assert!(column_exists(conn, "client_tokens", "client_address"));
            assert!(table_exists(conn, "client_enrollments"));
            assert!(table_exists(conn, "server_settings"));
            assert!(column_exists(conn, "client_enrollments", "advertised_endpoint"));
            Ok(())
        })
        .expect("inspect schema");
```

- [ ] **Step 2: Run it to verify it fails**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test store_schema_handshake fresh_store_has_current_schema`
Expected: FAIL — `assertion failed: ... 9 != 10` (or migration count).

- [ ] **Step 3: Create the migration**

Create `crates/portunus-server/src/store/migrations/V010__add_server_settings.sql`:

```sql
CREATE TABLE server_settings (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    advertised_endpoint TEXT
) STRICT;

INSERT INTO server_settings (id, advertised_endpoint) VALUES (1, NULL);

ALTER TABLE client_enrollments ADD COLUMN advertised_endpoint TEXT;
```

- [ ] **Step 4: Run it to verify it passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test store_schema_handshake fresh_store_has_current_schema`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/portunus-server/src/store/migrations/V010__add_server_settings.sql crates/portunus-server/tests/store_schema_handshake.rs
git commit -m "feat(store): V010 server_settings + client_enrollments.advertised_endpoint"
```

---

### Task 2: `SqliteSettingsStore`

**Files:**
- Create: `crates/portunus-server/src/store/settings_store.rs`
- Modify: `crates/portunus-server/src/store/mod.rs` (add `pub mod settings_store;` near other `pub mod` store submodules, e.g. next to `pub mod enrollment_store;`)

Note: grammar validation lives in `advertised::grammar` (Task 4) but that module does not exist yet. To keep tasks ordered and tests green, this task validates with a **temporary inline check** that is replaced by `advertised::grammar::validate_authority` in Task 4 Step 6. The store API and tests are stable across that swap.

- [ ] **Step 1: Declare the module**

In `crates/portunus-server/src/store/mod.rs`, add alongside the other store submodule declarations:

```rust
pub mod settings_store;
```

- [ ] **Step 2: Write the failing test**

Create `crates/portunus-server/src/store/settings_store.rs` with only the test module first:

```rust
//! Singleton `server_settings` row accessor (operator advertised-endpoint override).

use std::sync::Arc;

use crate::store::Store;
use crate::store::error::StoreError;

pub struct SqliteSettingsStore {
    store: Arc<Store>,
}

impl SqliteSettingsStore {
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn store() -> Arc<Store> {
        let dir = tempdir().unwrap();
        Arc::new(Store::open(dir.path()).unwrap())
    }

    #[test]
    fn get_is_none_by_default() {
        let s = SqliteSettingsStore::new(store());
        assert_eq!(s.get_advertised_endpoint().unwrap(), None);
    }

    #[test]
    fn set_then_get_round_trips() {
        let s = SqliteSettingsStore::new(store());
        s.set_advertised_endpoint(Some("public.example:7443".into()))
            .unwrap();
        assert_eq!(
            s.get_advertised_endpoint().unwrap(),
            Some("public.example:7443".to_string())
        );
    }

    #[test]
    fn set_none_clears() {
        let s = SqliteSettingsStore::new(store());
        s.set_advertised_endpoint(Some("public.example:7443".into()))
            .unwrap();
        s.set_advertised_endpoint(None).unwrap();
        assert_eq!(s.get_advertised_endpoint().unwrap(), None);
    }

    #[test]
    fn set_rejects_malformed() {
        let s = SqliteSettingsStore::new(store());
        for bad in [
            "https://x:7443",
            "x/y:7443",
            "host-only",
            "x:bad",
            "x:0",
            "x:70000",
            "[::1]:7443",
        ] {
            assert!(
                s.set_advertised_endpoint(Some(bad.into())).is_err(),
                "expected reject for {bad}"
            );
        }
    }

    #[test]
    fn set_empty_string_clears() {
        let s = SqliteSettingsStore::new(store());
        s.set_advertised_endpoint(Some("public.example:7443".into()))
            .unwrap();
        s.set_advertised_endpoint(Some(String::new())).unwrap();
        assert_eq!(s.get_advertised_endpoint().unwrap(), None);
    }
}
```

- [ ] **Step 3: Run it to verify it fails**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib store::settings_store`
Expected: FAIL — `get_advertised_endpoint`/`set_advertised_endpoint` not found.

- [ ] **Step 4: Implement get/set with temporary inline validation**

Add to `crates/portunus-server/src/store/settings_store.rs` (above the `#[cfg(test)]`):

```rust
impl SqliteSettingsStore {
    /// Read the operator override. `NULL`/empty → `None`.
    ///
    /// # Errors
    /// Propagates `StoreError` on a DB failure.
    pub fn get_advertised_endpoint(&self) -> Result<Option<String>, StoreError> {
        self.store.with_conn(|conn| {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT advertised_endpoint FROM server_settings WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_rusqlite)?
                .flatten();
            Ok(raw.filter(|s| !s.is_empty()))
        })
    }

    /// Write (or clear with `None`/empty) the operator override.
    /// Validates authority grammar before persisting.
    ///
    /// # Errors
    /// `StoreError::Internal` on grammar rejection; `StoreError` on DB failure.
    pub fn set_advertised_endpoint(&self, value: Option<String>) -> Result<(), StoreError> {
        let normalized = value.filter(|s| !s.is_empty());
        if let Some(v) = &normalized {
            validate_authority_inline(v).map_err(|reason| StoreError::Internal {
                message: format!("invalid advertised_endpoint: {reason}"),
            })?;
        }
        self.store.with_write_tx(|tx| {
            tx.execute(
                "UPDATE server_settings SET advertised_endpoint = ? WHERE id = 1",
                rusqlite::params![normalized],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }
}

// TEMPORARY — replaced by `crate::advertised::grammar::validate_authority`
// in Task 4 Step 6. Kept minimal but correct so this task's tests are stable.
fn validate_authority_inline(s: &str) -> Result<(), String> {
    if s.len() > 255 {
        return Err("too long".into());
    }
    if s.contains("://") || s.contains('/') || s.contains('@') || s.contains('[') {
        return Err("not a bare host:port".into());
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("whitespace/control".into());
    }
    let (host, port) = s.rsplit_once(':').ok_or("missing :port")?;
    let p: u32 = port.parse().map_err(|_| "bad port")?;
    if !(1..=65535).contains(&p) {
        return Err("port out of range".into());
    }
    if host.is_empty() {
        return Err("empty host".into());
    }
    let is_ipv4 = host.parse::<std::net::Ipv4Addr>().is_ok();
    let is_dns = host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    });
    if is_ipv4 || is_dns {
        Ok(())
    } else {
        Err("host not RFC1123 hostname or IPv4".into())
    }
}
```

Add the needed imports at the top of the file (under the existing `use` lines):

```rust
use rusqlite::OptionalExtension;

use crate::store::error::map_rusqlite;
```

(If `map_rusqlite` is not `pub` in `store::error`, instead import it from where `enrollment_store.rs` imports it — check `enrollment_store.rs` `use` lines and mirror exactly.)

- [ ] **Step 5: Run it to verify it passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib store::settings_store`
Expected: PASS (all 5 tests).

- [ ] **Step 6: Clippy + commit**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo clippy -p portunus-server --lib -- -D warnings`
Expected: clean.

```bash
cargo fmt --all
git add crates/portunus-server/src/store/settings_store.rs crates/portunus-server/src/store/mod.rs
git commit -m "feat(store): SqliteSettingsStore get/set advertised_endpoint"
```

---

## Phase 2 — `advertised` module

### Task 3: Module skeleton + public types

**Files:**
- Create: `crates/portunus-server/src/advertised/mod.rs`
- Modify: module tree. Find where sibling modules are declared (grep `pub mod operator;` / `pub mod grpc;` in `crates/portunus-server/src/lib.rs`; if no `lib.rs`, in `crates/portunus-server/src/main.rs`). Add `pub mod advertised;` there.

- [ ] **Step 1: Declare the module**

Add `pub mod advertised;` next to the existing `pub mod store;` / `pub mod operator;` declarations (same file that declares them).

- [ ] **Step 2: Write public types with a compile test**

Create `crates/portunus-server/src/advertised/mod.rs`:

```rust
//! Runtime resolution of the advertised credential-bundle endpoint.
//!
//! Resolve-once-at-creation, replay-at-redeem. Explicit operator config
//! (SQLite override / CLI / env) that is malformed or not SAN-covered is
//! a hard error; implicit candidates (request-Host derive, loopback)
//! skip-and-fall-through.

pub mod grammar;
pub mod host_header;
pub mod resolve;
pub mod san;

pub use resolve::resolve_advertised_endpoint;
pub use san::CertSanSet;

/// Which tier produced the resolved endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EndpointSource {
    Override,
    Seed,
    Derived,
    Loopback,
}

/// Successful resolution result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAdvertisedEndpoint {
    pub endpoint: String,
    pub source: EndpointSource,
}

/// Which explicit tier a hard error came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigTier {
    /// Tier 1 — SQLite operator override.
    Override,
    /// Tier 2 — CLI flag / env seed.
    Seed,
}

/// Resolution failure. All variants are terminal — creation never
/// fabricates an unusable endpoint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveEndpointError {
    #[error("configured advertised endpoint is malformed ({tier:?}): {reason}")]
    ConfiguredEndpointInvalid { tier: ConfigTier, reason: String },
    #[error("configured advertised endpoint host {host} is not covered by the server certificate SAN ({tier:?})")]
    ConfiguredEndpointNotCovered { tier: ConfigTier, host: String },
    #[error("no certificate-SAN-covered advertised endpoint candidate is available")]
    NoSanCoveredCandidate,
}

impl ResolveEndpointError {
    /// Stable machine code for HTTP 422 bodies.
    #[must_use]
    pub fn http_code(&self) -> &'static str {
        match self {
            Self::ConfiguredEndpointInvalid { .. } => "endpoint_invalid",
            Self::ConfiguredEndpointNotCovered { .. } | Self::NoSanCoveredCandidate => {
                "endpoint_not_in_cert_san"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_codes_are_stable() {
        assert_eq!(
            ResolveEndpointError::ConfiguredEndpointInvalid {
                tier: ConfigTier::Seed,
                reason: "x".into()
            }
            .http_code(),
            "endpoint_invalid"
        );
        assert_eq!(
            ResolveEndpointError::NoSanCoveredCandidate.http_code(),
            "endpoint_not_in_cert_san"
        );
    }
}
```

- [ ] **Step 3: Create empty submodule stubs so the crate compiles**

Create `crates/portunus-server/src/advertised/grammar.rs`:

```rust
//! Authority grammar validation (host:port, no scheme/path/userinfo/IPv6).
```

Create `crates/portunus-server/src/advertised/host_header.rs`:

```rust
//! HTTP `Host` header → bare host extraction for tier-3 auto-derive.
```

Create `crates/portunus-server/src/advertised/san.rs`:

```rust
//! Certificate SAN extraction + webpki-equivalent name matching.
```

Create `crates/portunus-server/src/advertised/resolve.rs`:

```rust
//! Tiered, SAN-filtered, fail-closed endpoint resolution.
```

- [ ] **Step 4: Run it to verify it passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib advertised::tests::http_codes_are_stable`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/portunus-server/src/advertised/ crates/portunus-server/src/lib.rs crates/portunus-server/src/main.rs
git commit -m "feat(advertised): module skeleton + ResolveEndpointError types"
```

(Stage whichever of `lib.rs`/`main.rs` you actually edited in Step 1.)

---

### Task 4: `grammar::validate_authority`

**Files:**
- Modify: `crates/portunus-server/src/advertised/grammar.rs`
- Modify: `crates/portunus-server/src/store/settings_store.rs` (swap temporary validator)

- [ ] **Step 1: Write the failing test**

Replace `crates/portunus-server/src/advertised/grammar.rs` contents:

```rust
//! Authority grammar validation (host:port, no scheme/path/userinfo/IPv6).
//!
//! The value is simultaneously a URI authority AND the client's TLS
//! verification domain, so it is validated strictly: exactly `host:port`,
//! host is RFC-1123 DNS or IPv4 (IPv6 literals rejected so the client's
//! `rsplit_once(':')` host parser stays correct), port 1..=65535,
//! length ≤ 255, no scheme/path/query/fragment/userinfo/whitespace/control.

/// Returns the validated `(host, port)` borrowed from `s`.
///
/// # Errors
/// Returns a human-readable reason string on any grammar violation.
pub fn validate_authority(s: &str) -> Result<(&str, u16), String> {
    if s.is_empty() {
        return Err("empty".into());
    }
    if s.len() > 263 {
        // 253 host + ':' + 5 port digits + small headroom; coarse anti-DoS bound, real host limit enforced below
        return Err("too long (> 255)".into());
    }
    if s.contains("://") {
        return Err("must not contain a scheme".into());
    }
    if s.contains('/') {
        return Err("must not contain a path".into());
    }
    if s.contains('?') || s.contains('#') {
        return Err("must not contain query/fragment".into());
    }
    if s.contains('@') {
        return Err("must not contain userinfo".into());
    }
    if s.contains('[') || s.contains(']') {
        return Err("IPv6 literals are not supported".into());
    }
    if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("must not contain whitespace/control characters".into());
    }
    let (host, port_str) = s.rsplit_once(':').ok_or("missing :port")?;
    if host.is_empty() {
        return Err("empty host".into());
    }
    if port_str.is_empty()
        || !port_str.bytes().all(|b| b.is_ascii_digit())
        || (port_str.len() > 1 && port_str.starts_with('0'))
    {
        return Err("port must be a decimal 1..=65535".into());
    }
    let port: u16 = port_str
        .parse()
        .map_err(|_| "port must be a decimal 1..=65535".to_string())?;
    if port == 0 {
        return Err("port must be 1..=65535".into());
    }
    if !is_ipv4(host) && !is_rfc1123_hostname(host) {
        return Err("host must be an RFC-1123 hostname or IPv4 address".into());
    }
    Ok((host, port))
}

fn is_ipv4(host: &str) -> bool {
    host.parse::<std::net::Ipv4Addr>().is_ok()
}

fn is_rfc1123_hostname(host: &str) -> bool {
    if host.len() > 253 {
        return false;
    }
    if host
        .split('.')
        .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()))
    {
        // All-numeric host that did not parse as IPv4 → malformed IP, reject.
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_dns_and_ipv4() {
        assert_eq!(
            validate_authority("public.example:7443").unwrap(),
            ("public.example", 7443)
        );
        assert_eq!(validate_authority("127.0.0.1:443").unwrap(), ("127.0.0.1", 443));
        assert_eq!(validate_authority("localhost:1").unwrap(), ("localhost", 1));
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "host-only",
            "https://x:7443",
            "x/y:7443",
            "user@x:7443",
            "[::1]:7443",
            "x:bad",
            "x:0",
            "x:70000",
            "x y:7443",
            "x:7443?q=1",
            &"a".repeat(300),
        ] {
            assert!(validate_authority(bad).is_err(), "should reject {bad:?}");
        }
    }
}
```

> Note: digit-only port guard, all-numeric-host rejection, and 263-char bound added per code review 2026-05-17.

- [ ] **Step 2: Run it to verify it fails, then passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib advertised::grammar`
Expected: PASS (the implementation is included in Step 1 — this is a combined write; if the harness requires a red phase, temporarily comment the `pub fn` body to observe FAIL, then restore).

- [ ] **Step 3: Swap the temporary validator in settings_store**

In `crates/portunus-server/src/store/settings_store.rs`:
- Delete the entire `fn validate_authority_inline` function.
- Replace its call site:

```rust
        if let Some(v) = &normalized {
            crate::advertised::grammar::validate_authority(v).map_err(|reason| {
                StoreError::Internal {
                    message: format!("invalid advertised_endpoint: {reason}"),
                }
            })?;
        }
```

- [ ] **Step 4: Run settings_store tests again**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib store::settings_store advertised::grammar`
Expected: PASS (all settings_store + grammar tests).

- [ ] **Step 5: Clippy + commit**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo clippy -p portunus-server --lib -- -D warnings`

```bash
cargo fmt --all
git add crates/portunus-server/src/advertised/grammar.rs crates/portunus-server/src/store/settings_store.rs
git commit -m "feat(advertised): authority grammar validation; settings_store uses it"
```

---

### Task 5: `host_header::host_from_header`

**Files:**
- Modify: `crates/portunus-server/src/advertised/host_header.rs`

- [ ] **Step 1: Write failing test + impl**

Replace `crates/portunus-server/src/advertised/host_header.rs`:

```rust
//! HTTP `Host` header → bare host extraction for tier-3 auto-derive.
//!
//! Strips the optional *browser* port and discards it (the control-plane
//! port comes from the server's resolved `control_listen`). Rejects
//! scheme/path/userinfo/whitespace/IPv6-literal headers (→ tier 3 skipped).

/// Build `host:control_port` from a raw `Host` header, or `None` if the
/// header is unusable for tier-3 derivation.
#[must_use]
pub fn host_from_header(raw: &str, control_port: u16) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 255 {
        return None;
    }
    if raw.contains("://")
        || raw.contains('/')
        || raw.contains('@')
        || raw.contains('[')
        || raw.contains(']')
        || raw.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    // Strip optional browser port.
    let host = match raw.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => h,
        Some(_) => return None,
        None => raw,
    };
    if host.is_empty() {
        return None;
    }
    let candidate = format!("{host}:{control_port}");
    // Must satisfy the full authority grammar.
    crate::advertised::grammar::validate_authority(&candidate).ok()?;
    Some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_browser_port_and_appends_control_port() {
        assert_eq!(
            host_from_header("localhost:5173", 7443).as_deref(),
            Some("localhost:7443")
        );
        assert_eq!(
            host_from_header("public.example:443", 7443).as_deref(),
            Some("public.example:7443")
        );
        assert_eq!(
            host_from_header("public.example", 7443).as_deref(),
            Some("public.example:7443")
        );
    }

    #[test]
    fn rejects_unusable_headers() {
        for bad in [
            "",
            "http://x",
            "x/y",
            "user@x",
            "[::1]:443",
            "x y",
            "x:",
            "x:bad",
        ] {
            assert_eq!(host_from_header(bad, 7443), None, "should reject {bad:?}");
        }
    }
}
```

- [ ] **Step 2: Run it**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib advertised::host_header`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
cargo fmt --all
git add crates/portunus-server/src/advertised/host_header.rs
git commit -m "feat(advertised): Host header parsing contract"
```

---

### Task 6: `san::CertSanSet`

**Files:**
- Modify: `crates/portunus-server/src/advertised/san.rs`

- [ ] **Step 1: Write failing tests + impl**

Replace `crates/portunus-server/src/advertised/san.rs`:

```rust
//! Certificate SAN extraction + webpki-equivalent name matching.
//!
//! Matching mirrors the client's rustls/webpki verifier:
//! - DNS SAN: ASCII-lowercase exact match.
//! - Wildcard `*.example.com`: matches exactly one leftmost label
//!   (`a.example.com`), NOT `a.b.example.com` and NOT bare `example.com`.
//! - IPv4 host: matched only against IP SAN entries (never DNS SAN).
//! IPv6 is impossible here — grammar rejects IPv6 endpoint hosts.

use std::net::IpAddr;

use x509_parser::prelude::*;

#[derive(Debug, Clone, Default)]
pub struct CertSanSet {
    dns: Vec<String>,
    ips: Vec<IpAddr>,
}

impl CertSanSet {
    /// Parse the leaf (first) certificate from a PEM bundle and collect
    /// its SAN DNS names + IP addresses.
    ///
    /// # Errors
    /// Returns a reason string if no PEM cert is found or parsing fails.
    pub fn from_pem(pem: &str) -> Result<Self, String> {
        let (_, pem_block) = parse_x509_pem(pem.as_bytes())
            .map_err(|e| format!("pem parse: {e}"))?;
        let (_, cert) = X509Certificate::from_der(&pem_block.contents)
            .map_err(|e| format!("der parse: {e}"))?;
        let mut dns = Vec::new();
        let mut ips = Vec::new();
        if let Ok(Some(san)) = cert.subject_alternative_name() {
            for name in &san.value.general_names {
                match name {
                    GeneralName::DNSName(d) => dns.push(d.to_ascii_lowercase()),
                    GeneralName::IPAddress(bytes) => {
                        if let Some(ip) = bytes_to_ip(bytes) {
                            ips.push(ip);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(Self { dns, ips })
    }

    /// webpki-equivalent coverage check for a bare host (DNS or IPv4).
    #[must_use]
    pub fn covers(&self, host: &str) -> bool {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return self.ips.iter().any(|s| *s == ip);
        }
        let host = host.to_ascii_lowercase();
        self.dns.iter().any(|san| dns_matches(san, &host))
    }
}

fn bytes_to_ip(bytes: &[u8]) -> Option<IpAddr> {
    match bytes.len() {
        4 => {
            let a: [u8; 4] = bytes.try_into().ok()?;
            Some(IpAddr::from(a))
        }
        16 => {
            let a: [u8; 16] = bytes.try_into().ok()?;
            Some(IpAddr::from(a))
        }
        _ => None,
    }
}

/// `san` is already ASCII-lowercased; `host` too.
fn dns_matches(san: &str, host: &str) -> bool {
    if let Some(suffix) = san.strip_prefix("*.") {
        // webpki (is_valid_dns_id) requires >=3 labels total, i.e. the
        // wildcard suffix must itself contain a dot. "*.com"/"*.example"
        // are MalformedDnsIdentifier in webpki and never match — mirror that.
        if !suffix.contains('.') {
            return false;
        }
        // Wildcard matches exactly one leftmost label.
        match host.split_once('.') {
            Some((label, rest)) => !label.is_empty() && rest == suffix,
            None => false,
        }
    } else {
        san == host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Self-signed leaf with SAN: DNS:public.example, DNS:*.wild.example,
    // DNS:localhost, IP:127.0.0.1. Generated once with rcgen; pasted as a
    // fixture so the test has no runtime cert-gen dependency.
    const FIXTURE_PEM: &str = include_str!("testdata/san_fixture.pem");

    fn set() -> CertSanSet {
        CertSanSet::from_pem(FIXTURE_PEM).expect("parse fixture")
    }

    #[test]
    fn exact_dns_case_insensitive() {
        assert!(set().covers("public.example"));
        assert!(set().covers("PUBLIC.EXAMPLE"));
        assert!(set().covers("localhost"));
    }

    #[test]
    fn wildcard_single_label_only() {
        let s = set();
        assert!(s.covers("a.wild.example"));
        assert!(!s.covers("a.b.wild.example"));
        assert!(!s.covers("wild.example"));
    }

    #[test]
    fn ipv4_matches_ip_san_only() {
        let s = set();
        assert!(s.covers("127.0.0.1"));
        assert!(!s.covers("127.0.0.2"));
    }

    #[test]
    fn miss_is_uncovered() {
        assert!(!set().covers("not.in.cert"));
    }
}
```

- [ ] **Step 2: Generate the test fixture cert**

Create `crates/portunus-server/src/advertised/testdata/` and generate a self-signed PEM with the exact SANs the tests expect. Run this one-off helper (a throwaway, not committed):

```bash
mkdir -p crates/portunus-server/src/advertised/testdata
cat > /tmp/san_openssl.cnf <<'EOF'
[req]
distinguished_name = dn
x509_extensions = v3
prompt = no
[dn]
CN = public.example
[v3]
subjectAltName = DNS:public.example,DNS:*.wild.example,DNS:localhost,IP:127.0.0.1
EOF
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -days 3650 -keyout /tmp/san_fixture.key \
  -out crates/portunus-server/src/advertised/testdata/san_fixture.pem \
  -config /tmp/san_openssl.cnf
```

Verify the SANs:

```bash
openssl x509 -in crates/portunus-server/src/advertised/testdata/san_fixture.pem -noout -text | grep -A1 "Subject Alternative Name"
```

Expected: `DNS:public.example, DNS:*.wild.example, DNS:localhost, IP Address:127.0.0.1`.

- [ ] **Step 3: Run the SAN tests**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib advertised::san`
Expected: PASS (4 tests).

- [ ] **Step 4: Clippy + commit**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo clippy -p portunus-server --lib -- -D warnings`

```bash
cargo fmt --all
git add crates/portunus-server/src/advertised/san.rs crates/portunus-server/src/advertised/testdata/san_fixture.pem
git commit -m "feat(advertised): cert SAN extraction + webpki-parity matching"
```

> Note: single-label wildcard suffix rejection + PEM-label check + absent/uppercase-SAN tests added per code review 2026-05-17.

---

### Task 7: `resolve::resolve_advertised_endpoint`

**Files:**
- Modify: `crates/portunus-server/src/advertised/resolve.rs`

The resolver takes already-fetched inputs (no `AppState` dependency yet, so it is unit-testable in isolation). `AppState` wiring is Task 8.

- [ ] **Step 1: Write failing tests + impl**

Replace `crates/portunus-server/src/advertised/resolve.rs`:

```rust
//! Tiered, SAN-filtered, fail-closed endpoint resolution.

use super::{
    ConfigTier, EndpointSource, ResolveEndpointError, ResolvedAdvertisedEndpoint,
    grammar::validate_authority, host_header::host_from_header, san::CertSanSet,
};

/// Inputs gathered by the caller (handler / offline path).
pub struct ResolveInputs<'a> {
    /// Tier 1 — SQLite operator override (already `None`-filtered for empty).
    pub override_value: Option<String>,
    /// Tier 2 — CLI flag / env seed.
    pub seed: Option<String>,
    /// Tier 3 — raw HTTP `Host` header, if this is a request-driven path.
    pub req_host: Option<&'a str>,
    /// Server's resolved control-plane port (for tiers 3 & 4).
    pub control_port: u16,
    /// Parsed leaf-cert SAN set.
    pub san: &'a CertSanSet,
}

/// Resolve the advertised endpoint per the spec's tiered contract.
///
/// # Errors
/// - `ConfiguredEndpointInvalid` — explicit tier present but malformed.
/// - `ConfiguredEndpointNotCovered` — explicit tier well-formed but host
///   not SAN-covered.
/// - `NoSanCoveredCandidate` — no explicit config and neither derive nor
///   loopback is SAN-covered.
pub fn resolve_advertised_endpoint(
    inputs: &ResolveInputs<'_>,
) -> Result<ResolvedAdvertisedEndpoint, ResolveEndpointError> {
    // Tier 1 — explicit SQLite override.
    if let Some(v) = inputs.override_value.as_deref() {
        return finalize_explicit(v, ConfigTier::Override, EndpointSource::Override, inputs.san);
    }
    // Tier 2 — explicit CLI/env seed.
    if let Some(v) = inputs.seed.as_deref() {
        return finalize_explicit(v, ConfigTier::Seed, EndpointSource::Seed, inputs.san);
    }
    // Tier 3 — implicit auto-derive from request Host.
    if let Some(raw) = inputs.req_host {
        if let Some(candidate) = host_from_header(raw, inputs.control_port) {
            let (host, _) = validate_authority(&candidate)
                .expect("host_from_header already grammar-validated");
            if inputs.san.covers(host) {
                return Ok(ResolvedAdvertisedEndpoint {
                    endpoint: candidate,
                    source: EndpointSource::Derived,
                });
            }
            tracing::warn!(
                event = "advertised.derive_uncovered",
                host = %host,
                "request-Host derived endpoint not SAN-covered; falling through"
            );
        }
    }
    // Tier 4 — implicit loopback fallback.
    let loopback = format!("127.0.0.1:{}", inputs.control_port);
    if let Ok((host, _)) = validate_authority(&loopback) {
        if inputs.san.covers(host) {
            return Ok(ResolvedAdvertisedEndpoint {
                endpoint: loopback,
                source: EndpointSource::Loopback,
            });
        }
    }
    tracing::warn!(
        event = "advertised.no_covered_candidate",
        "no SAN-covered advertised endpoint candidate available"
    );
    Err(ResolveEndpointError::NoSanCoveredCandidate)
}

fn finalize_explicit(
    value: &str,
    tier: ConfigTier,
    source: EndpointSource,
    san: &CertSanSet,
) -> Result<ResolvedAdvertisedEndpoint, ResolveEndpointError> {
    let (host, _) = validate_authority(value).map_err(|reason| {
        ResolveEndpointError::ConfiguredEndpointInvalid {
            tier,
            reason,
        }
    })?;
    if !san.covers(host) {
        return Err(ResolveEndpointError::ConfiguredEndpointNotCovered {
            tier,
            host: host.to_string(),
        });
    }
    Ok(ResolvedAdvertisedEndpoint {
        endpoint: value.to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_PEM: &str = include_str!("testdata/san_fixture.pem");

    fn san() -> CertSanSet {
        CertSanSet::from_pem(FIXTURE_PEM).unwrap()
    }

    fn base<'a>(san: &'a CertSanSet) -> ResolveInputs<'a> {
        ResolveInputs {
            override_value: None,
            seed: None,
            req_host: None,
            control_port: 7443,
            san,
        }
    }

    #[test]
    fn tier1_override_wins_when_covered() {
        let s = san();
        let mut i = base(&s);
        i.override_value = Some("public.example:7443".into());
        i.seed = Some("localhost:7443".into());
        let r = resolve_advertised_endpoint(&i).unwrap();
        assert_eq!(r.endpoint, "public.example:7443");
        assert_eq!(r.source, EndpointSource::Override);
    }

    #[test]
    fn tier1_malformed_is_hard_error_not_downgraded() {
        let s = san();
        let mut i = base(&s);
        i.override_value = Some("https://public.example:7443".into());
        i.seed = Some("localhost:7443".into());
        assert!(matches!(
            resolve_advertised_endpoint(&i),
            Err(ResolveEndpointError::ConfiguredEndpointInvalid {
                tier: ConfigTier::Override,
                ..
            })
        ));
    }

    #[test]
    fn tier2_seed_uncovered_is_hard_error_even_if_loopback_covered() {
        let s = san();
        let mut i = base(&s);
        i.seed = Some("not.in.cert:7443".into());
        // loopback 127.0.0.1 IS covered by the fixture, but seed is explicit.
        assert!(matches!(
            resolve_advertised_endpoint(&i),
            Err(ResolveEndpointError::ConfiguredEndpointNotCovered {
                tier: ConfigTier::Seed,
                ..
            })
        ));
    }

    #[test]
    fn tier2_bad_env_seed_is_invalid() {
        let s = san();
        let mut i = base(&s);
        i.seed = Some("x:bad".into());
        assert!(matches!(
            resolve_advertised_endpoint(&i),
            Err(ResolveEndpointError::ConfiguredEndpointInvalid {
                tier: ConfigTier::Seed,
                ..
            })
        ));
    }

    #[test]
    fn tier3_derive_used_when_covered() {
        let s = san();
        let mut i = base(&s);
        i.req_host = Some("public.example:443");
        let r = resolve_advertised_endpoint(&i).unwrap();
        assert_eq!(r.endpoint, "public.example:7443");
        assert_eq!(r.source, EndpointSource::Derived);
    }

    #[test]
    fn tier3_uncovered_falls_through_to_loopback() {
        let s = san();
        let mut i = base(&s);
        i.req_host = Some("not.in.cert:443");
        let r = resolve_advertised_endpoint(&i).unwrap();
        assert_eq!(r.endpoint, "127.0.0.1:7443");
        assert_eq!(r.source, EndpointSource::Loopback);
    }

    #[test]
    fn no_host_no_config_uses_loopback() {
        let s = san();
        let r = resolve_advertised_endpoint(&base(&s)).unwrap();
        assert_eq!(r.source, EndpointSource::Loopback);
    }
}
```

- [ ] **Step 2: Run it**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib advertised::resolve`
Expected: PASS (7 tests).

- [ ] **Step 3: Clippy + commit**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo clippy -p portunus-server --lib -- -D warnings`

```bash
cargo fmt --all
git add crates/portunus-server/src/advertised/resolve.rs
git commit -m "feat(advertised): tiered fail-closed resolver"
```

---

## Phase 3 — AppState / serve / main wiring

### Task 8: Replace `AppState.server_endpoint`

**Files:**
- Modify: `crates/portunus-server/src/state.rs:27-28,120-200`
- Modify: `crates/portunus-server/src/serve.rs:143-174`
- Modify: `crates/portunus-server/src/main.rs:797-841` (`build_offline_state`)
- Modify call sites in tests/benches (compile-fix only)

- [ ] **Step 1: Change AppState fields + constructor**

In `crates/portunus-server/src/state.rs`:

Remove the field (lines 27-28):
```rust
    /// `host:port` advertised in newly-issued credential bundles.
    pub server_endpoint: String,
```

Add in its place:
```rust
    /// Tier-2 seed: CLI `--advertised-endpoint` / `PORTUNUS_ADVERTISED_ENDPOINT`.
    pub advertised_seed: Option<String>,
    /// Server's resolved control-plane port (tiers 3 & 4).
    pub control_port: u16,
    /// Parsed leaf-cert SAN set (coverage gate for the resolver).
    pub cert_san: std::sync::Arc<crate::advertised::CertSanSet>,
    /// Operator advertised-endpoint override accessor.
    pub settings: std::sync::Arc<crate::store::settings_store::SqliteSettingsStore>,
```

Change `AppState::new` signature (lines 120-129): replace the `server_endpoint: impl Into<String>,` parameter with:
```rust
        advertised_seed: Option<String>,
        control_port: u16,
```

In the constructor body where the struct is built (around lines 183-200): remove `server_endpoint: server_endpoint.into(),` and add:
```rust
            advertised_seed,
            control_port,
            cert_san: std::sync::Arc::new(
                crate::advertised::CertSanSet::from_pem(server_cert_pem_ref)
                    .unwrap_or_default(),
            ),
            settings: std::sync::Arc::new(
                crate::store::settings_store::SqliteSettingsStore::new(std::sync::Arc::clone(&store)),
            ),
```

`server_cert_pem` is currently `impl Into<String>`. To both store it and parse it, bind it once at the top of `new`:
```rust
        let server_cert_pem: String = server_cert_pem.into();
        let server_cert_pem_ref: &str = &server_cert_pem;
```
and change the struct init `server_cert_pem: server_cert_pem.into(),` → `server_cert_pem,`.

(Using `unwrap_or_default()` means an unparseable cert yields an empty SAN set → resolver fails closed with `NoSanCoveredCandidate`, never panics at boot. Add a `tracing::warn!` if `from_pem` errors: bind it to a `match` instead of `unwrap_or_default` if you prefer logging — acceptable either way; prefer logging.)

- [ ] **Step 2: Fix serve.rs**

In `crates/portunus-server/src/serve.rs`, replace the `advertised` block (lines 150-158) — delete the `let advertised = opts.advertised_endpoint.unwrap_or_else(...)` entirely. Compute the control port from the bound gRPC addr and pass the seed straight through. Change the `AppState::new(...)` call (lines 162-171):

```rust
    let control_port = grpc_addr.port();
    let cfg_arc = Arc::new(cfg.clone());
    let state = Arc::new(
        AppState::new(
            Arc::clone(&tokens),
            Arc::clone(&operator_store),
            clients.clone(),
            opts.advertised_endpoint.clone(),
            control_port,
            tls.leaf_fingerprint_hex.clone(),
            tls.cert_pem.clone(),
            cfg.range_rule_max_ports,
            Arc::clone(&store),
        )
        .map_err(|e| PortunusError::Tls(format!("metrics: {e}")))?
        .with_server_config(cfg_arc),
    );
```

- [ ] **Step 3: Fix main.rs `build_offline_state`**

In `crates/portunus-server/src/main.rs`, the CLI struct field (lines 35-36) gains an explicit env fallback. The clap attribute stays `#[arg(long, global = true)]` (no `env`). Add resolution at the top of `run`/`main` where `cli` is available, OR inside each consumer. Simplest: a helper:

```rust
fn advertised_seed(cli: &Cli) -> Option<String> {
    cli.advertised_endpoint
        .clone()
        .or_else(|| std::env::var("PORTUNUS_ADVERTISED_ENDPOINT").ok())
        .filter(|s| !s.is_empty())
}
```

Replace every `cli.advertised_endpoint.clone()` (lines 445, 464, 480, 493) with `advertised_seed(&cli)`.

Change `build_offline_state` (lines 797-841): replace the `advertised_endpoint: Option<String>` param usage. Delete `let endpoint = advertised_endpoint.unwrap_or_else(|| "127.0.0.1:7443".to_string());` (line 822). The offline path has no bound socket; use the config's default control port. Read it from `ServerConfig`:

```rust
fn build_offline_state(
    data_dir: &std::path::Path,
    advertised_seed: Option<String>,
) -> Result<AppState, u8> {
    // ... unchanged up to the AppState::new call ...
    let control_port = portunus_core::config::ServerConfig::default_for_data_dir(data_dir)
        .control_listen_port()
        .unwrap_or(7443);
    AppState::new(
        tokens,
        operator_store,
        ConnectedClients::default(),
        advertised_seed,
        control_port,
        tls.leaf_fingerprint_hex,
        tls.cert_pem,
        portunus_core::config::default_range_rule_max_ports(),
        store,
    )
    .map_err(|e| {
        eprintln!("metrics: {e}");
        1u8
    })
}
```

If `ServerConfig` exposes no `control_listen_port()` accessor, hardcode `7443` (the documented default) with a `// default control_listen port; offline path has no bound socket` comment. Verify by grepping `control_listen` in `crates/portunus-core/src/config`.

- [ ] **Step 4: Fix the `AppState::new` test/bench call sites**

Update each call to pass `advertised_seed` then `control_port` instead of the old `server_endpoint` string:

- `crates/portunus-server/tests/http_client_enrollments_contract.rs:46-55` — was `"control.example.com:7443"`; change to `Some("control.example.com:7443".to_string()), 7443,`. **The cert PEM passed here is a fake (`-----BEGIN CERTIFICATE-----\nZm9v...`).** That will not parse → empty SAN → resolver fails closed and this contract test (which asserts a `control.example.com:7443` URI) will break. Fix: replace the fake PEM argument in this test's `AppState::new` with the real fixture: `include_str!("../src/advertised/testdata/san_fixture.pem")` and change the expected host from `control.example.com` to `public.example` (the fixture's SAN). Update all `control.example.com:7443` assertions in this file to `public.example:7443`.
- `crates/portunus-server/tests/rules_crud_sqlite.rs:38-47` — pass `Some("127.0.0.1:0".to_string()), 0,`; this test doesn't exercise enrollment, so SAN doesn't matter, but replace the dummy PEM with the fixture too to avoid `from_pem` warnings: `include_str!("../src/advertised/testdata/san_fixture.pem")`.
- `crates/portunus-server/benches/operator_api.rs:64` and the `crates/portunus-server/src/grpc/service.rs:947` test helper — same transformation (seed `None` is fine where enrollment isn't tested; use the fixture PEM).

- [ ] **Step 5: Build the whole crate + run existing suite**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server --all-targets`
Expected: compiles. Fix any remaining `server_endpoint` references the compiler flags (grep `state.server_endpoint` / `\.server_endpoint` — Task 9/10 handle `operator/cli.rs` and `grpc/enrollment.rs`; if the build fails only there, that is expected and addressed next; to keep this task green, temporarily replace those two reads with `compile_error!`-free stubs is NOT allowed — instead, do Tasks 9 & 10 before re-running the full build, and only run `cargo build` scoped to the lib here:)

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server --lib`
Expected: FAIL only in `operator/cli.rs` + `grpc/enrollment.rs` (the two known `server_endpoint` readers). That is acceptable at this checkpoint — do **not** commit yet; proceed to Task 9 and 10, then return to Step 6.

- [ ] **Step 6: After Tasks 9 & 10, full green build + commit**

(Return here once Tasks 9 and 10 are done.)
Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server --all-targets && PORTUNUS_SKIP_WEBUI=1 cargo clippy -p portunus-server --all-targets -- -D warnings`
Expected: clean.

```bash
cargo fmt --all
git add crates/portunus-server/src/state.rs crates/portunus-server/src/serve.rs crates/portunus-server/src/main.rs crates/portunus-server/tests/http_client_enrollments_contract.rs crates/portunus-server/tests/rules_crud_sqlite.rs crates/portunus-server/benches/operator_api.rs crates/portunus-server/src/grpc/service.rs
git commit -m "refactor(state): replace server_endpoint with seed/control_port/cert_san/settings"
```

---

## Phase 4 — Enrollment store persistence

### Task 9: Persist + return `advertised_endpoint` on enrollment rows

**Files:**
- Modify: `crates/portunus-server/src/store/enrollment_store.rs`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `enrollment_store.rs` (mirror the existing test helpers in that file for store setup — copy the pattern an existing test in this file uses to construct `ClientEnrollmentStore`):

```rust
    #[test]
    fn create_persists_and_redeem_returns_advertised_endpoint() {
        let store = test_store(); // use this file's existing helper name
        let es = ClientEnrollmentStore::new(Arc::clone(&store));
        let now = Utc::now();
        let created = es
            .create(CreateEnrollment {
                client_name: ClientName::from_str("edge-1").unwrap(),
                target: EnrollmentTarget::New { client_address: None },
                expires_at: now + chrono::Duration::seconds(300),
                now,
                advertised_endpoint: "public.example:7443".to_string(),
            })
            .unwrap();
        let issued = es.redeem(&created.code, Utc::now()).unwrap();
        assert_eq!(
            issued.advertised_endpoint.as_deref(),
            Some("public.example:7443")
        );
    }

    #[test]
    fn legacy_null_row_redeems_with_none_endpoint() {
        let store = test_store();
        let es = ClientEnrollmentStore::new(Arc::clone(&store));
        // Simulate a pre-V010 row: insert directly with NULL endpoint.
        let now = Utc::now();
        let code = "legacycode000000000000000000000000000000000000000000000000000000";
        store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO client_enrollments \
                     (client_name, client_address, code_hash, issued_at, expires_at, consumed_at, advertised_endpoint) \
                     VALUES (?, NULL, ?, ?, ?, NULL, NULL)",
                    rusqlite::params![
                        "legacy-1",
                        crate::store::fingerprint::hex(&crate::store::token::hash_token(code)),
                        now.to_rfc3339(),
                        (now + chrono::Duration::seconds(300)).to_rfc3339(),
                    ],
                )
                .unwrap();
                Ok(())
            })
            .unwrap();
        let issued = es.redeem(code, Utc::now()).unwrap();
        assert_eq!(issued.advertised_endpoint, None);
    }
```

(Use the actual `token`/`fingerprint` paths this file already imports — check its `use` block and match exactly. If the file has no `test_store()` helper, copy the construction lines from the nearest existing test in the same module.)

- [ ] **Step 2: Run to verify it fails**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib store::enrollment_store`
Expected: FAIL — `CreateEnrollment` has no `advertised_endpoint`; `IssuedClientCredential` has no `advertised_endpoint`.

- [ ] **Step 3: Implement**

In `crates/portunus-server/src/store/enrollment_store.rs`:

Add to `CreateEnrollment` (after `now`):
```rust
    pub advertised_endpoint: String,
```

Add to `IssuedClientCredential`:
```rust
    pub advertised_endpoint: Option<String>,
```

Add to `EnrollmentRow`:
```rust
    advertised_endpoint: Option<String>,
```

In `create()`, change the INSERT to include the new column:
```rust
        tx.execute(
            "INSERT INTO client_enrollments \
             (client_name, client_address, code_hash, issued_at, expires_at, consumed_at, advertised_endpoint) \
             VALUES (?, ?, ?, ?, ?, NULL, ?)",
            rusqlite::params![
                input.client_name.as_str(),
                client_address.as_deref(),
                code_hash,
                issued_at,
                expires_at,
                input.advertised_endpoint
            ],
        )
        .map_err(map_rusqlite)?;
```

In `redeem()`, extend the SELECT + row mapping:
```rust
            .prepare(
                "SELECT id, client_name, client_address, code_hash, expires_at, consumed_at, advertised_endpoint \
                     FROM client_enrollments",
            )
```
```rust
                Ok(EnrollmentRow {
                    id: row.get(0)?,
                    client_name: row.get(1)?,
                    client_address: row.get(2)?,
                    code_hash: row.get(3)?,
                    expires_at: row.get(4)?,
                    consumed_at: row.get(5)?,
                    advertised_endpoint: row.get(6)?,
                })
```

In the final `Ok(Ok(IssuedClientCredential { ... }))` add:
```rust
            advertised_endpoint: row.advertised_endpoint.clone(),
```

- [ ] **Step 4: Run to verify it passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib store::enrollment_store`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/portunus-server/src/store/enrollment_store.rs
git commit -m "feat(store): persist advertised_endpoint on enrollment, return at redeem"
```

---

## Phase 5 — Operator CLI resolve + errors

### Task 10: Resolve at enrollment creation; map errors

**Files:**
- Modify: `crates/portunus-server/src/operator/cli.rs` (`OperatorError`, `code()`, `exit_code()`, `enrollment_uri`, `create_enrollment_command`, `enroll_client`)
- Modify: `crates/portunus-server/src/grpc/enrollment.rs`

- [ ] **Step 1: Add `OperatorError` variant**

In `crates/portunus-server/src/operator/cli.rs`, add to `enum OperatorError` (near the other validation variants):

```rust
    /// Advertised-endpoint resolution failed (malformed/uncovered config
    /// or no SAN-covered candidate). 010-advertised-endpoint.
    #[error("advertised_endpoint: {0}")]
    AdvertisedEndpoint(#[from] crate::advertised::ResolveEndpointError),
```

In `fn code()` add an arm (before the catch-all):
```rust
            Self::AdvertisedEndpoint(e) => e.http_code(),
```

In `fn exit_code()` add `Self::AdvertisedEndpoint(_)` to the `=> 3` validation family group.

- [ ] **Step 2: Thread resolution through create**

Change `enrollment_uri` to take the resolved endpoint instead of reading `state.server_endpoint`:

```rust
fn enrollment_uri(state: &AppState, endpoint: &str, code: &str) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let cert = URL_SAFE_NO_PAD.encode(state.server_cert_pem.as_bytes());
    format!(
        "portunus://{}/enroll?pin=sha256:{}&code={}&cert={}",
        endpoint, state.server_cert_sha256, code, cert
    )
}
```

Change `create_enrollment_command` to resolve first, pass the endpoint into both `create(...)` and `enrollment_uri(...)`. Add a `req_host: Option<&str>` parameter:

```rust
fn create_enrollment_command(
    state: &AppState,
    name: ClientName,
    target: EnrollmentTarget,
    ttl_secs: u64,
    event: &'static str,
    req_host: Option<&str>,
) -> Result<EnrollmentCommand, OperatorError> {
    let now = Utc::now();
    let ttl = chrono::Duration::from_std(Duration::from_secs(ttl_secs)).map_err(|e| {
        StoreError::Internal { message: format!("invalid enrollment ttl: {e}") }
    })?;
    let override_value = state.settings.get_advertised_endpoint().map_err(OperatorError::from)?;
    let resolved = crate::advertised::resolve_advertised_endpoint(
        &crate::advertised::resolve::ResolveInputs {
            override_value,
            seed: state.advertised_seed.clone(),
            req_host,
            control_port: state.control_port,
            san: &state.cert_san,
        },
    )?;
    let enrollments = ClientEnrollmentStore::new(Arc::clone(&state.store));
    let created = enrollments.create(CreateEnrollment {
        client_name: name.clone(),
        target,
        expires_at: now + ttl,
        now,
        advertised_endpoint: resolved.endpoint.clone(),
    })?;
    let uri = enrollment_uri(state, &resolved.endpoint, &created.code);
    let command = format!("portunus-client enroll '{uri}'");
    info!(event, client_name = %created.client_name, expires_at = %created.expires_at);
    Ok(EnrollmentCommand {
        client_name: name,
        expires_at: created.expires_at,
        command,
        uri,
    })
}
```

Add `pub use resolve::ResolveInputs;` to `advertised/mod.rs` re-exports (so the path above resolves), or reference it as `crate::advertised::resolve::ResolveInputs` as written (the module is `pub`). Keep as written.

Update `enroll_client` signature to accept and forward `req_host`:

```rust
pub fn enroll_client(
    state: &AppState,
    raw_name: &str,
    client_address: Option<&str>,
    ttl_secs: u64,
    req_host: Option<&str>,
) -> Result<EnrollmentCommand, OperatorError> {
    let name = ClientName::from_str(raw_name)?;
    if ttl_secs == 0 {
        return Err(OperatorError::InvalidEnrollmentTtl(ttl_secs));
    }
    let address = client_address.map(validate_client_address).transpose()?;
    create_enrollment_command(
        state,
        name,
        EnrollmentTarget::New { client_address: address },
        ttl_secs,
        "audit.client_enrollment_created",
        req_host,
    )
}
```

If there is a sibling re-enrollment caller of `create_enrollment_command` (the `/v1/clients/{name}/enrollment` route → `post_client_reenrollment`), find that function in `operator/cli.rs` (grep `create_enrollment_command`) and add the `req_host` parameter there too, forwarding the same value.

- [ ] **Step 3: Fix offline `enroll-client` caller**

In `crates/portunus-server/src/main.rs` `Cmd::EnrollClient`, the call `cli::enroll_client(&state, &name, address.as_deref(), ttl_secs)` → add `, None` (offline has no request Host).

- [ ] **Step 4: Update gRPC enrollment to replay persisted value**

This is Task 11 — do it now (same compile unit). See Task 11, then return.

- [ ] **Step 5: Build + commit (after Task 11)**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server --all-targets`
Expected: clean (this completes Task 8 Step 5's pending build).

```bash
cargo fmt --all
git add crates/portunus-server/src/operator/cli.rs crates/portunus-server/src/main.rs
git commit -m "feat(operator): resolve advertised endpoint at enrollment creation"
```

---

## Phase 6 — gRPC replay + legacy path

### Task 11: gRPC enroll replays persisted endpoint; legacy NULL resolves

**Files:**
- Modify: `crates/portunus-server/src/grpc/enrollment.rs`

- [ ] **Step 1: Write the failing test**

Add a `#[cfg(test)]` test to `grpc/enrollment.rs` (or the existing gRPC test module — mirror `src/grpc/service.rs:947` test-state pattern). The test creates an enrollment via `cli::enroll_client(&state, "edge", None, 300, Some("public.example:443"))`, extracts the `code` from the returned `uri`, calls the gRPC `enroll` with that code, and asserts the returned `WireCredentialBundle.server_endpoint == "public.example:7443"` (NOT `127.0.0.1:...`). Use the fixture PEM so SAN covers `public.example`.

```rust
#[tokio::test]
async fn redeemed_bundle_endpoint_matches_creation_resolution() {
    let state = test_state_with_fixture_cert(); // build Arc<AppState>; cert = san_fixture.pem
    let cmd = crate::operator::cli::enroll_client(
        &state, "edge-x", None, 300, Some("public.example:443"),
    )
    .expect("create enrollment");
    // uri: portunus://public.example:7443/enroll?...&code=CODE&cert=...
    let code = cmd
        .uri
        .split("code=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();

    let svc = ClientEnrollmentService::new(Arc::clone(&state));
    let resp = svc
        .enroll(tonic::Request::new(EnrollClientRequest { code }))
        .await
        .expect("enroll ok");
    assert_eq!(resp.into_inner().server_endpoint, "public.example:7443");
}
```

(Build `test_state_with_fixture_cert` by copying the `AppState::new` construction from the updated `grpc/service.rs:947` helper, passing `include_str!("../advertised/testdata/san_fixture.pem")` as the cert PEM, `advertised_seed = None`, `control_port = 7443`.)

- [ ] **Step 2: Run to verify it fails**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib grpc::enrollment`
Expected: FAIL to compile (`state.server_endpoint` removed) — confirms the dependency.

- [ ] **Step 3: Implement replay + legacy resolve**

Replace the response construction in `enroll` (currently lines 53-60):

```rust
        let server_endpoint = match issued.advertised_endpoint.clone() {
            Some(ep) => ep,
            None => {
                // Legacy pre-V010 row: resolve once, fail closed.
                let override_value = self
                    .state
                    .settings
                    .get_advertised_endpoint()
                    .map_err(|e| Status::internal(format!("settings: {e}")))?;
                crate::advertised::resolve_advertised_endpoint(
                    &crate::advertised::resolve::ResolveInputs {
                        override_value,
                        seed: self.state.advertised_seed.clone(),
                        req_host: None,
                        control_port: self.state.control_port,
                        san: &self.state.cert_san,
                    },
                )
                .map_err(|e| {
                    warn!(event = "client.enrollment_failed", error = %e);
                    Status::failed_precondition(e.http_code())
                })?
                .endpoint
            }
        };
        Ok(Response::new(WireCredentialBundle {
            version: 1,
            client_name: issued.client_name.to_string(),
            server_endpoint,
            server_cert_sha256: self.state.server_cert_sha256.clone(),
            server_cert_pem: self.state.server_cert_pem.clone(),
            token: issued.token,
        }))
```

- [ ] **Step 4: Run to verify it passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib grpc::enrollment`
Expected: PASS.

- [ ] **Step 5: Now complete Task 8 Step 6 and Task 10 Step 5 builds**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server --all-targets && PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib`
Expected: full lib suite green.

- [ ] **Step 6: Commit**

```bash
cargo fmt --all
git add crates/portunus-server/src/grpc/enrollment.rs
git commit -m "feat(grpc): replay persisted endpoint; legacy NULL rows resolve fail-closed"
```

---

## Phase 7 — Operator HTTP surface

### Task 12: `post_client_enrollments` passes request Host

**Files:**
- Modify: `crates/portunus-server/src/operator/http.rs:178-200` (and `post_client_reenrollment` similarly)

- [ ] **Step 1: Write the failing test**

In `crates/portunus-server/tests/http_client_enrollments_contract.rs`, add a test that sends `POST /v1/client-enrollments` with a `Host: public.example` header (no explicit override/seed) and asserts the returned `uri` starts with `portunus://public.example:7443/enroll?`. (The fixture cert covers `public.example`; `control_port` in this harness is 7443 — confirm/set in `build_router()`.)

```rust
#[tokio::test]
async fn enrollment_uri_derives_from_request_host() {
    let (router, _tokens, _t, _dir) = build_router();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/client-enrollments")
        .header("content-type", "application/json")
        .header("host", "public.example")
        .header("x-portunus-csrf", "1")
        // plus whatever auth header build_router's req() helper sets — copy from req()
        .body(Body::from(
            json!({"name":"edge-h","address":"e.example.com","ttl_secs":300}).to_string(),
        ))
        .unwrap();
    let resp = router.oneshot(request).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = body_json(resp).await;
    assert!(
        body["uri"].as_str().unwrap().starts_with("portunus://public.example:7443/enroll?"),
        "got {}", body["uri"]
    );
}
```

(Copy auth/CSRF header wiring from this file's existing `req()` helper so the request passes `auth_middleware`.)

- [ ] **Step 2: Run to verify it fails**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test http_client_enrollments_contract enrollment_uri_derives_from_request_host`
Expected: FAIL — handler ignores Host; URI uses loopback.

- [ ] **Step 3: Implement Host extraction**

In `crates/portunus-server/src/operator/http.rs`, change `post_client_enrollments` to read the `Host` header and forward it. Add `axum::http::HeaderMap` to the extractor list:

```rust
async fn post_client_enrollments(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    headers: axum::http::HeaderMap,
    Json(body): Json<EnrollmentBody>,
) -> Result<(StatusCode, Json<EnrollmentResponse>), ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    let req_host = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok());
    let enrollment = cli::enroll_client(
        &state,
        &body.name,
        Some(&body.address),
        body.ttl_secs.unwrap_or(600),
        req_host,
    )?;
    Ok((StatusCode::CREATED, Json(EnrollmentResponse {
        client_name: enrollment.client_name.to_string(),
        expires_at: enrollment.expires_at.to_rfc3339(),
        command: enrollment.command,
        uri: enrollment.uri,
    })))
}
```

Do the same for `post_client_reenrollment` (find it in the same file; add `headers: HeaderMap`, derive `req_host`, forward into whatever cli fn it calls — that fn got the `req_host` param in Task 10).

`ApiError` already maps `OperatorError` via `From`; ensure the `From<OperatorError>` status match (http.rs ~1420) sends `OperatorError::AdvertisedEndpoint(_)` to **422**. Add to the `StatusCode::UNPROCESSABLE_ENTITY` group (find the existing 422 arm — the multi-target/SNI capability group uses 422; add `| OperatorError::AdvertisedEndpoint(_)` there). If there is no explicit 422 arm, add:

```rust
            OperatorError::AdvertisedEndpoint(_) => StatusCode::UNPROCESSABLE_ENTITY,
```

- [ ] **Step 4: Run to verify it passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test http_client_enrollments_contract`
Expected: PASS (new test + existing, now asserting `public.example`).

- [ ] **Step 5: Commit**

```bash
cargo fmt --all
git add crates/portunus-server/src/operator/http.rs crates/portunus-server/tests/http_client_enrollments_contract.rs
git commit -m "feat(operator): derive enrollment endpoint from request Host"
```

---

### Task 13: `GET`/`PUT /v1/settings/advertised-endpoint`

**Files:**
- Modify: `crates/portunus-server/src/operator/http.rs` (router + 2 handlers + body/response types)
- Test: `crates/portunus-server/tests/http_settings_advertised_endpoint.rs` (new)

- [ ] **Step 1: Write the failing integration test**

Create `crates/portunus-server/tests/http_settings_advertised_endpoint.rs`. Copy the `build_router()` / `req()` / `body_json()` helpers from `http_client_enrollments_contract.rs` (same harness; duplicate the helper block — these contract tests intentionally keep harness code local). Tests:

```rust
// helpers copied from http_client_enrollments_contract.rs (build_router, req, body_json)

#[tokio::test]
async fn get_returns_200_with_effective_when_resolvable() {
    let (router, _t, _a, _d) = build_router(); // fixture cert covers public.example + loopback
    let resp = router.clone().oneshot(get_req("/v1/settings/advertised-endpoint")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let b = body_json(resp).await;
    assert!(b["override"].is_null());
    assert_eq!(b["source"], "loopback");
    assert_eq!(b["effective"], "127.0.0.1:7443");
}

#[tokio::test]
async fn put_then_get_round_trips() {
    let (router, _t, _a, _d) = build_router();
    let put = router.clone().oneshot(put_req(
        "/v1/settings/advertised-endpoint",
        json!({"advertised_endpoint": "public.example:7443"}),
    )).await.unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let get = router.oneshot(get_req("/v1/settings/advertised-endpoint")).await.unwrap();
    let b = body_json(get).await;
    assert_eq!(b["override"], "public.example:7443");
    assert_eq!(b["source"], "override");
    assert_eq!(b["effective"], "public.example:7443");
}

#[tokio::test]
async fn put_rejects_grammar_with_422_endpoint_invalid() {
    let (router, _t, _a, _d) = build_router();
    let resp = router.oneshot(put_req(
        "/v1/settings/advertised-endpoint",
        json!({"advertised_endpoint": "https://x:7443"}),
    )).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let b = body_json(resp).await;
    assert_eq!(b["error"]["code"], "endpoint_invalid");
}

#[tokio::test]
async fn put_rejects_uncovered_host_with_422_not_in_cert_san() {
    let (router, _t, _a, _d) = build_router();
    let resp = router.oneshot(put_req(
        "/v1/settings/advertised-endpoint",
        json!({"advertised_endpoint": "not.in.cert:7443"}),
    )).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let b = body_json(resp).await;
    assert_eq!(b["error"]["code"], "endpoint_not_in_cert_san");
}

#[tokio::test]
async fn put_missing_csrf_is_rejected() {
    let (router, _t, _a, _d) = build_router();
    // build a PUT WITHOUT the X-Portunus-CSRF header (copy req() and drop it)
    let resp = router.oneshot(put_req_no_csrf(
        "/v1/settings/advertised-endpoint",
        json!({"advertised_endpoint": "public.example:7443"}),
    )).await.unwrap();
    assert!(resp.status().is_client_error());
}
```

Add `get_req` / `put_req` / `put_req_no_csrf` mirroring this repo's existing `req()` (GET = method GET no body; PUT = method PUT + JSON body + `X-Portunus-CSRF: 1` + the auth header `req()` uses).

- [ ] **Step 2: Run to verify it fails**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test http_settings_advertised_endpoint`
Expected: FAIL — route not found (404).

- [ ] **Step 3: Implement handlers + route**

In `crates/portunus-server/src/operator/http.rs`:

Add request/response types near the other body structs:

```rust
#[derive(serde::Deserialize)]
struct AdvertisedEndpointBody {
    /// `null`/empty clears the override.
    advertised_endpoint: Option<String>,
}

#[derive(serde::Serialize)]
struct AdvertisedEndpointView {
    r#override: Option<String>,
    effective: Option<String>,
    source: Option<crate::advertised::EndpointSource>,
    diagnostic: Option<String>,
}
```

Add handlers:

```rust
async fn get_advertised_endpoint(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
) -> Result<Json<AdvertisedEndpointView>, ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    let override_value = state
        .settings
        .get_advertised_endpoint()
        .map_err(ApiError::from_store)?;
    let view = match crate::advertised::resolve_advertised_endpoint(
        &crate::advertised::resolve::ResolveInputs {
            override_value: override_value.clone(),
            seed: state.advertised_seed.clone(),
            req_host: None,
            control_port: state.control_port,
            san: &state.cert_san,
        },
    ) {
        Ok(r) => AdvertisedEndpointView {
            r#override: override_value,
            effective: Some(r.endpoint),
            source: Some(r.source),
            diagnostic: None,
        },
        Err(e) => AdvertisedEndpointView {
            r#override: override_value,
            effective: None,
            source: None,
            diagnostic: Some(e.to_string()),
        },
    };
    Ok(Json(view))
}

async fn put_advertised_endpoint(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Json(body): Json<AdvertisedEndpointBody>,
) -> Result<Json<AdvertisedEndpointView>, ApiError> {
    crate::operator::rbac::require_role(&identity, portunus_auth::OperatorRole::Superadmin)
        .map_err(|_| ApiError::new(StatusCode::FORBIDDEN, "role_required", "superadmin only"))?;
    let value = body.advertised_endpoint.clone().filter(|s| !s.is_empty());
    // Grammar first (422 endpoint_invalid), then SAN coverage (422 not_in_cert_san).
    if let Some(v) = &value {
        let (host, _) = crate::advertised::grammar::validate_authority(v)
            .map_err(|reason| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "endpoint_invalid", reason))?;
        if !state.cert_san.covers(host) {
            return Err(ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "endpoint_not_in_cert_san",
                format!("host {host} is not covered by the server certificate SAN; reissue/redeploy the cert to cover it"),
            ));
        }
    }
    state
        .settings
        .set_advertised_endpoint(value)
        .map_err(ApiError::from_store)?;
    // Return the fresh effective view (reuse GET logic).
    get_advertised_endpoint(State(state), Extension(identity)).await
}
```

Add an `ApiError::from_store` helper if none exists (mirror existing store-error mapping in this file; if `ApiError: From<StoreError>` already exists, use `.map_err(ApiError::from)` and delete the `from_store` references).

Register routes in `router()` inside the `protected` block, next to `/v1/client-enrollments`:

```rust
        .route(
            "/v1/settings/advertised-endpoint",
            get(get_advertised_endpoint).put(put_advertised_endpoint),
        )
```

- [ ] **Step 4: Run to verify it passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test http_settings_advertised_endpoint`
Expected: PASS (5 tests).

- [ ] **Step 5: Clippy + commit**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo clippy -p portunus-server --all-targets -- -D warnings`

```bash
cargo fmt --all
git add crates/portunus-server/src/operator/http.rs crates/portunus-server/tests/http_settings_advertised_endpoint.rs
git commit -m "feat(operator): GET/PUT /v1/settings/advertised-endpoint"
```

---

## Phase 8 — Web UI

### Task 14: Settings page field + API hooks + i18n

**Files:**
- Create: `webui/src/api/settings.ts`
- Modify: `webui/src/pages/Settings.tsx`
- Modify: `webui/src/i18n/en.json`, `webui/src/i18n/zh-CN.json`

- [ ] **Step 1: Add API hooks**

Create `webui/src/api/settings.ts` (mirror `webui/src/api/clients.ts` patterns — `apiFetch`, `useQuery`, `useMutation`, `useQueryClient`):

```ts
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "./client";

export interface AdvertisedEndpointView {
  override: string | null;
  effective: string | null;
  source: "override" | "seed" | "derived" | "loopback" | null;
  diagnostic: string | null;
}

export const ADVERTISED_ENDPOINT_KEY = ["settings", "advertised-endpoint"] as const;

export function useAdvertisedEndpoint() {
  return useQuery({
    queryKey: ADVERTISED_ENDPOINT_KEY,
    queryFn: () =>
      apiFetch<AdvertisedEndpointView>("/v1/settings/advertised-endpoint"),
  });
}

export function useSetAdvertisedEndpoint() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (advertised_endpoint: string | null) =>
      apiFetch<AdvertisedEndpointView>("/v1/settings/advertised-endpoint", {
        method: "PUT",
        body: JSON.stringify({ advertised_endpoint }),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ADVERTISED_ENDPOINT_KEY });
    },
  });
}
```

- [ ] **Step 2: Add i18n strings**

In `webui/src/i18n/en.json`, under the `settings` object add:

```json
"advertisedHeading": "Client connect address",
"advertisedDescription": "host:port the client uses to reach the gRPC control plane. Leave empty to auto-derive from this page's host. On Railway/reverse-proxied setups set the TCP-proxy host:port and ensure the server certificate covers that host.",
"advertisedEffective": "Effective: {{effective}} ({{source}})",
"advertisedSave": "Save",
"advertisedClear": "Clear (auto)",
"advertisedDiagnostic": "Not resolvable: {{diagnostic}}"
```

Add the same keys to `webui/src/i18n/zh-CN.json` with Chinese translations (mirror the tone of existing zh-CN entries):

```json
"advertisedHeading": "客户端连接地址",
"advertisedDescription": "客户端用于连接 gRPC 控制面的 host:port。留空则按本页域名自动推导。Railway/反向代理场景请填 TCP 代理的 host:port，并确保服务器证书 SAN 覆盖该 host。",
"advertisedEffective": "当前生效: {{effective}}（{{source}}）",
"advertisedSave": "保存",
"advertisedClear": "清除（自动）",
"advertisedDiagnostic": "无法解析: {{diagnostic}}"
```

- [ ] **Step 3: Add the Settings card**

In `webui/src/pages/Settings.tsx`, add a new `Card` after the language card. Use a controlled `<input>` seeded from `data.override ?? ""`, a Save button calling `useSetAdvertisedEndpoint().mutate(value || null)`, a Clear button calling `.mutate(null)`, and render `data.effective`/`data.source` plus `data.diagnostic` (if present, in a destructive/warning style). Follow the existing card structure already in the file:

```tsx
import { useState } from "react";
// ...existing imports...
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  useAdvertisedEndpoint,
  useSetAdvertisedEndpoint,
} from "@/api/settings";

function AdvertisedEndpointCard() {
  const { t } = useTranslation();
  const { data } = useAdvertisedEndpoint();
  const save = useSetAdvertisedEndpoint();
  const [value, setValue] = useState<string | null>(null);
  const current = value ?? data?.override ?? "";
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("settings.advertisedHeading")}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <p className="text-sm text-muted-foreground">
          {t("settings.advertisedDescription")}
        </p>
        <Input
          value={current}
          placeholder="proxy.example.com:34567"
          onChange={(e) => setValue(e.target.value)}
        />
        <div className="flex gap-2">
          <Button
            onClick={() => save.mutate(current.trim() === "" ? null : current.trim())}
            disabled={save.isPending}
          >
            {t("settings.advertisedSave")}
          </Button>
          <Button
            variant="outline"
            onClick={() => {
              setValue("");
              save.mutate(null);
            }}
            disabled={save.isPending}
          >
            {t("settings.advertisedClear")}
          </Button>
        </div>
        {data?.effective && (
          <p className="text-sm">
            {t("settings.advertisedEffective", {
              effective: data.effective,
              source: data.source,
            })}
          </p>
        )}
        {data?.diagnostic && (
          <p className="text-sm text-destructive">
            {t("settings.advertisedDiagnostic", { diagnostic: data.diagnostic })}
          </p>
        )}
        {save.isError && (
          <p className="text-sm text-destructive">
            {(save.error as Error).message}
          </p>
        )}
      </CardContent>
    </Card>
  );
}
```

Render `<AdvertisedEndpointCard />` inside the page's root `<div>` after the language `Card`. If `@/components/ui/input` or `button` paths differ, grep `webui/src/components/ui` and use the actual exports (this repo is shadcn-based; both exist).

- [ ] **Step 4: Typecheck + build the UI**

Run: `cd webui && pnpm install --frozen-lockfile && pnpm build`
Expected: `tsc -b` passes, `vite build` succeeds, size-limit within budget.

- [ ] **Step 5: Commit**

```bash
cd /Users/zingerbee/Documents/forward-rs
git add webui/src/api/settings.ts webui/src/pages/Settings.tsx webui/src/i18n/en.json webui/src/i18n/zh-CN.json
git commit -m "feat(webui): advertised endpoint settings field"
```

---

## Phase 9 — Full regression

### Task 15: Workspace green + spec coverage sweep

**Files:** none (verification only)

- [ ] **Step 1: Full workspace test**

Run: `cargo test --workspace`
Expected: all green. (No `PORTUNUS_SKIP_WEBUI` here — `webui/dist` exists from Task 14 Step 4, so the embed build works. If iterating without a UI build, use `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace`.)

- [ ] **Step 2: Full clippy + fmt gate**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: clean.

- [ ] **Step 3: Manual spec-coverage checklist**

Confirm each is covered by a passing test or code path; fix gaps before closing:
- Resolve-once-at-creation + replay-at-redeem → Task 11 test.
- Tier precedence + explicit hard-error vs implicit fallthrough → Task 7 tests.
- `ConfiguredEndpointInvalid` for bad CLI *and* env seed → Task 7 `tier2_bad_env_seed_is_invalid` (env path exercised via `advertised_seed`); add an explicit env-var test in `main.rs` only if a unit-testable seam exists, else rely on the seed-path test (documented equivalence).
- SAN webpki parity (wildcard single-label, case-insensitive, IPv4→IP) → Task 6 tests.
- Host parsing contract → Task 5 tests.
- GET always 200 incl. `effective:null`+diagnostic → Task 13 (add one more test: set an uncovered override is impossible via PUT, so simulate diagnostic by constructing state where seed is uncovered — if not feasible via HTTP, assert the success+loopback path only and note GET-null is unit-covered by resolver Err mapping; acceptable).
- 422 codes `endpoint_invalid` / `endpoint_not_in_cert_san` → Task 13 tests.
- Legacy NULL redeem fail-closed → Task 9 + Task 11 (extend Task 11 with a legacy-NULL + uncovered seed → `failed_precondition` assertion).
- Migration V010 + schema handshake → Task 1.
- Railway script unchanged; env read directly → Task 8 Step 3 helper.

- [ ] **Step 4: Final commit (if any checklist fixes)**

```bash
cargo fmt --all
git add -A
git commit -m "test: close advertised-endpoint spec-coverage gaps"
```

- [ ] **Step 5: Update the spec status**

In `docs/superpowers/specs/2026-05-17-advertised-endpoint-runtime-config-design.md`, change `Status:` line to `Status: Implemented`. Commit:

```bash
git add docs/superpowers/specs/2026-05-17-advertised-endpoint-runtime-config-design.md
git commit -m "docs: mark advertised-endpoint spec implemented"
```

---

## Self-Review Notes (author)

- **Spec coverage:** every Design §1–§6 + Testing bullet maps to Tasks 1–15 (see Task 15 Step 3 checklist). Migration/Compatibility legacy path → Tasks 9/11.
- **Type consistency:** `ResolveInputs`, `ResolvedAdvertisedEndpoint`, `EndpointSource`, `ResolveEndpointError`, `ConfigTier`, `CertSanSet`, `SqliteSettingsStore`, `CreateEnrollment.advertised_endpoint: String`, `IssuedClientCredential.advertised_endpoint: Option<String>` are used identically across Tasks 3/7/9/10/11/13.
- **Ordering hazard:** Tasks 8/10/11 are interlocked (removing `server_endpoint` breaks two readers). The plan explicitly defers the full build to Task 11 Step 5 and keeps lib-scoped builds green per task; do them in order 8→9→10→11 without committing Task 8 until 11 is done (Task 8 Step 6 / Task 10 Step 5 / Task 11 Step 6 are the commit points).
- **Known soft spot:** env-var seed (`PORTUNUS_ADVERTISED_ENDPOINT`) has no direct HTTP test; covered transitively by the resolver seed-path tests + the `advertised_seed` helper. Acceptable per spec (CLI and env are the same tier-2 string).
