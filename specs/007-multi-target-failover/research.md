# Research: Multi-target failover

**Phase**: 0 (pre-implementation)
**Date**: 2026-05-08
**Status**: complete — no NEEDS CLARIFICATION carried forward into Phase 1.

This document records the design decisions for v0.7. Each entry has the form
**Decision** / **Rationale** / **Alternatives considered**, plus a short note
on the inherited baseline from earlier releases where relevant.

---

## R-001 — Wire shape: extend `Rule` additively, do not introduce a `MultiTargetRule`

**Decision**: Add a single repeated field `repeated Target targets = 9` to the
existing `forward.v1.Rule` proto3 message. Each `Target` carries
`(host, port, priority)`. Existing `target_host` (field 3) and `target_port`
(field 4) fields are preserved as the canonical encoding for **single-target**
rules.

**Rationale**:
- proto3's "additive field" pattern preserves wire compatibility (R-002 spells
  it out). v0.6.0 senders that emit only `target_host` + `target_port`
  continue to be decoded correctly by v0.7 receivers (and vice versa for
  the single-target case).
- A separate `MultiTargetRule` message would force every consumer to
  branch on rule kind, doubling the surface of every code path that
  touches rules (push, list, stats, persistence). The single-message
  approach concentrates the branch in exactly one place: the rule
  activation site on the client (`match targets.len() { 1 => fast_path,
  _ => failover_path }`).
- The constitution's MAJOR-version-on-breaking-change rule (governance)
  would force `forward.v2` if we had to drop or rename existing fields.
  Adding optional fields is a MINOR change and stays in `forward.v1`.

**Alternatives considered**:
- A new `MultiTargetRule` message — rejected for the surface-doubling
  reason above.
- Reusing `Rule.target_host` as a comma-joined list — rejected because
  it can't carry per-target priority and breaks every existing consumer
  that parses it as a single hostname (v0.3.0 DNS resolver in particular).
- Sending targets as a side-channel via a new `RuleTargets` message — rejected
  as it adds a second message to the rule lifecycle (push, then targets;
  two atomic operations vs one) and complicates the persistence story.

---

## R-002 — Single-target back-compat encoding rule

**Decision**: A single-target rule MUST be encoded with `targets` empty
**and** `target_host`/`target_port` populated (the v0.6.0 shape). A
multi-target rule (length ≥ 2) MUST be encoded with `targets` populated
**and** `target_host`/`target_port` left default (empty / 0). The receiver
canonicalises both shapes into the in-memory representation
`Vec<Target>` (length ≥ 1).

**Rationale**:
- A v0.7 receiver decoding a v0.6.0 sender sees `targets.len() == 0` →
  promotes `(target_host, target_port)` to a one-element targets list at
  decode time. The single-target hot path (which only reads `targets[0]`)
  is byte-identical.
- A v0.6.0 receiver decoding a v0.7 sender (multi-target rule) sees
  `target_host == ""`, `target_port == 0`, and silently drops the unknown
  `targets` field. This is the **expected wire-incompatibility for the
  multi-target case** — a v0.6.0 client cannot understand multi-target
  semantics. The server detects this by inspecting `Hello.client_version`
  + `Hello.supported_protocols` (R-007 below) and rejects the push at the
  operator HTTP layer with a clear error before it reaches the client.
- The "two encodings" rule, with both populated only as a transient
  invalid state, makes the validator simple: reject any rule where both
  shapes are present **on the wire** (FR-004 covers this at the HTTP
  ingress; the proto can't enforce mutual exclusion structurally).

**Alternatives considered**:
- Always emit `targets[]` (length ≥ 1) and deprecate `target_host` +
  `target_port` — rejected because it forces a wire change for every
  v0.6.0-shaped operator request, breaking the byte-identical promise.
- Only ever emit `target_host` + `target_port` and use `targets[]` only
  when length ≥ 2 — accepted as the canonical encoding (this decision).

---

## R-003 — Health-state machine: passive default, active opt-in

**Decision**: Every multi-target rule runs a **passive** failure detector by
default. Three consecutive connect failures within a 30-second sliding window
mark a target Failed; two consecutive successes flip it back to Healthy. An
**active** TCP-connect probe runs only when the rule's
`health_check_interval_secs` is set, and feeds the same state machine.

**Rationale**:
- Passive detection is "free" — the data plane is already attempting to
  connect to the target; tracking the outcome costs one atomic counter
  bump per attempt.
- The 3-failures-in-30 s threshold is the standard L4 LB default
  (HAProxy's `inter 30s rise 2 fall 3` is the same shape) and avoids
  flapping under brief packet loss.
- Active probing exists for the case where end-user traffic is bursty
  (a target could be down for an hour before the next end-user
  connection notices). Operators who care about that specific case opt
  in; others pay nothing.
- Per-target probe interval lets an operator say "probe my critical DB
  target every 5 s but probe my secondary cache every 30 s" by setting
  one rule-level interval — not per-target — keeps the surface small.
- The "active probe shares the data plane's connect timeout" rule
  (FR-014) means an operator who tightens connect timeouts gets faster
  probes for free.

**Alternatives considered**:
- Always-on active probing — rejected as resource usage on a busy
  client (50 rules × 2 targets × 5 s probes = 20 syscalls/s baseline)
  is wasteful for the common case.
- L7 health checks (HTTP `GET /health`, gRPC health protocol) —
  rejected as out of scope for v1 (per spec assumptions). Operators can
  put a v1-protocol upstream like envoy in front of their target.
- Configurable thresholds (operator picks N, M, window) — rejected for
  v1 to keep the surface small. The defaults are visible; operators who
  need a different policy can request configurability in v0.8.

---

## R-004 — Selection policy: strict priority order, no weights

**Decision**: For every new connection (TCP) or new flow (UDP), select the
**lowest-priority-number target whose current health is Healthy**. Ties
within the same priority are broken by index in the targets list (stable
order). When **all targets are non-Healthy**, select the lowest-priority
target anyway (FR-007 — never silently drop).

**Rationale**:
- Priority order matches the operator's mental model ("primary, then
  secondary"). Weighted distribution would require a second concept
  (weights) and a runtime selection algorithm (random, round-robin,
  least-conn) that is significantly more code and more surface.
- The "always attempt the highest-priority target when all are
  Failed" rule prevents a silent-drop failure mode where the operator's
  end-users see "connection refused" with no upstream attempt logged.
  The attempt either succeeds (and flips the target back toward
  Healthy) or fails (and the error reaches the end-user the same way
  it would in v0.6.0 with a single-target failing rule).
- Stable tie-break by list index makes the feature deterministic and
  testable; operators can rely on "the rule I pushed at index 0 is the
  one that gets chosen".

**Alternatives considered**:
- Weighted random distribution — out of scope (spec).
- Round-robin within a priority — rejected for v1 to keep the
  semantic simple. The next release may revisit if a real workload
  demands it.
- Refuse to attempt when all are Failed — rejected; FR-007 explicitly
  rules this out.

---

## R-005 — UDP failover semantics: per-flow stickiness

**Decision**: For UDP rules, the chosen upstream is **bound at first packet**
of a new flow (keyed by `(source_addr, source_port)`) and stays for the
lifetime of that flow (until idle-evicted by the existing v0.4.0
`udp_flow_idle_secs` mechanism). Failover applies to **new flows**, not to
already-bound ones.

**Rationale**:
- Migrating an in-flight UDP flow mid-stream would require two-way
  coordination with the new upstream (which doesn't know the flow
  exists). The operator's expectation under failover is "new traffic
  goes to the healthy target" — not "every flow gets renegotiated".
- The existing v0.4.0 idle-eviction (60 s default) gives a natural
  upper bound on how long a flow can stay pinned to a now-Failed
  target: a chatty flow stays pinned (which is correct — it's still
  reaching the upstream); a silent flow gets evicted within 60 s and
  the next packet from the same `(addr, port)` re-selects.
- TCP gets failover-on-new-connection for the same reason — once
  the three-way handshake is done, the connection is locked in.

**Alternatives considered**:
- Migrate UDP flows mid-stream — rejected for the coordination problem
  above.
- Pin the upstream for `udp_max_flows_per_rule` flows even when the
  target Fails — rejected as it would compound failure (every flow
  to the failed target is dead until idle-evict).

---

## R-006 — Persistence: read-tolerant, write-canonical

**Decision**: `rules.json` (the operator-side persisted rules store) writes
rules in the **v0.7 canonical shape**: single-target rules emit `target_host`
+ `target_port` (v0.6.0 shape); multi-target rules emit a `targets[]` array.
The read path is tolerant of either shape on disk. There is no schema-version
bump.

**Rationale**:
- An operator who upgrades server from v0.6.0 to v0.7 reboots the
  daemon; `rules.json` on disk is in v0.6.0 shape; the v0.7 read path
  promotes `(target_host, target_port)` to a one-element targets list
  in memory; on the next write (push or remove of any rule), the file
  is rewritten in v0.7 canonical shape (single-target rules unchanged,
  any new multi-target rules emit `targets[]`).
- A schema-version bump would force a one-shot migration tool and a
  release-notes step. The tolerant-read approach is operationally
  simpler with no actual cost.
- Downgrading from v0.7 back to v0.6.0 with no multi-target rules
  in `rules.json` works trivially; downgrading with multi-target
  rules in `rules.json` is documented as **unsupported** (the v0.6.0
  reader would silently load a broken rule). Release notes mention
  this; operators who need rollback do `forward-server gen-token`
  on a fresh config dir for v0.6.0 if they truly need to roll back.

**Alternatives considered**:
- Bump the schema version + ship a migration tool — rejected as
  over-engineered for an additive change.
- Always write the new `targets[]` shape, deprecating the v0.6.0 shape
  on disk — rejected because it makes a v0.6.0→v0.7→v0.6.0 round-trip
  for a single-target rule fail (v0.6.0 would see an unknown field).

---

## R-007 — Server-side guard for old clients

**Decision**: When the operator pushes a multi-target rule to a client whose
`Hello.client_version` is < `0.7.0`, the server rejects the push at the
operator HTTP layer with `422 multi_target_unsupported_by_client`. The
server does NOT downgrade the rule to single-target.

**Rationale**:
- A v0.6.0 client decoding a multi-target rule on the wire would see
  `target_host == ""`, `target_port == 0`, and emit `RuleStatus.failed`
  with reason "target_resolution_failed" (it would try to resolve the
  empty hostname). That's a confusing failure mode for an operator who
  just pushed `--target a:80 --target b:80`.
- Failing fast at the operator API gives a clear, actionable error
  ("upgrade your client to v0.7+") before the rule ever leaves the
  control plane.
- Single-target rules to v0.6.0 clients continue to work unchanged
  (they don't trigger the new code path).

**Alternatives considered**:
- Auto-downgrade to the first target only — rejected as it silently
  changes the operator's intent (they asked for failover; they get a
  single target with no failover).
- No server-side guard, let the client's `RuleStatus.failed` surface —
  rejected as the failure cause "target_resolution_failed" is misleading
  for the actual root cause "client is too old".

---

## R-008 — Active probe: shared connect timeout, no probe-overlap

**Decision**: The active probe uses the same connect timeout the data plane
uses for end-user-driven connect attempts. Probes for the same target NEVER
overlap: if a probe is in flight when the next interval elapses, the
probe-scheduler defers the next probe until the current one completes (or
times out).

**Rationale**:
- Sharing the connect timeout means an operator who tightens timeouts
  for end-user traffic gets faster probes "for free" — and an operator
  who relaxes them gets less aggressive probes. One knob, two effects.
- Defer-on-overlap is the standard pattern for active probes (HAProxy,
  Envoy both do this) — without it, a slow target would accumulate
  in-flight probes, each holding an fd, and starve the rest of the
  probe schedule.
- A separate probe timeout would be a second knob the operator has to
  reason about (and would, in practice, want to keep equal to the
  connect timeout — so just use the connect timeout).

**Alternatives considered**:
- Independent probe timeout — rejected for surface-bloat (above).
- Allow overlap — rejected for the fd-exhaustion failure mode.

---

## R-009 — Per-target metrics cardinality

**Decision**: The Prometheus `/metrics` endpoint exposes `forward_rule_target_failovers_total{client, rule}` (rule-level) but does NOT expose per-target counters. Per-target byte / connection / health counters are surfaced only via `rule-stats --per-target` (CLI) and the Web UI rule-detail page.

**Rationale**:
- Per-target series at scale (50 rules × 5 targets × 4 counter types =
  1,000 series per host) blow the cardinality budget that 002-port-
  range-forward and 004-udp-forward set (both also keep per-port detail
  query-only for the same reason).
- The rule-level `target_failovers_total` is enough to alert on the
  one operationally-critical event ("did failover happen?") without
  needing per-target detail in the alert path.
- The Web UI is the operator's primary surface for the "which target
  is healthy and how much traffic" view (SC-005); query-only per-target
  counters there don't burden the metrics path.

**Alternatives considered**:
- Per-target Prometheus series with a `target_index` label — rejected
  for the cardinality budget.
- Per-target series gated behind an opt-in server flag — rejected as
  yet-another-knob; the existing `--per-target` CLI surface is enough.

---

## R-010 — Web UI rule-detail rendering: badges + per-target byte counters

**Decision**: The Web UI rule detail page renders the targets list as a
table with columns `[priority, host:port, health badge, last-failure,
last-success, bytes in, bytes out, conns]`. Health badges use the same
"Healthy / Failed" colour system as the rule-state badges introduced in 006.
Per-target byte counters update on the existing 5 s SSE tick already
plumbed for `RuleStatsSnapshot` (no new SSE channel needed).

**Rationale**:
- Reusing the 006 SSE channel means no new server endpoint and no new
  client-side connection. The 006 `StatsSnapshot` JSON shape grows two
  fields (per-target counters + target_failovers_total); the SPA reads
  them out of the existing snapshot.
- The 5 s cadence is already proven operationally and is the same
  cadence the per-rule live counters use today.
- Health badges reuse the existing badge component family (no new
  visual primitives).

**Alternatives considered**:
- A separate per-target SSE channel — rejected as it adds a server
  endpoint, doubles the SSE connection cost, and complicates the auth
  story (per-target ownership = per-rule ownership; same gate).
- Polling the per-rule stats endpoint at 1 s for sub-second-fresh
  health view — rejected as 5 s is plenty for "is the primary still
  Healthy?" and 1 s polling would multiply server load.

---

## R-011 — Rule update semantics: full replace, no partial diff

**Decision**: A push-rule that targets an existing `rule_id` is treated as a
**full replace** of the rule's body (including the targets list). There is no
"add a target" or "remove a target" mutation in v1.

**Rationale**:
- The v0.6.0 push-rule semantic is already "full replace" (you push the
  rule's whole body; the server activates it). Extending that to
  targets is consistent.
- A diff-based mutation surface ("add this one target", "remove that
  one target") would need a new HTTP route, a new validation path, and
  a new conflict-resolution rule for "what happens if two operators
  add a target at once?". Out of scope for v1.
- An operator who wants to change the targets list re-pushes the rule;
  the existing graceful-drain semantics handle in-flight connections.

**Alternatives considered**:
- Per-target hot-add / hot-remove — rejected as out of scope for v1.

---

## R-012 — Health-state lifecycle vs rule lifecycle

**Decision**: Per-target health state lives in the **rule's runtime state**
on the client. When a rule is removed (or replaced via push-rule), all
per-target health state for that rule is dropped. When the client restarts,
all per-target health state is reset to "Healthy" — the first connection
after restart is the first health signal for each target.

**Rationale**:
- Persisting health across restarts is a "neat-to-have" that adds a
  serialization surface (where do we store it? how often do we flush?
  what about across host migrations?). The first connection after
  restart treating all targets as Healthy means **at most one** end-user
  connection sees the Failed-but-not-yet-detected state — same as v0.6.0
  behaviour with a single Failed target.
- Replacing a rule resets state because the new rule may have a
  completely different targets list; carrying state from the old list
  is meaningless.

**Alternatives considered**:
- Persist health to disk every N seconds — rejected for the surface
  growth + the trivial first-connection-after-restart cost.

---

## R-013 — Defaults summary (operator-visible)

| Knob | Default | Source | Operator override |
|---|---|---|---|
| `passive_failure_threshold` | 3 consecutive failures | R-003 | not configurable in v1 |
| `passive_failure_window` | 30 s | R-003 | not configurable in v1 |
| `passive_recovery_threshold` | 2 consecutive successes | R-003 | not configurable in v1 |
| `health_check_interval_secs` | unset (no active probe) | R-003 | per-rule, in `Rule.health_check_interval_secs` |
| Connect timeout (probe + data plane) | inherits rule-level / server default | R-008 | unchanged from v0.6.0 |
| Selection policy when all Failed | always attempt highest-priority | R-004 / FR-007 | not configurable |

**Rationale**: Keep the v1 surface small. Every knob added now is one we
have to support indefinitely. R-003's defaults are the standard L4-LB
shape and align with operator expectations. If a real workload demands a
custom threshold, surface it in v0.8 with an explicit Spec Kit feature.
