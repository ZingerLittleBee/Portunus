# Phase 0 — Research

**Feature**: 002-port-range-forward
**Date**: 2026-05-07

The spec already resolved the three high-impact ambiguities via
`/speckit-clarify` (default cap 1024 ports, observability split between
aggregate Prometheus + opt-in `--per-port` CLI, additive wire shape).
This document records the remaining design decisions made during
Phase 0 — each is a "best practice" or "patterns" question rather than
a NEEDS CLARIFICATION blocker.

---

## R-001: Range fan-out concurrency on the client

**Decision**: Bind all listeners **serially** in a single async function
(`forwarder::range::bind_all`), accumulate `Vec<TcpListener>` on success,
or release every successful bind and return `Failed(reason, offending_port)`
on the first failure. After all binds succeed, spawn one accept loop per
listener, all under one `JoinSet` owned by the existing `forwarder::run`
task; share one `CancellationToken` for fast shutdown and one
`drain_timeout` for in-flight connections.

**Rationale**:
- Each `TcpListener::bind` is non-blocking and completes in ~tens of µs
  on Linux when the port is free; binding 1024 ports serially is
  sub-second and well inside the SC-001 budget. Parallelism here would
  buy nothing and would complicate the all-or-nothing rollback.
- One `JoinSet` per rule (instead of one per listener) preserves the
  existing 1-rule-1-task ownership model from
  `forward-client/src/forwarder/mod.rs`. The lifecycle (`Activated`
  once, `Removed` once) is unchanged from a control-plane observer's
  perspective.
- Sharing one `CancellationToken` per rule keeps the "stop accept within
  1 s" guarantee (FR-016 / current `cancel_stops_accept_within_one_second`
  test) intact for ranges — cancel propagates to every accept loop in
  the same tick.

**Alternatives considered**:
- *Parallel binds via `futures::future::try_join_all`*: rejected.
  Saves <1 ms per range install, complicates the cleanup-on-failure
  branch (need to await a `JoinSet` of partial successes), and obscures
  the `Failed{offending_port}` error when multiple ports fail
  simultaneously.
- *One `tokio::task` per listener (no `JoinSet`)*: rejected. Drain /
  cancel coordination becomes unbounded; a hung accept loop would
  prevent `Removed` from firing.

---

## R-002: Range-vs-range conflict detection in the server rule store

**Decision**: Replace the v0.1.0 `HashMap<(ClientName, u16), RuleId>`
secondary index with a per-client interval set keyed by
`(ClientName, listen_start)` and ordered. On `push`, walk the entries
for `client_name` whose `listen_start ≤ candidate.listen_end`, and
reject if any existing rule's `[start, end]` overlaps the candidate's
`[start, end]`. The first conflicting port (`max(existing.start,
candidate.start)`) is reported in the error.

**Rationale**:
- Worst-case rules-per-client is small (operator-defined, typically
  <100 even with ranges); a `BTreeMap<u16, RuleId>` keyed by listen_start
  per client is `O(log N)` lookup + a short scan and is dead simple.
- Naming the first conflicting port matches the spec acceptance
  scenario for US4 ("error references port 30005 or the overlap range").
- Single-port rules are stored as `[port, port]` intervals so the same
  conflict check covers FR-005 backward compat.

**Alternatives considered**:
- *Interval tree*: rejected. The expected N is too small to justify the
  data structure complexity.
- *Brute-force scan over all rules*: workable but wasteful at high rule
  counts; the per-client `BTreeMap` is a tiny upgrade and stays simpler
  than an interval tree.

---

## R-003: Per-port stats exposure (`--per-port`)

**Decision**: The client extends `RuleStats` with an optional repeated
field `per_port` (vector of `(port, bytes_in, bytes_out, active_conns)`)
in `forward.v1.RuleStats`. The client populates it always for range
rules and leaves it empty for single-port rules. The server caches the
last received per-port snapshot keyed by `RuleId` in
`AppState::per_port_stats`. The HTTP API exposes
`GET /v1/rules/{rule_id}/stats?per_port=true` returning the per-port
breakdown alongside the aggregate; CLI `rule-stats <id> --per-port`
appends `?per_port=true` and renders a per-port table.

Prometheus collectors **do not change** — only the existing aggregate
labels (`client_name`, `rule_id`) are exported (FR-009 / SC-002).

**Rationale**:
- Reuses the existing `StatsReport` push channel — no new RPC, no new
  poll endpoint.
- Per-port detail is on-demand, so the on-wire cost is paid only when
  `--per-port` is requested. (Compromise: client always reports it for
  range rules so the operator's request can be answered immediately
  from the cache without a round-trip.) For a 1024-port rule that adds
  ~1024 × ~24 bytes = ~24 KiB per stats interval, ≈5 KiB/s — well
  inside any reasonable control-plane budget.
- Keeping per-port out of Prometheus preserves the cardinality budget
  (Constitution IV; SC-002 explicit).

**Alternatives considered**:
- *Separate gRPC RPC `GetRuleDetail(rule_id)` polled on demand*:
  rejected. Adds a new RPC + new auth/audit surface for marginal
  benefit. The push-based path is consistent with v0.1.0.
- *Suppress per-port reporting unless the operator opted in*: rejected.
  Would require an extra round-trip every time `--per-port` is invoked;
  the bandwidth saving is too small to justify the extra mechanism and
  latency.

---

## R-004: Default cap value source

**Decision**: Default cap = 1024 ports. Loaded from `ServerConfig`
field `range_rule_max_ports` (default 1024) in `server.toml`.
Overridable per server.

**Rationale** (from spec § Clarifications Q1, recorded here for
implementation reference):
- 1024 matches the Linux default soft `RLIMIT_NOFILE`. Each listening
  socket consumes one fd; an in-flight forwarded connection consumes
  two more (accepted client socket + outbound socket). A 1024-port rule
  at zero connections leaves zero fd headroom under the default ulimit
  — operators with any nontrivial connection count MUST raise their
  ulimit OR pick a smaller cap, both of which match operator
  expectations for forwarding workloads.
- Covers typical real workloads named in the spec (NodePort 30000-32767
  is wider, but operators typically don't expose the whole range
  through one rule; 1024 covers SIP/RTP, game-server clusters, etc.).

**Alternatives considered**:
- *No cap by default*: rejected. Spec FR-008 requires a configurable
  default; "no cap" risks a typo (e.g., `30000-65535`) silently
  exhausting fds.
- *Cap = 256*: too restrictive for real range workloads.
- *Cap = 4096*: encourages misuse; no listed workload needs it.

---

## R-005: Persistence backward-compatibility

**Decision**: Existing rules persistence (whatever shape ships in the
v0.1.0 rules store, if/when one is added — see spec 001 deferral) loads
unchanged because the new fields `listen_port_end` and `target_port_end`
deserialize as `Option<u16> = None` for old records. v0.1.0's persisted
rules become `range_size = 1` rules at runtime, with no migration step
and no schema version bump beyond proto field additions.

**Rationale**: Aligns with the spec's clarification Q3 (additive
shape, zero migration). Honored at three layers:
1. **proto** — new fields are `optional uint32` (proto3 syntax).
2. **server in-memory** — `Rule` struct uses `Option<u16>` for the new
   fields.
3. **rules.json on disk** — `serde_json` skips missing fields with
   `#[serde(default)]`.

**Alternatives considered**:
- *Bump rules.json `version` from 1 → 2 with explicit migration*:
  rejected. Adds operator overhead with no behavior gain because
  v0.1.0 rules are valid v0.2.0 rules unmodified.
- *Replace single-port `Rule` with a tagged union (`enum Rule {
  SinglePort, Range }`)*: rejected. Forces a wire change and a code
  diff at every callsite; spec Q3 explicitly chose additive optional
  fields to avoid this.

---

## R-006: Port-range CLI syntax

**Decision**: Extend the existing `push-rule` verb with optional range
suffix on both listen and target ports:

```
forward-server push-rule <client> <listen_port>[-<listen_end>] \
  <target_host>:<target_port>[-<target_end>] [--protocol tcp]
```

Examples:
- `push-rule edge-01 18080 10.0.0.5:8080`             → single port (today, unchanged)
- `push-rule edge-01 30000-30050 10.0.0.5:30000-30050` → range, same offset
- `push-rule edge-01 30000-30050 10.0.0.5:40000-40050` → range, shifted offset

Adds `--per-port` to `rule-stats`:
```
forward-server rule-stats <rule_id> [--per-port] [--format text|json]
```

**Rationale**:
- One CLI verb, one HTTP endpoint, one proto message — matches Q3.
- The `start-end` syntax is unambiguous, mirrors common conventions
  (lsof, iptables, semantic) and parses with one `split('-')` per
  side.
- Validation rules (start ≤ end, length match between sides) live in
  `forward-core::PortRange` so the CLI, HTTP API, and gRPC server share
  one validator.

**Alternatives considered**:
- *Separate `push-range-rule` verb*: rejected. Doubles operator
  cognitive load and violates Q3 "one CLI verb".
- *Comma-separated discrete ports (`30000,30005,30010`)*: rejected by
  spec § Out of Scope.

---

## R-007: Conflict-error error code

**Decision**: Reuse the frozen v1 error code `port_in_use` (CLI exit 5
/ HTTP 409) for any range overlap. The error `message` names the
specific offending port(s). No new error code is introduced.

**Rationale**:
- Stability guarantee in `operator-api.md` says new codes may be
  added but existing ones cannot be renamed; reusing `port_in_use`
  keeps the v1 surface frozen.
- Operators already script against exit 5 for "the port I wanted is
  already taken"; ranges are a strict superset of that condition.

**Alternatives considered**:
- *New code `range_overlap`*: rejected. Adds nothing — operators only
  need to know "give me the port number that conflicts", which the
  message field carries.

---

## R-008: Atomicity model for `Pending → Active`

**Decision**: A range rule reaches `Active` only after **all** listeners
in the range bind successfully. The forwarder reports a single
`RuleStatus { outcome = ACTIVATED }` for the rule. If any bind fails,
the forwarder releases every successful listener (drops their
`TcpListener`) before reporting `RuleStatus { outcome = FAILED, reason }`,
where `reason` carries `port_in_use:<offending_port>` (or the relevant
error category). The server transitions the rule to `Failed` exactly as
today (FR-004); the operator must `remove` to free the rule slot, same
as v0.1.0 single-port semantics.

**Rationale**: Matches spec FR-004 (all-or-nothing) and US4. Reusing
the existing `Failed` state machine keeps the operator surface
identical (they already understand "Failed needs removal").

**Alternatives considered**:
- *Partial activation with per-port `Failed` reasons*: rejected by
  spec edge case "all-or-nothing".
- *Auto-retry failed binds*: rejected — v0.1.0 already says no
  auto-retry (data-model.md Q4); range rules inherit that.

---

## Open items (deferred to /speckit-tasks or later)

- Exact criterion benchmark thresholds for the install-fan-out
  benchmark — set during /speckit-tasks once we have a baseline number
  on the dev host.
- Whether to add a `range_size` derived field on the HTTP `GET /v1/rules`
  response for operator convenience — punted to a follow-up because
  callers can compute it from the start/end fields.
