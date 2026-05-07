# Phase 1 — Data Model

**Feature**: 002-port-range-forward
**Date**: 2026-05-07
**Inherits from**: `specs/001-tcp-forward-mvp/data-model.md` — every entity
defined there is unchanged unless restated below. This document only
records the *deltas*.

---

## PortRange (NEW newtype, `forward-core`)

`PortRange { start: u16, end: u16 }` — a contiguous, inclusive port
range. Single-port rules use `start == end`.

| Field | Type | Validation |
|---|---|---|
| `start` | `u16` | `1..=65535` (0 rejected as not a real listening port). |
| `end`   | `u16` | `1..=65535`, AND `start ≤ end` (FR-002). |

Constructors:
- `PortRange::single(port)` → `start = end = port` (used to lift v0.1.0
  single-port rules into the unified path).
- `PortRange::new(start, end) -> Result<Self, PortRangeError>` →
  validates ordering.
- `PortRange::pair(listen, target) -> Result<(PortRange, PortRange), PortRangeError>` →
  validates ordering on both sides AND `len(listen) == len(target)`
  (FR-002).

Methods:
- `len(&self) -> u32` → `(end - start) as u32 + 1` (returns `u32` to
  avoid overflow at the 65535-65535 / 1-65535 edge).
- `contains(&self, port) -> bool`
- `overlaps(&self, other) -> bool` → `self.start ≤ other.end && other.start ≤ self.start`
  (used by the rule store conflict check).
- `iter(&self) -> impl Iterator<Item = u16>`
- `target_for(listen_port: u16, listen: &PortRange, target: &PortRange) -> Option<u16>`
  → returns `target.start + (listen_port - listen.start)` if the
  invariants hold (callers verify with `len()` first).

Error variant:
```rust
pub enum PortRangeError {
    Inverted,                    // start > end
    LengthMismatch { listen_len: u32, target_len: u32 },
    OutOfBounds,                 // start == 0 or > 65535
    ExceedsCap { requested: u32, cap: u32 },
}
```

`ExceedsCap` is not built in `PortRange` itself (the cap is operator
config); it's surfaced where the cap is known (server rule store).

---

## RangeCap (NEW config field, server-only)

| Field | Type | Default | Notes |
|---|---|---|---|
| `range_rule_max_ports` | `u32` | `1024` | Maximum ports any single rule may span. Loaded from `server.toml`. Hot-reload not required (changes apply on next push). |

Added to `ServerConfig` as a new field with `#[serde(default = "default_range_cap")]` so existing `server.toml` files continue to load with the 1024 default.

---

## Rule (EXTENDED, server in-memory)

The existing `Rule` struct in `crates/forward-server/src/rules.rs`
gains two optional fields. Wire / persistence shape is identical
(serde defaults handle the absence on disk).

| Field | Type | Notes |
|---|---|---|
| `id` | `RuleId` | unchanged |
| `client_name` | `ClientName` | unchanged |
| `listen_port` | `u16` | range start (interpreted as `listen_port_start`) |
| `listen_port_end` | `Option<u16>` | **NEW** — `None` = single-port (legacy), `Some(end) >= listen_port` = range up to `end` (inclusive) |
| `target_host` | `String` | unchanged |
| `target_port` | `u16` | range start on target side |
| `target_port_end` | `Option<u16>` | **NEW** — symmetric to `listen_port_end` |
| `protocol` | `Protocol` | unchanged (TCP only in v1) |
| `state` | `RuleState` | unchanged |
| `created_at` | `DateTime<Utc>` | unchanged |
| `last_state_change_at` | `DateTime<Utc>` | unchanged |

Helpers (added to `impl Rule`):
- `listen_range(&self) -> PortRange` — returns `PortRange { start: listen_port, end: listen_port_end.unwrap_or(listen_port) }`.
- `target_range(&self) -> PortRange` — symmetric.
- `range_size(&self) -> u32` — convenience for logs/audit/HTTP responses.
- `is_range(&self) -> bool` — `listen_port_end.is_some_and(|e| e > listen_port)`. Useful for nicer log lines, NOT for control flow (see "single-port = range size 1" invariant).

**Invariant**: `listen_port_end.is_some() == target_port_end.is_some()`,
i.e. both new fields are present together. Enforced at construction
(rule store push) and on persistence load.

**Persistence shape** (additive, see `contracts/persistence.md`):
```json
{
  "version": 1,
  "rules": [
    { "id": 7, "client_name": "edge-01", "listen_port": 18080, "target_host": "10.0.0.5", "target_port": 8080, "protocol": "Tcp" },
    { "id": 8, "client_name": "edge-01", "listen_port": 30000, "listen_port_end": 30050, "target_host": "10.0.0.5", "target_port": 30000, "target_port_end": 30050, "protocol": "Tcp" }
  ]
}
```

The first record is a v0.1.0-shaped rule (single port); the second is
a range rule. Both load on the v0.2.0 server. The server writes new
range rules with `listen_port_end` / `target_port_end` set; rewriting
existing single-port records is not required (omitting the optional
fields on write is also acceptable).

---

## ServerRuleStore (EXTENDED)

The store keeps its API name and `(ClientName, RuleId)` ownership but
swaps the secondary index for an interval-aware structure (per R-002):

```rust
struct Inner {
    rules: HashMap<RuleId, Rule>,
    /// Per-client BTreeMap from `listen_port_start` → `RuleId` for any
    /// rule in `Active` or `Failed` state. Used for the overlap check.
    by_client_listen: HashMap<ClientName, BTreeMap<u16, RuleId>>,
}
```

Push validation (extends the existing checker):

1. `PortRange::pair(listen, target)?` — fails fast on inverted /
   length-mismatch (FR-002).
2. `if listen.len() > config.range_rule_max_ports → ExceedsCap` (FR-008).
3. Conflict check against `by_client_listen[client_name]`:
   range over entries with key `≤ candidate.listen.end`; for each,
   load the existing `Rule`, compute `existing.listen_range()`, and if
   it `overlaps(candidate.listen)` AND state is `Active | Failed`,
   return `PortInUse { offending_port: max(existing.start, candidate.start) }`.

State machine and `(Active | Failed) blocks reuse` semantics from spec
001 are unchanged.

Removal: when a rule is removed, drop its `(client_name → start)` entry
from `by_client_listen`.

`PortInUse` error gains an associated `offending_port: u16` field so
the HTTP API and CLI can surface it in the error message.

---

## RuleStats (EXTENDED, client-side)

The client's per-rule stats struct gains a per-port aggregate alongside
the existing whole-rule counters.

| Field | Type | Notes |
|---|---|---|
| `rule_id` | `RuleId` | unchanged |
| `bytes_in` | `AtomicU64` | aggregate across all ports in the range |
| `bytes_out` | `AtomicU64` | aggregate across all ports in the range |
| `active_connections` | `AtomicU32` | aggregate across all ports |
| `per_port` | `BTreeMap<u16, PerPortCounters>` | **NEW** — one entry per port in the range |

```rust
pub struct PerPortCounters {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub active_connections: AtomicU32,
}
```

Lookup is `O(log N)` per accept/close; for a 1024-port rule this is
≈10 cache-friendly comparisons — negligible relative to the syscall
cost of `accept()`. The map is constructed once on `forwarder::run`
startup (one entry per port in the range) and never resized at runtime.

For single-port rules `per_port` contains exactly one entry; the
existing aggregate counters and the single per-port counter increment
together (no double-counting because per-port and aggregate use
separate atomics).

`StatsReport` (gRPC) adds a repeated `PerPortStats` to each
`RuleStats` message. The server's `RuleStatsCache` is supplemented with
`AppState::per_port_stats: Arc<RwLock<HashMap<RuleId, BTreeMap<u16, PerPortCounters>>>>`
holding only the latest snapshot per rule (overwritten each report).

---

## ClientRule (EXTENDED, client in-memory)

| Field | Type | Notes |
|---|---|---|
| `rule_id` | `RuleId` | unchanged |
| `listen_range` | `PortRange` | **NEW** — replaces the bare `listen_port: u16` from v0.1.0; built from the proto rule via `Rule::listen_range()` (single-port rules become a range of size 1) |
| `target_host` | `String` | unchanged |
| `target_range` | `PortRange` | **NEW** — symmetric to `listen_range` |

`ClientRule::is_range()`, `len()`, `target_for(listen_port)` mirror the
helpers on `forward-server`'s `Rule` (no shared crate needed —
`PortRange` lives in `forward-core`).

---

## RuleUpdate (proto, EXTENDED)

The wire envelope is unchanged. The `Rule` message (see
`contracts/forward.proto`) gains optional `listen_port_end` and
`target_port_end` fields. Existing v0.1.0 clients reading a v0.2.0
rule see them as their default (0) and continue to work — but
operationally a v0.1.0 client should not be talking to a v0.2.0 server
that pushes range rules to it; this is documented as an operator
constraint in `quickstart.md`.

---

## AuditEvent (EXTENDED)

Existing `audit.rule_push` / `audit.rule_remove` events gain optional
fields:
- `listen_port_end: Option<u16>` (mirrors the rule field)
- `target_port_end: Option<u16>`
- `range_size: u32` (derived; emitted unconditionally so log analytics
  can pivot on it)

No new event types. Single-port rule pushes still log
`listen_port_end = null, range_size = 1`.

---

## Configuration deltas

### ServerConfig (`server.toml`)

| Field | Type | Default | Notes |
|---|---|---|---|
| `range_rule_max_ports` | `u32` | `1024` | **NEW**. FR-008 cap. Loaded with `serde(default)`. Operators on hosts with raised `RLIMIT_NOFILE` may raise this; operators on stricter limits should lower it. |

### ClientConfig

No changes. Range rules are server-driven; the client receives them via
the existing control stream and runs them via the existing forwarder
task family.

---

## State transitions

Unchanged from v0.1.0 (`Pending → Active | Failed → Removed`). The only
behavioral delta is the precondition for `Pending → Active`:

- v0.1.0: `Active` iff one `TcpListener::bind` succeeded.
- v0.2.0: `Active` iff **every** `TcpListener::bind` in the range
  succeeded. (`Failed` if any failed; the forwarder rolls back the
  successful binds before reporting.)

---

## Validation map (FR → entity)

| FR | Enforced where |
|---|---|
| FR-001 | `Rule` carries a single ID; `ServerRuleStore::push` accepts the range form |
| FR-002 | `PortRange::pair` (inverted / length-mismatch) |
| FR-003 | `PortRange::target_for` + client `forwarder::range::bind_all` |
| FR-004 | `forwarder::range::bind_all` (atomic; releases on failure) |
| FR-005 | `Option<u16>` fields + `serde(default)` (proto, struct, persistence) |
| FR-006 | `forwarder::run` drain loop already covers all listeners under one rule via shared `proxy_cancel` |
| FR-007 | `Rule` has one ID; HTTP / CLI render one row per rule |
| FR-008 | `ServerRuleStore::push` consults `RangeCap` |
| FR-009 | `metrics.rs` labels stay `(client_name, rule_id)` only |
| FR-010 | `ServerRuleStore::push` interval check (R-002) |
| FR-011 | `RuleStats.per_port` + `--per-port` HTTP query + CLI flag |
