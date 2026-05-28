# portunus-standalone Stats TUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add per-rule traffic monitoring to `portunus-standalone` via a Unix-domain-socket JSON-line protocol and a `ratatui`-based TUI client (`portunus-standalone stats`), without adding HTTP, web UI, or any new control-plane surface.

**Architecture:** Daemon exposes an additive UDS server that streams cumulative per-rule counters (from `RuleStats`) as JSON lines. The TUI client (same binary, `stats` subcommand) maintains a client-side 60 s ring buffer and computes rates from a daemon-side `uptime_ms` monotonic clock. The forwarder crate (`portunus-forwarder`) gains three new counter groups (`connections_total`, `ErrorCounters`, `target_failovers_total: Arc<AtomicU64>`) wired into existing tracing call sites.

**Tech Stack:** Rust 2024, MSRV 1.88; `tokio` (UnixListener), `ratatui` + `crossterm` (TUI, gated by `stats-tui` feature), `serde_json`, `clap` v4 subcommands.

**Spec:** `docs/superpowers/specs/2026-05-28-standalone-stats-tui-design.md`. Read it before starting any task.

---

## File map

```
crates/portunus-forwarder/src/forwarder/
├── stats.rs                    MODIFY  add connections_total, ErrorCounters, target_failovers_total
├── mod.rs                      MODIFY  bump port_in_use at 2 rule.failed call sites + connections_total on accept
├── failover_path.rs            MODIFY  bump port_in_use at 2 rule.failed sites; clone target_failovers_total from RuleStats; bump connections_total on accept
└── udp/
    ├── listener.rs             MODIFY  bump emsgsize/icmp_evict/upstream_connect_failed/addflow_dropped counters
    └── demux.rs                MODIFY  bump emsgsize/icmp_evict/wouldblock counters

crates/portunus-standalone/
├── Cargo.toml                  MODIFY  add ratatui, crossterm, serde_json (under stats-tui feature)
├── src/
│   ├── main.rs                 MODIFY  clap subcommand dispatch (daemon | stats)
│   ├── config.rs               MODIFY  [stats] section with platform-aware default
│   ├── runtime.rs              MODIFY  spawn stats server when [stats] enabled
│   └── stats/                  NEW
│       ├── mod.rs              types: Hello, RuleMeta, TargetMeta, Snapshot, RuleSnap, ErrorSnap, ProcessSnap
│       ├── server.rs           UnixListener + per-accept tasks; ticker → write JSON lines
│       ├── client.rs           UDS reader + VecDeque ring buffer + rate calc; --once printer
│       └── tui/
│           ├── mod.rs          event loop, render dispatch
│           ├── state.rs        AppState: selected_idx, sort, filter, paused, baseline
│           ├── format.rs       human-readable bytes/rate
│           └── render.rs       Overview/Detail/Errors render fns
├── tests/
│   ├── stats_server.rs         NEW   integration test: spawn daemon, connect UnixStream, read Hello + N Snapshots
│   └── stats_once.rs           NEW   e2e test: `stats --once` JSON shape
└── contrib/
    ├── portunus-standalone.service   MODIFY  add RuntimeDirectory=portunus
    ├── Dockerfile                    MODIFY  pre-create /run/portunus/ chown 65532:65532
    └── portunus.example.toml         MODIFY  commented [stats] block

docs/content/docs/operations/standalone.mdx     MODIFY  add "Live stats dashboard" section
docs/content/docs/zh/operations/standalone.mdx  MODIFY  ditto, zh
CHANGELOG.md                                    MODIFY  v1.6.0 entry
```

---

## Task 1: Add `connections_total` field to `RuleStats`

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/stats.rs`
- Test: `crates/portunus-forwarder/src/forwarder/stats.rs` (same file, `#[cfg(test)]` module at bottom)

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block (search for `mod tests` near the bottom of the file):

```rust
#[test]
fn connections_total_starts_at_zero_and_increments() {
    let s = RuleStats::new();
    assert_eq!(s.connections_total.load(Ordering::Relaxed), 0);
    s.inc_connection();
    s.inc_connection();
    assert_eq!(s.connections_total.load(Ordering::Relaxed), 2);
}
```

- [ ] **Step 2: Run the test to verify it fails**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::stats::tests::connections_total_starts_at_zero_and_increments
```

Expected: FAIL with `connections_total` / `inc_connection` not defined.

- [ ] **Step 3: Add the field + helper**

In the `pub struct RuleStats` definition (around line 101), add after `active_connections`:

```rust
    /// 015-standalone-stats-tui: monotonic count of accepted TCP
    /// connections per rule. UDP rules carry the field but never
    /// increment it (use `datagrams_in` / `flows_active` instead).
    pub connections_total: AtomicU64,
```

In `RuleStats::for_range` (search for `for_range` impl, ~line 166), add:

```rust
            connections_total: AtomicU64::new(0),
```

inside the `Arc::new(Self { ... })` literal (alphabetical-ish ordering is fine; keep near `active_connections`).

Do the same in any other `Self { ... }` literal in the file that constructs `RuleStats` (search for `bytes_in: AtomicU64::new(0)` and add `connections_total` alongside it).

Add the helper method to `impl RuleStats` (search for an existing `pub fn inc_` method for a good spot):

```rust
    /// 015-standalone-stats-tui: bump on each accepted TCP connection.
    /// TCP-only call sites; UDP listener path does not invoke this.
    pub fn inc_connection(&self) {
        self.connections_total.fetch_add(1, Ordering::Relaxed);
    }
```

- [ ] **Step 4: Verify the test passes**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::stats::tests::connections_total_starts_at_zero_and_increments
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder
```

Expected: test passes; full build succeeds (if not, the `Self { ... }` literal step above missed a constructor — `grep -n 'bytes_in: AtomicU64::new' crates/portunus-forwarder/src/forwarder/stats.rs` to find them).

- [ ] **Step 5: Commit**

```sh
git add crates/portunus-forwarder/src/forwarder/stats.rs
git commit -m "feat(forwarder): add RuleStats.connections_total monotonic counter"
```

---

## Task 2: Wire `connections_total` at TCP accept sites

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/mod.rs`
- Modify: `crates/portunus-forwarder/src/forwarder/failover_path.rs`
- Test: `crates/portunus-forwarder/tests/` or existing integration tests

- [ ] **Step 1: Locate TCP accept sites**

```sh
grep -n "TcpListener\|accept()\|listener.accept" crates/portunus-forwarder/src/forwarder/mod.rs crates/portunus-forwarder/src/forwarder/failover_path.rs | head -20
```

Identify the **single-target accept** site in `forwarder/mod.rs` (look for the `loop { match listener.accept().await ... }` pattern in the spawned TCP rule task) and the **failover accept** site in `failover_path.rs` (same pattern under failover supervision).

- [ ] **Step 2: Write the failing integration test**

Append to `crates/portunus-forwarder/tests/` — pick an existing integration test file that already spawns a TCP forwarder (likely `crates/portunus-forwarder/tests/` is empty; check first). If empty, add to whatever crate-internal test you can find that does an end-to-end TCP forward (look in `crates/portunus-standalone/tests/smoke.rs`):

If no suitable test exists, add to `crates/portunus-standalone/tests/smoke.rs` after the existing TCP smoke test:

```rust
#[test]
fn tcp_accept_increments_connections_total() {
    // Use the same helpers as the smoke test above to spawn a
    // single-target TCP rule, then assert that after one client
    // connect, RuleStats.connections_total == 1.
    //
    // The standalone smoke test framework exposes RuleStats via
    // the spawned rule's StatsRegistry — see the existing
    // `tcp_forwards_one_byte` for the pattern.
    // ... (mirror the existing test, then read connections_total)
}
```

If the smoke test framework doesn't expose `RuleStats`, **skip this integration test** and rely on Task 1's unit test plus the e2e flow in Task 22.

- [ ] **Step 3: Add the increment in `forwarder/mod.rs`**

Find the existing single-target TCP `accept()` block in `forwarder/mod.rs`. After a successful `accept().await` (which yields `(stream, peer_addr)`), insert:

```rust
        rule_stats.inc_connection();
```

The `rule_stats: Arc<RuleStats>` is already in scope in that task (it's the same handle the existing `bytes_in` updates reference). If the variable is named differently in your specific function, use the local name — grep `RuleStats` in the function body to confirm.

- [ ] **Step 4: Add the increment in `failover_path.rs`**

Same idea. In `failover_path.rs`, find the per-accept block (search for `accept` near line 152 or 290 per the spec). After successful accept, add:

```rust
        rule_stats.inc_connection();
```

Make sure `rule_stats` is in scope; otherwise plumb the `Arc<RuleStats>` clone into the accept loop the same way `target_failovers_total` is already plumbed.

- [ ] **Step 5: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace --lib
```

Expected: build succeeds, existing tests pass.

- [ ] **Step 6: Commit**

```sh
git add crates/portunus-forwarder/src/forwarder/mod.rs crates/portunus-forwarder/src/forwarder/failover_path.rs
git commit -m "feat(forwarder): bump connections_total on each TCP accept"
```

---

## Task 3: Add `ErrorCounters` struct and embed in `RuleStats`

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/stats.rs`

- [ ] **Step 1: Write the failing test**

Append to the existing `#[cfg(test)] mod tests` block in `stats.rs`:

```rust
#[test]
fn error_counters_default_zero_and_bump() {
    let s = RuleStats::new();
    assert_eq!(s.errors.port_in_use.load(Ordering::Relaxed), 0);
    assert_eq!(s.errors.upstream_connect_failed.load(Ordering::Relaxed), 0);
    s.errors.inc_port_in_use();
    s.errors.inc_upstream_connect_failed();
    s.errors.inc_upstream_connect_failed();
    assert_eq!(s.errors.port_in_use.load(Ordering::Relaxed), 1);
    assert_eq!(s.errors.upstream_connect_failed.load(Ordering::Relaxed), 2);
}
```

- [ ] **Step 2: Run the test to verify it fails**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::stats::tests::error_counters_default_zero_and_bump
```

Expected: FAIL.

- [ ] **Step 3: Define `ErrorCounters`**

In `stats.rs`, add the struct before `pub struct RuleStats`:

```rust
/// 015-standalone-stats-tui: per-rule failure event counters.
/// Each `AtomicU64` is bumped from the existing `tracing::warn!` /
/// `tracing::info!` call site for the matching event name. Counters
/// are cumulative since the rule was activated.
#[derive(Debug, Default)]
pub struct ErrorCounters {
    /// `rule.failed` (port_in_use) — TCP bind failure.
    pub port_in_use: AtomicU64,
    /// `rule.udp_upstream_connect_failed` — connect(2) on a UDP
    /// upstream socket failed before the flow was installed.
    pub upstream_connect_failed: AtomicU64,
    /// `rule.udp_flow_evicted_icmp` — kernel returned an ICMP
    /// error on the connected upstream socket; flow evicted.
    pub icmp_evict: AtomicU64,
    /// `rule.udp_emsgsize` — datagram too large for the path MTU.
    pub emsgsize: AtomicU64,
    /// `rule.udp_reply_wouldblock` — reply send_to returned
    /// WouldBlock; datagram dropped.
    pub wouldblock: AtomicU64,
    /// `rule.udp_addflow_dropped` — new-flow datagram dropped
    /// because the per-rule flow table is at capacity.
    pub addflow_dropped: AtomicU64,
}

impl ErrorCounters {
    pub fn inc_port_in_use(&self) {
        self.port_in_use.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_upstream_connect_failed(&self) {
        self.upstream_connect_failed.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_icmp_evict(&self) {
        self.icmp_evict.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_emsgsize(&self) {
        self.emsgsize.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_wouldblock(&self) {
        self.wouldblock.fetch_add(1, Ordering::Relaxed);
    }
    pub fn inc_addflow_dropped(&self) {
        self.addflow_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot all six counters at once.
    #[must_use]
    pub fn snapshot(&self) -> ErrorSnapshot {
        ErrorSnapshot {
            port_in_use: self.port_in_use.load(Ordering::Relaxed),
            upstream_connect_failed: self.upstream_connect_failed.load(Ordering::Relaxed),
            icmp_evict: self.icmp_evict.load(Ordering::Relaxed),
            emsgsize: self.emsgsize.load(Ordering::Relaxed),
            wouldblock: self.wouldblock.load(Ordering::Relaxed),
            addflow_dropped: self.addflow_dropped.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ErrorSnapshot {
    pub port_in_use: u64,
    pub upstream_connect_failed: u64,
    pub icmp_evict: u64,
    pub emsgsize: u64,
    pub wouldblock: u64,
    pub addflow_dropped: u64,
}
```

Then add to `pub struct RuleStats`, near `connections_total`:

```rust
    /// 015-standalone-stats-tui: failure-event counters paired with
    /// existing tracing call sites.
    pub errors: ErrorCounters,
```

And in every `Self { ... }` constructor literal:

```rust
            errors: ErrorCounters::default(),
```

- [ ] **Step 4: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-forwarder --lib forwarder::stats::tests::error_counters_default_zero_and_bump
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder
```

Expected: test passes, build succeeds.

- [ ] **Step 5: Commit**

```sh
git add crates/portunus-forwarder/src/forwarder/stats.rs
git commit -m "feat(forwarder): add ErrorCounters embedded in RuleStats"
```

---

## Task 4: Wire `port_in_use` counter at `rule.failed` sites

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/mod.rs`
- Modify: `crates/portunus-forwarder/src/forwarder/failover_path.rs`

- [ ] **Step 1: Find every `rule.failed` emission**

```sh
grep -n 'event = "rule.failed"' crates/portunus-forwarder/src/forwarder/mod.rs crates/portunus-forwarder/src/forwarder/failover_path.rs
```

Expected output (line numbers may drift):
```
crates/portunus-forwarder/src/forwarder/mod.rs:214
crates/portunus-forwarder/src/forwarder/mod.rs:622
crates/portunus-forwarder/src/forwarder/failover_path.rs:70
crates/portunus-forwarder/src/forwarder/failover_path.rs:662
```

- [ ] **Step 2: Add `rule_stats.errors.inc_port_in_use()` immediately before each tracing call**

For **each** of the four sites above, locate the `tracing::warn!` (or `info!`) block containing `event = "rule.failed"` and the surrounding `reason = "port_in_use"`. Immediately **before** the `tracing::` macro call, insert:

```rust
            rule_stats.errors.inc_port_in_use();
```

(Use the locally-scoped `RuleStats` `Arc` name — `rule_stats` in the standard sites; if a different name is used in scope, adapt. Search the local function for `RuleStats` to confirm the binding.)

If the `Arc<RuleStats>` is not in scope at one of the sites, plumb it through — the failover supervisor already passes `rule_stats` to its accept loop, see `failover_path.rs:120`.

- [ ] **Step 3: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace
```

Expected: build + test green.

- [ ] **Step 4: Commit**

```sh
git add crates/portunus-forwarder/src/forwarder/mod.rs crates/portunus-forwarder/src/forwarder/failover_path.rs
git commit -m "feat(forwarder): bump port_in_use counter at all rule.failed sites"
```

---

## Task 5: Wire UDP error counters at every listener site

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/udp/listener.rs`

- [ ] **Step 1: Find every UDP error event in listener**

```sh
grep -n 'event = "rule\.udp_' crates/portunus-forwarder/src/forwarder/udp/listener.rs
```

Expected sites:
- `rule.udp_flow_evicted_icmp` at lines ~332, ~387, ~473, ~705
- `rule.udp_emsgsize` at lines ~344, ~400, ~484
- `rule.udp_upstream_connect_failed` at line ~626
- `rule.udp_addflow_dropped` at line ~669

- [ ] **Step 2: At each site, add the matching counter increment immediately before the tracing call**

For ICMP evict sites (4 in this file):
```rust
                                rule_stats.errors.inc_icmp_evict();
```

For emsgsize sites (3 in this file):
```rust
                                rule_stats.errors.inc_emsgsize();
```

For the one connect_failed site:
```rust
                    rule_stats.errors.inc_upstream_connect_failed();
```

For the one addflow_dropped site:
```rust
            rule_stats.errors.inc_addflow_dropped();
```

(Match local indentation; if the local handle name isn't `rule_stats`, adapt — grep `RuleStats` in the function to confirm.)

- [ ] **Step 3: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace
```

- [ ] **Step 4: Commit**

```sh
git add crates/portunus-forwarder/src/forwarder/udp/listener.rs
git commit -m "feat(forwarder): bump UDP error counters in listener path"
```

---

## Task 6: Wire UDP error counters at every demux site

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/udp/demux.rs`

- [ ] **Step 1: Find sites**

```sh
grep -n 'event = "rule\.udp_' crates/portunus-forwarder/src/forwarder/udp/demux.rs
```

Expected:
- `rule.udp_reply_wouldblock` at line ~113
- `rule.udp_flow_evicted_icmp` at line ~135
- `rule.udp_emsgsize` at line ~146

- [ ] **Step 2: Add increments**

At the wouldblock site:
```rust
                            rule_stats.errors.inc_wouldblock();
```

At the icmp_evict site:
```rust
                        rule_stats.errors.inc_icmp_evict();
```

At the emsgsize site:
```rust
                        rule_stats.errors.inc_emsgsize();
```

(Confirm `rule_stats` binding name in this file — grep for `RuleStats`.)

- [ ] **Step 3: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace
```

- [ ] **Step 4: Commit**

```sh
git add crates/portunus-forwarder/src/forwarder/udp/demux.rs
git commit -m "feat(forwarder): bump UDP error counters in demux path"
```

---

## Task 7: Migrate `target_failovers_total` ownership to `RuleStats`

**Files:**
- Modify: `crates/portunus-forwarder/src/forwarder/stats.rs`
- Modify: `crates/portunus-forwarder/src/forwarder/failover_path.rs`

- [ ] **Step 1: Add the field to `RuleStats`**

In `stats.rs`, in `pub struct RuleStats`, add (near `errors`):

```rust
    /// 015-standalone-stats-tui: aggregate count of failover events
    /// across the rule's target list. Always present; for
    /// single-target rules the counter stays 0. The `Arc` is shared
    /// with the failover supervisor in `failover_path.rs` so both
    /// the failover hot path and the stats server observe one value.
    pub target_failovers_total: Arc<AtomicU64>,
```

(Import already present: `use std::sync::Arc;` near the top.)

In every `Self { ... }` constructor literal:

```rust
            target_failovers_total: Arc::new(AtomicU64::new(0)),
```

- [ ] **Step 2: Failover constructor reuses the existing `Arc`**

In `failover_path.rs`, find where `target_failovers_total: Arc<AtomicU64>` is currently allocated (the constructor that owns the failover state; search for `target_failovers_total = Arc::clone` or `target_failovers_total: Arc::new(AtomicU64::new(0))`). Replace any **fresh allocation** with `Arc::clone(&rule_stats.target_failovers_total)`.

Concretely: search `failover_path.rs` for `Arc::new(AtomicU64::new(0))` near the existing `target_failovers_total` plumbing (line ~215 per spec). If a fresh `Arc` is being created for the counter, replace with:

```rust
        let target_failovers_total = Arc::clone(&rule_stats.target_failovers_total);
```

Existing `Arc::clone(&target_failovers_total)` calls deeper in the file (for spawning per-task counters at lines ~120, ~136, ~152) stay as-is — they just clone whichever `Arc` is in scope.

- [ ] **Step 3: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-forwarder
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace
```

Expected: green. If any test references `target_failovers_total` as an owned field on the failover state, update it to read via `rule_stats.target_failovers_total`.

- [ ] **Step 4: Commit**

```sh
git add crates/portunus-forwarder/src/forwarder/stats.rs crates/portunus-forwarder/src/forwarder/failover_path.rs
git commit -m "refactor(forwarder): own target_failovers_total Arc on RuleStats"
```

---

## Task 8: Add `stats-tui` Cargo feature + dependencies

**Files:**
- Modify: `crates/portunus-standalone/Cargo.toml`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Add `ratatui` / `crossterm` to workspace deps**

In the root `Cargo.toml`, under `[workspace.dependencies]`, append:

```toml
ratatui   = { version = "0.29", default-features = false, features = ["crossterm"] }
crossterm = { version = "0.28", default-features = false, features = ["events"] }
```

(Use the latest stable line if newer; `0.29` is current as of plan date. Pin major.minor in workspace, let crates inherit.)

- [ ] **Step 2: Update standalone crate Cargo.toml**

In `crates/portunus-standalone/Cargo.toml`, under `[dependencies]`:

```toml
serde_json         = { workspace = true }
ratatui            = { workspace = true, optional = true }
crossterm          = { workspace = true, optional = true }
```

Add a `[features]` section (place between `[lints]` and `[[bin]]`):

```toml
[features]
default   = ["stats-tui"]
stats-tui = ["dep:ratatui", "dep:crossterm"]
```

- [ ] **Step 3: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone --no-default-features
```

Expected: both build (no code uses ratatui yet, so neither variant pulls it in regardless).

- [ ] **Step 4: Commit**

```sh
git add Cargo.toml crates/portunus-standalone/Cargo.toml
git commit -m "build(standalone): add stats-tui feature + ratatui/crossterm deps"
```

---

## Task 9: Define UDS protocol types (`stats::mod`)

**Files:**
- Create: `crates/portunus-standalone/src/stats/mod.rs`
- Modify: `crates/portunus-standalone/src/main.rs` (declare `mod stats;`)

- [ ] **Step 1: Write the failing test**

Create `crates/portunus-standalone/src/stats/mod.rs` with this content (test inline):

```rust
//! UDS stats protocol types. Shared by the daemon-side server
//! (`stats::server`) and the client (`stats::client`).
//!
//! Wire format: JSON-lines (one document per `\n`-terminated line)
//! over a Unix domain socket. The server sends exactly one `Hello`
//! immediately after `accept()`, then a `Snapshot` every
//! `refresh_ms`.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub v: u32,
    pub daemon_version: String,
    pub daemon_started_at_ms: u64,
    pub refresh_ms: u64,
    pub rules: Vec<RuleMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleMeta {
    pub id: String,
    pub name: String,
    pub proto: String, // "tcp" | "udp"
    pub listen: String,
    pub targets: Vec<TargetMeta>,
    pub splice_capable: bool,
    pub udp_max_flows: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetMeta {
    pub host: String,
    pub port: u16,
    pub priority: u32,
    pub proxy_protocol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub t_ms: u64,
    pub uptime_ms: u64,
    pub seq: u64,
    pub process: ProcessSnap,
    pub r: Vec<RuleSnap>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ProcessSnap {
    pub fd_open: Option<u32>,
    pub fd_limit: Option<u64>,
    pub rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSnap {
    pub id: String,
    #[serde(rename = "in")]
    pub bytes_in: u64,
    pub out: u64,
    pub conns_active: u32,
    pub conns_total: u64,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub flows_active: u32,
    pub target_failovers_total: u64,
    pub err: ErrorSnap,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ErrorSnap {
    pub port_in_use: u64,
    pub upstream_connect_failed: u64,
    pub icmp_evict: u64,
    pub emsgsize: u64,
    pub wouldblock: u64,
    pub addflow_dropped: u64,
    pub dns_failures: u64,
    pub flows_dropped_overflow: u64,
}

pub mod server;
pub mod client;
#[cfg(feature = "stats-tui")]
pub mod tui;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_serde_roundtrip() {
        let snap = Snapshot {
            t_ms: 1748400015234,
            uptime_ms: 304_523,
            seq: 42,
            process: ProcessSnap {
                fd_open: Some(217),
                fd_limit: Some(65535),
                rss_bytes: Some(18_874_368),
            },
            r: vec![RuleSnap {
                id: "118".into(),
                bytes_in: 1024,
                out: 2048,
                conns_active: 3,
                conns_total: 124,
                datagrams_in: 0,
                datagrams_out: 0,
                flows_active: 0,
                target_failovers_total: 0,
                err: ErrorSnap::default(),
            }],
        };
        let s = serde_json::to_string(&snap).unwrap();
        let back: Snapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(back.t_ms, snap.t_ms);
        assert_eq!(back.r.len(), 1);
        assert_eq!(back.r[0].bytes_in, 1024);
        // ensure the JSON uses the short field name "in"
        assert!(s.contains("\"in\":1024"));
    }

    #[test]
    fn hello_serde_roundtrip() {
        let h = Hello {
            v: PROTOCOL_VERSION,
            daemon_version: "1.6.0".into(),
            daemon_started_at_ms: 1748400000000,
            refresh_ms: 1000,
            rules: vec![RuleMeta {
                id: "abc".into(),
                name: "ssh".into(),
                proto: "tcp".into(),
                listen: "2222".into(),
                targets: vec![TargetMeta {
                    host: "10.0.0.5".into(),
                    port: 22,
                    priority: 0,
                    proxy_protocol: None,
                }],
                splice_capable: true,
                udp_max_flows: None,
            }],
        };
        let s = serde_json::to_string(&h).unwrap();
        let back: Hello = serde_json::from_str(&s).unwrap();
        assert_eq!(back.v, 1);
        assert_eq!(back.rules[0].targets[0].host, "10.0.0.5");
    }
}
```

Also create empty stubs so the module declarations compile:

`crates/portunus-standalone/src/stats/server.rs`:
```rust
//! UDS stats server — Task 11 implements this.
```

`crates/portunus-standalone/src/stats/client.rs`:
```rust
//! UDS stats client — Task 13 implements this.
```

Add `mod stats;` to `crates/portunus-standalone/src/main.rs` near the other `mod` declarations at the top:

```rust
mod stats;
```

- [ ] **Step 2: Run the tests to verify they pass**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --lib stats::tests
```

Wait — the binary crate has no `lib` target. The `--lib` flag will error. Instead:

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --bin portunus-standalone stats::tests
```

Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```sh
git add crates/portunus-standalone/src/stats/ crates/portunus-standalone/src/main.rs
git commit -m "feat(standalone): define UDS stats protocol types"
```

---

## Task 10: Add `[stats]` config section with platform-aware default

**Files:**
- Modify: `crates/portunus-standalone/src/config.rs`

- [ ] **Step 1: Write the failing test**

Append to `#[cfg(test)] mod tests` in `config.rs` (or wherever existing config tests live — search `mod tests` near the bottom of the file):

```rust
#[test]
fn stats_default_enabled_with_platform_path() {
    let toml = r#"
[[rule]]
name = "x"
protocol = "tcp"
listen_port = 1
target = "1.1.1.1:1"
"#;
    let cfg: Config = toml::from_str(toml).unwrap();
    assert!(cfg.stats.enabled);
    assert_eq!(cfg.stats.refresh_ms, 1000);
    #[cfg(target_os = "linux")]
    assert_eq!(
        cfg.stats.socket_path.as_os_str(),
        std::ffi::OsStr::new("/run/portunus/standalone.sock"),
    );
    #[cfg(target_os = "macos")]
    {
        let p = cfg.stats.socket_path.display().to_string();
        assert!(p.ends_with("portunus-standalone.sock"));
    }
}

#[test]
fn stats_refresh_ms_validation() {
    let toml = r#"
[stats]
refresh_ms = 100  # too low
[[rule]]
name = "x"
protocol = "tcp"
listen_port = 1
target = "1.1.1.1:1"
"#;
    let cfg: Result<Config, _> = toml::from_str(toml);
    // Either toml fails to parse or our validator rejects it.
    if let Ok(c) = cfg {
        assert!(c.validate().is_err(),
                "refresh_ms=100 must be rejected by validate()");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --bin portunus-standalone config::tests::stats
```

Expected: FAIL.

- [ ] **Step 3: Add `StatsConfig` to `config.rs`**

Near the top of `config.rs`, add `use std::path::PathBuf;` if not already present.

Add the struct:

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatsConfig {
    #[serde(default = "default_stats_enabled")]
    pub enabled: bool,
    #[serde(default = "default_stats_socket_path")]
    pub socket_path: PathBuf,
    #[serde(default = "default_stats_refresh_ms")]
    pub refresh_ms: u64,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            enabled: default_stats_enabled(),
            socket_path: default_stats_socket_path(),
            refresh_ms: default_stats_refresh_ms(),
        }
    }
}

fn default_stats_enabled() -> bool {
    true
}

fn default_stats_socket_path() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/run/portunus/standalone.sock")
    }
    #[cfg(target_os = "macos")]
    {
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        base.join("portunus-standalone.sock")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        PathBuf::from("portunus-standalone.sock")
    }
}

fn default_stats_refresh_ms() -> u64 {
    1000
}
```

Add the field to the top-level `Config` (find `pub struct Config`):

```rust
    #[serde(default)]
    pub stats: StatsConfig,
```

In the `Config::validate(&self)` method (search for `pub fn validate`), add at the end (before `Ok(())`):

```rust
        if self.stats.enabled
            && !(250..=5000).contains(&self.stats.refresh_ms)
        {
            return Err(ConfigError::Validation {
                msg: format!(
                    "[stats] refresh_ms must be in 250..=5000 (got {})",
                    self.stats.refresh_ms
                ),
            });
        }
```

If `ConfigError::Validation { msg }` is not an existing variant, use whatever generic variant already exists in `ConfigError` for "bad value" — search the file's `#[derive(Error)] enum ConfigError` for an existing variant that takes a message.

- [ ] **Step 4: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --bin portunus-standalone config::tests::stats
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone
```

Expected: tests pass.

- [ ] **Step 5: Commit**

```sh
git add crates/portunus-standalone/src/config.rs
git commit -m "feat(standalone): add [stats] config section with platform-aware default"
```

---

## Task 11: Implement UDS stats server

**Files:**
- Modify: `crates/portunus-standalone/src/stats/server.rs`
- Test: `crates/portunus-standalone/tests/stats_server.rs` (new)

- [ ] **Step 1: Write the failing integration test**

Create `crates/portunus-standalone/tests/stats_server.rs`:

```rust
//! Integration test for the UDS stats server. Spawns a server,
//! connects via UnixStream, reads Hello and one Snapshot, then
//! disconnects.

use std::sync::Arc;
use std::time::Duration;

use portunus_core::{PortRange, RuleId};
use portunus_forwarder::RuleStats;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn server_emits_hello_then_snapshots() {
    use portunus_standalone_test_support as _; // see note below
}
```

The test above is a stub — the standalone crate is a binary, not a library, so we can't directly call its internals from a `tests/` integration test. We have two options:

**Option A (preferred):** Make `crates/portunus-standalone` build a library target alongside the binary. Add to its `Cargo.toml`:

```toml
[lib]
name = "portunus_standalone"
path = "src/lib.rs"
```

Create `crates/portunus-standalone/src/lib.rs` mirroring `main.rs`'s `mod` decls but `pub`:

```rust
pub mod config;
pub mod reporter;
pub mod runtime;
pub mod signal;
pub mod stats;
```

In `main.rs`, replace the `mod foo;` lines with `use portunus_standalone::{config, ...};` as needed. (The binary keeps its `fn main()`.)

**Option B:** Make `stats::server` testable via doctests or in-module `#[cfg(test)]` async tests.

Pick **Option A** — it unlocks better testability for the rest of this plan.

After making the lib target, rewrite the test:

```rust
use std::sync::Arc;
use std::time::Duration;

use portunus_core::{PortRange, RuleId};
use portunus_forwarder::RuleStats;
use portunus_standalone::stats::{server, Hello, Snapshot};
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn server_emits_hello_then_snapshots() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("test.sock");

    // Construct one fake rule with stats.
    let rule_id = RuleId::from_u64(1);
    let stats = RuleStats::for_range(PortRange::single(2222));

    let registry: server::Registry = Arc::new(parking_lot::RwLock::new(
        std::collections::HashMap::from([
            (rule_id, server::RuleEntry {
                stats: Arc::clone(&stats),
                meta: server::RuleMetaStatic {
                    name: "test-rule".into(),
                    proto: "tcp".into(),
                    listen: "2222".into(),
                    targets: vec![server::TargetMetaStatic {
                        host: "1.1.1.1".into(),
                        port: 22,
                        priority: 0,
                        proxy_protocol: None,
                    }],
                    splice_capable: true,
                    udp_max_flows: None,
                },
            }),
        ]),
    ));

    let cancel = CancellationToken::new();
    let started_at_ms = 12345;
    let handle = server::spawn(
        sock.clone(),
        Arc::clone(&registry),
        Duration::from_millis(250),
        started_at_ms,
        cancel.clone(),
    ).await.unwrap();

    // Connect from the client side.
    let stream = UnixStream::connect(&sock).await.unwrap();
    let mut reader = BufReader::new(stream).lines();

    let line = tokio::time::timeout(Duration::from_secs(2), reader.next_line())
        .await.unwrap().unwrap().expect("hello line");
    let hello: Hello = serde_json::from_str(&line).unwrap();
    assert_eq!(hello.v, 1);
    assert_eq!(hello.rules.len(), 1);
    assert_eq!(hello.rules[0].name, "test-rule");

    // Bump some counters and confirm they appear in a snapshot.
    stats.bytes_in.fetch_add(100, std::sync::atomic::Ordering::Relaxed);

    let line = tokio::time::timeout(Duration::from_secs(2), reader.next_line())
        .await.unwrap().unwrap().expect("snapshot line");
    let snap: Snapshot = serde_json::from_str(&line).unwrap();
    assert_eq!(snap.r.len(), 1);
    assert_eq!(snap.r[0].bytes_in, 100);

    cancel.cancel();
    let _ = handle.await;
}
```

Note: this test references `parking_lot::RwLock`. If the workspace doesn't already use `parking_lot`, use `std::sync::RwLock` and unwrap the lock in the server. Check first:

```sh
grep -n 'parking_lot' Cargo.toml
```

Use the standard `RwLock` if `parking_lot` is not already in workspace.

- [ ] **Step 2: Run to verify it fails**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --test stats_server
```

Expected: FAIL (`server::spawn` does not exist).

- [ ] **Step 3: Implement the server**

Replace `crates/portunus-standalone/src/stats/server.rs` content:

```rust
//! UDS stats server. Listens on a Unix-domain socket; per accepted
//! connection, sends a Hello immediately then a Snapshot every
//! `refresh_ms` until the client disconnects or the daemon cancels.
//!
//! Daemon-side state is just the read-only counters held in
//! `RuleStats`; the server never mutates them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use portunus_core::RuleId;
use portunus_forwarder::RuleStats;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;
use tokio::time::{interval, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use super::{
    ErrorSnap, Hello, ProcessSnap, PROTOCOL_VERSION, RuleMeta, RuleSnap, Snapshot,
    TargetMeta,
};

pub type Registry = Arc<RwLock<HashMap<RuleId, RuleEntry>>>;

#[derive(Debug, Clone)]
pub struct RuleEntry {
    pub stats: Arc<RuleStats>,
    pub meta: RuleMetaStatic,
}

#[derive(Debug, Clone)]
pub struct RuleMetaStatic {
    pub name: String,
    pub proto: String,
    pub listen: String,
    pub targets: Vec<TargetMetaStatic>,
    pub splice_capable: bool,
    pub udp_max_flows: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct TargetMetaStatic {
    pub host: String,
    pub port: u16,
    pub priority: u32,
    pub proxy_protocol: Option<String>,
}

/// Spawn the UDS server. The returned `JoinHandle` resolves when
/// the supervisor task exits (cancel or hard error).
///
/// # Errors
/// Returns `std::io::Error` if the parent dir cannot be created or
/// the socket cannot be bound.
pub async fn spawn(
    socket_path: PathBuf,
    registry: Registry,
    refresh: Duration,
    daemon_started_at_ms: u64,
    cancel: CancellationToken,
) -> std::io::Result<JoinHandle<()>> {
    // Ensure parent dir exists.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Clean up any stale socket file.
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    // 0660 — owner + group rw, world none.
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o660);
    let _ = std::fs::set_permissions(&socket_path, perms);

    tracing::info!(
        event = "standalone.stats_socket_listening",
        path = %socket_path.display(),
        refresh_ms = refresh.as_millis() as u64,
    );

    let start_instant = Instant::now();

    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    let _ = std::fs::remove_file(&socket_path);
                    break;
                }
                accept = listener.accept() => {
                    let (stream, _addr) = match accept {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                event = "standalone.stats_accept_error",
                                error = %e
                            );
                            continue;
                        }
                    };
                    let registry = Arc::clone(&registry);
                    let cancel = cancel.clone();
                    tokio::spawn(handle_client(
                        stream,
                        registry,
                        refresh,
                        daemon_started_at_ms,
                        start_instant,
                        cancel,
                    ));
                }
            }
        }
    }))
}

async fn handle_client(
    mut stream: UnixStream,
    registry: Registry,
    refresh: Duration,
    daemon_started_at_ms: u64,
    start_instant: Instant,
    cancel: CancellationToken,
) {
    // Send Hello.
    let hello = build_hello(&registry, daemon_started_at_ms, refresh);
    if let Err(e) = write_line(&mut stream, &hello).await {
        tracing::debug!(event = "standalone.stats_client_write_failed",
                        stage = "hello", error = %e);
        return;
    }

    let mut tick = interval(refresh);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut seq: u64 = 0;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = tick.tick() => {
                seq = seq.wrapping_add(1);
                let snap = build_snapshot(&registry, start_instant, seq);
                if let Err(e) = write_line(&mut stream, &snap).await {
                    tracing::debug!(
                        event = "standalone.stats_client_disconnected",
                        error = %e
                    );
                    break;
                }
            }
        }
    }
}

async fn write_line<T: serde::Serialize>(
    stream: &mut UnixStream,
    value: &T,
) -> std::io::Result<()> {
    let mut buf = serde_json::to_vec(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    buf.push(b'\n');
    stream.write_all(&buf).await?;
    stream.flush().await
}

fn build_hello(
    registry: &Registry,
    daemon_started_at_ms: u64,
    refresh: Duration,
) -> Hello {
    let g = registry.read().unwrap();
    let rules = g.iter().map(|(id, entry)| RuleMeta {
        id: id.to_string(),
        name: entry.meta.name.clone(),
        proto: entry.meta.proto.clone(),
        listen: entry.meta.listen.clone(),
        targets: entry.meta.targets.iter().map(|t| TargetMeta {
            host: t.host.clone(),
            port: t.port,
            priority: t.priority,
            proxy_protocol: t.proxy_protocol.clone(),
        }).collect(),
        splice_capable: entry.meta.splice_capable,
        udp_max_flows: entry.meta.udp_max_flows,
    }).collect();
    Hello {
        v: PROTOCOL_VERSION,
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        daemon_started_at_ms,
        refresh_ms: refresh.as_millis() as u64,
        rules,
    }
}

fn build_snapshot(
    registry: &Registry,
    start_instant: Instant,
    seq: u64,
) -> Snapshot {
    let uptime_ms = start_instant.elapsed().as_millis() as u64;
    let t_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let process = collect_process_info();

    let g = registry.read().unwrap();
    let r = g.iter().map(|(id, entry)| {
        let s = &entry.stats;
        let err = s.errors.snapshot();
        RuleSnap {
            id: id.to_string(),
            bytes_in: s.bytes_in.load(Ordering::Relaxed),
            out: s.bytes_out.load(Ordering::Relaxed),
            conns_active: s.active_connections.load(Ordering::Relaxed),
            conns_total: s.connections_total.load(Ordering::Relaxed),
            datagrams_in: s.datagrams_in.load(Ordering::Relaxed),
            datagrams_out: s.datagrams_out.load(Ordering::Relaxed),
            flows_active: s.active_flows.load(Ordering::Relaxed),
            target_failovers_total: s.target_failovers_total.load(Ordering::Relaxed),
            err: ErrorSnap {
                port_in_use: err.port_in_use,
                upstream_connect_failed: err.upstream_connect_failed,
                icmp_evict: err.icmp_evict,
                emsgsize: err.emsgsize,
                wouldblock: err.wouldblock,
                addflow_dropped: err.addflow_dropped,
                dns_failures: s.dns_failures.load(Ordering::Relaxed),
                flows_dropped_overflow: s.flows_dropped_overflow.load(Ordering::Relaxed),
            },
        }
    }).collect();

    Snapshot { t_ms, uptime_ms, seq, process, r }
}

#[cfg(target_os = "linux")]
fn collect_process_info() -> ProcessSnap {
    let fd_open = std::fs::read_dir("/proc/self/fd").ok()
        .map(|d| d.count() as u32);
    let fd_limit = read_rlimit_nofile_soft();
    let rss_bytes = read_proc_status_rss();
    ProcessSnap { fd_open, fd_limit, rss_bytes }
}

#[cfg(not(target_os = "linux"))]
fn collect_process_info() -> ProcessSnap {
    ProcessSnap::default()
}

#[cfg(target_os = "linux")]
fn read_rlimit_nofile_soft() -> Option<u64> {
    let mut rlim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) };
    if rc == 0 { Some(rlim.rlim_cur as u64) } else { None }
}

#[cfg(target_os = "linux")]
fn read_proc_status_rss() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}
```

- [ ] **Step 4: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --test stats_server
```

Expected: PASS (`server_emits_hello_then_snapshots`).

- [ ] **Step 5: Commit**

```sh
git add crates/portunus-standalone/src/lib.rs crates/portunus-standalone/src/main.rs crates/portunus-standalone/src/stats/server.rs crates/portunus-standalone/Cargo.toml crates/portunus-standalone/tests/stats_server.rs
git commit -m "feat(standalone): UDS stats server with Hello + Snapshot stream"
```

---

## Task 12: Wire stats server into the standalone runtime

**Files:**
- Modify: `crates/portunus-standalone/src/runtime.rs`
- Modify: `crates/portunus-standalone/src/main.rs` (pass `started_at_ms` if needed)

- [ ] **Step 1: Build a Registry alongside the rule registry**

Find where `runtime.rs` constructs `rule_stats_handles: Arc<RwLock<HashMap<RuleId, Arc<RuleStats>>>>` (search `rule_stats_handles`). Replace usages of just `Arc<RuleStats>` storage with `stats::server::RuleEntry { stats, meta }`:

In `runtime.rs`, change the declaration:

```rust
    let rule_entries: stats::server::Registry =
        Arc::new(std::sync::RwLock::new(HashMap::new()));
```

(Keep the existing `rule_stats_handles` map alongside it if the reporter still references it; alternatively, refactor the reporter to read from `rule_entries.read().unwrap().iter().map(|(id, e)| (id, e.stats.clone()))`. Simpler: keep both maps in sync.)

When each rule is activated, in the same code block that inserts into `rule_stats_handles`, also build a `RuleMetaStatic` from the `EffectiveRule` (or whatever the per-rule config is named in the standalone runtime) and insert a `RuleEntry`. Mirror existing field reads: name from `effective.name`, listen formatted as `"2222"` or `"8000-8009"`, targets enumerated from `effective.target` / `effective.targets`.

Sample insertion (place next to the existing `rule_stats_handles` insert):

```rust
        let meta = stats::server::RuleMetaStatic {
            name: effective.name.clone(),
            proto: match effective.protocol {
                portunus_core::Protocol::Tcp => "tcp".into(),
                portunus_core::Protocol::Udp => "udp".into(),
            },
            listen: format_listen(&effective),       // implement below
            targets: collect_targets(&effective),    // implement below
            splice_capable: matches!(effective.protocol, portunus_core::Protocol::Tcp),
            udp_max_flows: effective.udp_max_flows,
        };
        if let Ok(mut g) = rule_entries.write() {
            g.insert(rule_id, stats::server::RuleEntry {
                stats: Arc::clone(&stats),
                meta,
            });
        }
```

Add the helper functions near the top of `runtime.rs`:

```rust
fn format_listen(rule: &EffectiveRule) -> String {
    if rule.listen_range.start == rule.listen_range.end {
        rule.listen_range.start.to_string()
    } else {
        format!("{}-{}", rule.listen_range.start, rule.listen_range.end)
    }
}

fn collect_targets(rule: &EffectiveRule) -> Vec<stats::server::TargetMetaStatic> {
    rule.targets.iter().map(|t| stats::server::TargetMetaStatic {
        host: t.host.clone(),
        port: t.port,
        priority: t.priority,
        proxy_protocol: t.proxy_protocol.as_ref().map(|p| match p {
            // adapt to actual ProxyProtocol enum variants
            _ => "v2".to_string(),
        }),
    }).collect()
}
```

If the actual fields/names of `EffectiveRule` differ, grep `EffectiveRule` in `runtime.rs` to find the real shape and adapt.

- [ ] **Step 2: Spawn the stats server if enabled**

After all rules are activated and before `tokio::select` on shutdown (find where the existing `reporter_handle` is spawned), add:

```rust
    let stats_handle = if cfg.stats.enabled {
        let daemon_started_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64).unwrap_or(0);
        let refresh = std::time::Duration::from_millis(cfg.stats.refresh_ms);
        match stats::server::spawn(
            cfg.stats.socket_path.clone(),
            Arc::clone(&rule_entries),
            refresh,
            daemon_started_at_ms,
            cancel.clone(),
        ).await {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::error!(
                    event = "standalone.stats_socket_bind_failed",
                    path = %cfg.stats.socket_path.display(),
                    error = %e,
                );
                None
            }
        }
    } else {
        None
    };
```

After the existing graceful-shutdown branch (where `reporter_handle.await` is done), also await `stats_handle`:

```rust
    if let Some(h) = stats_handle {
        let _ = h.await;
    }
```

- [ ] **Step 3: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --test stats_server
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --test smoke
```

Expected: all green.

- [ ] **Step 4: Manual smoke**

```sh
mkdir -p /tmp/portunus-test
cat > /tmp/portunus-test/portunus.toml <<'EOF'
[stats]
socket_path = "/tmp/portunus-test/stats.sock"

[[rule]]
name = "smoke"
protocol = "tcp"
listen_port = 17777
target = "127.0.0.1:7"
EOF

PORTUNUS_SKIP_WEBUI=1 cargo run -p portunus-standalone --bin portunus-standalone -- --config /tmp/portunus-test/portunus.toml &
sleep 1
ls -l /tmp/portunus-test/stats.sock
nc -U /tmp/portunus-test/stats.sock | head -3
kill %1 2>/dev/null
```

Expected: socket file exists with mode 0660; `nc -U` prints at least one JSON line starting with `{"v":1,...`.

- [ ] **Step 5: Commit**

```sh
git add crates/portunus-standalone/src/runtime.rs
git commit -m "feat(standalone): wire UDS stats server into runtime"
```

---

## Task 13: Implement UDS stats client + ring buffer

**Files:**
- Modify: `crates/portunus-standalone/src/stats/client.rs`

- [ ] **Step 1: Write the failing unit test**

Replace `crates/portunus-standalone/src/stats/client.rs` content:

```rust
//! UDS stats client + 60 s ring buffer + rate calculation.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;

use super::{Hello, RuleSnap, Snapshot};

const RING_WINDOW_MS: u64 = 60_000;

#[derive(Debug)]
pub struct Client {
    pub hello: Hello,
    pub ring: VecDeque<Snapshot>,
    pub capacity: usize,
}

impl Client {
    /// Connect, read Hello, return a Client ready to ingest snapshots.
    pub async fn connect(path: &Path) -> std::io::Result<(Self, BufReader<UnixStream>)> {
        let stream = UnixStream::connect(path).await?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        let hello: Hello = serde_json::from_str(line.trim_end())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let cap = ((RING_WINDOW_MS as f64) / (hello.refresh_ms as f64))
            .ceil() as usize + 1;
        Ok((
            Client { hello, ring: VecDeque::with_capacity(cap), capacity: cap },
            reader,
        ))
    }

    pub fn push(&mut self, snap: Snapshot) {
        self.ring.push_back(snap);
        while self.ring.len() > self.capacity {
            self.ring.pop_front();
        }
    }

    /// Bytes-per-second rate for a given rule id, derived from the
    /// most recent two snapshots using `uptime_ms` (monotonic) for dt.
    /// Returns 0 if fewer than two samples or dt is 0.
    pub fn in_rate(&self, rule_id: &str) -> u64 {
        self.field_rate(rule_id, |s| s.bytes_in)
    }
    pub fn out_rate(&self, rule_id: &str) -> u64 {
        self.field_rate(rule_id, |s| s.out)
    }

    fn field_rate(&self, rule_id: &str, f: impl Fn(&RuleSnap) -> u64) -> u64 {
        if self.ring.len() < 2 { return 0; }
        let last = self.ring.back().unwrap();
        let prev = self.ring.get(self.ring.len() - 2).unwrap();
        let dt_ms = last.uptime_ms.saturating_sub(prev.uptime_ms);
        if dt_ms == 0 { return 0; }
        let cur = last.r.iter().find(|r| r.id == rule_id).map(&f).unwrap_or(0);
        let pre = prev.r.iter().find(|r| r.id == rule_id).map(&f).unwrap_or(0);
        cur.saturating_sub(pre).saturating_mul(1000) / dt_ms
    }
}

/// One-shot mode: connect, read Hello + one Snapshot, return both as JSON.
pub async fn once(path: &Path) -> std::io::Result<String> {
    let (client, mut reader) = Client::connect(path).await?;
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let snap: Snapshot = serde_json::from_str(line.trim_end())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let out = serde_json::json!({
        "hello": client.hello,
        "snapshot": snap,
    });
    serde_json::to_string_pretty(&out)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{ErrorSnap, ProcessSnap, RuleSnap};

    fn snap(uptime_ms: u64, seq: u64, in_bytes: u64) -> Snapshot {
        Snapshot {
            t_ms: 0,
            uptime_ms,
            seq,
            process: ProcessSnap::default(),
            r: vec![RuleSnap {
                id: "x".into(),
                bytes_in: in_bytes,
                out: 0,
                conns_active: 0,
                conns_total: 0,
                datagrams_in: 0,
                datagrams_out: 0,
                flows_active: 0,
                target_failovers_total: 0,
                err: ErrorSnap::default(),
            }],
        }
    }

    #[test]
    fn ring_capacity_pins_window_at_60_seconds() {
        let hello = Hello {
            v: 1, daemon_version: "1.6.0".into(),
            daemon_started_at_ms: 0, refresh_ms: 250,
            rules: vec![],
        };
        let cap = ((60_000f64) / (hello.refresh_ms as f64)).ceil() as usize + 1;
        assert_eq!(cap, 241);
    }

    #[test]
    fn rate_uses_uptime_ms_not_wall_clock() {
        let mut c = Client {
            hello: Hello {
                v: 1, daemon_version: "x".into(),
                daemon_started_at_ms: 0, refresh_ms: 1000,
                rules: vec![],
            },
            ring: VecDeque::new(),
            capacity: 60,
        };
        c.push(snap(1000, 1, 0));
        c.push(snap(2000, 2, 10_000));
        // 10 KB over 1 s → 10_000 B/s
        assert_eq!(c.in_rate("x"), 10_000);
    }

    #[test]
    fn rate_is_zero_with_lt_two_snapshots() {
        let mut c = Client {
            hello: Hello {
                v: 1, daemon_version: "x".into(),
                daemon_started_at_ms: 0, refresh_ms: 1000,
                rules: vec![],
            },
            ring: VecDeque::new(),
            capacity: 60,
        };
        c.push(snap(1000, 1, 500));
        assert_eq!(c.in_rate("x"), 0);
    }
}
```

- [ ] **Step 2: Run unit tests**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --lib stats::client::tests
```

Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```sh
git add crates/portunus-standalone/src/stats/client.rs
git commit -m "feat(standalone): UDS stats client + ring buffer + rate calc"
```

---

## Task 14: Add `stats` subcommand dispatch in `main.rs`

**Files:**
- Modify: `crates/portunus-standalone/src/main.rs`

- [ ] **Step 1: Rewrite the `Cli` struct to support subcommands**

In `main.rs`, replace the existing `#[derive(Parser)] struct Cli` block with:

```rust
#[derive(Parser, Debug)]
#[command(
    name = "portunus-standalone",
    version,
    about = "Standalone TCP/UDP forwarder"
)]
struct Cli {
    /// Path to standalone.toml. If omitted, the loader searches
    /// $PORTUNUS_STANDALONE_CONFIG, ./portunus.toml.
    #[arg(short, long, global = true)]
    config: Option<std::path::PathBuf>,

    /// Validate config and exit (0 = valid, 2 = invalid).
    #[arg(long)]
    check: bool,

    /// Override log level.
    #[arg(long, global = true)]
    log_level: Option<String>,

    /// Override log format: "json" or "pretty".
    #[arg(long, global = true)]
    log_format: Option<String>,

    /// Disable the [stats] UDS server entirely.
    #[arg(long)]
    no_stats: bool,

    /// Override [stats] socket_path (daemon mode only).
    #[arg(long)]
    stats_socket: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Option<Subcommand>,
}

#[derive(clap::Subcommand, Debug)]
enum Subcommand {
    /// Connect to a running daemon's stats UDS and render a TUI.
    Stats {
        /// Path to the stats UDS (overrides daemon default).
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
        /// Print one snapshot as JSON and exit (no TUI).
        #[arg(long)]
        once: bool,
    },
}
```

- [ ] **Step 2: Dispatch on the subcommand**

In `fn main()`, after `let cli = Cli::parse();`, branch:

```rust
    if let Some(Subcommand::Stats { socket, once }) = &cli.command {
        return run_stats(socket.clone(), *once);
    }
```

Add the new function:

```rust
fn run_stats(socket: Option<std::path::PathBuf>, once: bool) -> ExitCode {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    rt.block_on(async move {
        let path = match socket {
            Some(p) => p,
            None => default_stats_socket_path_runtime(),
        };
        if once {
            match portunus_standalone::stats::client::once(&path).await {
                Ok(s) => {
                    println!("{s}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: stats --once: {e}");
                    ExitCode::from(2)
                }
            }
        } else {
            #[cfg(feature = "stats-tui")]
            {
                portunus_standalone::stats::tui::run(&path).await
            }
            #[cfg(not(feature = "stats-tui"))]
            {
                eprintln!(
                    "error: this build was compiled without --features stats-tui; only `stats --once` is available"
                );
                ExitCode::from(2)
            }
        }
    })
}

fn default_stats_socket_path_runtime() -> std::path::PathBuf {
    // Mirrors config::default_stats_socket_path. Duplicated because
    // the user may run `stats` without loading a config file.
    #[cfg(target_os = "linux")]
    { std::path::PathBuf::from("/run/portunus/standalone.sock") }
    #[cfg(target_os = "macos")]
    {
        let base = std::env::var_os("TMPDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
        base.join("portunus-standalone.sock")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    { std::path::PathBuf::from("portunus-standalone.sock") }
}
```

Apply `--no-stats` / `--stats-socket` to the daemon config in the no-subcommand branch — find where `cfg` is built and add:

```rust
    if cli.no_stats {
        cfg.stats.enabled = false;
    }
    if let Some(p) = cli.stats_socket {
        cfg.stats.socket_path = p;
    }
```

(`cfg` becomes `let mut cfg = ...;`.)

- [ ] **Step 3: Verify it compiles + `--help` shows subcommand**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone
target/debug/portunus-standalone --help
target/debug/portunus-standalone stats --help
```

Expected: `--help` mentions `Commands:` including `stats`. `stats --help` shows `--socket` and `--once`.

- [ ] **Step 4: Commit**

```sh
git add crates/portunus-standalone/src/main.rs
git commit -m "feat(standalone): add 'stats' subcommand with --once and TUI dispatch"
```

---

## Task 15: e2e test — `stats --once` returns valid JSON

**Files:**
- Create: `crates/portunus-standalone/tests/stats_once.rs`

- [ ] **Step 1: Write the test**

```rust
//! e2e: spawn daemon, then run `portunus-standalone stats --once`,
//! parse the returned JSON, assert shape.

use std::process::{Command, Stdio};
use std::time::Duration;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::tempdir;

#[test]
fn stats_once_prints_hello_and_snapshot() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("stats.sock");
    let config = dir.path().join("portunus.toml");
    std::fs::write(
        &config,
        format!(
            r#"
[stats]
socket_path = "{}"
refresh_ms = 250

[[rule]]
name = "smoke"
protocol = "tcp"
listen_port = 19191
target = "127.0.0.1:7"
"#,
            sock.display()
        ),
    ).unwrap();

    // Spawn daemon.
    let mut daemon = Command::cargo_bin("portunus-standalone").unwrap()
        .arg("--config").arg(&config)
        .arg("--log-level").arg("warn")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn().unwrap();

    // Wait for socket to appear (poll up to 5 s).
    let mut waited = 0;
    while !sock.exists() && waited < 50 {
        std::thread::sleep(Duration::from_millis(100));
        waited += 1;
    }
    assert!(sock.exists(), "stats socket did not appear");

    // Run `stats --once`.
    let out = Command::cargo_bin("portunus-standalone").unwrap()
        .arg("stats").arg("--socket").arg(&sock).arg("--once")
        .output().unwrap();
    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(out.status.success(), "stats --once failed: {}", String::from_utf8_lossy(&out.stderr));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)
        .expect("output must be valid JSON");
    assert_eq!(json["hello"]["v"], 1);
    assert_eq!(json["hello"]["rules"][0]["name"], "smoke");
    assert!(json["snapshot"]["uptime_ms"].as_u64().is_some());
    assert!(json["snapshot"]["r"][0]["id"].as_str().is_some());
}
```

Add `serde_json` to `[dev-dependencies]` in `crates/portunus-standalone/Cargo.toml`:

```toml
serde_json  = { workspace = true }
```

- [ ] **Step 2: Run**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --test stats_once
```

Expected: PASS.

- [ ] **Step 3: Commit**

```sh
git add crates/portunus-standalone/tests/stats_once.rs crates/portunus-standalone/Cargo.toml
git commit -m "test(standalone): e2e stats --once round-trip via temp socket"
```

---

## Task 16: TUI — format helpers + AppState

**Files:**
- Create: `crates/portunus-standalone/src/stats/tui/mod.rs`
- Create: `crates/portunus-standalone/src/stats/tui/state.rs`
- Create: `crates/portunus-standalone/src/stats/tui/format.rs`

- [ ] **Step 1: `format.rs`**

Create `crates/portunus-standalone/src/stats/tui/format.rs`:

```rust
//! Human-readable byte / rate formatters.

pub fn fmt_bytes(b: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    if b < 1024 { return format!("{b}   B"); }
    let mut f = b as f64;
    let mut i = 0;
    while f >= 1024.0 && i < UNITS.len() - 1 {
        f /= 1024.0;
        i += 1;
    }
    format!("{f:6.1} {}", UNITS[i])
}

pub fn fmt_rate(bps: u64) -> String {
    format!("{}/s", fmt_bytes(bps).trim_start())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_zero() { assert_eq!(fmt_bytes(0).trim(), "0   B"); }

    #[test]
    fn bytes_kb() {
        let s = fmt_bytes(1500);
        assert!(s.contains("KB"), "got {s}");
    }

    #[test]
    fn rate_appends_per_second() {
        let s = fmt_rate(2048);
        assert!(s.ends_with("/s"));
        assert!(s.contains("KB"));
    }
}
```

- [ ] **Step 2: `state.rs`**

Create `crates/portunus-standalone/src/stats/tui/state.rs`:

```rust
//! TUI application state. Holds selection, sort, filter, pause,
//! and an optional client-side baseline for "session reset".

use std::collections::HashMap;

use crate::stats::{RuleSnap, Snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Detail,
    Errors,
}

impl Tab {
    pub fn next(self) -> Self {
        match self {
            Tab::Overview => Tab::Detail,
            Tab::Detail => Tab::Errors,
            Tab::Errors => Tab::Overview,
        }
    }
    pub fn prev(self) -> Self {
        match self {
            Tab::Overview => Tab::Errors,
            Tab::Detail => Tab::Overview,
            Tab::Errors => Tab::Detail,
        }
    }
    pub fn index(self) -> usize {
        match self { Tab::Overview => 0, Tab::Detail => 1, Tab::Errors => 2 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    RateIn,
    TotalIn,
    Name,
    Conns,
}

impl SortKey {
    pub fn cycle(self) -> Self {
        match self {
            SortKey::RateIn  => SortKey::TotalIn,
            SortKey::TotalIn => SortKey::Name,
            SortKey::Name    => SortKey::Conns,
            SortKey::Conns   => SortKey::RateIn,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            SortKey::RateIn  => "rate",
            SortKey::TotalIn => "total",
            SortKey::Name    => "name",
            SortKey::Conns   => "conns",
        }
    }
}

#[derive(Debug)]
pub struct AppState {
    pub tab: Tab,
    pub selected: usize,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub filter: String,
    pub paused: bool,
    pub show_help: bool,
    /// Captured cumulative values for client-side "session reset".
    pub baseline: HashMap<String, BaselineEntry>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BaselineEntry {
    pub bytes_in: u64,
    pub out: u64,
    pub conns_total: u64,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            tab: Tab::Overview,
            selected: 0,
            sort: SortKey::RateIn,
            sort_desc: true,
            filter: String::new(),
            paused: false,
            show_help: false,
            baseline: HashMap::new(),
        }
    }

    pub fn reset_baseline(&mut self, snap: &Snapshot) {
        self.baseline.clear();
        for r in &snap.r {
            self.baseline.insert(r.id.clone(), BaselineEntry {
                bytes_in: r.bytes_in,
                out: r.out,
                conns_total: r.conns_total,
            });
        }
    }

    /// Display value: cumulative minus baseline (if set), saturating.
    pub fn displayed_in(&self, rule: &RuleSnap) -> u64 {
        self.baseline.get(&rule.id)
            .map(|b| rule.bytes_in.saturating_sub(b.bytes_in))
            .unwrap_or(rule.bytes_in)
    }
    pub fn displayed_out(&self, rule: &RuleSnap) -> u64 {
        self.baseline.get(&rule.id)
            .map(|b| rule.out.saturating_sub(b.out))
            .unwrap_or(rule.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{ErrorSnap, ProcessSnap};

    fn rule(id: &str, in_b: u64) -> RuleSnap {
        RuleSnap {
            id: id.into(), bytes_in: in_b, out: 0,
            conns_active: 0, conns_total: 0,
            datagrams_in: 0, datagrams_out: 0,
            flows_active: 0, target_failovers_total: 0,
            err: ErrorSnap::default(),
        }
    }

    #[test]
    fn baseline_reset_subtracts() {
        let mut s = AppState::new();
        let snap = Snapshot {
            t_ms: 0, uptime_ms: 0, seq: 0,
            process: ProcessSnap::default(),
            r: vec![rule("x", 1000)],
        };
        s.reset_baseline(&snap);
        let later = rule("x", 1500);
        assert_eq!(s.displayed_in(&later), 500);
    }

    #[test]
    fn tab_cycle() {
        assert_eq!(Tab::Overview.next(), Tab::Detail);
        assert_eq!(Tab::Errors.next(), Tab::Overview);
        assert_eq!(Tab::Overview.prev(), Tab::Errors);
    }
}
```

- [ ] **Step 3: `mod.rs` stub**

Create `crates/portunus-standalone/src/stats/tui/mod.rs`:

```rust
//! Ratatui-based TUI for `portunus-standalone stats`.

pub mod format;
pub mod render;
pub mod state;

use std::path::Path;
use std::process::ExitCode;

/// Stub — Task 18 implements the real event loop.
pub async fn run(_socket: &Path) -> ExitCode {
    eprintln!("error: TUI not yet implemented");
    ExitCode::from(2)
}
```

Create a stub for `render.rs`:

```rust
//! Render Overview/Detail/Errors tabs — Task 17 implements this.
```

- [ ] **Step 4: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --lib stats::tui --features stats-tui
```

Expected: 5 tests pass (3 from format, 2 from state).

- [ ] **Step 5: Commit**

```sh
git add crates/portunus-standalone/src/stats/tui/
git commit -m "feat(standalone/tui): app state + format helpers"
```

---

## Task 17: TUI — Overview / Detail / Errors render functions

**Files:**
- Modify: `crates/portunus-standalone/src/stats/tui/render.rs`

- [ ] **Step 1: Write the failing snapshot test**

Replace `crates/portunus-standalone/src/stats/tui/render.rs` with:

```rust
//! Render functions for the three TUI tabs.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Gauge, Paragraph, Row, Sparkline, Table, TableState, Tabs,
};

use super::format::{fmt_bytes, fmt_rate};
use super::state::{AppState, Tab};
use crate::stats::client::Client;

pub fn render(frame: &mut Frame, area: Rect, client: &Client, state: &mut AppState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(1), // tabs
            Constraint::Min(5),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    render_header(frame, layout[0], client);
    render_tabs(frame, layout[1], state.tab);
    match state.tab {
        Tab::Overview => render_overview(frame, layout[2], client, state),
        Tab::Detail   => render_detail(frame, layout[2], client, state),
        Tab::Errors   => render_errors(frame, layout[2], client, state),
    }
    render_footer(frame, layout[3], client);

    if state.show_help {
        render_help_overlay(frame, area);
    }
}

fn render_header(frame: &mut Frame, area: Rect, client: &Client) {
    let title = format!(" portunus-standalone stats — daemon v{} ",
                        client.hello.daemon_version);
    let p = Paragraph::new(title).style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_widget(p, area);
}

fn render_tabs(frame: &mut Frame, area: Rect, current: Tab) {
    let titles = vec!["Overview", "Detail", "Errors"];
    let t = Tabs::new(titles)
        .select(current.index())
        .style(Style::default())
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    frame.render_widget(t, area);
}

fn render_overview(frame: &mut Frame, area: Rect, client: &Client, state: &mut AppState) {
    let rows: Vec<Row> = client.hello.rules.iter().enumerate().map(|(_, meta)| {
        let last = client.ring.back();
        let snap_row = last.and_then(|s| s.r.iter().find(|r| r.id == meta.id));
        let in_rate = client.in_rate(&meta.id);
        let out_rate = client.out_rate(&meta.id);
        let (conns, conns_total) = snap_row
            .map(|r| (r.conns_active, r.conns_total))
            .unwrap_or((0, 0));
        Row::new(vec![
            Cell::from(meta.name.clone()),
            Cell::from(meta.proto.clone()),
            Cell::from(meta.listen.clone()),
            Cell::from(fmt_rate(in_rate)),
            Cell::from(fmt_rate(out_rate)),
            Cell::from(format!("{conns}/{conns_total}")),
        ])
    }).collect();

    let header = Row::new(vec!["name", "proto", "listen", "in rate", "out rate", "conns"])
        .style(Style::default().add_modifier(Modifier::BOLD));

    let table = Table::new(rows, [
        Constraint::Length(18),
        Constraint::Length(5),
        Constraint::Length(12),
        Constraint::Length(13),
        Constraint::Length(13),
        Constraint::Length(12),
    ])
    .header(header)
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol("▶ ");

    let mut ts = TableState::default();
    ts.select(Some(state.selected.min(client.hello.rules.len().saturating_sub(1))));
    frame.render_stateful_widget(table, area, &mut ts);
}

fn render_detail(frame: &mut Frame, area: Rect, client: &Client, state: &AppState) {
    if client.hello.rules.is_empty() {
        let p = Paragraph::new("no rules").block(Block::default().borders(Borders::ALL));
        frame.render_widget(p, area);
        return;
    }
    let idx = state.selected.min(client.hello.rules.len() - 1);
    let meta = &client.hello.rules[idx];
    let last = client.ring.back();
    let snap = last.and_then(|s| s.r.iter().find(|r| r.id == meta.id));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Detail · {} ({} {}) ",
                       meta.name, meta.proto, meta.listen));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let vchunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // in spark
            Constraint::Length(2), // out spark
            Constraint::Length(2), // gauge
            Constraint::Min(1),    // text
        ])
        .split(inner);

    let in_series: Vec<u64> = client.ring.iter()
        .filter_map(|s| s.r.iter().find(|r| r.id == meta.id))
        .map(|r| r.bytes_in)
        .collect();
    let in_rates = pairwise_rates(&in_series, &client.ring.iter().map(|s| s.uptime_ms).collect::<Vec<_>>());
    let sp_in = Sparkline::default()
        .block(Block::default().title(format!("in  {}",  fmt_rate(client.in_rate(&meta.id)))))
        .data(&in_rates);
    frame.render_widget(sp_in, vchunks[0]);

    let out_series: Vec<u64> = client.ring.iter()
        .filter_map(|s| s.r.iter().find(|r| r.id == meta.id))
        .map(|r| r.out)
        .collect();
    let out_rates = pairwise_rates(&out_series, &client.ring.iter().map(|s| s.uptime_ms).collect::<Vec<_>>());
    let sp_out = Sparkline::default()
        .block(Block::default().title(format!("out {}", fmt_rate(client.out_rate(&meta.id)))))
        .data(&out_rates);
    frame.render_widget(sp_out, vchunks[1]);

    if let (Some(s), Some(max)) = (snap, meta.udp_max_flows) {
        let ratio = (s.flows_active as f64 / max as f64).min(1.0);
        let g = Gauge::default()
            .block(Block::default().title(format!("flows {}/{max}", s.flows_active)))
            .ratio(ratio);
        frame.render_widget(g, vchunks[2]);
    } else if let Some(s) = snap {
        let p = Paragraph::new(format!("conns active {} / total {}", s.conns_active, s.conns_total));
        frame.render_widget(p, vchunks[2]);
    }

    if let Some(s) = snap {
        let lines = vec![
            Line::from(format!("total in  {}", fmt_bytes(state.displayed_in(s)))),
            Line::from(format!("total out {}", fmt_bytes(state.displayed_out(s)))),
            Line::from(format!("target_failovers_total {}", s.target_failovers_total)),
        ];
        frame.render_widget(Paragraph::new(lines), vchunks[3]);
    }
}

fn render_errors(frame: &mut Frame, area: Rect, client: &Client, _state: &AppState) {
    let last = client.ring.back();
    let mut rows: Vec<Row> = Vec::new();
    if let Some(snap) = last {
        for meta in &client.hello.rules {
            if let Some(r) = snap.r.iter().find(|r| r.id == meta.id) {
                push_err_row(&mut rows, &meta.name, "port_in_use",             r.err.port_in_use);
                push_err_row(&mut rows, &meta.name, "upstream_connect_failed", r.err.upstream_connect_failed);
                push_err_row(&mut rows, &meta.name, "icmp_evict",              r.err.icmp_evict);
                push_err_row(&mut rows, &meta.name, "emsgsize",                r.err.emsgsize);
                push_err_row(&mut rows, &meta.name, "wouldblock",              r.err.wouldblock);
                push_err_row(&mut rows, &meta.name, "addflow_dropped",         r.err.addflow_dropped);
                push_err_row(&mut rows, &meta.name, "dns_failures",            r.err.dns_failures);
                push_err_row(&mut rows, &meta.name, "flows_dropped_overflow",  r.err.flows_dropped_overflow);
            }
        }
    }
    let header = Row::new(vec!["rule", "event", "count"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let table = Table::new(rows, [
        Constraint::Length(20),
        Constraint::Length(28),
        Constraint::Length(10),
    ])
    .header(header)
    .block(Block::default().title(" Errors (cumulative) ").borders(Borders::ALL));
    frame.render_widget(table, area);
}

fn push_err_row(rows: &mut Vec<Row>, rule: &str, name: &str, count: u64) {
    if count == 0 { return; }
    let style = if count > 0 { Style::default().fg(Color::Yellow) } else { Style::default() };
    rows.push(Row::new(vec![
        Cell::from(rule.to_string()),
        Cell::from(name.to_string()),
        Cell::from(count.to_string()),
    ]).style(style));
}

fn render_footer(frame: &mut Frame, area: Rect, client: &Client) {
    let total_in: u64 = client.hello.rules.iter()
        .map(|m| client.in_rate(&m.id))
        .sum();
    let total_out: u64 = client.hello.rules.iter()
        .map(|m| client.out_rate(&m.id))
        .sum();
    let fd = client.ring.back()
        .and_then(|s| Some((s.process.fd_open?, s.process.fd_limit?)))
        .map(|(o, l)| format!("fd {o}/{l}"))
        .unwrap_or_else(|| "fd —".into());
    let txt = format!(" rules {}  total in {}  out {}  {fd}  [q]uit [?]help ",
                      client.hello.rules.len(),
                      fmt_rate(total_in), fmt_rate(total_out));
    frame.render_widget(Paragraph::new(txt), area);
}

fn render_help_overlay(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(60, 60, area);
    let text = vec![
        Line::from("Keys:"),
        Line::from("  ↑ ↓ j k        select rule"),
        Line::from("  ← → h l Tab    cycle tab"),
        Line::from("  Enter          jump to Detail"),
        Line::from("  s              cycle sort"),
        Line::from("  r              reverse sort"),
        Line::from("  /              filter (Esc clears)"),
        Line::from("  p              pause"),
        Line::from("  c              session reset"),
        Line::from("  q / Ctrl-C     quit"),
        Line::from("  ?              toggle help"),
    ];
    let block = Block::default().title(" Help ").borders(Borders::ALL);
    let p = Paragraph::new(text).block(block);
    frame.render_widget(p, popup);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup[1])[1]
}

fn pairwise_rates(field_series: &[u64], uptime_series: &[u64]) -> Vec<u64> {
    if field_series.len() < 2 { return vec![]; }
    field_series.windows(2).zip(uptime_series.windows(2))
        .map(|(fs, us)| {
            let dt_ms = us[1].saturating_sub(us[0]);
            if dt_ms == 0 { return 0; }
            fs[1].saturating_sub(fs[0]).saturating_mul(1000) / dt_ms
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::collections::VecDeque;
    use crate::stats::{ErrorSnap, Hello, ProcessSnap, RuleMeta, RuleSnap, Snapshot, TargetMeta};

    fn fake_client() -> Client {
        let hello = Hello {
            v: 1, daemon_version: "1.6.0".into(),
            daemon_started_at_ms: 0, refresh_ms: 1000,
            rules: vec![RuleMeta {
                id: "abc".into(), name: "smoke".into(),
                proto: "tcp".into(), listen: "2222".into(),
                targets: vec![TargetMeta {
                    host: "1.1.1.1".into(), port: 22, priority: 0, proxy_protocol: None,
                }],
                splice_capable: true, udp_max_flows: None,
            }],
        };
        let mut ring: VecDeque<Snapshot> = VecDeque::new();
        ring.push_back(Snapshot {
            t_ms: 0, uptime_ms: 1000, seq: 1,
            process: ProcessSnap { fd_open: Some(10), fd_limit: Some(1024), rss_bytes: None },
            r: vec![RuleSnap {
                id: "abc".into(), bytes_in: 0, out: 0,
                conns_active: 0, conns_total: 0,
                datagrams_in: 0, datagrams_out: 0,
                flows_active: 0, target_failovers_total: 0,
                err: ErrorSnap::default(),
            }],
        });
        ring.push_back(Snapshot {
            t_ms: 0, uptime_ms: 2000, seq: 2,
            process: ProcessSnap { fd_open: Some(10), fd_limit: Some(1024), rss_bytes: None },
            r: vec![RuleSnap {
                id: "abc".into(), bytes_in: 1024, out: 2048,
                conns_active: 1, conns_total: 5,
                datagrams_in: 0, datagrams_out: 0,
                flows_active: 0, target_failovers_total: 0,
                err: ErrorSnap::default(),
            }],
        });
        Client { hello, ring, capacity: 60 }
    }

    #[test]
    fn overview_renders_rule_row() {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::super::state::AppState::new();
        let client = fake_client();
        terminal.draw(|f| render(f, f.area(), &client, &mut state)).unwrap();
        let buf = terminal.backend().buffer();
        let s = buffer_to_string(buf);
        assert!(s.contains("smoke"), "buffer:\n{s}");
        assert!(s.contains("tcp"));
    }

    #[test]
    fn footer_shows_rule_count() {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::super::state::AppState::new();
        let client = fake_client();
        terminal.draw(|f| render(f, f.area(), &client, &mut state)).unwrap();
        let s = buffer_to_string(terminal.backend().buffer());
        assert!(s.contains("rules 1"), "footer missing rules count:\n{s}");
    }

    fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }
}
```

- [ ] **Step 2: Run tests**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-standalone --lib stats::tui::render --features stats-tui
```

Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```sh
git add crates/portunus-standalone/src/stats/tui/render.rs
git commit -m "feat(standalone/tui): Overview/Detail/Errors render fns + snapshot tests"
```

---

## Task 18: TUI event loop + keybindings

**Files:**
- Modify: `crates/portunus-standalone/src/stats/tui/mod.rs`

- [ ] **Step 1: Replace the stub with the real event loop**

Replace `crates/portunus-standalone/src/stats/tui/mod.rs`:

```rust
//! Ratatui TUI for `portunus-standalone stats`.

pub mod format;
pub mod render;
pub mod state;

use std::io;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::io::AsyncBufReadExt;

use crate::stats::client::Client;
use crate::stats::Snapshot;
use state::{AppState, Tab};

pub async fn run(socket: &Path) -> ExitCode {
    match run_inner(socket).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: stats TUI: {e}");
            ExitCode::from(2)
        }
    }
}

async fn run_inner(socket: &Path) -> io::Result<()> {
    let (mut client, mut reader) = Client::connect(socket).await?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = AppState::new();
    let r = run_loop(&mut client, &mut reader, &mut terminal, &mut state).await;

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    r
}

async fn run_loop(
    client: &mut Client,
    reader: &mut tokio::io::BufReader<tokio::net::UnixStream>,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
) -> io::Result<()> {
    let mut line_buf = String::new();
    loop {
        // Try to receive a snapshot with a short timeout so we can poll key events.
        line_buf.clear();
        let read = tokio::time::timeout(Duration::from_millis(50), reader.read_line(&mut line_buf)).await;
        match read {
            Ok(Ok(0)) => {
                // EOF — daemon closed the socket.
                return Ok(());
            }
            Ok(Ok(_)) => {
                if let Ok(snap) = serde_json::from_str::<Snapshot>(line_buf.trim_end()) {
                    if !state.paused {
                        client.push(snap);
                    }
                }
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => { /* timeout — fall through */ }
        }

        if event::poll(Duration::from_millis(10))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press { continue; }
                match (k.code, k.modifiers) {
                    (KeyCode::Char('q'), _)
                    | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(()),
                    (KeyCode::Char('?'), _) => state.show_help = !state.show_help,
                    (KeyCode::Esc, _) => {
                        if state.show_help {
                            state.show_help = false;
                        } else if !state.filter.is_empty() {
                            state.filter.clear();
                        }
                    }
                    (KeyCode::Char('p'), _) => state.paused = !state.paused,
                    (KeyCode::Char('s'), _) => state.sort = state.sort.cycle(),
                    (KeyCode::Char('r'), _) => state.sort_desc = !state.sort_desc,
                    (KeyCode::Char('c'), _) => {
                        if let Some(snap) = client.ring.back() {
                            state.reset_baseline(snap);
                        }
                    }
                    (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                        state.selected = state.selected.saturating_sub(1);
                    }
                    (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                        let n = client.hello.rules.len().saturating_sub(1);
                        if state.selected < n { state.selected += 1; }
                    }
                    (KeyCode::Tab, _) | (KeyCode::Right, _) | (KeyCode::Char('l'), _) => {
                        state.tab = state.tab.next();
                    }
                    (KeyCode::BackTab, _) | (KeyCode::Left, _) | (KeyCode::Char('h'), _) => {
                        state.tab = state.tab.prev();
                    }
                    (KeyCode::Enter, _) => state.tab = Tab::Detail,
                    _ => {}
                }
            }
        }

        terminal.draw(|f| render::render(f, f.area(), client, state))?;
    }
}
```

- [ ] **Step 2: Verify it builds with `stats-tui`**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone --features stats-tui
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone --no-default-features
```

Expected: both build green.

- [ ] **Step 3: Manual smoke (optional in this task, mandatory in VPS test)**

In a real terminal:

```sh
# Term 1 — run daemon
cd /tmp/portunus-test && PORTUNUS_SKIP_WEBUI=1 cargo run -p portunus-standalone -- --config portunus.toml

# Term 2 — TUI
cargo run -p portunus-standalone -- stats --socket /tmp/portunus-test/stats.sock
```

Expected: a working TUI with three tabs and the smoke rule listed.

- [ ] **Step 4: Commit**

```sh
git add crates/portunus-standalone/src/stats/tui/mod.rs
git commit -m "feat(standalone/tui): event loop, keybindings, ratatui+crossterm terminal"
```

---

## Task 19: contrib + Docker + systemd integration

**Files:**
- Modify: `crates/portunus-standalone/contrib/portunus-standalone.service`
- Modify: `crates/portunus-standalone/contrib/Dockerfile`
- Modify: `crates/portunus-standalone/contrib/portunus.example.toml`

- [ ] **Step 1: systemd unit — `RuntimeDirectory`**

Edit `crates/portunus-standalone/contrib/portunus-standalone.service`. Add immediately under `[Service]`:

```
RuntimeDirectory=portunus
RuntimeDirectoryMode=0755
```

This auto-creates `/run/portunus/` owned by `portunus:portunus` with the right perms on each start, and cleans it up on stop. Add `/run/portunus` to `ReadWritePaths` if `ProtectSystem=strict` blocks writes:

```
ReadWritePaths=/run/portunus
```

- [ ] **Step 2: Dockerfile — pre-create `/run/portunus/`**

Edit `crates/portunus-standalone/contrib/Dockerfile`. In the **final** stage (the distroless one), before the final `USER` directive, add:

```dockerfile
# Pre-create the stats UDS directory writable by the nonroot user.
COPY --from=builder --chown=65532:65532 /tmp/run-portunus /run/portunus
```

And in the **builder** stage, before the final binary build, add:

```dockerfile
RUN mkdir -p /tmp/run-portunus && chown 65532:65532 /tmp/run-portunus && chmod 0755 /tmp/run-portunus
```

(If the builder image is distroless-incompatible, an alternative is to add a tiny `--from=alpine` stage just to materialise that directory. Inspect the existing Dockerfile to see what builder stage it uses.)

If the Dockerfile already has a non-trivial structure that makes this awkward, fall back to using a `VOLUME` declaration and document that users must `--mount type=tmpfs,destination=/run/portunus` at runtime. Prefer the pre-create approach.

- [ ] **Step 3: Example TOML — commented `[stats]` block**

Edit `crates/portunus-standalone/contrib/portunus.example.toml`. Add a block near the top (after `[global]`):

```toml
# ─────────── Stats UDS (optional; default ON) ───────────
# A daemon-local Unix-domain socket streams per-rule traffic snapshots.
# Use `portunus-standalone stats` to view the live TUI dashboard, or
# `portunus-standalone stats --once` for a one-shot JSON dump.
#
# [stats]
# enabled     = true
# socket_path = "/run/portunus/standalone.sock"
# refresh_ms  = 1000
```

- [ ] **Step 4: Verify**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone
# systemd unit validation (lint only):
systemd-analyze verify crates/portunus-standalone/contrib/portunus-standalone.service 2>&1 || true
# Docker build (if docker available):
docker build -f crates/portunus-standalone/contrib/Dockerfile -t portunus-standalone:dev . 2>&1 | tail -5
```

systemd-analyze warnings about absolute paths in a non-system context are fine.

- [ ] **Step 5: Commit**

```sh
git add crates/portunus-standalone/contrib/
git commit -m "build(standalone): contrib — RuntimeDirectory in systemd unit, pre-create /run/portunus in Dockerfile, [stats] in example TOML"
```

---

## Task 20: Documentation — fumadocs Live Stats Dashboard section

**Files:**
- Modify: `docs/content/docs/operations/standalone.mdx`
- Modify: `docs/content/docs/zh/operations/standalone.mdx`

- [ ] **Step 1: EN — insert section before "Observability"**

In `docs/content/docs/operations/standalone.mdx`, search for `## Observability` and insert a new section just before it:

```mdx
## Live stats dashboard

`portunus-standalone stats` is a TUI dashboard that streams per-rule
traffic counters from the running daemon over a Unix-domain socket.
No HTTP, no Prometheus, no extra processes — just a single binary
subcommand.

```sh
# Interactive TUI (in a terminal):
portunus-standalone stats

# One-shot JSON snapshot (scriptable):
portunus-standalone stats --once | jq '.snapshot.r[] | {id, in, out, conns_active}'
```

For Docker installs:

```sh
docker exec -it portunus-standalone portunus-standalone stats
```

The dashboard shows three tabs:

- **Overview** — per-rule table: current in/out rate, active
  connections, UDP flow count.
- **Detail** — 60 s sparklines for the selected rule, plus cumulative
  totals and (for UDP) a saturation gauge against `udp_max_flows`.
- **Errors** — non-zero cumulative failure counters per rule
  (`port_in_use`, `upstream_connect_failed`, `icmp_evict`, `emsgsize`,
  `wouldblock`, `addflow_dropped`, `dns_failures`,
  `flows_dropped_overflow`).

Default keybindings: `q` quit, `?` help, `Tab`/`h`/`l`/`←`/`→` cycle
tab, `↑↓jk` select row, `p` pause, `s` cycle sort, `r` reverse sort,
`/` filter, `c` reset baseline.

### Configuration

```toml
[stats]
enabled     = true                                # default true
socket_path = "/run/portunus/standalone.sock"     # Linux default
refresh_ms  = 1000                                # 250..=5000
```

CLI overrides on the daemon: `--no-stats`, `--stats-socket <PATH>`.

The TUI client picks the default socket per platform:

- Linux → `/run/portunus/standalone.sock` (`RuntimeDirectory=portunus`
  in the shipped systemd unit creates this automatically)
- macOS → `$TMPDIR/portunus-standalone.sock`
- Override with `--socket <PATH>` if either default doesn't apply.

The snapshot cadence is daemon-driven; the client has no request
channel. To change the cadence, edit `[stats] refresh_ms` and restart
the daemon.
```

- [ ] **Step 2: ZH — mirror in zh/**

In `docs/content/docs/zh/operations/standalone.mdx`, insert before `## 可观测性`:

```mdx
## 实时流量面板

`portunus-standalone stats` 是一个 TUI 看板，通过 Unix 域 socket
从运行中的 daemon 流式获取每条规则的实时计数器。无 HTTP、无
Prometheus、无额外进程——只是同一个二进制的子命令。

```sh
# 交互式 TUI：
portunus-standalone stats

# 一次性 JSON 快照（脚本场景）：
portunus-standalone stats --once | jq '.snapshot.r[] | {id, in, out, conns_active}'
```

Docker 安装：

```sh
docker exec -it portunus-standalone portunus-standalone stats
```

三个标签页：

- **Overview** —— 每规则一行：当前 in/out 速率、活跃连接、UDP flow 数。
- **Detail** —— 选中规则的 60s sparkline + 累计字节数 +（UDP）按
  `udp_max_flows` 显示的饱和 gauge。
- **Errors** —— 每条规则的非零失败累计计数（`port_in_use`、
  `upstream_connect_failed`、`icmp_evict`、`emsgsize`、`wouldblock`、
  `addflow_dropped`、`dns_failures`、`flows_dropped_overflow`）。

键位：`q` 退出，`?` 帮助，`Tab`/`h`/`l`/`←`/`→` 切 tab，`↑↓jk` 选行，
`p` 暂停，`s` 切排序，`r` 翻转排序方向，`/` 过滤，`c` 重置基线。

### 配置

```toml
[stats]
enabled     = true                                # 默认 true
socket_path = "/run/portunus/standalone.sock"     # Linux 默认
refresh_ms  = 1000                                # 250..=5000
```

daemon CLI 覆盖：`--no-stats`、`--stats-socket <PATH>`。

TUI 客户端按平台选默认 socket：

- Linux → `/run/portunus/standalone.sock`（shipped systemd unit 的
  `RuntimeDirectory=portunus` 会自动创建）
- macOS → `$TMPDIR/portunus-standalone.sock`
- 都不合适时用 `--socket <PATH>` 覆盖。

快照频率由 daemon 决定，client 没有请求通道。要改频率得编辑
`[stats] refresh_ms` 后重启 daemon。
```

- [ ] **Step 3: Verify markdown**

```sh
ls docs/content/docs/operations/standalone.mdx docs/content/docs/zh/operations/standalone.mdx
# Optional: run the docs site dev server if available
# (cd docs && pnpm dev) > /tmp/docs.log 2>&1 &
```

- [ ] **Step 4: Commit**

```sh
git add docs/content/docs/operations/standalone.mdx docs/content/docs/zh/operations/standalone.mdx
git commit -m "docs(standalone): document live stats dashboard (EN+ZH)"
```

---

## Task 21: CHANGELOG entry

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add a v1.6.0 entry**

At the top of `CHANGELOG.md` (above the existing v1.5.x line), add:

```md
## [1.6.0] — Unreleased

### Added
- `portunus-standalone stats` — TUI dashboard for per-rule traffic
  observability over a Unix-domain socket. Three tabs (Overview /
  Detail / Errors), 60 s sparklines, session-reset baseline, regex
  filter, sortable, pauseable. `stats --once` prints a single JSON
  snapshot for scripts.
- `[stats]` config section (`enabled`, `socket_path`, `refresh_ms`)
  with platform-aware defaults: `/run/portunus/standalone.sock` on
  Linux, `$TMPDIR/portunus-standalone.sock` on macOS. Override via
  `--no-stats` / `--stats-socket` on the daemon.
- `RuleStats.connections_total`, `RuleStats.errors: ErrorCounters`,
  and migrated `target_failovers_total: Arc<AtomicU64>` onto
  `RuleStats`. Existing tracing call sites for `rule.failed`,
  `rule.udp_*` events now bump matching `AtomicU64` counters
  alongside the log emit.
- `stats-tui` Cargo feature (default on). Build with
  `--no-default-features` for a smaller binary without `ratatui` /
  `crossterm`; `stats --once` still works in that build.
- systemd unit gains `RuntimeDirectory=portunus`; Docker image
  pre-creates `/run/portunus/` with UID 65532 ownership.

### Changed
- Spec: `docs/superpowers/specs/2026-05-28-standalone-stats-tui-design.md`.

### Tests
- New: `tests/stats_server.rs` (UDS server round-trip),
  `tests/stats_once.rs` (e2e `stats --once`).
- `ratatui::backend::TestBackend` snapshot tests cover Overview,
  Detail, Errors tabs.
```

- [ ] **Step 2: Commit**

```sh
git add CHANGELOG.md
git commit -m "docs: CHANGELOG entry for v1.6.0 — standalone stats TUI"
```

---

## Task 22: Final integration smoke + workspace gates

**Files:** none — verification only.

- [ ] **Step 1: Workspace tests**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace
```

Expected: all green.

- [ ] **Step 2: Workspace clippy with pedantic warnings as errors**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo clippy --workspace --all-targets -- -D warnings
```

Expected: green.

- [ ] **Step 3: Build both feature variants**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone --release
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-standalone --release --no-default-features
```

Both must succeed. Note the binary sizes:

```sh
ls -la target/release/portunus-standalone
```

Expected: TUI variant ~1–2 MB larger than no-default-features variant.

- [ ] **Step 4: Bench gate**

Run the existing UDP perf bench with and without an attached stats client:

```sh
PORTUNUS_SKIP_WEBUI=1 cargo bench -p portunus-forwarder --bench data_plane 2>&1 | tee /tmp/bench-no-stats.txt
```

Inspect the output; if there's a >5% median regression vs `crates/portunus-client/benches/baselines/v0.1.0.json`, investigate. If no regression, proceed.

- [ ] **Step 5: Final commit (if any drift)**

```sh
git status
# If anything is uncommitted from the gate runs, commit it.
git log --oneline -25
```

Expected: a clean ~21 commits since branch start.

---

## Self-review notes

**Spec coverage:**
- §6.3 (a) `connections_total` → Tasks 1+2
- §6.3 (b) `ErrorCounters` + wiring → Tasks 3, 4, 5, 6
- §6.3 (c) `target_failovers_total` ownership → Task 7
- §6.3 (d) `dns_failures` / `flows_dropped_overflow` surfacing → Task 11 (server reads them)
- §5 Protocol types → Task 9
- §6.2 Config + platform defaults → Task 10
- §11 server implementation → Task 11
- §6.4 wiring into runtime → Task 12
- §7 Client + ring buffer + rate calc → Task 13
- §7.2 subcommand dispatch + `--once` semantics + `--render-ms` removal → Task 14
- §7.5 keybindings → Task 18
- §7.4 widgets → Task 17
- §8 feature flag + `stats-tui` off behaviour → Tasks 8 + 14
- §13 deliverable contrib bits → Task 19
- §13 deliverable docs → Tasks 20, 21
- §9 testing strategy → Tasks throughout + Task 22 gate

**Out of scope confirmed in spec §2, not in plan:** Prometheus subcommand, daemon-side ring buffer, per-target health, per-peer drill-down, web UI, live log tail.

**VPS testing (not a plan task — handled by execution agent):**
After Task 22 passes, the orchestrator agent SSHes to 207.241.173.217 and:
1. Builds a static binary (`cross build --release --target x86_64-unknown-linux-musl` or local `cargo build --release` if matching), scps it up, runs `scripts/install.sh standalone` flow, verifies `portunus-standalone stats --once` returns valid JSON.
2. Builds the Docker image locally, `docker save` it, scps the tar, `docker load` on VPS, `docker run` with the example TOML, verifies `docker exec ... portunus-standalone stats --once` works.

Both paths confirm `/run/portunus/standalone.sock` permissions and end-to-end traffic counters.
