# Standalone Forwarder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract `portunus-client`'s data plane into a proto-free `portunus-forwarder` lib crate, then ship a new `portunus-standalone` binary that drives multi-rule TCP/UDP forwarding from a TOML config — no gRPC, no SQLite, no Web UI.

**Architecture:** Promote `Protocol` to `portunus-core`. `git mv` `forwarder/`, `resolver/`, `shutdown.rs` from `portunus-client` to `portunus-forwarder`. Rewrite wire-construction helpers (`drain_to_proto` etc.) as wire-neutral snapshot getters; relocate proto translation into `portunus-client` `From<Snapshot> for proto::*` impls. `portunus-client` behavior is byte-identical (existing `*_wire_compat` tests are the regression gate). New `portunus-standalone` binary reads TOML → builds `ClientRule`s → spawns `run_forwarder` per rule with its own startup-gate + fatal channel + SIGHUP-aware signal handler. No StatsSink trait; each binary owns its reporter.

**Tech Stack:** Rust 2024, MSRV 1.88. Workspace deps: tonic, prost, tokio, tokio-util, hickory-resolver, nix, blake3, toml (new), clap. portunus-standalone Cargo deps exclude tonic/tonic-prost/prost/portunus-proto/portunus-auth.

**Spec:** [docs/superpowers/specs/2026-05-14-standalone-forwarder-design.md](../specs/2026-05-14-standalone-forwarder-design.md)

---

## File Structure

### Workspace
- Modify: `Cargo.toml` — add `crates/portunus-forwarder` and `crates/portunus-standalone` to members (members glob `crates/*` already covers them); add `toml` workspace dep

### portunus-core (`crates/portunus-core/`)
- Create: `src/protocol.rs` — authoritative `Protocol` enum + serde (lowercase) + Display + FromStr
- Modify: `src/lib.rs` — `pub mod protocol; pub use protocol::Protocol;`

### portunus-proto (`crates/portunus-proto/`)
- Modify: `src/lib.rs` (or build.rs-generated wrapper module) — `impl From<portunus_core::Protocol> for v1::Protocol` + reverse; keep `v1::Protocol` wire i32 untouched

### portunus-server (`crates/portunus-server/`)
- Modify: `src/rules.rs:37-56` — delete local `Protocol` enum, replace with `pub use portunus_core::Protocol;` + `impl From<portunus_core::Protocol> for serde_json`-compatible JSON (already lowercase, no extra glue)
- Modify: any match sites referencing the old `crate::rules::Protocol` — point to `portunus_core::Protocol`

### portunus-forwarder (`crates/portunus-forwarder/`)
- Create: `Cargo.toml` — deps `portunus-core, portunus-auth, tokio, tokio-util, tokio-rustls, rustls, tokio-stream, nix, hickory-resolver, async-trait, tracing, thiserror, rand, blake3 (optional, only if used)`. **No** `portunus-proto`.
- Create: `src/lib.rs` — `pub mod` declarations + `pub use` re-exports per spec §3.3
- Move (via git mv): `src/forwarder/` (entire directory tree) ← from `crates/portunus-client/src/forwarder/`
- Move: `src/resolver/` ← from `crates/portunus-client/src/resolver/`
- Move: `src/shutdown.rs` ← from `crates/portunus-client/src/shutdown.rs`
- Create: `benches/*.rs` (re-targeted from portunus-client benches)
- Modify (post-move): `src/forwarder/mod.rs` and submodules — replace `portunus_proto::v1::Protocol` with `portunus_core::Protocol`
- Modify: `src/forwarder/rate_limit/stats.rs` — delete `drain_to_proto`, add `drain() -> Option<RateLimitStatsSnapshot>`
- Modify: `src/forwarder/rate_limit/scope.rs` — delete `drain_to_proto` on `OwnerRateLimitStatsRegistry`, add `drain() -> Vec<OwnerRateLimitStatsSnapshot>`; reject reason enum mirrors via `RateLimitRejectReason`
- Modify: `src/forwarder/rate_limit/mod.rs` — add `pub use scope::*; pub use stats::*;` so lib.rs short paths compile
- Modify: `src/forwarder/sni/mod.rs` — add `pub use listener::*;`
- Modify: `src/forwarder/sni/listener.rs` — `SniListenerCounters::snapshot(listen_port) -> SniListenerStatsSnapshot` (proto-free)
- Modify: `src/forwarder/stats.rs` — add `RuleStatsSnapshotBasic` + `RuleStats::snapshot_basic() -> RuleStatsSnapshotBasic`; add all snapshot type definitions (PerPort/PerTarget/RateLimit/OwnerRateLimit/SniListener/TargetHealth/RateLimitRejectReason/RuleStatsSnapshot)
- Modify: `src/forwarder/failover.rs` — keep `Health` private; add proto-free `MultiTargetObservability::snapshot_per_target(targets: &[MultiTarget]) -> (u64, Vec<PerTargetStatsSnapshot>)`
- Modify: `src/resolver/mod.rs` — add `LiveResolver::<HickoryResolver>::with_system_defaults() -> io::Result<Self>`

### portunus-client (`crates/portunus-client/`)
- Modify: `Cargo.toml` — remove direct deps that became forwarder-only (`nix`, `hickory-resolver`, `async-trait`, `tokio-rustls`, `rustls`, `tokio-stream`); add `portunus-forwarder = { workspace = true }`
- Modify: `src/main.rs` — `use portunus_forwarder::shutdown::Shutdown;` etc.
- Modify: `src/control.rs` — replace `use crate::forwarder::...` with `use portunus_forwarder::...`; add wire translation `From<*Snapshot> for proto::v1::*`; add `build_rule_stats_snapshot(rule_id, &slot) -> RuleStatsSnapshot`; refactor `send_stats_report` to use `build_rule_stats_snapshot(...).into()`
- Modify: `src/port_groups.rs` — replace `proto::v1::SniListenerStats` returns with `Vec<SniListenerStatsSnapshot>`; client-side mapper translates to proto at the `send_stats_report` boundary
- Delete (after git mv): `src/forwarder/`, `src/resolver/`, `src/shutdown.rs`

### portunus-standalone (`crates/portunus-standalone/`)
- Create: `Cargo.toml` — deps `portunus-core, portunus-forwarder, tokio, tokio-util, tracing, tracing-subscriber, clap, toml, serde, blake3, thiserror, libc (for getrlimit)`. **No** tonic/proto/auth.
- Create: `src/main.rs` — clap CLI + tokio runtime bootstrap
- Create: `src/config.rs` — TOML schema structs + `Config::load_from_search_paths` + `Config::validate` + `into_iter_rules` + `RuleId::derive(name)` (blake3 prefix) + collision detection
- Create: `src/signal.rs` — `install_standalone_signal_handler(shutdown) -> io::Result<JoinHandle<()>>` with SIGHUP no-op + shutdown.cancelled() select arm
- Create: `src/reporter.rs` — `spawn_standalone_reporter(rule_stats, registry, interval, cancel) -> JoinHandle<()>`
- Create: `src/runtime.rs` — `pub async fn run(cfg: Config) -> ExitCode` with startup gate, fatal channel, fatal_flag, biased select
- Create: `tests/fixtures/valid_minimal.toml`, `tests/fixtures/valid_full.toml`, `tests/fixtures/valid_udp.toml`, `tests/fixtures/invalid_unknown_field.toml`, `tests/fixtures/invalid_range_mismatch.toml`, `tests/fixtures/invalid_no_rules.toml`
- Create: `tests/check_mode.rs` — 6 fixture exit-code tests
- Create: `tests/smoke.rs` — TCP echo loopback + SIGHUP ignored + fatal exit 1 cases

### portunus-e2e (`crates/portunus-e2e/`)
- Create: `tests/standalone_tcp_basic.rs` — single-rule TCP echo
- Create: `tests/standalone_failover.rs` — kill primary target, verify secondary picks up
- Create: `tests/standalone_proxy_v2.rs` — v2 prelude reaches backend

### Docs / Ops
- Create: `docs/content/docs/operations/standalone.mdx` (English)
- Create: `docs/content/docs/zh/operations/standalone.mdx` (中文)
- Modify: `docs/content/docs/operations/meta.json` + `docs/content/docs/zh/operations/meta.json` — register new page
- Modify: `README.md` — add Standalone section + example TOML
- Modify: `CHANGELOG.md` — v1.5.0 entry
- Modify: `Makefile` — `standalone` target (cargo build), `standalone-check` target (--check fixture sweep)

---

## Phase 1: portunus-core::Protocol + skeleton

Goal: introduce authoritative `Protocol` in `portunus-core`; migrate `portunus-server` and `portunus-proto` to use it; create empty `portunus-forwarder` crate. **Workspace builds + all tests pass at end of phase.**

### Task 1.1: Create empty portunus-forwarder crate

**Files:**
- Create: `crates/portunus-forwarder/Cargo.toml`
- Create: `crates/portunus-forwarder/src/lib.rs`

- [ ] **Step 1: Inspect workspace member glob**

Run: `grep -A2 'members' Cargo.toml`
Expected: `members = ["crates/*"]` — new crate auto-included by glob.

- [ ] **Step 2: Create Cargo.toml**

```toml
[package]
name = "portunus-forwarder"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "Portunus data-plane library — TCP/UDP forwarding shared by portunus-client and portunus-standalone."

[lints]
workspace = true

[dependencies]
# Filled in Phase 2 when modules move in. Empty for now keeps the crate
# compiling as a stub.
```

- [ ] **Step 3: Create lib.rs stub**

```rust
//! Portunus data-plane library. See spec
//! `docs/superpowers/specs/2026-05-14-standalone-forwarder-design.md`.
//!
//! Phase 1 stub — modules land in Phase 2.
```

- [ ] **Step 4: Verify workspace build**

Run: `cargo build -p portunus-forwarder`
Expected: PASS (empty lib, no warnings beyond unused-crate-deps warns from other crates).

- [ ] **Step 5: Verify full workspace still builds**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/portunus-forwarder/
git commit -m "feat(forwarder): scaffold empty portunus-forwarder crate"
```

### Task 1.2: portunus-core::Protocol enum

**Files:**
- Create: `crates/portunus-core/src/protocol.rs`
- Modify: `crates/portunus-core/src/lib.rs`
- Test: `crates/portunus-core/src/protocol.rs` (#[cfg(test)] mod tests)

- [ ] **Step 1: Write failing tests in protocol.rs**

```rust
// Inside crates/portunus-core/src/protocol.rs

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn serde_round_trips_lowercase() {
        let cases = [(Protocol::Tcp, "\"tcp\""), (Protocol::Udp, "\"udp\"")];
        for (p, json) in cases {
            assert_eq!(serde_json::to_string(&p).unwrap(), json);
            assert_eq!(serde_json::from_str::<Protocol>(json).unwrap(), p);
        }
    }

    #[test]
    fn from_str_accepts_lowercase_only() {
        assert_eq!(Protocol::from_str("tcp").unwrap(), Protocol::Tcp);
        assert_eq!(Protocol::from_str("udp").unwrap(), Protocol::Udp);
        assert!(Protocol::from_str("TCP").is_err());
        assert!(Protocol::from_str("http").is_err());
    }

    #[test]
    fn display_matches_serde_repr() {
        assert_eq!(Protocol::Tcp.to_string(), "tcp");
        assert_eq!(Protocol::Udp.to_string(), "udp");
    }
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test -p portunus-core protocol::tests`
Expected: COMPILE ERROR — `Protocol` not in scope.

- [ ] **Step 3: Implement Protocol**

Add to top of `crates/portunus-core/src/protocol.rs`:

```rust
//! Authoritative `Protocol` enum used by all crates in the workspace.
//! Phase 1 of the standalone-forwarder spec; replaces the per-crate
//! `Protocol` types in `portunus-proto`, `portunus-server`, and the
//! data-plane modules.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

impl Protocol {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

impl fmt::Display for Protocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown protocol {0:?}; expected one of: tcp, udp")]
pub struct ParseProtocolError(pub String);

impl FromStr for Protocol {
    type Err = ParseProtocolError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tcp" => Ok(Self::Tcp),
            "udp" => Ok(Self::Udp),
            other => Err(ParseProtocolError(other.to_owned())),
        }
    }
}
```

- [ ] **Step 4: Register module**

Modify `crates/portunus-core/src/lib.rs` — add (alphabetical position):

```rust
pub mod protocol;

// ...existing pub use lines...
pub use protocol::{Protocol, ParseProtocolError};
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p portunus-core protocol::tests`
Expected: PASS, 3 tests.

- [ ] **Step 6: Workspace build (no Protocol consumer yet, still green)**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/portunus-core/
git commit -m "feat(core): add authoritative Protocol enum"
```

### Task 1.3: portunus-proto ↔ core::Protocol conversions

**Files:**
- Modify: `crates/portunus-proto/src/lib.rs` (or wherever wire re-exports live)
- Test: `crates/portunus-proto/tests/protocol_conversion.rs`

- [ ] **Step 1: Inspect current proto Protocol surface**

Run: `grep -rn "pub enum Protocol\|impl Protocol" crates/portunus-proto/src/ | head -10`
Note where `v1::Protocol` is exposed.

- [ ] **Step 2: Write failing conversion test**

Create `crates/portunus-proto/tests/protocol_conversion.rs`:

```rust
use portunus_core::Protocol as CoreProto;
use portunus_proto::v1::Protocol as WireProto;

#[test]
fn core_to_wire_round_trips() {
    let cases = [(CoreProto::Tcp, WireProto::Tcp), (CoreProto::Udp, WireProto::Udp)];
    for (c, w) in cases {
        assert_eq!(WireProto::from(c), w);
        assert_eq!(CoreProto::try_from(w).unwrap(), c);
    }
}

#[test]
fn wire_unspecified_fails_try_from() {
    assert!(CoreProto::try_from(WireProto::Unspecified).is_err());
}
```

- [ ] **Step 3: Run test to confirm it fails**

Run: `cargo test -p portunus-proto --test protocol_conversion`
Expected: COMPILE ERROR — `From` / `TryFrom` impls missing.

- [ ] **Step 4: Add conversion impls**

In `crates/portunus-proto/src/lib.rs` (add after the `pub mod v1` block):

```rust
impl From<portunus_core::Protocol> for v1::Protocol {
    fn from(p: portunus_core::Protocol) -> Self {
        match p {
            portunus_core::Protocol::Tcp => v1::Protocol::Tcp,
            portunus_core::Protocol::Udp => v1::Protocol::Udp,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("wire Protocol::Unspecified cannot be converted to core::Protocol")]
pub struct UnspecifiedProtocolError;

impl TryFrom<v1::Protocol> for portunus_core::Protocol {
    type Error = UnspecifiedProtocolError;
    fn try_from(p: v1::Protocol) -> Result<Self, Self::Error> {
        match p {
            v1::Protocol::Tcp => Ok(portunus_core::Protocol::Tcp),
            v1::Protocol::Udp => Ok(portunus_core::Protocol::Udp),
            v1::Protocol::Unspecified => Err(UnspecifiedProtocolError),
        }
    }
}
```

If `portunus-proto/Cargo.toml` lacks `thiserror`, add it:

```toml
[dependencies]
portunus-core = { workspace = true }
thiserror = { workspace = true }
# ...existing deps...
```

- [ ] **Step 5: Run conversion test**

Run: `cargo test -p portunus-proto --test protocol_conversion`
Expected: PASS, 2 tests.

- [ ] **Step 6: Workspace build still green (no consumer wired yet)**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/portunus-proto/
git commit -m "feat(proto): bridge core::Protocol <-> v1::Protocol"
```

### Task 1.4: Migrate portunus-server to core::Protocol

**Files:**
- Modify: `crates/portunus-server/src/rules.rs:37-56`
- Modify: any match sites referencing `crate::rules::Protocol` or `rules::Protocol`

- [ ] **Step 1: Enumerate every reference inside the server crate**

Run: `grep -rn "rules::Protocol\|rule::Protocol\|crate::rules::Protocol" crates/portunus-server/src/`
Save the list — every match will switch to `portunus_core::Protocol`.

- [ ] **Step 2: Replace the enum definition**

In `crates/portunus-server/src/rules.rs`, delete lines 37-56 (the local
`Protocol` enum + `impl Protocol`) and replace with:

```rust
// 011/012 (standalone-forwarder spec §3.5) — single authoritative
// `Protocol` enum now lives in portunus_core. JSON wire shape unchanged
// (still `"tcp"` / `"udp"` lowercase via serde rename_all).
pub use portunus_core::Protocol;
```

If `as_str()` was used elsewhere in the crate, note it — `core::Protocol`
provides the same method.

- [ ] **Step 3: Replace any qualified references**

For each match from Step 1 that says `rules::Protocol::Tcp`, leave it
alone (the `pub use` re-export above keeps the path valid). Only modify
references that wrote `crate::rules::Protocol` explicitly (no change
needed) or imported things like `crate::rules::Protocol as P`.

- [ ] **Step 4: Build server**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server`
Expected: PASS. If a compile error mentions a private item like
`Protocol::as_wire` that didn't exist on local enum, it's a method that
needs porting — port it onto `core::Protocol` as `pub fn` if so.

- [ ] **Step 5: Run server tests**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server`
Expected: PASS — JSON wire is unchanged because both enums use the same
`#[serde(rename_all = "lowercase")]`. If any `rules.json` snapshot test
fails, the diff is whitespace only and the regression is in the test
fixture, not the code.

- [ ] **Step 6: Commit**

```bash
git add crates/portunus-server/
git commit -m "refactor(server): adopt core::Protocol; drop local enum"
```

### Task 1.5: Workspace gate

**Files:**
- (run only)

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: no diff.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Full test sweep**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Commit any fmt/clippy fixups**

If anything was changed:

```bash
git add -u
git commit -m "chore: clippy/fmt sweep after Protocol migration"
```

Otherwise skip.

---

## Phase 2: Move data plane + make forwarder proto-free + snapshot getters

Goal: bulk move `forwarder/`, `resolver/`, `shutdown.rs` into `portunus-forwarder`. Strip `portunus_proto` references; add `*Snapshot` types and `snapshot_basic()` / `drain()` / `snapshot_per_target()` getters. Client builds a `RuleStatsSnapshot` per rule, then `Into::into()` to proto. All existing `*_wire_compat` tests must remain byte-identical.

### Task 2.1: git mv forwarder + resolver + shutdown.rs into portunus-forwarder

**Files:**
- Move via `git mv`: `crates/portunus-client/src/forwarder/` → `crates/portunus-forwarder/src/forwarder/`
- Move: `crates/portunus-client/src/resolver/` → `crates/portunus-forwarder/src/resolver/`
- Move: `crates/portunus-client/src/shutdown.rs` → `crates/portunus-forwarder/src/shutdown.rs`
- Modify: `crates/portunus-forwarder/Cargo.toml`
- Modify: `crates/portunus-forwarder/src/lib.rs`
- Modify: `crates/portunus-client/Cargo.toml`
- Modify: `crates/portunus-client/src/main.rs`
- Modify: `crates/portunus-client/src/control.rs` (use path replacement only)
- Modify: `crates/portunus-client/src/port_groups.rs` (use path replacement only)

- [ ] **Step 1: git mv the directories**

```bash
git mv crates/portunus-client/src/forwarder crates/portunus-forwarder/src/forwarder
git mv crates/portunus-client/src/resolver  crates/portunus-forwarder/src/resolver
git mv crates/portunus-client/src/shutdown.rs crates/portunus-forwarder/src/shutdown.rs
```

- [ ] **Step 2: Update portunus-forwarder/Cargo.toml deps**

Replace the empty `[dependencies]` block with:

```toml
[dependencies]
portunus-core = { workspace = true }
portunus-auth = { workspace = true }
tokio        = { workspace = true }
tokio-util   = { workspace = true }
tokio-rustls = { workspace = true }
tokio-stream = { workspace = true }
rustls       = { workspace = true }
nix          = { workspace = true }
hickory-resolver = { version = "0.24", default-features = false, features = ["tokio-runtime", "system-config"] }
async-trait  = "0.1"
tracing      = { workspace = true }
thiserror    = { workspace = true }
rand         = { workspace = true }
serde        = { workspace = true }

[dev-dependencies]
tempfile     = { workspace = true }
tokio-test   = { workspace = true }
criterion    = { workspace = true }
```

Note: deliberately **no** `portunus-proto`. Phase 2.2 onwards will assert this.

- [ ] **Step 3: Update portunus-forwarder/src/lib.rs**

```rust
//! Portunus data-plane library — TCP/UDP forwarding shared between
//! portunus-client (gRPC control plane) and portunus-standalone (TOML).

pub mod forwarder;
pub mod resolver;
pub mod shutdown;

// Public surface (per spec §3.3). Phase 2 lands the full re-export list
// after submodule cleanup; for the bulk-move commit we keep top-level
// modules pub so the `use portunus_forwarder::forwarder::*` callers from
// portunus-client keep compiling.
```

- [ ] **Step 4: Add portunus-forwarder dep to portunus-client**

In `crates/portunus-client/Cargo.toml`, add to `[dependencies]`:

```toml
portunus-forwarder = { path = "../portunus-forwarder", version = "0.0.0" }
```

(Or use the workspace dep table — match repo style. Adjust `version` to
match `workspace.package.version` if the workspace publishes one.)

Do **not** remove `nix`, `hickory-resolver`, etc. yet — Task 2.12 cleans
them up after every `use` path has been migrated.

- [ ] **Step 5: Bulk replace use paths in portunus-client**

Run a sed sweep — exact paths because we want zero surprise:

```bash
# Run from repo root
cd crates/portunus-client/src

# `crate::forwarder::` -> `portunus_forwarder::forwarder::`
grep -rln 'crate::forwarder::' . | xargs sed -i.bak 's|crate::forwarder::|portunus_forwarder::forwarder::|g'

# `crate::resolver::` -> `portunus_forwarder::resolver::`
grep -rln 'crate::resolver::' . | xargs sed -i.bak 's|crate::resolver::|portunus_forwarder::resolver::|g'

# `crate::shutdown::` -> `portunus_forwarder::shutdown::`
grep -rln 'crate::shutdown::' . | xargs sed -i.bak 's|crate::shutdown::|portunus_forwarder::shutdown::|g'

# Remove .bak files
find . -name '*.bak' -delete

cd ../../../
```

For `main.rs`, remove the `mod forwarder;`, `mod resolver;`, `mod
shutdown;` lines — they no longer exist as sibling modules. Replace any
`use crate::shutdown::Shutdown;` left over.

- [ ] **Step 6: Build portunus-client (expect lots of failures, fix in subsequent tasks)**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-client`
Expected: at this commit there will likely be **broken builds** if
forwarder/* still imports proto by paths that have moved — but the goal
of THIS commit is to ratify the move. Compile errors about
`portunus_proto::*` references inside forwarder are expected and get
cleaned up in Task 2.2. Compile errors about path mismatches inside
portunus-client should be 0 (sed swept them).

If portunus-client itself fails on `use crate::forwarder::...` you missed
one — grep again and fix.

- [ ] **Step 7: Commit the bulk move (broken intermediate state ok)**

```bash
git add -A
git commit -m "refactor(forwarder): git mv forwarder/, resolver/, shutdown.rs to portunus-forwarder

Intermediate state: portunus_proto references inside the moved code are
still present and the workspace does not build. Phase 2.2 makes the
forwarder crate proto-free."
```

### Task 2.2: Strip portunus_proto from forwarder crate (Protocol enum only)

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/mod.rs` (use line; protocol uses)
- Modify: `crates/portunus-forwarder/src/forwarder/proxy.rs:350`
- Modify: `crates/portunus-forwarder/src/forwarder/splice.rs:22`

- [ ] **Step 1: Enumerate proto refs inside forwarder**

Run:

```bash
grep -rn 'portunus_proto' crates/portunus-forwarder/src/ | wc -l
grep -rn 'portunus_proto' crates/portunus-forwarder/src/
```

Catalog every match. The expected residual offenders after the cleanup
are:
- `forwarder/mod.rs:37` `use portunus_proto::v1::Protocol;`
- `forwarder/splice.rs:22` same
- `forwarder/proxy.rs:350` `portunus_proto::v1::Protocol::Tcp`
- `forwarder/rate_limit/stats.rs:19` proto types (Task 2.3 handles)
- `forwarder/rate_limit/scope.rs:790/795/1516` proto types (Task 2.3-2.4)
- `port_groups.rs` references — **stays in client**, not in forwarder, so skip

For this task, fix only the Protocol-enum references (other proto refs
are wire types — handled later).

- [ ] **Step 2: Replace Protocol imports**

Run:

```bash
# Inside crates/portunus-forwarder/src
grep -rln 'use portunus_proto::v1::Protocol' . \
  | xargs sed -i.bak 's|use portunus_proto::v1::Protocol|use portunus_core::Protocol|g'

grep -rln 'portunus_proto::v1::Protocol::' . \
  | xargs sed -i.bak 's|portunus_proto::v1::Protocol::|portunus_core::Protocol::|g'

find . -name '*.bak' -delete
```

- [ ] **Step 3: Build portunus-forwarder (expect remaining proto refs to fail)**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder`
Expected: failures originate from `rate_limit/stats.rs` and `rate_limit/scope.rs`
referencing `portunus_proto::v1::OwnerRateLimitStats` and friends.
**No** Protocol-related errors should remain.

If new errors appear in any other forwarder module, the sed missed
something. Grep again to confirm.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "refactor(forwarder): drop portunus_proto::v1::Protocol; use core::Protocol"
```

### Task 2.3: Wire-neutral RateLimitStatsSnapshot + drain() (per-rule)

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/stats.rs` (add snapshot type)
- Modify: `crates/portunus-forwarder/src/forwarder/rate_limit/stats.rs` (replace drain_to_proto with drain)
- Test: `crates/portunus-forwarder/src/forwarder/rate_limit/stats.rs` (#[cfg(test)] mod tests)

- [ ] **Step 1: Add RateLimitStatsSnapshot + RateLimitRejectReason to stats.rs**

Append to `crates/portunus-forwarder/src/forwarder/stats.rs`:

```rust
/// Mirrors `portunus_proto::v1::RateLimitRejectReason` 1:1.
/// Forwarder stays proto-free; client maps via `From`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RateLimitRejectReason {
    Unspecified,
    ConnConcurrent,
    ConnRate,
    UdpFlowRate,
    OwnerConcurrent,
    OwnerConnRate,
    OwnerUdpFlowRate,
}

/// Wire-neutral mirror of `proto::v1::RateLimitStats` (spec §5.2).
/// Lives in forwarder so the data plane can build it without touching
/// proto; client translates via `From<RateLimitStatsSnapshot> for
/// proto::v1::RateLimitStats`.
#[derive(Clone, Debug, Default)]
pub struct RateLimitStatsSnapshot {
    pub reject_total: Vec<(RateLimitRejectReason, u64)>,
    pub throttle_micros_in: u64,
    pub throttle_micros_out: u64,
    pub active_connections: u32,
}
```

- [ ] **Step 2: Write failing test**

In `crates/portunus-forwarder/src/forwarder/rate_limit/stats.rs` (#[cfg(test)] mod tests, add to existing):

```rust
#[test]
fn drain_returns_none_when_accumulator_is_empty() {
    let acc = RateLimitStatsAccumulator::new();
    assert!(acc.drain().is_none(), "empty accumulator must drain to None for proto3 default-stripping");
}

#[test]
fn drain_returns_snapshot_with_active_only_when_set() {
    let acc = RateLimitStatsAccumulator::new();
    acc.set_active_connections(5);
    let snap = acc.drain().expect("non-empty accumulator drains to Some");
    assert_eq!(snap.active_connections, 5);
    assert_eq!(snap.throttle_micros_in, 0);
    assert_eq!(snap.throttle_micros_out, 0);
    assert!(snap.reject_total.is_empty());
}
```

- [ ] **Step 3: Verify tests compile-fail**

Run: `cargo test -p portunus-forwarder rate_limit::stats::tests`
Expected: COMPILE ERROR — `drain()` method missing.

- [ ] **Step 4: Implement drain() and delete drain_to_proto()**

Replace the existing `drain_to_proto` method on
`RateLimitStatsAccumulator` (in `forwarder/rate_limit/stats.rs`) with:

```rust
use crate::forwarder::stats::{RateLimitRejectReason, RateLimitStatsSnapshot};

impl RateLimitStatsAccumulator {
    /// proto-free snapshot. Returns `None` for an accumulator with all
    /// counters at default — preserves proto3 default-stripping semantics.
    pub fn drain(&self) -> Option<RateLimitStatsSnapshot> {
        let reject_total = self.drain_reject_pairs(); // existing private helper if any, or inline atomic swaps here
        let throttle_micros_in = self.throttle_micros_in.swap(0, Ordering::Relaxed);
        let throttle_micros_out = self.throttle_micros_out.swap(0, Ordering::Relaxed);
        let active_connections = self.active_connections.load(Ordering::Relaxed);
        if reject_total.is_empty()
            && throttle_micros_in == 0
            && throttle_micros_out == 0
            && active_connections == 0
        {
            return None;
        }
        Some(RateLimitStatsSnapshot {
            reject_total,
            throttle_micros_in,
            throttle_micros_out,
            active_connections,
        })
    }
}
```

Adjust `drain_reject_pairs` / atomic field names to match what's
currently in `stats.rs` — preserve atomic ordering semantics from the
pre-existing `drain_to_proto`. The proto-shape return type goes away.

- [ ] **Step 5: Verify tests pass**

Run: `cargo test -p portunus-forwarder rate_limit::stats::tests`
Expected: PASS.

- [ ] **Step 6: Confirm the only callers of drain_to_proto are client-side**

Run: `grep -rn 'drain_to_proto' crates/`
Expected: only `crates/portunus-client/src/control.rs` mentions the old
name (will be replaced in Task 2.10). No forwarder hits.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(forwarder): RateLimitStatsAccumulator.drain() returns proto-free snapshot"
```

### Task 2.4: OwnerRateLimitStatsRegistry::drain → Vec<OwnerRateLimitStatsSnapshot>

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/stats.rs`
- Modify: `crates/portunus-forwarder/src/forwarder/rate_limit/scope.rs:790-810`
- Test: `crates/portunus-forwarder/src/forwarder/rate_limit/scope.rs` (#[cfg(test)] mod tests)

- [ ] **Step 1: Add OwnerRateLimitStatsSnapshot**

Append to `crates/portunus-forwarder/src/forwarder/stats.rs`:

```rust
#[derive(Clone, Debug, Default)]
pub struct OwnerRateLimitStatsSnapshot {
    pub owner_id: String,
    pub stats: RateLimitStatsSnapshot,
}
```

- [ ] **Step 2: Write failing test**

In `crates/portunus-forwarder/src/forwarder/rate_limit/scope.rs` (#[cfg(test)] mod tests):

```rust
#[test]
fn registry_drain_is_empty_for_unused_owners() {
    let reg = OwnerRateLimitStatsRegistry::default();
    assert!(reg.drain().is_empty());
}

#[test]
fn registry_drain_returns_owner_with_active_count() {
    let reg = OwnerRateLimitStatsRegistry::default();
    let acc = reg.get_or_insert("owner-a");
    acc.set_active_connections(3);
    let snaps = reg.drain();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].owner_id, "owner-a");
    assert_eq!(snaps[0].stats.active_connections, 3);
}
```

- [ ] **Step 3: Verify test fails**

Run: `cargo test -p portunus-forwarder rate_limit::scope::tests::registry_drain`
Expected: COMPILE ERROR.

- [ ] **Step 4: Replace drain_to_proto with drain()**

In `crates/portunus-forwarder/src/forwarder/rate_limit/scope.rs`, remove the
existing `drain_to_proto(&self) -> Vec<proto::v1::OwnerRateLimitStats>`
method and add:

```rust
use crate::forwarder::stats::{OwnerRateLimitStatsSnapshot, RateLimitStatsSnapshot};

impl OwnerRateLimitStatsRegistry {
    pub fn drain(&self) -> Vec<OwnerRateLimitStatsSnapshot> {
        let mut out = Vec::new();
        for (owner_id, acc) in self.iter_owners() {
            if let Some(stats) = acc.drain() {
                out.push(OwnerRateLimitStatsSnapshot {
                    owner_id: owner_id.into(),
                    stats,
                });
            }
        }
        out
    }
}
```

(Adapt `iter_owners` to whatever the existing internal iterator is — the
key invariant is "include only owners whose accumulator drained to
`Some`".)

Also delete the `use portunus_proto::v1::OwnerRateLimitStats;` line if
present.

- [ ] **Step 5: Also strip the proto reject-reason cast at scope.rs:1516**

The existing code at line 1516 reads:

```rust
portunus_proto::v1::RateLimitRejectReason::OwnerConcurrent as i32
```

That's a proto integer label. Replace it with the wire-neutral
`RateLimitRejectReason::OwnerConcurrent` from forwarder/stats.rs and
adjust whatever stores/forwards the value (almost certainly an i32 atomic
keyed by reason — change the key from i32 to `RateLimitRejectReason`, or
keep i32 with a small `as_i32` helper on the wire-neutral enum that
matches the proto values exactly).

Add to `forwarder/stats.rs`:

```rust
impl RateLimitRejectReason {
    /// Stable mapping to the proto enum values; required so the data
    /// plane can use a `[u64; N]` array keyed by `as_index()` without
    /// pulling in proto types.
    #[must_use]
    pub fn as_index(self) -> usize {
        match self {
            Self::Unspecified       => 0,
            Self::ConnConcurrent    => 1,
            Self::ConnRate          => 2,
            Self::UdpFlowRate       => 3,
            Self::OwnerConcurrent   => 4,
            Self::OwnerConnRate     => 5,
            Self::OwnerUdpFlowRate  => 6,
        }
    }
}
```

Replace internal hot-path references like
`portunus_proto::v1::RateLimitRejectReason::OwnerConcurrent as i32` with
`RateLimitRejectReason::OwnerConcurrent.as_index()`.

- [ ] **Step 6: Verify tests pass**

Run: `cargo test -p portunus-forwarder rate_limit::scope::tests`
Expected: PASS.

- [ ] **Step 7: Confirm no proto refs remain in rate_limit/**

Run: `grep -rn 'portunus_proto' crates/portunus-forwarder/src/forwarder/rate_limit/`
Expected: 0 hits.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor(forwarder): OwnerRateLimitStatsRegistry.drain() and reject-reason index proto-free"
```

### Task 2.5: SniListenerCounters proto-free snapshot

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/stats.rs` (add SniListenerStatsSnapshot)
- Modify: `crates/portunus-forwarder/src/forwarder/sni/listener.rs` (snapshot getter)
- Test: `crates/portunus-forwarder/src/forwarder/sni/listener.rs` (#[cfg(test)] mod tests, append)

- [ ] **Step 1: Append SniListenerStatsSnapshot to stats.rs**

```rust
/// Wire-neutral mirror of `proto::v1::SniListenerStats` (fields 1-6).
#[derive(Clone, Debug, Default)]
pub struct SniListenerStatsSnapshot {
    pub listen_port: u16,
    pub sni_route_miss_total: u64,
    pub client_hello_parse_failures_total: u64,
    /// Bucket counters in order of `portunus_core::PEEK_HISTOGRAM_BUCKETS_SECS`.
    pub client_hello_peek_bucket_counts: Vec<u64>,
    pub client_hello_peek_sum_micros: u64,
    pub client_hello_peek_count: u64,
}
```

- [ ] **Step 2: Failing test**

In `forwarder/sni/listener.rs`:

```rust
#[test]
fn snapshot_returns_zeroed_for_fresh_counters() {
    let c = SniListenerCounters::default();
    let snap = c.snapshot(8443);
    assert_eq!(snap.listen_port, 8443);
    assert_eq!(snap.sni_route_miss_total, 0);
    assert_eq!(snap.client_hello_parse_failures_total, 0);
    assert_eq!(snap.client_hello_peek_count, 0);
    assert_eq!(snap.client_hello_peek_sum_micros, 0);
    // Bucket counts vector has 1 entry per histogram bucket
    assert_eq!(snap.client_hello_peek_bucket_counts.len(),
               portunus_core::PEEK_HISTOGRAM_BUCKETS_SECS.len());
}
```

- [ ] **Step 3: Verify failure**

Run: `cargo test -p portunus-forwarder sni::listener::tests::snapshot_returns_zeroed_for_fresh_counters`
Expected: COMPILE ERROR — `snapshot` not in scope.

- [ ] **Step 4: Implement snapshot()**

In `forwarder/sni/listener.rs`, add (and remove any proto-returning
helper if one exists — the spec says wire construction moves out):

```rust
use crate::forwarder::stats::SniListenerStatsSnapshot;
use std::sync::atomic::Ordering;

impl SniListenerCounters {
    /// proto-free snapshot of this listener's counters at the given port.
    /// Caller (client.port_groups) loops over its bound listeners.
    #[must_use]
    pub fn snapshot(&self, listen_port: u16) -> SniListenerStatsSnapshot {
        let bucket_counts = self.peek_bucket_counts
            .iter()
            .map(|a| a.load(Ordering::Relaxed))
            .collect();
        SniListenerStatsSnapshot {
            listen_port,
            sni_route_miss_total: self.sni_route_miss_total.load(Ordering::Relaxed),
            client_hello_parse_failures_total: self.client_hello_parse_failures_total.load(Ordering::Relaxed),
            client_hello_peek_bucket_counts: bucket_counts,
            client_hello_peek_sum_micros: self.client_hello_peek_sum_micros.load(Ordering::Relaxed),
            client_hello_peek_count: self.client_hello_peek_count.load(Ordering::Relaxed),
        }
    }
}
```

If the struct fields are named differently, match the existing struct
exactly. The point: every counter is `load`-ed, no proto types involved.

- [ ] **Step 5: Test passes**

Run: `cargo test -p portunus-forwarder sni::listener::tests::snapshot_returns_zeroed_for_fresh_counters`
Expected: PASS.

- [ ] **Step 6: Confirm no proto in sni/**

Run: `grep -rn 'portunus_proto' crates/portunus-forwarder/src/forwarder/sni/`
Expected: 0 hits.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(forwarder): SniListenerCounters.snapshot() proto-free, full peek histogram fields"
```

### Task 2.6: RuleStats::snapshot_basic() + RuleStatsSnapshotBasic + RuleStatsSnapshot + supporting snapshots

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/stats.rs` (large additions)
- Test: `crates/portunus-forwarder/src/forwarder/stats.rs` (#[cfg(test)] mod tests, append)

- [ ] **Step 1: Append all remaining snapshot types**

Append to `crates/portunus-forwarder/src/forwarder/stats.rs` (after existing types from Task 2.3-2.5):

```rust
use portunus_core::RuleId;

/// PerPort detail for range rules. Empty for single-port rules to keep
/// proto3 default-stripping behavior.
#[derive(Clone, Debug, Default)]
pub struct PerPortStatsSnapshot {
    pub listen_port: u16,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
}

/// PerTarget detail for multi-target rules.
#[derive(Clone, Debug, Default)]
pub struct PerTargetStatsSnapshot {
    pub index: u32,
    pub host: String,
    pub port: u16,
    pub priority: u32,
    pub health: TargetHealth,
    pub consecutive_failures: u32,
    pub last_failure_at_unix_ms: u64,
    pub last_success_at_unix_ms: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub connections_accepted: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetHealth {
    #[default]
    Healthy,
    Failed,
}

impl TargetHealth {
    /// Mirrors `forwarder::failover::Health::as_wire()` — Healthy=0, Failed=1.
    /// Written into `proto::v1::PerTargetStats.health: uint32`.
    #[must_use]
    pub fn as_wire(self) -> u32 {
        match self {
            Self::Healthy => 0,
            Self::Failed => 1,
        }
    }
}

/// Basic per-rule counters owned by `RuleStats`. Excludes per-target /
/// rate-limit / multi-target state (those live in sibling structs on
/// the client `RuleSlot` and are assembled by
/// `portunus-client::control::build_rule_stats_snapshot`).
#[derive(Clone, Debug, Default)]
pub struct RuleStatsSnapshotBasic {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
    pub per_port: Vec<PerPortStatsSnapshot>,
    pub dns_failures: u64,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub active_flows: u32,
    pub flows_dropped_overflow: u64,
    pub sni_route_exact_total: u64,
    pub sni_route_wildcard_total: u64,
    pub sni_route_fallback_total: u64,
}

/// Complete snapshot used by client → proto wire translation. Assembled
/// by `portunus-client::control::build_rule_stats_snapshot(rule_id, &slot)`.
#[derive(Clone, Debug)]
pub struct RuleStatsSnapshot {
    pub rule_id: RuleId,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
    pub per_port: Vec<PerPortStatsSnapshot>,
    pub dns_failures: u64,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub active_flows: u32,
    pub flows_dropped_overflow: u64,
    pub target_failovers_total: u64,
    pub per_target: Vec<PerTargetStatsSnapshot>,
    pub sni_route_exact_total: u64,
    pub sni_route_wildcard_total: u64,
    pub sni_route_fallback_total: u64,
    pub rate_limit: Option<RateLimitStatsSnapshot>,
}
```

- [ ] **Step 2: Failing test for snapshot_basic()**

Append to the tests module in `forwarder/stats.rs`:

```rust
#[test]
fn snapshot_basic_zeroed_for_fresh_stats() {
    use portunus_core::PortRange;
    let stats = RuleStats::for_range(PortRange::single(8080));
    let snap = stats.snapshot_basic();
    assert_eq!(snap.bytes_in, 0);
    assert_eq!(snap.bytes_out, 0);
    assert_eq!(snap.active_connections, 0);
    assert_eq!(snap.dns_failures, 0);
    assert_eq!(snap.datagrams_in, 0);
    assert_eq!(snap.per_port.len(), 1, "single-port rule still allocates one per-port slot");
    assert_eq!(snap.per_port[0].listen_port, 8080);
}
```

- [ ] **Step 3: Verify failure**

Run: `cargo test -p portunus-forwarder stats::tests::snapshot_basic_zeroed_for_fresh_stats`
Expected: COMPILE ERROR — `snapshot_basic` not in scope.

- [ ] **Step 4: Implement snapshot_basic()**

In `forwarder/stats.rs`, add (use existing private atomic accessors —
the helper methods like `snapshot_per_port_with_udp()`, `snapshot_dns_failures()`,
`snapshot_datagrams_in/out()`, `snapshot_active_flows()`,
`snapshot_flows_dropped_overflow()` already exist on `RuleStats`):

```rust
use std::sync::atomic::Ordering;

impl RuleStats {
    /// Wire-neutral snapshot of the basic counters owned by this struct.
    /// Caller (client/standalone reporter) assembles full
    /// `RuleStatsSnapshot` or consumes basic only.
    #[must_use]
    pub fn snapshot_basic(&self) -> RuleStatsSnapshotBasic {
        let (bytes_in, bytes_out, active_connections) = self.snapshot();
        let per_port = self.snapshot_per_port_with_udp()
            .into_iter()
            .map(|(port, bin, bout, active, dgin, dgout)| PerPortStatsSnapshot {
                listen_port: port,
                bytes_in: bin,
                bytes_out: bout,
                active_connections: active,
                datagrams_in: dgin,
                datagrams_out: dgout,
            })
            .collect();
        RuleStatsSnapshotBasic {
            bytes_in,
            bytes_out,
            active_connections,
            per_port,
            dns_failures: self.snapshot_dns_failures(),
            datagrams_in: self.snapshot_datagrams_in(),
            datagrams_out: self.snapshot_datagrams_out(),
            active_flows: self.snapshot_active_flows(),
            flows_dropped_overflow: self.snapshot_flows_dropped_overflow(),
            sni_route_exact_total: self.sni_route_exact_total.load(Ordering::Relaxed),
            sni_route_wildcard_total: self.sni_route_wildcard_total.load(Ordering::Relaxed),
            sni_route_fallback_total: self.sni_route_fallback_total.load(Ordering::Relaxed),
        }
    }
}
```

Adapt to actual signatures — `RuleStats::snapshot()` returns `(u64, u64, u32)`
per the current implementation; if it returns something else, project the
right fields.

- [ ] **Step 5: Tests pass**

Run: `cargo test -p portunus-forwarder stats::tests`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(forwarder): RuleStats.snapshot_basic() + complete snapshot type set"
```

### Task 2.7: MultiTargetObservability::snapshot_per_target

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/failover.rs`
- Test: `crates/portunus-forwarder/src/forwarder/failover.rs` (#[cfg(test)] mod tests)

- [ ] **Step 1: Failing test**

Append to the tests module in `forwarder/failover.rs`:

```rust
#[test]
fn snapshot_per_target_returns_empty_for_no_targets() {
    use crate::forwarder::stats::PerTargetStatsSnapshot;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    let obs = MultiTargetObservability {
        target_failovers_total: Arc::new(AtomicU64::new(0)),
        states: vec![],
    };
    let (total, per_target): (u64, Vec<PerTargetStatsSnapshot>) =
        obs.snapshot_per_target(&[]);
    assert_eq!(total, 0);
    assert!(per_target.is_empty());
}
```

- [ ] **Step 2: Verify failure**

Run: `cargo test -p portunus-forwarder failover::tests::snapshot_per_target_returns_empty_for_no_targets`
Expected: COMPILE ERROR — `snapshot_per_target` not in scope.

- [ ] **Step 3: Implement snapshot_per_target()**

In `forwarder/failover.rs`, add (this is the proto-free equivalent of
the current `build_per_target` function in `portunus-client/src/control.rs:1092-1130`):

```rust
use crate::forwarder::stats::{PerTargetStatsSnapshot, TargetHealth};
use std::sync::atomic::Ordering;

impl MultiTargetObservability {
    /// Build proto-free per-target snapshot. Mirrors the priorty-ordered
    /// `targets` slice (`Vec<MultiTarget>`); skips targets whose
    /// `HealthState` mutex is currently held (try_lock pattern lifted
    /// from the prior client implementation — stats stay off the
    /// data-plane hot path).
    ///
    /// Returns `(target_failovers_total, per_target)` so the caller can
    /// fill both `RuleStats` proto fields with one call.
    #[must_use]
    pub fn snapshot_per_target(&self, targets: &[crate::forwarder::MultiTarget])
        -> (u64, Vec<PerTargetStatsSnapshot>)
    {
        let total = self.target_failovers_total.load(Ordering::Relaxed);
        let mut out = Vec::with_capacity(targets.len());
        for (idx, t) in targets.iter().enumerate() {
            let Ok(state) = self.states[idx].try_lock() else { continue; };
            let last_failure_at_unix_ms = state
                .last_failure_at()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            let last_success_at_unix_ms = state
                .last_success_at()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
            let (bytes_in, bytes_out) = state.snapshot_bytes();
            let connections_accepted = state.snapshot_connections();
            let health = match state.health() {
                Health::Healthy => TargetHealth::Healthy,
                Health::Failed  => TargetHealth::Failed,
            };
            out.push(PerTargetStatsSnapshot {
                index: u32::try_from(idx).unwrap_or(u32::MAX),
                host: t.spec.host.clone(),
                port: t.spec.port,
                priority: t.spec.priority,
                health,
                consecutive_failures: state.consecutive_failures(),
                last_failure_at_unix_ms,
                last_success_at_unix_ms,
                bytes_in,
                bytes_out,
                connections_accepted,
            });
        }
        (total, out)
    }
}
```

- [ ] **Step 4: Test passes**

Run: `cargo test -p portunus-forwarder failover::tests::snapshot_per_target_returns_empty_for_no_targets`
Expected: PASS.

- [ ] **Step 5: Build full crate**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder`
Expected: PASS. If `Health` is private and used in `snapshot_per_target`,
that's fine — same module.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(forwarder): MultiTargetObservability.snapshot_per_target proto-free"
```

### Task 2.8: LiveResolver::with_system_defaults helper

**Files:**
- Modify: `crates/portunus-forwarder/src/resolver/mod.rs`
- Test: `crates/portunus-forwarder/src/resolver/mod.rs` (#[cfg(test)] mod tests)

- [ ] **Step 1: Failing test**

Append to tests:

```rust
#[tokio::test]
async fn with_system_defaults_constructs_resolver() {
    let r = LiveResolver::<HickoryResolver>::with_system_defaults();
    // CI hosts can have weird /etc/resolv.conf; we only assert "constructed".
    assert!(r.is_ok(), "default system resolver construct should succeed in test env");
}
```

- [ ] **Step 2: Verify failure**

Run: `cargo test -p portunus-forwarder resolver::tests::with_system_defaults_constructs_resolver`
Expected: COMPILE ERROR.

- [ ] **Step 3: Implement**

In `forwarder/resolver/mod.rs`:

```rust
use hickory_resolver::config::ResolverConfig;
use std::io;
use std::sync::Arc;

impl LiveResolver<HickoryResolver> {
    /// Build a resolver wired to the system `/etc/resolv.conf` with
    /// default transport options. Convenience for callers that don't
    /// want to chain `HickoryResolver::from_system(&ResolverConfig::default())`
    /// + `LiveResolver::new` themselves.
    pub fn with_system_defaults() -> io::Result<Self> {
        let config = ResolverConfig::default();
        let inner = Arc::new(HickoryResolver::from_system(&config)?);
        Ok(Self::new(inner, config))
    }
}
```

- [ ] **Step 4: Test passes**

Run: `cargo test -p portunus-forwarder resolver::tests::with_system_defaults_constructs_resolver`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(forwarder): LiveResolver::with_system_defaults"
```

### Task 2.9: Public API surface — lib.rs re-exports + submodule re-exports

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/rate_limit/mod.rs`
- Modify: `crates/portunus-forwarder/src/forwarder/sni/mod.rs`
- Modify: `crates/portunus-forwarder/src/lib.rs`

- [ ] **Step 1: Add submodule re-exports**

In `crates/portunus-forwarder/src/forwarder/rate_limit/mod.rs`, after the
`pub mod` declarations:

```rust
pub use scope::*;
pub use stats::*;
```

In `crates/portunus-forwarder/src/forwarder/sni/mod.rs`, after the
existing `pub mod` declarations (if `listener` is currently `pub mod
listener;`):

```rust
pub use listener::*;
```

- [ ] **Step 2: Replace lib.rs with the full public surface (spec §3.3)**

```rust
//! Portunus data-plane library — TCP/UDP forwarding shared between
//! portunus-client (gRPC control plane) and portunus-standalone (TOML).

pub mod forwarder;
pub mod resolver;
pub mod shutdown;

// Rules and lifecycle
pub use forwarder::{
    ClientRule, MultiTarget, MultiTargetObservability,
    RuleStatusEvent, run as run_forwarder,
};

// Quota (client constructs; standalone never instantiates)
pub use forwarder::quota::QuotaHandle;

// Wire-neutral stats snapshot types + getters
pub use forwarder::stats::{
    RuleStats,
    RuleStatsSnapshot, RuleStatsSnapshotBasic,
    PerPortStatsSnapshot, PerTargetStatsSnapshot,
    RateLimitStatsSnapshot, OwnerRateLimitStatsSnapshot,
    SniListenerStatsSnapshot,
    RateLimitRejectReason, TargetHealth,
};

// SNI data-plane entry points (client port_groups consumes)
pub use forwarder::sni::{
    SniListener, SniListenerCounters, SniRouteResolver, SniRuleSlot,
};

// Rate limit control-plane handles (client constructs)
pub use forwarder::rate_limit::{
    RateLimitScopeManager,
    OwnerRateLimitHandle, OwnerRateLimitStatsRegistry,
    RateLimitStatsAccumulator,
    RuleRateLimitHandle,
};

// PROXY protocol (real names — `ProxyProtocolVersion` lives in portunus-core)
pub use forwarder::proxy_protocol::ProxyProtocolPrelude;

// Resolver
pub use resolver::{Resolve, LiveResolver, HickoryResolver};

// Shutdown primitive (signal handling stays out of lib — see spec §4.5)
pub use shutdown::Shutdown;
```

- [ ] **Step 3: Build crate**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder`
Expected: PASS. Common failures: `RuleStats::new` is `#[cfg(test)]` so
not re-exportable in lib.rs (it isn't in the re-export list above —
verify); a type was renamed in earlier tasks but the re-export line
still has the old name.

If `run` collides with anything in scope, rename the re-export with `as
run_forwarder` (already shown above).

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(forwarder): public API surface — lib.rs re-exports + submodule re-exports"
```

### Task 2.10: portunus-client wire translation — From<Snapshot> for proto::*

**Files:**
- Create: `crates/portunus-client/src/wire.rs` (new module hosting From impls)
- Modify: `crates/portunus-client/src/main.rs` (declare `mod wire;`)

- [ ] **Step 1: Create wire.rs skeleton**

Add `crates/portunus-client/src/wire.rs`:

```rust
//! Wire-translation impls: convert proto-free snapshot types from
//! portunus-forwarder into the proto wire types used on the bidi gRPC
//! stream.
//!
//! Spec: `docs/superpowers/specs/2026-05-14-standalone-forwarder-design.md`
//! §3.4 and §5.2. Keep these impls byte-identical to the previous inline
//! constructions in control.rs (validated by `*_wire_compat` tests).

use portunus_forwarder::{
    OwnerRateLimitStatsSnapshot, PerPortStatsSnapshot, PerTargetStatsSnapshot,
    RateLimitRejectReason, RateLimitStatsSnapshot, RuleStatsSnapshot,
    SniListenerStatsSnapshot, TargetHealth,
};
use portunus_proto::v1 as proto;

impl From<RateLimitRejectReason> for proto::RateLimitRejectReason {
    fn from(r: RateLimitRejectReason) -> Self {
        match r {
            RateLimitRejectReason::Unspecified      => proto::RateLimitRejectReason::Unspecified,
            RateLimitRejectReason::ConnConcurrent   => proto::RateLimitRejectReason::ConnConcurrent,
            RateLimitRejectReason::ConnRate         => proto::RateLimitRejectReason::ConnRate,
            RateLimitRejectReason::UdpFlowRate      => proto::RateLimitRejectReason::UdpFlowRate,
            RateLimitRejectReason::OwnerConcurrent  => proto::RateLimitRejectReason::OwnerConcurrent,
            RateLimitRejectReason::OwnerConnRate    => proto::RateLimitRejectReason::OwnerConnRate,
            RateLimitRejectReason::OwnerUdpFlowRate => proto::RateLimitRejectReason::OwnerUdpFlowRate,
        }
    }
}

impl From<RateLimitStatsSnapshot> for proto::RateLimitStats {
    fn from(s: RateLimitStatsSnapshot) -> Self {
        proto::RateLimitStats {
            reject_total: s.reject_total.into_iter().map(|(reason, total)| proto::RateLimitRejectCount {
                reason: proto::RateLimitRejectReason::from(reason) as i32,
                total,
            }).collect(),
            throttle_micros_in: s.throttle_micros_in,
            throttle_micros_out: s.throttle_micros_out,
            active_connections: s.active_connections,
        }
    }
}

impl From<OwnerRateLimitStatsSnapshot> for proto::OwnerRateLimitStats {
    fn from(s: OwnerRateLimitStatsSnapshot) -> Self {
        proto::OwnerRateLimitStats {
            owner_id: s.owner_id,
            stats: Some(s.stats.into()),
        }
    }
}

impl From<PerPortStatsSnapshot> for proto::PerPortStats {
    fn from(s: PerPortStatsSnapshot) -> Self {
        proto::PerPortStats {
            listen_port: u32::from(s.listen_port),
            bytes_in: s.bytes_in,
            bytes_out: s.bytes_out,
            active_connections: s.active_connections,
            datagrams_in: s.datagrams_in,
            datagrams_out: s.datagrams_out,
        }
    }
}

impl From<PerTargetStatsSnapshot> for proto::PerTargetStats {
    fn from(s: PerTargetStatsSnapshot) -> Self {
        proto::PerTargetStats {
            index: s.index,
            host: s.host,
            port: u32::from(s.port),
            priority: s.priority,
            health: s.health.as_wire(),
            consecutive_failures: s.consecutive_failures,
            last_failure_at_unix_ms: s.last_failure_at_unix_ms,
            last_success_at_unix_ms: s.last_success_at_unix_ms,
            bytes_in: s.bytes_in,
            bytes_out: s.bytes_out,
            connections_accepted: s.connections_accepted,
        }
    }
}

impl From<SniListenerStatsSnapshot> for proto::SniListenerStats {
    fn from(s: SniListenerStatsSnapshot) -> Self {
        proto::SniListenerStats {
            listen_port: u32::from(s.listen_port),
            sni_route_miss_total: s.sni_route_miss_total,
            client_hello_parse_failures_total: s.client_hello_parse_failures_total,
            client_hello_peek_bucket_counts: s.client_hello_peek_bucket_counts,
            client_hello_peek_sum_micros: s.client_hello_peek_sum_micros,
            client_hello_peek_count: s.client_hello_peek_count,
        }
    }
}

impl From<RuleStatsSnapshot> for proto::RuleStats {
    fn from(s: RuleStatsSnapshot) -> Self {
        proto::RuleStats {
            rule_id: s.rule_id.0,
            bytes_in: s.bytes_in,
            bytes_out: s.bytes_out,
            active_connections: s.active_connections,
            per_port: s.per_port.into_iter().map(Into::into).collect(),
            dns_failures: s.dns_failures,
            datagrams_in: s.datagrams_in,
            datagrams_out: s.datagrams_out,
            active_flows: s.active_flows,
            flows_dropped_overflow: s.flows_dropped_overflow,
            target_failovers_total: s.target_failovers_total,
            per_target: s.per_target.into_iter().map(Into::into).collect(),
            sni_route_exact_total: s.sni_route_exact_total,
            sni_route_wildcard_total: s.sni_route_wildcard_total,
            sni_route_fallback_total: s.sni_route_fallback_total,
            rate_limit: s.rate_limit.map(Into::into),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_rule_stats_round_trip_byte_identical() {
        // proto::RuleStats default has rule_id=0 etc. Round-tripping
        // through RuleStatsSnapshot must reproduce the default bytes
        // exactly (proto3 default-stripping). This anchors wire compat.
        let snap = RuleStatsSnapshot {
            rule_id: portunus_core::RuleId(0),
            bytes_in: 0, bytes_out: 0, active_connections: 0,
            per_port: vec![], dns_failures: 0,
            datagrams_in: 0, datagrams_out: 0,
            active_flows: 0, flows_dropped_overflow: 0,
            target_failovers_total: 0, per_target: vec![],
            sni_route_exact_total: 0, sni_route_wildcard_total: 0,
            sni_route_fallback_total: 0,
            rate_limit: None,
        };
        let p: proto::RuleStats = snap.into();
        assert_eq!(p, proto::RuleStats::default());
    }
}
```

- [ ] **Step 2: Wire in main.rs**

In `crates/portunus-client/src/main.rs`, add at top:

```rust
mod wire;
```

- [ ] **Step 3: Tests pass**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client wire::tests`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/portunus-client/src/wire.rs crates/portunus-client/src/main.rs
git commit -m "feat(client): From<Snapshot> for proto::* wire translation layer"
```

### Task 2.11: build_rule_stats_snapshot in client/control.rs

**Files:**
- Modify: `crates/portunus-client/src/control.rs:1085-1252` (refactor send_stats_report)
- Modify: `crates/portunus-client/src/port_groups.rs` — `snapshot_listener_stats` returns `Vec<SniListenerStatsSnapshot>`

- [ ] **Step 1: Replace port_groups::snapshot_listener_stats return type**

In `crates/portunus-client/src/port_groups.rs:353-370`, replace
`Vec<proto::SniListenerStats>` with `Vec<SniListenerStatsSnapshot>`,
using each listener's `SniListenerCounters::snapshot(port)` from
forwarder. Delete proto imports if unused.

```rust
use portunus_forwarder::SniListenerStatsSnapshot;

impl PortGroupManager {
    pub fn snapshot_listener_stats(&self) -> Vec<SniListenerStatsSnapshot> {
        self.iter_listeners()
            .map(|(port, counters)| counters.snapshot(port))
            .collect()
    }
}
```

- [ ] **Step 2: Build to confirm port_groups compiles in isolation (control.rs still calls .into() chain we have not added — temp expected failure)**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-client`
Expected: failure in `control.rs:1230` where the old `sni_listener_stats`
is being plugged straight into `StatsReport.sni_listener_stats`. Will
fix in step 4.

- [ ] **Step 3: Add build_rule_stats_snapshot helper**

In `crates/portunus-client/src/control.rs`, near the existing
`build_per_target` function (around line 1092), add:

```rust
use portunus_forwarder::{
    PerTargetStatsSnapshot, RuleStatsSnapshot,
};

/// Assemble a complete wire-neutral `RuleStatsSnapshot` from a RuleSlot.
/// Encapsulates the prior inline construction at the StatsReport call
/// site. `*_wire_compat` tests verify byte-identical wire output.
fn build_rule_stats_snapshot(
    rule_id: portunus_core::RuleId,
    slot: &RuleSlot,
) -> RuleStatsSnapshot {
    let basic = slot.stats.snapshot_basic();
    // is_range gates per_port (wire byte-stability with v0.1.0 — single
    // port rules emit empty per_port).
    let per_port = if slot.is_range { basic.per_port } else { Vec::new() };
    let (target_failovers_total, per_target) = match slot.multi_target_obs.as_ref() {
        Some(obs) => obs.snapshot_per_target(&slot.targets_view),
        None => (0, Vec::new()),
    };
    let rate_limit = slot.rate_limit_stats.as_ref().and_then(|acc| {
        if let Some(limiter) = slot.rate_limit_limiter.as_ref() {
            acc.set_active_connections(limiter.active_connections());
        }
        acc.drain()
    });
    RuleStatsSnapshot {
        rule_id,
        bytes_in: basic.bytes_in,
        bytes_out: basic.bytes_out,
        active_connections: basic.active_connections,
        per_port,
        dns_failures: basic.dns_failures,
        datagrams_in: basic.datagrams_in,
        datagrams_out: basic.datagrams_out,
        active_flows: basic.active_flows,
        flows_dropped_overflow: basic.flows_dropped_overflow,
        target_failovers_total,
        per_target,
        sni_route_exact_total: basic.sni_route_exact_total,
        sni_route_wildcard_total: basic.sni_route_wildcard_total,
        sni_route_fallback_total: basic.sni_route_fallback_total,
        rate_limit,
    }
}
```

- [ ] **Step 4: Refactor send_stats_report**

In `send_stats_report` (control.rs:1132-1252), replace the inline `stats:
Vec<ProtoRuleStats> = rules.iter().map(...).collect();` block with:

```rust
let stats: Vec<ProtoRuleStats> = rules
    .iter()
    .map(|(rule_id, slot)| build_rule_stats_snapshot(*rule_id, slot).into())
    .collect();
```

Replace the `sni_listener_stats = port_groups.snapshot_listener_stats();`
that previously produced `Vec<proto::SniListenerStats>` with mapping:

```rust
let sni_listener_stats: Vec<proto::SniListenerStats> = port_groups
    .snapshot_listener_stats()
    .into_iter()
    .map(Into::into)
    .collect();
```

Replace `owner_rate_limit_stats: owner_rate_limit_stats.drain_to_proto()`
with:

```rust
owner_rate_limit_stats: owner_rate_limit_stats
    .drain()
    .into_iter()
    .map(Into::into)
    .collect(),
```

Delete the now-orphan `build_per_target` function — its logic moved into
`MultiTargetObservability::snapshot_per_target`.

- [ ] **Step 5: Build client**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-client`
Expected: PASS.

- [ ] **Step 6: Run wire-compat tests**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client wire_compat`
Expected: PASS — these are the byte-stability gates. Failures here mean
the snapshot path is dropping a field; debug by comparing the previous
inline construction to `build_rule_stats_snapshot` field by field.

- [ ] **Step 7: Full client test sweep**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "refactor(client): build_rule_stats_snapshot + From conversion in send_stats_report"
```

### Task 2.12: Clean up portunus-client/Cargo.toml deps

**Files:**
- Modify: `crates/portunus-client/Cargo.toml`

- [ ] **Step 1: Audit which deps are still used directly in portunus-client**

```bash
grep -rn 'use nix::\|nix::' crates/portunus-client/src/ | head
grep -rn 'use hickory_resolver\|hickory_resolver::' crates/portunus-client/src/ | head
grep -rn 'use async_trait\|#\[async_trait' crates/portunus-client/src/ | head
grep -rn 'use tokio_rustls\|tokio_rustls::' crates/portunus-client/src/ | head
grep -rn 'use tokio_stream\|tokio_stream::' crates/portunus-client/src/ | head
grep -rn 'use rustls::\|rustls::' crates/portunus-client/src/ | head
```

If any returns hits, the dep stays. Most likely results post-migration:
- `tokio-rustls` / `rustls` likely still used in `control.rs` for TLS dial
- `tokio-stream` likely used in gRPC streaming
- `nix`, `hickory-resolver`, `async-trait` likely **no longer** referenced
  (they're now forwarder-only)

- [ ] **Step 2: Remove confirmed-unused deps**

Edit `crates/portunus-client/Cargo.toml`:

```toml
[dependencies]
portunus-core = { workspace = true }
portunus-auth = { workspace = true }
portunus-proto = { workspace = true }
portunus-forwarder = { workspace = true }

tokio = { workspace = true }
tokio-util = { workspace = true }
tokio-rustls = { workspace = true }       # KEEP if Step 1 grep returned hits; drop otherwise
tonic = { workspace = true }
tonic-prost = { workspace = true }
prost = { workspace = true }
rustls = { workspace = true }             # KEEP if used; drop otherwise
clap = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true }
thiserror = { workspace = true }
rand = { workspace = true }
base64 = { workspace = true }
tokio-stream = { workspace = true }       # KEEP if used; drop otherwise

# Removed (now in portunus-forwarder):
#   nix             — splice/pipe2 in forwarder/splice.rs
#   hickory-resolver — forwarder/resolver
#   async-trait     — forwarder Resolve trait
```

For each `# KEEP if used` line, delete it if Step 1's grep was empty.

- [ ] **Step 3: Build workspace**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build --workspace`
Expected: PASS.

- [ ] **Step 4: Workspace test sweep**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Format + clippy gate**

Run: `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/portunus-client/Cargo.toml
git commit -m "chore(client): drop deps now provided via portunus-forwarder"
```

### Task 2.13: Move benches into portunus-forwarder

**Files:**
- Move via `git mv`: `crates/portunus-client/benches/*` → `crates/portunus-forwarder/benches/`
- Modify: `crates/portunus-forwarder/Cargo.toml` (register benches)
- Modify: `crates/portunus-client/Cargo.toml` (drop bench entries)
- Modify: `.github/workflows/bench.yml` (if it pins a `-p` package)

- [ ] **Step 1: git mv benches**

```bash
git mv crates/portunus-client/benches crates/portunus-forwarder/benches
```

- [ ] **Step 2: Register benches in portunus-forwarder/Cargo.toml**

Append to `[dev-dependencies]`:

```toml
criterion = { workspace = true }
```

Append at end of file:

```toml
[[bench]]
name = "data_plane"
harness = false

[[bench]]
name = "range_install"
harness = false

[[bench]]
name = "dns_resolver"
harness = false

[[bench]]
name = "udp_data_plane"
harness = false

[[bench]]
name = "sni_route"
harness = false
```

- [ ] **Step 3: Remove bench entries from portunus-client/Cargo.toml**

Delete the `[[bench]]` sections at the bottom of `crates/portunus-client/Cargo.toml`.

- [ ] **Step 4: Update any `use crate::forwarder::*` inside bench files**

```bash
grep -rln 'use crate::forwarder\|use crate::resolver' crates/portunus-forwarder/benches/ \
  | xargs sed -i.bak 's|use crate::forwarder::|use portunus_forwarder::|g; s|use crate::resolver::|use portunus_forwarder::resolver::|g'
find crates/portunus-forwarder/benches -name '*.bak' -delete
```

- [ ] **Step 5: Build benches (no run, just compile)**

Run: `cargo build --benches -p portunus-forwarder`
Expected: PASS.

- [ ] **Step 6: Update bench CI workflow (if needed)**

Read `.github/workflows/bench.yml`. If it pins `cargo bench -p
portunus-client --bench data_plane`, change to `-p portunus-forwarder
--bench data_plane`. If the workflow uses workspace bench command, no
change.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "chore: move criterion benches to portunus-forwarder"
```

---

## Phase 3: portunus-standalone binary

Goal: build the standalone binary with TOML config + clap CLI + startup gate + fatal channel + SIGHUP-aware signal handler + 60s logging reporter. End-of-phase: `cargo run -p portunus-standalone -- --check tests/fixtures/valid_full.toml` exits 0.

### Task 3.1: Crate scaffold

**Files:**
- Create: `crates/portunus-standalone/Cargo.toml`
- Create: `crates/portunus-standalone/src/main.rs`

- [ ] **Step 1: Cargo.toml**

```toml
[package]
name = "portunus-standalone"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
description = "Standalone TOML-driven TCP/UDP forwarder (no gRPC control plane)."

[lints]
workspace = true

[[bin]]
name = "portunus-standalone"
path = "src/main.rs"

[dependencies]
portunus-core      = { workspace = true }
portunus-forwarder = { workspace = true }
tokio              = { workspace = true }
tokio-util         = { workspace = true }
tracing            = { workspace = true }
tracing-subscriber = { workspace = true }
clap               = { workspace = true }
serde              = { workspace = true }
thiserror          = { workspace = true }
toml               = { workspace = true }
blake3             = { workspace = true }
libc               = "0.2"

[dev-dependencies]
assert_cmd = { workspace = true }
tempfile   = { workspace = true }
```

Add `toml` to workspace deps in root `Cargo.toml` if not present:

```toml
toml = "0.8"
```

- [ ] **Step 2: Stub main.rs**

```rust
use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!("portunus-standalone: phase 3 scaffolding — config loader lands in T3.2");
    ExitCode::SUCCESS
}
```

- [ ] **Step 3: Build**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/portunus-standalone/
git commit -m "feat(standalone): scaffold binary crate"
```

### Task 3.2: Config schema + parsing + RuleId derivation

**Files:**
- Create: `crates/portunus-standalone/src/config.rs`
- Modify: `crates/portunus-standalone/src/main.rs` (declare `mod config;`)
- Test: `crates/portunus-standalone/src/config.rs` (#[cfg(test)] mod tests)

- [ ] **Step 1: Write failing tests for the most important config edges**

Add to bottom of new `crates/portunus-standalone/src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_str: &str) -> Result<Config, ConfigError> {
        Config::from_toml_str(toml_str)
    }

    #[test]
    fn minimal_tcp_rule_parses() {
        let cfg = parse(r#"
            [[rule]]
            name = "ssh"
            protocol = "tcp"
            listen_port = 2222
            target = "10.0.0.5:22"
        "#).unwrap();
        cfg.validate().unwrap();
        let rules: Vec<_> = cfg.iter_rules().collect();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name, "ssh");
    }

    #[test]
    fn duplicate_name_rejected() {
        let cfg = parse(r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 1
            target = "1.1.1.1:1"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 2
            target = "1.1.1.1:2"
        "#).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateName(ref n) if n == "a"));
    }

    #[test]
    fn empty_config_rejected() {
        let cfg = parse("").unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::NoRules));
    }

    #[test]
    fn unknown_field_rejected() {
        let err = parse(r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 1
            target = "1.1.1.1:1"
            bind_addr = "0.0.0.0"
        "#).unwrap_err();
        assert!(matches!(err, ConfigError::TomlParse(_)));
    }

    #[test]
    fn target_and_targets_mutually_exclusive() {
        let cfg = parse(r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 1
            target = "1.1.1.1:1"
            targets = [{ host = "x", port = 1, priority = 0 }]
        "#).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::TargetExclusivity));
    }

    #[test]
    fn range_size_mismatch_rejected() {
        let cfg = parse(r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_ports = "8000-8009"
            target = "1.1.1.1:8000-8019"
        "#).unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::RangeSizeMismatch { .. }));
    }

    #[test]
    fn rule_id_derived_via_blake3_prefix() {
        let id_a = derive_rule_id("ssh-tunnel");
        let id_b = derive_rule_id("ssh-tunnel");
        let id_c = derive_rule_id("game-udp");
        assert_eq!(id_a, id_b, "deterministic for same name");
        assert_ne!(id_a, id_c, "different names → different ids");
    }
}
```

- [ ] **Step 2: Confirm failure**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone config::tests`
Expected: COMPILE ERROR — no `Config` type yet.

- [ ] **Step 3: Implement config.rs**

```rust
//! TOML config schema for portunus-standalone.
//! Spec: docs/superpowers/specs/2026-05-14-standalone-forwarder-design.md §4.2

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use portunus_core::{Protocol, PortRange, RuleId, RuleTarget, ProxyProtocolVersion};
use portunus_forwarder::{ClientRule, MultiTarget};
use serde::Deserialize;
use thiserror::Error;
use tracing::warn;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found at any search path: {0:?}")]
    NoConfigFile(Vec<PathBuf>),
    #[error("failed to read {path}: {source}")]
    Io { path: PathBuf, #[source] source: std::io::Error },
    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),
    #[error("configuration must define at least one [[rule]]")]
    NoRules,
    #[error("rule name {0:?} is used more than once")]
    DuplicateName(String),
    #[error("rule id collision: {prev:?} and {current:?} both hash to {id}")]
    RuleIdCollision { prev: String, current: String, id: RuleId },
    #[error("rule {rule:?}: `target` and `targets` are mutually exclusive")]
    TargetExclusivity,
    #[error("rule {rule:?}: must specify exactly one of `target` or `targets`")]
    NoTarget { rule: String },
    #[error("rule {rule:?}: `targets` cannot be combined with port range listen_ports")]
    TargetsAndRange,
    #[error("rule {rule:?}: listen range size ({listen_size}) does not match target range size ({target_size})")]
    RangeSizeMismatch { rule: String, listen_size: u32, target_size: u32 },
    #[error("rule {rule:?}: invalid listen_ports {value:?}: {reason}")]
    InvalidPortRange { rule: String, value: String, reason: String },
    #[error("rule {rule:?}: protocol={proto} cannot use {field}")]
    FieldProtocolMismatch { rule: String, proto: String, field: &'static str },
    #[error("rule {rule:?}: failed to parse target {target:?}: {reason}")]
    InvalidTarget { rule: String, target: String, reason: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub global: GlobalConfig,
    #[serde(default)]
    pub defaults: DefaultsConfig,
    #[serde(default, rename = "rule")]
    pub rules: Vec<RawRule>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalConfig {
    #[serde(default = "default_log_level")]    pub log_level: String,
    #[serde(default = "default_log_format")]   pub log_format: String,
    #[serde(default = "default_drain_secs")]   pub shutdown_drain_secs: u64,
}

fn default_log_level() -> String { "info".into() }
fn default_log_format() -> String { "json".into() }
fn default_drain_secs() -> u64 { 30 }

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    #[serde(default = "default_udp_max_flows")]      pub udp_max_flows: u32,
    #[serde(default = "default_udp_flow_idle_secs")] pub udp_flow_idle_secs: u32,
    #[serde(default)]                                pub prefer_ipv6: bool,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self { udp_max_flows: 1024, udp_flow_idle_secs: 60, prefer_ipv6: false }
    }
}

fn default_udp_max_flows() -> u32 { 1024 }
fn default_udp_flow_idle_secs() -> u32 { 60 }

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRule {
    pub name: String,
    pub protocol: String,                       // "tcp" | "udp"
    pub listen_port: Option<u16>,
    pub listen_ports: Option<String>,           // "lo-hi"
    pub target: Option<String>,                 // "host:port" or "host:lo-hi"
    pub targets: Option<Vec<RawTarget>>,
    #[serde(default)]
    pub prefer_ipv6: Option<bool>,
    pub health_check_interval_secs: Option<u32>,
    #[serde(default)]
    pub proxy_protocol: Option<String>,         // "off" | "v1" | "v2"
    pub udp_max_flows: Option<u32>,
    pub udp_flow_idle_secs: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTarget {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub priority: u32,
}

#[derive(Debug, Clone)]
pub struct ParsedRule {
    pub rule_id: RuleId,
    pub name: String,
    pub protocol: Protocol,
    pub listen_range: PortRange,
    pub target_host: String,           // for single-target fast path
    pub target_range: PortRange,
    pub prefer_ipv6: bool,
    pub health_check_interval_secs: Option<u32>,
    pub targets: Vec<MultiTarget>,     // empty for single-target without PROXY
    pub udp_max_flows: u32,
    pub udp_flow_idle_secs: u32,
    pub is_range: bool,
}

#[must_use]
pub fn derive_rule_id(name: &str) -> RuleId {
    let h = blake3::hash(name.as_bytes());
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&h.as_bytes()[..8]);
    RuleId(u64::from_le_bytes(arr))
}

impl Config {
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        Ok(toml::from_str(s)?)
    }

    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Io { path: path.to_owned(), source: e })?;
        Self::from_toml_str(&text)
    }

    /// Search order:
    ///   1. $PORTUNUS_STANDALONE_CONFIG
    ///   2. ./standalone.toml
    ///   3. /etc/portunus/standalone.toml
    pub fn load_default() -> Result<(Self, PathBuf), ConfigError> {
        let mut attempted = Vec::new();
        if let Ok(env) = std::env::var("PORTUNUS_STANDALONE_CONFIG") {
            let p = PathBuf::from(env);
            attempted.push(p.clone());
            if p.exists() { return Self::load_from(&p).map(|c| (c, p)); }
        }
        for cand in ["./standalone.toml", "/etc/portunus/standalone.toml"] {
            let p = PathBuf::from(cand);
            attempted.push(p.clone());
            if p.exists() { return Self::load_from(&p).map(|c| (c, p)); }
        }
        Err(ConfigError::NoConfigFile(attempted))
    }

    pub fn validate(&self) -> Result<HashMap<RuleId, String>, ConfigError> {
        if self.rules.is_empty() {
            return Err(ConfigError::NoRules);
        }
        let mut names = HashSet::new();
        let mut registry = HashMap::new();
        for r in &self.rules {
            if !names.insert(r.name.clone()) {
                return Err(ConfigError::DuplicateName(r.name.clone()));
            }
            let id = derive_rule_id(&r.name);
            if let Some(prev) = registry.insert(id, r.name.clone()) {
                return Err(ConfigError::RuleIdCollision { prev, current: r.name.clone(), id });
            }
            self.validate_rule(r)?;
        }
        Ok(registry)
    }

    fn validate_rule(&self, r: &RawRule) -> Result<(), ConfigError> {
        let proto = parse_protocol(&r.protocol)
            .map_err(|_| ConfigError::FieldProtocolMismatch {
                rule: r.name.clone(), proto: r.protocol.clone(), field: "protocol",
            })?;

        // listen_port vs listen_ports
        let (listen_lo, listen_hi) = match (r.listen_port, r.listen_ports.as_ref()) {
            (Some(p), None) => (p, p),
            (None, Some(s)) => parse_port_range(s).map_err(|reason|
                ConfigError::InvalidPortRange { rule: r.name.clone(), value: s.clone(), reason }
            )?,
            (Some(_), Some(_)) | (None, None) => return Err(ConfigError::InvalidPortRange {
                rule: r.name.clone(),
                value: format!("{:?}/{:?}", r.listen_port, r.listen_ports),
                reason: "exactly one of `listen_port` or `listen_ports` must be set".into(),
            }),
        };

        // target vs targets
        match (r.target.as_ref(), r.targets.as_ref()) {
            (None, None) => return Err(ConfigError::NoTarget { rule: r.name.clone() }),
            (Some(_), Some(_)) => return Err(ConfigError::TargetExclusivity),
            (None, Some(ts)) => {
                if listen_lo != listen_hi {
                    return Err(ConfigError::TargetsAndRange);
                }
                if ts.is_empty() {
                    return Err(ConfigError::NoTarget { rule: r.name.clone() });
                }
            }
            (Some(t), None) => {
                let (_, tlo, thi) = parse_target_string(t)
                    .map_err(|reason| ConfigError::InvalidTarget {
                        rule: r.name.clone(), target: t.clone(), reason,
                    })?;
                let listen_size = u32::from(listen_hi - listen_lo + 1);
                let target_size = u32::from(thi - tlo + 1);
                if listen_size != target_size {
                    return Err(ConfigError::RangeSizeMismatch {
                        rule: r.name.clone(), listen_size, target_size,
                    });
                }
            }
        }

        // protocol-specific warnings
        if matches!(proto, Protocol::Tcp) {
            if r.udp_max_flows.is_some() || r.udp_flow_idle_secs.is_some() {
                warn!(rule = %r.name, "udp_* fields ignored on TCP rule");
            }
        } else if matches!(proto, Protocol::Udp) && r.proxy_protocol.is_some() {
            return Err(ConfigError::FieldProtocolMismatch {
                rule: r.name.clone(), proto: "udp".into(), field: "proxy_protocol",
            });
        }

        Ok(())
    }

    /// Consume self and yield assembled ParsedRule iterator. Mirrors the
    /// validate() walk; assumes validate() succeeded.
    pub fn into_iter_rules(self) -> impl Iterator<Item = ParsedRule> {
        let defaults = self.defaults.clone();
        self.rules.into_iter().map(move |r| {
            let name = r.name.clone();
            let rule_id = derive_rule_id(&name);
            let proto = parse_protocol(&r.protocol).expect("validated upstream");
            let (listen_lo, listen_hi) = match (r.listen_port, r.listen_ports.as_ref()) {
                (Some(p), None) => (p, p),
                (None, Some(s)) => parse_port_range(s).expect("validated upstream"),
                _ => unreachable!("validate enforces XOR"),
            };
            let listen_range = PortRange::new(listen_lo, listen_hi).expect("validated upstream");
            let is_range = listen_lo != listen_hi;
            let prefer_ipv6 = r.prefer_ipv6.unwrap_or(defaults.prefer_ipv6);
            let proxy_protocol = match r.proxy_protocol.as_deref() {
                Some("v1") => Some(ProxyProtocolVersion::V1),
                Some("v2") => Some(ProxyProtocolVersion::V2),
                Some("off") | None => None,
                Some(other) => panic!("validate should have rejected {other:?}"),
            };
            // Single-target vs multi-target desugar (spec §4.2.4)
            let (target_host, target_range, targets) = match (r.target.as_ref(), r.targets) {
                (Some(t), None) => {
                    let (host, tlo, thi) = parse_target_string(t).expect("validated");
                    let target_range = PortRange::new(tlo, thi).expect("validated");
                    if proxy_protocol.is_some() {
                        // PROXY enabled — desugar into one-element targets[] (loses
                        // fast path; documented opt-in cost).
                        let mt = MultiTarget {
                            spec: RuleTarget {
                                host: host.clone(),
                                port: tlo,
                                priority: 0,
                                proxy_protocol,
                            },
                            target: classify_target(&host),
                        };
                        (host, target_range, vec![mt])
                    } else {
                        // Fast path preserved: targets = []
                        (host, target_range, Vec::new())
                    }
                }
                (None, Some(ts)) => {
                    // listen_lo == listen_hi enforced by validate (TargetsAndRange).
                    let target_range = PortRange::single(listen_lo);
                    let mts = ts.into_iter().map(|t| {
                        let target = classify_target(&t.host);
                        MultiTarget {
                            spec: RuleTarget {
                                host: t.host.clone(),
                                port: t.port,
                                priority: t.priority,
                                proxy_protocol,    // apply-all
                            },
                            target,
                        }
                    }).collect::<Vec<_>>();
                    let first_host = mts.first().map(|m| m.spec.host.clone()).unwrap_or_default();
                    (first_host, target_range, mts)
                }
                _ => unreachable!("validate enforces target XOR targets"),
            };

            ParsedRule {
                rule_id,
                name,
                protocol: proto,
                listen_range,
                target_host,
                target_range,
                prefer_ipv6,
                health_check_interval_secs: r.health_check_interval_secs,
                targets,
                udp_max_flows: r.udp_max_flows.unwrap_or(defaults.udp_max_flows),
                udp_flow_idle_secs: r.udp_flow_idle_secs.unwrap_or(defaults.udp_flow_idle_secs),
                is_range,
            }
        })
    }
}

impl ParsedRule {
    #[must_use]
    pub fn into_client_rule(self) -> ClientRule {
        ClientRule {
            rule_id: self.rule_id,
            listen_range: self.listen_range,
            target_host: self.target_host,
            target: portunus_core::Target::Dns(self.target_host.clone()),  // resolver layer treats DNS+IP uniformly; classify_target picks correctly
            target_range: self.target_range,
            prefer_ipv6: self.prefer_ipv6,
            protocol: self.protocol,
            udp_max_flows: self.udp_max_flows,
            udp_flow_idle_secs: self.udp_flow_idle_secs,
            targets: self.targets,
            health_check_interval_secs: self.health_check_interval_secs,
            // Standalone never enables these:
            quota: None,
            // Other fields per current ClientRule layout — fill defaults
            // matching the v0.6 fast path for single-target rules.
            ..Default::default()    // ClientRule::default() must exist; otherwise list explicitly
        }
    }
}

fn parse_protocol(s: &str) -> Result<Protocol, ()> {
    match s.to_ascii_lowercase().as_str() {
        "tcp" => Ok(Protocol::Tcp),
        "udp" => Ok(Protocol::Udp),
        _ => Err(()),
    }
}

fn parse_port_range(s: &str) -> Result<(u16, u16), String> {
    let (lo, hi) = s.split_once('-').ok_or_else(|| format!("expected `lo-hi`, got {s:?}"))?;
    let lo: u16 = lo.parse().map_err(|e| format!("lo {lo:?}: {e}"))?;
    let hi: u16 = hi.parse().map_err(|e| format!("hi {hi:?}: {e}"))?;
    if lo > hi { return Err(format!("lo {lo} > hi {hi}")); }
    Ok((lo, hi))
}

fn parse_target_string(s: &str) -> Result<(String, u16, u16), String> {
    // "host:port" or "host:lo-hi"
    let (host, port_part) = s.rsplit_once(':').ok_or_else(|| format!("expected host:port, got {s:?}"))?;
    if let Ok(p) = port_part.parse::<u16>() {
        return Ok((host.into(), p, p));
    }
    let (lo, hi) = parse_port_range(port_part)?;
    Ok((host.into(), lo, hi))
}

fn classify_target(host: &str) -> portunus_core::Target {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        portunus_core::Target::Ip(ip)
    } else {
        portunus_core::Target::Dns(host.into())
    }
}
```

If `ClientRule::default()` is not available, replace `..Default::default()`
with explicit per-field initialization listing every field. Inspect the
actual `ClientRule` struct first.

If `Target` enum variants differ, adjust `classify_target` accordingly.

- [ ] **Step 4: Wire mod config in main.rs**

```rust
mod config;

fn main() -> std::process::ExitCode {
    eprintln!("portunus-standalone: phase 3 scaffolding — runtime lands in T3.5");
    std::process::ExitCode::SUCCESS
}
```

- [ ] **Step 5: Add toml to workspace deps**

In root `Cargo.toml` under `[workspace.dependencies]`:

```toml
toml = "0.8"
```

- [ ] **Step 6: Run tests**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone config::tests`
Expected: PASS, 7 tests.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(standalone): TOML config schema + parse + validate + RuleId blake3 derivation"
```

### Task 3.3: Signal handler (SIGINT/SIGTERM/SIGHUP + shutdown.cancelled)

**Files:**
- Create: `crates/portunus-standalone/src/signal.rs`
- Modify: `crates/portunus-standalone/src/main.rs` (declare module)
- Test: `crates/portunus-standalone/src/signal.rs` (#[cfg(test)] mod tests)

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use portunus_forwarder::Shutdown;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handler_exits_when_shutdown_cancelled_externally() {
        let shutdown = Shutdown::new();
        let handle = install_standalone_signal_handler(shutdown.clone())
            .expect("signal install ok in test env");
        shutdown.trigger();
        // Handler must exit; await with a 2-second budget.
        let r = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(r.is_ok(), "signal task must exit when shutdown cancelled");
    }
}
```

- [ ] **Step 2: Verify failure**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone signal::tests`
Expected: COMPILE ERROR.

- [ ] **Step 3: Implement**

```rust
//! Standalone signal handler — SIGINT/SIGTERM trigger shutdown,
//! SIGHUP is a no-op (logged), and the handler also exits cleanly when
//! some other actor triggers `shutdown` (avoids signal_task.await
//! deadlock — spec v5 finding 1).

use std::io;
use tokio::task::JoinHandle;
use portunus_forwarder::Shutdown;

#[cfg(unix)]
pub fn install_standalone_signal_handler(shutdown: Shutdown) -> io::Result<JoinHandle<()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigint  = signal(SignalKind::interrupt())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup  = signal(SignalKind::hangup())?;
    let cancel = shutdown.token();
    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::debug!(event = "standalone.signal_handler_exit",
                                    reason = "shutdown_triggered_externally");
                    return;
                }
                _ = sigint.recv() => {
                    tracing::info!(event = "shutdown.signal", signal = "SIGINT");
                    shutdown.trigger();
                    return;
                }
                _ = sigterm.recv() => {
                    tracing::info!(event = "shutdown.signal", signal = "SIGTERM");
                    shutdown.trigger();
                    return;
                }
                _ = sighup.recv() => {
                    tracing::info!(event = "standalone.sighup_ignored");
                    // loop continues
                }
            }
        }
    }))
}

#[cfg(not(unix))]
pub fn install_standalone_signal_handler(shutdown: Shutdown) -> io::Result<JoinHandle<()>> {
    let cancel = shutdown.token();
    Ok(tokio::spawn(async move {
        tokio::select! {
            _ = cancel.cancelled() => {}
            _ = tokio::signal::ctrl_c() => {
                tracing::info!(event = "shutdown.signal", signal = "CTRL_C");
                shutdown.trigger();
            }
        }
    }))
}
```

- [ ] **Step 4: Test passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone signal::tests`
Expected: PASS.

- [ ] **Step 5: Wire module**

In `main.rs`:

```rust
mod config;
mod signal;
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(standalone): SIGHUP-aware signal handler with shutdown.cancelled escape arm"
```

### Task 3.4: Standalone reporter

**Files:**
- Create: `crates/portunus-standalone/src/reporter.rs`
- Modify: `crates/portunus-standalone/src/main.rs` (declare module)
- Test: `crates/portunus-standalone/src/reporter.rs` (#[cfg(test)] mod tests)

- [ ] **Step 1: Failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};
    use std::time::Duration;

    use portunus_core::{PortRange, RuleId};
    use portunus_forwarder::{RuleStats, Shutdown};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reporter_exits_on_cancel() {
        let stats_map: Arc<RwLock<HashMap<RuleId, Arc<RuleStats>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let registry: Arc<HashMap<RuleId, String>> = Arc::new(HashMap::new());
        let shutdown = Shutdown::new();
        let h = spawn_standalone_reporter(
            stats_map, registry, Duration::from_millis(50), shutdown.token(),
        );
        shutdown.trigger();
        let r = tokio::time::timeout(Duration::from_secs(2), h).await;
        assert!(r.is_ok(), "reporter must exit promptly on cancel");
    }
}
```

- [ ] **Step 2: Failure**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone reporter::tests`
Expected: COMPILE ERROR.

- [ ] **Step 3: Implement**

```rust
//! Standalone reporter — 60s tracing dump per rule. Spec §4.4.
//! Each tick reads `RuleStats::snapshot_basic()`; lock-poison errors
//! warn-and-skip-tick (no panic).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use portunus_core::RuleId;
use portunus_forwarder::RuleStats;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

pub fn spawn_standalone_reporter(
    rule_stats: Arc<RwLock<HashMap<RuleId, Arc<RuleStats>>>>,
    registry: Arc<HashMap<RuleId, String>>,
    interval: Duration,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => {
                    let map = match rule_stats.read() {
                        Ok(g) => g,
                        Err(e) => {
                            tracing::warn!(event = "standalone.reporter_lock_poisoned",
                                           error = %e);
                            continue;
                        }
                    };
                    for (rule_id, rs) in map.iter() {
                        let snap = rs.snapshot_basic();
                        let name = registry.get(rule_id).map(String::as_str).unwrap_or("?");
                        tracing::info!(
                            event = "standalone.stats",
                            rule = %rule_id,
                            rule_name = %name,
                            in_bytes = snap.bytes_in,
                            out_bytes = snap.bytes_out,
                            active_conns = snap.active_connections,
                            datagrams_in = snap.datagrams_in,
                            datagrams_out = snap.datagrams_out,
                            active_flows = snap.active_flows,
                        );
                    }
                }
            }
        }
    })
}
```

- [ ] **Step 4: Test passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone reporter::tests`
Expected: PASS.

- [ ] **Step 5: Wire module**

In `main.rs`:

```rust
mod config;
mod reporter;
mod signal;
```

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(standalone): 60s reporter using RuleStats.snapshot_basic + rule_name field"
```

### Task 3.5: Runtime — startup gate + fatal channel + biased select

**Files:**
- Create: `crates/portunus-standalone/src/runtime.rs`
- Modify: `crates/portunus-standalone/src/main.rs` (declare + call into run)
- Test: `crates/portunus-standalone/src/runtime.rs` (#[cfg(test)] mod tests if feasible — most testing happens in T3.6 via integration tests)

- [ ] **Step 1: Implement runtime.rs (no unit test — heavy integration in T3.6)**

```rust
//! Standalone runtime. Spec §4.3 / §4.5.

use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use portunus_core::RuleId;
use portunus_forwarder::{
    LiveResolver, RuleStats, RuleStatusEvent, Shutdown, run_forwarder,
};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::reporter::spawn_standalone_reporter;
use crate::signal::install_standalone_signal_handler;

pub async fn run(cfg: Config, registry: HashMap<RuleId, String>) -> ExitCode {
    let registry = Arc::new(registry);
    let shutdown = Shutdown::new();

    let signal_task = match install_standalone_signal_handler(shutdown.clone()) {
        Ok(j) => j,
        Err(e) => {
            error!(event = "standalone.signal_install_failed", error = %e);
            return ExitCode::from(1);
        }
    };

    let resolver = match LiveResolver::with_system_defaults() {
        Ok(r) => Arc::new(r),
        Err(e) => {
            error!(event = "standalone.resolver_init_failed", error = %e);
            shutdown.trigger();
            let _ = signal_task.await;
            return ExitCode::from(1);
        }
    };

    // Best-effort RLIMIT_NOFILE log
    #[cfg(unix)]
    log_fd_limit();

    let drain = Duration::from_secs(cfg.global.shutdown_drain_secs);
    let rule_stats_handles: Arc<RwLock<HashMap<RuleId, Arc<RuleStats>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let reporter_handle = spawn_standalone_reporter(
        Arc::clone(&rule_stats_handles),
        Arc::clone(&registry),
        Duration::from_secs(60),
        shutdown.token(),
    );

    let (status_tx, mut status_rx) = mpsc::channel(64);
    let (fatal_tx, mut fatal_rx) = mpsc::channel::<()>(1);

    let mut joinset = JoinSet::new();
    let expected: HashSet<RuleId> = registry.keys().copied().collect();
    let stats_for_main = Arc::clone(&rule_stats_handles);

    for parsed in cfg.into_iter_rules() {
        let rule_id = parsed.rule_id;
        let rule = parsed.into_client_rule();
        let stats = RuleStats::for_range(rule.listen_range);
        match stats_for_main.write() {
            Ok(mut g) => { g.insert(rule_id, Arc::clone(&stats)); }
            Err(e) => {
                error!(event = "standalone.stats_registry_poisoned",
                       %rule_id, error = %e);
                // continue spawning forwarder anyway — reporter just misses
                // this rule's stats logs
            }
        }
        joinset.spawn(run_forwarder(
            rule,
            Arc::clone(&resolver),
            status_tx.clone(),
            shutdown.token(),
            drain,
            stats,
        ));
    }
    drop(status_tx);

    // ─── Startup gate ───
    let mut pending = expected;
    let mut startup_failures: Vec<(RuleId, String)> = Vec::new();
    while !pending.is_empty() {
        match status_rx.recv().await {
            Some(RuleStatusEvent::Activated { rule_id }) => { pending.remove(&rule_id); }
            Some(RuleStatusEvent::Failed { rule_id, reason }) => {
                pending.remove(&rule_id);
                startup_failures.push((rule_id, reason));
            }
            Some(RuleStatusEvent::Removed { rule_id }) => {
                warn!(event = "standalone.unexpected_removed", %rule_id);
                pending.remove(&rule_id);
            }
            None => break,
        }
    }
    if !startup_failures.is_empty() {
        eprintln!("error: {} rule(s) failed to bind:", startup_failures.len());
        for (id, why) in &startup_failures {
            let name = registry.get(id).map(String::as_str).unwrap_or("?");
            eprintln!("  - {name} ({id}): {why}");
        }
        shutdown.trigger();
        while joinset.join_next().await.is_some() {}
        let _ = reporter_handle.await;
        let _ = signal_task.await;
        return ExitCode::from(1);
    }

    // ─── Run-time status forwarder ───
    let registry_clone = Arc::clone(&registry);
    let fatal_tx_clone = fatal_tx.clone();
    tokio::spawn(async move {
        while let Some(ev) = status_rx.recv().await {
            match ev {
                RuleStatusEvent::Failed { rule_id, reason } => {
                    let name = registry_clone.get(&rule_id).map(String::as_str).unwrap_or("?");
                    error!(event = "rule.failed", %rule_id, rule_name = %name, %reason);
                    let _ = fatal_tx_clone.try_send(());
                }
                RuleStatusEvent::Removed { rule_id } => {
                    let name = registry_clone.get(&rule_id).map(String::as_str).unwrap_or("?");
                    info!(event = "rule.removed", %rule_id, rule_name = %name);
                }
                RuleStatusEvent::Activated { rule_id } => {
                    let name = registry_clone.get(&rule_id).map(String::as_str).unwrap_or("?");
                    info!(event = "rule.reactivated", %rule_id, rule_name = %name);
                }
            }
        }
    });
    drop(fatal_tx);

    // ─── Main select ───
    let mut fatal_flag = false;
    loop {
        tokio::select! {
            biased;
            Some(()) = fatal_rx.recv() => {
                error!(event = "standalone.fatal_shutdown");
                fatal_flag = true;
                shutdown.trigger();
            }
            join = joinset.join_next() => {
                match join {
                    Some(Err(e)) => {
                        error!(event = "standalone.task_panic", error = %e);
                        fatal_flag = true;
                        shutdown.trigger();
                    }
                    Some(Ok(_)) => continue,
                    None => break,
                }
            }
        }
    }

    // Tail: force-trigger shutdown so reporter / signal task exit. Idempotent.
    if !shutdown.token().is_cancelled() {
        shutdown.trigger();
    }
    let _ = reporter_handle.await;
    let _ = signal_task.await;
    info!(event = "standalone.stopped");
    if fatal_flag { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

#[cfg(unix)]
fn log_fd_limit() {
    let mut rlim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: getrlimit is a thread-safe POSIX call; we pass a valid mutable
    // pointer.
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        tracing::debug!(event = "standalone.rlimit_query_failed", error = %err);
        return;
    }
    tracing::info!(event = "standalone.rlimit_nofile",
                   soft = rlim.rlim_cur as u64, hard = rlim.rlim_max as u64);
    if rlim.rlim_cur < 4096 {
        tracing::warn!(event = "standalone.rlimit_nofile_low",
                       soft = rlim.rlim_cur as u64,
                       "set LimitNOFILE / --ulimit nofile to at least 4096");
    }
}
```

If the code references `libc::rlimit` and the workspace doesn't already
have `libc`, add it to `[dependencies]` in
`crates/portunus-standalone/Cargo.toml` (already added in T3.1).

- [ ] **Step 2: Build**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone`
Expected: PASS. Common issues:
  - `ClientRule` field mismatch — adjust `ParsedRule::into_client_rule()`
  - `unsafe_code = "deny"` workspace lint — the `getrlimit` call needs a
    local `#[allow(unsafe_code)]` annotation on `fn log_fd_limit`. Add it
    if clippy fails.

- [ ] **Step 3: Wire main.rs**

```rust
mod config;
mod reporter;
mod runtime;
mod signal;

use std::process::ExitCode;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "portunus-standalone", version, about = "Standalone TCP/UDP forwarder")]
struct Cli {
    /// Path to standalone.toml. If omitted, the loader searches
    /// $PORTUNUS_STANDALONE_CONFIG, ./standalone.toml, /etc/portunus/standalone.toml.
    #[arg(short, long)]
    config: Option<std::path::PathBuf>,
    /// Validate config and exit (0 = valid, 2 = invalid).
    #[arg(long)]
    check: bool,
    /// Override [global].log_level
    #[arg(long)]
    log_level: Option<String>,
    /// Override [global].log_format
    #[arg(long)]
    log_format: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (cfg, _path) = match cli.config.as_deref() {
        Some(p) => match config::Config::load_from(p) {
            Ok(c) => (c, p.to_path_buf()),
            Err(e) => { eprintln!("error: {e}"); return ExitCode::from(2); }
        },
        None => match config::Config::load_default() {
            Ok(t) => t,
            Err(e) => { eprintln!("error: {e}"); return ExitCode::from(2); }
        },
    };

    let registry = match cfg.validate() {
        Ok(r) => r,
        Err(e) => { eprintln!("error: {e}"); return ExitCode::from(2); }
    };

    if cli.check {
        println!("ok");
        return ExitCode::SUCCESS;
    }

    init_tracing(&cli, &cfg);

    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: tokio runtime: {e}");
            return ExitCode::from(1);
        }
    };
    rt.block_on(runtime::run(cfg, registry))
}

fn init_tracing(cli: &Cli, cfg: &config::Config) {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    let level = cli.log_level.as_deref().unwrap_or(&cfg.global.log_level);
    let format = cli.log_format.as_deref().unwrap_or(&cfg.global.log_format);

    let filter = EnvFilter::try_new(level)
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    match format {
        "pretty" => {
            let _ = registry.with(fmt::layer().pretty().with_writer(std::io::stderr)).try_init();
        }
        _ => {
            let _ = registry.with(fmt::layer().json().with_writer(std::io::stderr)).try_init();
        }
    }
}
```

- [ ] **Step 4: Build full binary**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(standalone): runtime — startup gate + fatal channel + biased select + tracing init"
```

### Task 3.6: --check mode integration tests

**Files:**
- Create: `crates/portunus-standalone/tests/fixtures/valid_minimal.toml`
- Create: `crates/portunus-standalone/tests/fixtures/valid_full.toml`
- Create: `crates/portunus-standalone/tests/fixtures/valid_udp.toml`
- Create: `crates/portunus-standalone/tests/fixtures/invalid_unknown_field.toml`
- Create: `crates/portunus-standalone/tests/fixtures/invalid_range_mismatch.toml`
- Create: `crates/portunus-standalone/tests/fixtures/invalid_no_rules.toml`
- Create: `crates/portunus-standalone/tests/check_mode.rs`

- [ ] **Step 1: Write fixtures**

`valid_minimal.toml`:

```toml
[[rule]]
name = "ssh"
protocol = "tcp"
listen_port = 2222
target = "10.0.0.5:22"
```

`valid_full.toml`:

```toml
[global]
log_level = "debug"

[defaults]
udp_max_flows = 4096

[[rule]]
name = "ssh-tunnel"
protocol = "tcp"
listen_port = 2222
target = "10.0.0.5:22"

[[rule]]
name = "web-range"
protocol = "tcp"
listen_ports = "8000-8009"
target = "10.0.0.10:8000-8009"

[[rule]]
name = "ha-https"
protocol = "tcp"
listen_port = 8443
targets = [
  { host = "primary.internal",   port = 443, priority = 0  },
  { host = "secondary.internal", port = 443, priority = 10 },
]
proxy_protocol = "v2"
health_check_interval_secs = 5

[[rule]]
name = "game-udp"
protocol = "udp"
listen_port = 27015
target = "10.0.0.20:27015"
udp_flow_idle_secs = 120
```

`valid_udp.toml`:

```toml
[[rule]]
name = "udp-echo"
protocol = "udp"
listen_port = 9999
target = "127.0.0.1:9998"
```

`invalid_unknown_field.toml`:

```toml
[[rule]]
name = "a"
protocol = "tcp"
listen_port = 1
target = "1.1.1.1:1"
bind_addr = "0.0.0.0"
```

`invalid_range_mismatch.toml`:

```toml
[[rule]]
name = "a"
protocol = "tcp"
listen_ports = "8000-8009"
target = "1.1.1.1:8000-8019"
```

`invalid_no_rules.toml`:

```toml
[global]
log_level = "info"
```

- [ ] **Step 2: Write check_mode.rs**

```rust
use assert_cmd::Command;

fn fixture(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("tests/fixtures");
    p.push(name);
    p
}

#[test]
fn check_valid_minimal_exits_0() {
    Command::cargo_bin("portunus-standalone").unwrap()
        .args(["--check", "--config"]).arg(fixture("valid_minimal.toml"))
        .assert().code(0);
}

#[test]
fn check_valid_full_exits_0() {
    Command::cargo_bin("portunus-standalone").unwrap()
        .args(["--check", "--config"]).arg(fixture("valid_full.toml"))
        .assert().code(0);
}

#[test]
fn check_valid_udp_exits_0() {
    Command::cargo_bin("portunus-standalone").unwrap()
        .args(["--check", "--config"]).arg(fixture("valid_udp.toml"))
        .assert().code(0);
}

#[test]
fn check_unknown_field_exits_2() {
    Command::cargo_bin("portunus-standalone").unwrap()
        .args(["--check", "--config"]).arg(fixture("invalid_unknown_field.toml"))
        .assert().code(2)
        .stderr(predicates::str::contains("unknown field"));
}

#[test]
fn check_range_mismatch_exits_2() {
    Command::cargo_bin("portunus-standalone").unwrap()
        .args(["--check", "--config"]).arg(fixture("invalid_range_mismatch.toml"))
        .assert().code(2)
        .stderr(predicates::str::contains("range size"));
}

#[test]
fn check_no_rules_exits_2() {
    Command::cargo_bin("portunus-standalone").unwrap()
        .args(["--check", "--config"]).arg(fixture("invalid_no_rules.toml"))
        .assert().code(2)
        .stderr(predicates::str::contains("at least one"));
}
```

Add to `crates/portunus-standalone/Cargo.toml`:

```toml
[dev-dependencies]
predicates = "3"
# (assert_cmd + tempfile already added)
```

- [ ] **Step 3: Tests run**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --test check_mode`
Expected: PASS, 6 tests.

If `unknown field` doesn't show in stderr, your toml crate version
formats errors differently — relax the assertion to
`predicates::str::contains("bind_addr")`.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "test(standalone): --check fixture sweep — 6 cases, exit 0 + 2"
```

### Task 3.7: Smoke test — TCP echo loopback end-to-end

**Files:**
- Create: `crates/portunus-standalone/tests/smoke.rs`

- [ ] **Step 1: Implement smoke test**

```rust
//! Smoke: spin up an echo TCP server on a random localhost port, write
//! a TOML config that forwards another random port to it, launch the
//! standalone binary as a subprocess, verify bytes echo, then send
//! SIGTERM and verify graceful exit.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::thread;
use std::time::{Duration, Instant};

fn pick_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn spawn_echo() -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        loop {
            if stop_clone.load(Ordering::Relaxed) { break; }
            match listener.accept() {
                Ok((mut sock, _)) => {
                    thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = sock.read(&mut buf) {
                            if n == 0 { break; }
                            if sock.write_all(&buf[..n]).is_err() { break; }
                        }
                        let _ = sock.shutdown(Shutdown::Both);
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    (port, stop)
}

fn wait_for_listen(port: u16, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(),
                                       Duration::from_millis(200)).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn tcp_echo_loopback() {
    let (backend_port, backend_stop) = spawn_echo();
    let frontend_port = pick_port();
    let cfg = format!(r#"
[global]
log_level = "warn"

[[rule]]
name = "echo"
protocol = "tcp"
listen_port = {frontend_port}
target = "127.0.0.1:{backend_port}"
"#);

    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), &cfg).unwrap();

    let bin = assert_cmd::cargo::cargo_bin("portunus-standalone");
    let mut child = Command::new(&bin)
        .arg("--config").arg(tmp.path())
        .stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().expect("spawn standalone");

    // Wait up to 5s for forwarder to start listening
    let listening = wait_for_listen(frontend_port, Instant::now() + Duration::from_secs(5));
    assert!(listening, "frontend port {frontend_port} should be listening");

    // Echo round trip
    let mut s = TcpStream::connect(format!("127.0.0.1:{frontend_port}")).unwrap();
    s.write_all(b"hello, standalone").unwrap();
    let mut buf = [0u8; 17];
    s.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"hello, standalone");
    drop(s);

    // Tear down
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // SIGTERM (15)
        unsafe { libc::kill(child.id() as i32, libc::SIGTERM); }
        let status = child.wait().expect("standalone wait");
        let code = status.code().unwrap_or_else(|| status.signal().expect("signaled"));
        assert!(code == 0 || code == 143, "expected clean exit, got {code}");
    }
    backend_stop.store(true, Ordering::Relaxed);
}
```

Add to `[dev-dependencies]`:

```toml
libc = "0.2"
```

- [ ] **Step 2: Run**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --test smoke -- --test-threads=1`
Expected: PASS. Single-thread mode avoids port reuse churn.

If the test flakes on slow CI, increase `wait_for_listen` deadline.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "test(standalone): smoke — TCP echo loopback + SIGTERM clean exit"
```

### Task 3.8: Workspace gate

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: no diff.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Full test sweep**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Commit fmt/clippy fixups**

```bash
git add -u
git commit -m "chore: fmt/clippy sweep after Phase 3" || true
```

---

## Phase 4: E2E + docs + release plumbing

### Task 4.1: E2E — single-rule TCP echo via portunus-standalone

**Files:**
- Create: `crates/portunus-e2e/tests/standalone_tcp_basic.rs`

- [ ] **Step 1: Skim existing portunus-e2e patterns**

Run:

```bash
ls crates/portunus-e2e/tests/ | head
head -60 crates/portunus-e2e/tests/$(ls crates/portunus-e2e/tests/ | head -1)
```

Note how the existing e2e tests spawn processes — keep parity.

- [ ] **Step 2: Implement test**

Most of this test is the same shape as `smoke.rs` but lives in
`crates/portunus-e2e/`. Use the existing e2e harness if it provides one;
otherwise copy the pattern from `smoke.rs`. Confirm that the e2e crate
already depends on `assert_cmd` / `tempfile` — add if needed.

```rust
//! E2E: portunus-standalone forwards TCP echo. Mirrors crate-local
//! smoke.rs but lives in e2e for the project-wide regression matrix.

// Body identical to crates/portunus-standalone/tests/smoke.rs::tcp_echo_loopback;
// imports may differ if portunus-e2e has its own utility module.
```

If portunus-e2e has its own helper for spawning binaries, prefer that.

- [ ] **Step 3: Run**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-e2e --test standalone_tcp_basic -- --test-threads=1`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/portunus-e2e/
git commit -m "e2e(standalone): TCP echo via portunus-standalone binary"
```

### Task 4.2: E2E — multi-target failover

**Files:**
- Create: `crates/portunus-e2e/tests/standalone_failover.rs`

- [ ] **Step 1: Implement**

```rust
//! E2E: standalone failover. Start two backends, configure with
//! priority 0 + 10, kill the primary, verify next connection routes to
//! the secondary.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

fn spawn_marker_server(marker: &'static [u8]) -> (u16, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        loop {
            if stop_clone.load(Ordering::Relaxed) { break; }
            match listener.accept() {
                Ok((mut s, _)) => {
                    thread::spawn(move || {
                        let _ = s.write_all(marker);
                        let _ = s.flush();
                        // Read & echo a single chunk so the client can verify
                        let mut buf = [0u8; 64];
                        if let Ok(n) = s.read(&mut buf) {
                            let _ = s.write_all(&buf[..n]);
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    (port, stop)
}

fn wait_for_listen(port: u16, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(),
                                       Duration::from_millis(200)).is_ok() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn failover_routes_to_secondary_after_primary_drops() {
    let (primary_port, primary_stop) = spawn_marker_server(b"PRIMARY\n");
    let (secondary_port, secondary_stop) = spawn_marker_server(b"SECONDARY\n");
    let frontend_port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();

    let cfg = format!(r#"
[global]
log_level = "warn"

[[rule]]
name = "failover"
protocol = "tcp"
listen_port = {frontend_port}
targets = [
  {{ host = "127.0.0.1", port = {primary_port},   priority = 0  }},
  {{ host = "127.0.0.1", port = {secondary_port}, priority = 10 }},
]
"#);
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), &cfg).unwrap();

    let bin = assert_cmd::cargo::cargo_bin("portunus-standalone");
    let mut child = Command::new(&bin).arg("--config").arg(tmp.path())
        .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();

    assert!(wait_for_listen(frontend_port, Instant::now() + Duration::from_secs(5)));

    // First request → primary
    let mut s = TcpStream::connect(format!("127.0.0.1:{frontend_port}")).unwrap();
    let mut buf = vec![0u8; 8];
    s.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"PRIMARY\n");
    drop(s);

    // Kill primary; wait briefly for failover health detector
    primary_stop.store(true, Ordering::Relaxed);
    thread::sleep(Duration::from_millis(500));

    // Second request → secondary
    let mut s = TcpStream::connect(format!("127.0.0.1:{frontend_port}")).unwrap();
    let mut buf = vec![0u8; 10];
    s.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"SECONDARY\n");
    drop(s);

    #[cfg(unix)] unsafe { libc::kill(child.id() as i32, libc::SIGTERM); }
    let _ = child.wait();
    secondary_stop.store(true, Ordering::Relaxed);
}
```

- [ ] **Step 2: Run**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-e2e --test standalone_failover -- --test-threads=1`
Expected: PASS. If flaky on the failover timing, bump the sleep.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "e2e(standalone): multi-target failover after primary drops"
```

### Task 4.3: E2E — PROXY protocol v2 prelude

**Files:**
- Create: `crates/portunus-e2e/tests/standalone_proxy_v2.rs`

- [ ] **Step 1: Implement**

```rust
//! E2E: PROXY v2 prelude. Standalone is configured with proxy_protocol="v2";
//! backend reads the first 16 bytes (PROXY v2 signature) before echoing.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PROXY_V2_SIG: [u8; 12] = [0x0D, 0x0A, 0x0D, 0x0A, 0x00, 0x0D, 0x0A, 0x51, 0x55, 0x49, 0x54, 0x0A];

fn wait_for_listen(port: u16, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(),
                                       Duration::from_millis(200)).is_ok() { return true; }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn proxy_v2_prelude_reaches_backend() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let backend_port = listener.local_addr().unwrap().port();

    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    thread::spawn(move || {
        if let Ok((mut s, _)) = listener.accept() {
            let mut sig = [0u8; 12];
            if s.read_exact(&mut sig).is_ok() {
                let mut rest = [0u8; 4];
                let _ = s.read_exact(&mut rest); // ver+cmd, family+proto, len_hi, len_lo
                let len = u16::from_be_bytes([rest[2], rest[3]]) as usize;
                let mut addr_block = vec![0u8; len];
                let _ = s.read_exact(&mut addr_block);
                let _ = tx.send(sig.to_vec());
                let mut payload = [0u8; 32];
                if let Ok(n) = s.read(&mut payload) {
                    let _ = s.write_all(&payload[..n]);
                }
            }
        }
    });

    let frontend_port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let cfg = format!(r#"
[[rule]]
name = "proxyv2"
protocol = "tcp"
listen_port = {frontend_port}
targets = [{{ host = "127.0.0.1", port = {backend_port}, priority = 0 }}]
proxy_protocol = "v2"
"#);
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), &cfg).unwrap();

    let bin = assert_cmd::cargo::cargo_bin("portunus-standalone");
    let mut child = Command::new(&bin).arg("--config").arg(tmp.path())
        .stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();

    assert!(wait_for_listen(frontend_port, Instant::now() + Duration::from_secs(5)));

    let mut s = TcpStream::connect(format!("127.0.0.1:{frontend_port}")).unwrap();
    s.write_all(b"data after prelude").unwrap();

    let sig = rx.recv_timeout(Duration::from_secs(5)).expect("backend saw prelude");
    assert_eq!(sig, PROXY_V2_SIG, "first 12 bytes must be the PROXY v2 signature");

    #[cfg(unix)] unsafe { libc::kill(child.id() as i32, libc::SIGTERM); }
    let _ = child.wait();
}
```

- [ ] **Step 2: Run**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-e2e --test standalone_proxy_v2 -- --test-threads=1`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "e2e(standalone): PROXY v2 prelude reaches backend"
```

### Task 4.4: Operator docs (mdx, EN + 中文)

**Files:**
- Create: `docs/content/docs/operations/standalone.mdx`
- Create: `docs/content/docs/zh/operations/standalone.mdx`
- Modify: `docs/content/docs/operations/meta.json`
- Modify: `docs/content/docs/zh/operations/meta.json`

- [ ] **Step 1: English doc**

`docs/content/docs/operations/standalone.mdx`:

````mdx
---
title: Standalone Forwarder
description: TOML-driven TCP/UDP forwarding without the gRPC control plane.
---

`portunus-standalone` is a single-binary, no-control-plane sibling of
`portunus-client`. It reads rules from a TOML file and runs the same
data plane as `portunus-client` — splice on Linux, DNS resolution,
multi-target failover, PROXY protocol prelude.

## Install

```sh
cargo build --release -p portunus-standalone
sudo install -m 0755 target/release/portunus-standalone /usr/local/bin/
```

## Configuration file

The binary searches in this order if `--config` is omitted:

1. `$PORTUNUS_STANDALONE_CONFIG`
2. `./standalone.toml`
3. `/etc/portunus/standalone.toml`

```toml
[global]
log_level            = "info"
log_format           = "json"
shutdown_drain_secs  = 30

[defaults]
udp_max_flows        = 1024
udp_flow_idle_secs   = 60
prefer_ipv6          = false

[[rule]]
name        = "ssh-tunnel"
protocol    = "tcp"
listen_port = 2222
target      = "10.0.0.5:22"

[[rule]]
name              = "ha-https"
protocol          = "tcp"
listen_port       = 8443
targets           = [
  { host = "primary.internal",   port = 443, priority = 0  },
  { host = "secondary.internal", port = 443, priority = 10 },
]
proxy_protocol      = "v2"
health_check_interval_secs = 5
```

## Operations

- **Validate config without binding**: `portunus-standalone --check --config standalone.toml`. Exits 0 if valid, 2 if not.
- **Graceful shutdown**: SIGTERM or SIGINT. The process drains in-flight forwarded connections for up to `shutdown_drain_secs` then exits 0.
- **SIGHUP**: ignored. Restart the process to apply config changes.
- **Logging**: every log line carries `rule=<rule_id>` and `rule_name=<name>`. Stats are emitted every 60s as `event=standalone.stats`.

## Limitations (v1.5)

These features live in `portunus-client` only — for now:

- TLS SNI routing
- Bandwidth / QoS rate limiting
- Per-user traffic quotas
- Web UI / HTTP management surface
- Hot reload (use process restart)
- Explicit `bind` address (listener always binds wildcard `0.0.0.0` + `[::]`)
````

- [ ] **Step 2: Chinese doc**

`docs/content/docs/zh/operations/standalone.mdx`:

````mdx
---
title: 单机版转发器
description: 基于 TOML 配置的 TCP/UDP 转发器,无需 gRPC 控制平面。
---

`portunus-standalone` 是 `portunus-client` 的单机版,不连接任何控制平面;
它从 TOML 文件读取规则,使用与 `portunus-client` 相同的数据面 —— Linux
splice、DNS 解析、多目标 failover、PROXY protocol 前缀。

## 安装

```sh
cargo build --release -p portunus-standalone
sudo install -m 0755 target/release/portunus-standalone /usr/local/bin/
```

## 配置文件

未指定 `--config` 时,按顺序查找:

1. `$PORTUNUS_STANDALONE_CONFIG`
2. `./standalone.toml`
3. `/etc/portunus/standalone.toml`

(配置示例同英文版。)

## 运维

- **仅校验不绑定**:`portunus-standalone --check --config standalone.toml`。合法返回 0,非法返回 2。
- **优雅关停**:SIGTERM / SIGINT。进程会在 `shutdown_drain_secs` 内排干 in-flight 连接后退出 0。
- **SIGHUP**:忽略。修改配置请重启进程生效。
- **日志**:所有日志含 `rule=<rule_id>` 和 `rule_name=<名字>` 字段。每 60s 输出一次 `event=standalone.stats`。

## v1.5 范围之外

下列特性仅在 `portunus-client` 控制面提供:

- TLS SNI 路由
- 带宽 / QoS 限速
- 用户流量配额
- Web UI / HTTP 管理面
- 热重载(请重启进程)
- 显式 `bind` 地址(监听器永远绑 `0.0.0.0` 和 `[::]`)
````

- [ ] **Step 3: Register pages in meta.json**

Read existing `docs/content/docs/operations/meta.json` to confirm the
ordering convention, then add `"standalone"` to the `pages` array.
Same for zh.

- [ ] **Step 4: Commit**

```bash
git add docs/content/docs/operations/standalone.mdx \
        docs/content/docs/zh/operations/standalone.mdx \
        docs/content/docs/operations/meta.json \
        docs/content/docs/zh/operations/meta.json
git commit -m "docs(operations): standalone forwarder runbook (EN+zh)"
```

### Task 4.5: README + CHANGELOG + Makefile

**Files:**
- Modify: `README.md`
- Modify: `CHANGELOG.md`
- Modify: `Makefile`

- [ ] **Step 1: README — add Standalone section**

Add a `## Standalone forwarder` section near the existing client/server
sections in `README.md`. Brief 5-line intro + link to the runbook.

```markdown
## Standalone forwarder (v1.5+)

For deployments that don't need the gRPC control plane,
`portunus-standalone` runs the same data plane driven by a TOML config.
See [docs/operations/standalone](./docs/content/docs/operations/standalone.mdx).
```

- [ ] **Step 2: CHANGELOG v1.5.0 entry**

Add to top of `CHANGELOG.md`:

```markdown
## v1.5.0 — 2026-05-DD

### Added
- `portunus-standalone` binary — TOML-driven TCP/UDP forwarder without
  the gRPC control plane. Balanced feature pack: TCP/UDP, port ranges,
  DNS targets, multi-target failover, PROXY protocol v1/v2.
- New crate `portunus-forwarder` houses the shared data plane consumed
  by both `portunus-client` and `portunus-standalone`. `portunus-client`
  behavior is unchanged — wire bytes are byte-identical to v1.4.0
  (validated by the existing `*_wire_compat` test suite).

### Changed
- `portunus-core::Protocol` is now the authoritative `Protocol` enum;
  `portunus-server` and `portunus-proto` derive their representations
  from it via `From`/`TryFrom`. Wire shape unchanged.
```

- [ ] **Step 3: Makefile targets**

Append to `Makefile`:

```make
standalone: ## Build portunus-standalone in release mode.
	cargo build --release -p portunus-standalone

standalone-check: ## Validate every TOML fixture (CI smoke).
	@for f in crates/portunus-standalone/tests/fixtures/valid_*.toml; do \
		echo "Checking $$f"; \
		PORTUNUS_SKIP_WEBUI=1 cargo run --quiet -p portunus-standalone -- --check --config "$$f" || exit 1; \
	done
```

- [ ] **Step 4: Smoke test the make target**

Run: `make standalone-check`
Expected: prints "ok" 3 times (one per `valid_*.toml`).

- [ ] **Step 5: Commit**

```bash
git add README.md CHANGELOG.md Makefile
git commit -m "docs/build: README+CHANGELOG+Makefile for standalone v1.5.0"
```

### Task 4.6: Workspace final gate

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: no diff.

- [ ] **Step 2: Clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Full test sweep + e2e**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Wire-compat anchor**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client wire_compat`
Expected: PASS. If any test failed earlier in the chain you must NOT
land Phase 4 — go back and fix.

- [ ] **Step 5: Bench dry run (don't measure regression — just compile)**

Run: `cargo bench -p portunus-forwarder --no-run`
Expected: PASS. Actual regression measurement happens on the perf host
per Constitution Principle II.

- [ ] **Step 6: Commit any chore fixups**

```bash
git add -u
git commit -m "chore: final fmt/clippy sweep for v1.5.0" || true
```

---

## Plan self-review (done by author at write time)

1. **Spec coverage** — every section of the spec has at least one task:
   §1-2 (Goal/audience) → Phase intros; §3.1 layout → T1.1+T2.1; §3.2 forwarder
   inner layout → T2.1; §3.3 public API → T2.9; §3.4 proto-free seams →
   T2.2-T2.5; §3.5 Protocol upgrade → T1.2-T1.5; §4.1 CLI → T3.5; §4.2 TOML
   schema → T3.2; §4.2.2 wildcard bind → enforced by schema in T3.2; §4.2.3
   RuleId blake3 → T3.2; §4.2.4-5 PROXY desugar → T3.2 into_iter_rules;
   §4.2.6 errors → T3.2 + T3.6 fixtures; §4.3 startup gate + fatal → T3.5;
   §4.4 reporter → T3.4; §4.5 SIGHUP handler → T3.3; §4.6 rlimit → T3.5;
   §5.1 mechanical migration → T2.1+T2.12; §5.2 snapshots + From → T2.3-T2.7,
   T2.10-T2.11; §5.3 Protocol upgrade → Phase 1; §5.4 Quota unchanged → no
   task (zero diff); §5.5 test re-homing → T2.1 git mv; §5.6 LiveResolver
   helper → T2.8; §5.7 Shutdown unchanged → no task; §6 tests → T3.6, T3.7,
   T4.1-T4.3; §7 phases → matches the four Phase sections; §8 risks → mitigated
   by tests; §9 OOS → enforced by deny_unknown_fields in T3.2.

2. **Placeholders** — no "TBD" / "implement later" / "similar to". Every
   code-touching step shows code.

3. **Type consistency** — `RuleStatsSnapshot { rule_id, ... }` consistent
   between T2.6 definition and T2.10/T2.11 use; `snapshot_basic()` matches
   between T2.6 and T3.4 reporter use; `ParsedRule::into_client_rule`
   referenced consistently between T3.2 and T3.5; `install_standalone_signal_handler`
   name consistent across T3.3 and T3.5.

---

## Plan complete

Plan saved to `docs/superpowers/plans/2026-05-14-standalone-forwarder.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, two-stage review (spec compliance, then code quality) between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using `superpowers:executing-plans`, batch execution with checkpoints for review.

**Which approach?**
