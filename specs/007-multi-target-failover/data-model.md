# Data Model: Multi-target failover

**Phase**: 1 (design) | **Feature**: 007-multi-target-failover | **Date**: 2026-05-08

This document derives the entities, validation rules, and state transitions from `spec.md` (Key Entities, FR-001..FR-021). Wire-shape and HTTP-body details live in `contracts/`.

---

## 1. Entity: `Target`

A single upstream within a rule. **Inline-on-rule** — not a first-class top-level entity, no separate persistence file, no separate ID space.

### Fields

| Field | Type | Required | Default | Source / Constraint |
|---|---|---|---|---|
| `host` | `string` | yes | — | IP literal (v4 or v6) OR DNS name. Same syntax accepted by the v0.3.0 resolver — empty string rejected at validation. |
| `port` | `u16` | yes | — | Range `1..=65535`. `0` rejected at validation. |
| `priority` | `u32` | no | row index at push time | Lower number = higher priority. Operators that omit `priority` get the row index. Two targets MAY share a priority value — ties broken by row order (deterministic). |

### Validation rules

- **V-T1**: `host` MUST be non-empty and match the resolver's accepted-host-syntax check (reuses `forward-core::parse_host`).
- **V-T2**: `port` MUST be in `1..=65535`.
- **V-T3**: Within a rule, no two targets MAY share the same `(host, port)` pair (FR-005). Validator runs at push time on `forward-server`; client trusts server-validated rules.
- **V-T4** *(implicit)*: maximum targets per rule is **8**. Operationally a rule with > 8 targets is almost certainly a misuse (priority order beyond rank ~3 has no sensible semantics with the strict-priority policy). Cap is enforced at the HTTP / CLI / proto-validation layer; future weighted policy can lift it.

### Relationships

- **Rule 1—N Target**: a rule owns its targets inline. Targets do not exist outside a rule.

---

## 2. Entity: `Rule` (extended from v0.6.0)

The v0.6.0 rule shape gains two additive fields. Everything else (id, listen port range, protocol, owner, DNS preference) is inherited unchanged.

### New fields

| Field | Type | Required | Default | Source / Constraint |
|---|---|---|---|---|
| `targets` | `Vec<Target>` | yes (≥ 1) | one-element list synthesised from legacy `target_host` + `target_port` if absent | Length 1..=8. Length 1 MUST round-trip byte-identically to a v0.6.0 single-target rule (FR-002). |
| `health_check_interval_secs` | `Option<u32>` | no | `None` (no active probing) | When `Some(n)`, n MUST be in `1..=3600`. When `None`, system MUST NOT schedule any probe task (FR-015). |

### Back-compat encoding contract

The on-the-wire `Rule` proto carries BOTH the legacy `target_host` / `target_port` fields AND the new `targets` field — but a given rule populates exactly one shape:

- **Single-target rules** (length 1): `target_host` + `target_port` populated; `targets` left empty (proto3 zero-length repeated). Bytes-on-wire identical to v0.6.0.
- **Multi-target rules** (length ≥ 2): `targets` populated; `target_host` left as empty string and `target_port` left as `0`. The first target's `(host, port)` is NOT mirrored into the legacy fields — readers detect "multi-target rule" by `!targets.is_empty()`.

This encoding rule (R-002 in `research.md`) keeps the single-target hot path's serialised form unchanged.

### Validation rules

- **V-R1**: `targets.len()` MUST be in `1..=8`. `0` is the "send legacy shape" convention and is converted to a one-element list at parse time.
- **V-R2**: When `targets.len() >= 2`, the legacy `target_host` MUST be empty and `target_port` MUST be 0 — server rejects any rule that supplies both shapes (FR-004).
- **V-R3**: When `targets.len() == 0` AND `target_host`/`target_port` are also unset, the server rejects with `targets_required` (FR-004 "neither" branch).
- **V-R4**: All targets share the rule's protocol (TCP or UDP) — there is no per-target protocol override.
- **V-R5**: All `Target` validation rules (V-T1..V-T4) apply.
- **V-R6**: `health_check_interval_secs`, when set, in `1..=3600` (1 second to 1 hour).

### Persistence

- `forward-server/persistence.rs` writes the `Rule` shape verbatim into `rules.json` (atomic write, mode 0600).
- **Read tolerance**: any v0.6.0-shaped rule (single-target only) loaded from disk is promoted to a one-element `targets` list at read time. No schema-version bump.
- Per-target health state is **NOT persisted** — see `HealthState` lifecycle below.

---

## 3. Entity: `HealthState` (per-target, in-memory only)

Lives inside `forward-client/src/forwarder/failover.rs`. One instance per `(rule_id, target_index)` pair. Exists only when `targets.len() >= 2` — single-target rules don't allocate any health state.

### Fields

| Field | Type | Initial | Notes |
|---|---|---|---|
| `state` | `Healthy \| Failed` | `Healthy` | New rule activation starts every target Healthy (assumption "failover state is ephemeral"). |
| `consecutive_failures` | `u32` | `0` | Incremented on each connect failure attributed to this target. Reset to `0` on any success. |
| `consecutive_successes` | `u32` | `0` | Incremented on each connect success attributed to this target while in `Failed` state. Used only for the Failed→Healthy transition. Reset to `0` on any failure or after the transition. |
| `failure_window_start` | `Option<Instant>` | `None` | Timestamp of the first failure in the current 30-second sliding window. Set when `consecutive_failures` increments from 0. |
| `last_failure_at` | `Option<SystemTime>` | `None` | Wall-clock time of the most recent failure (for operator surface). |
| `last_success_at` | `Option<SystemTime>` | `None` | Wall-clock time of the most recent success (for operator surface). |
| `bytes_in` | `u64` | `0` | Per-target lifetime byte counter (rule-lifetime, resets on rule replace / client restart). |
| `bytes_out` | `u64` | `0` | Same. |
| `connections_accepted` | `u64` | `0` | TCP connections (or UDP flows) routed through this target. |

### State machine

```text
                  ┌──────────────────────────────────────────────────┐
                  │                                                  │
                  │ 3 failures within a 30 s sliding window          │
                  ▼                                                  │
        ┌──────────────────┐                                ┌────────┴─────────┐
        │     Healthy      │ ──────────── failure ────────▶ │      Failed      │
        │ (selectable for  │                                │ (skipped during  │
        │ new connections) │ ◀─── 2 consecutive successes ─ │ selection unless │
        └──────────────────┘                                │ all are Failed)  │
                                                            └──────────────────┘
```

### Transitions

| From | To | Trigger | Side effects |
|---|---|---|---|
| `Healthy` | `Healthy` | success | `consecutive_failures = 0`, `failure_window_start = None`, `last_success_at = now` |
| `Healthy` | `Healthy` | failure (count < 3 OR outside window) | `consecutive_failures += 1`, `failure_window_start` set if `None` or window expired-and-restarted, `last_failure_at = now` |
| `Healthy` | `Failed` | failure (count reaches 3 within window) | `state = Failed`, `consecutive_successes = 0`, `last_failure_at = now`, `target_failovers_total += 1`, emit log `event = "rule.target.health_changed"` (Healthy→Failed) |
| `Failed` | `Failed` | failure | `consecutive_failures += 1`, `consecutive_successes = 0`, `last_failure_at = now` |
| `Failed` | `Failed` | success (count < 2) | `consecutive_successes += 1`, `last_success_at = now` |
| `Failed` | `Healthy` | success (count reaches 2) | `state = Healthy`, `consecutive_failures = 0`, `consecutive_successes = 0`, `failure_window_start = None`, `last_success_at = now`, `target_failovers_total += 1`, emit log `event = "rule.target.health_changed"` (Failed→Healthy) |

### Lifecycle (R-012 in `research.md`)

- **Birth**: on rule activation, when `targets.len() >= 2`. Initial `state = Healthy` for all targets.
- **Reset on rule replace**: a push of a new rule with the same `rule_id` (current v0.6.0 update semantics) tears down all per-target health for that rule and re-births fresh state.
- **Death**: on rule deletion or client shutdown.
- **Not persisted**: ephemeral by design (assumption "failover state is in-process and ephemeral").

### Failure / success attribution (active vs passive)

| Source | Counts as |
|---|---|
| Data-plane TCP connect failure (passive) | failure for the attempted target |
| Data-plane TCP connect success (passive) | success for the chosen target |
| UDP first-packet upstream-bind failure | failure for the attempted target |
| UDP first-packet upstream-bind success | success for the chosen target |
| Active TCP-connect probe failure | failure for the probed target |
| Active TCP-connect probe success | success for the probed target |
| Bytes flowing on an in-flight connection | NOT a health signal (only the initial connect outcome counts) |

### Selection algorithm (per new connection / new UDP flow)

```text
Inputs: targets (sorted by (priority, row_index) ASC), per-target HealthState

candidates ← targets.iter().filter(|t| h(t).state == Healthy)
if candidates.is_empty():
    # FR-007: still attempt, don't silently drop
    selected ← targets[0]   # highest-priority overall
else:
    selected ← candidates.first()
return selected
```

For the single-target hot path, this whole module is bypassed: the rule activation matches `targets.len() == 1` and goes straight to the v0.6.0 `connect_target(targets[0])` call (Constitution Principle II).

---

## 4. Entity: `RuleStats` (extended from v0.6.0)

Existing rule-level fields unchanged. Two additive fields:

| Field | Type | Source |
|---|---|---|
| `target_failovers_total` | `u64` | counter incremented by every health-state transition (Healthy↔Failed) on this rule |
| `per_target` | `Vec<TargetStats>` | one entry per target in priority order |

### `TargetStats` shape

| Field | Type | Source |
|---|---|---|
| `index` | `u32` | row index in `Rule.targets` (stable for the rule's lifetime) |
| `host` | `string` | mirror of `Target.host` |
| `port` | `u32` | mirror of `Target.port` |
| `priority` | `u32` | mirror of `Target.priority` |
| `health` | `Healthy` \| `Failed` | from `HealthState.state` |
| `consecutive_failures` | `u32` | from `HealthState` |
| `last_failure_at` | `Option<SystemTime>` | from `HealthState` |
| `last_success_at` | `Option<SystemTime>` | from `HealthState` |
| `bytes_in` | `u64` | from `HealthState` |
| `bytes_out` | `u64` | from `HealthState` |
| `connections_accepted` | `u64` | from `HealthState` |

### Cardinality discipline (FR-018, R-009)

- **Default `/metrics`**: only `forward_rule_target_failovers_total{client, rule}` is added per rule. Per-target counters are NOT exported as default series.
- **Per-target stats**: surfaced behind `--per-target` on the CLI (`rule-stats`) and on the Web UI rule detail page (`/rules/{id}`). Both fetch `RuleStats` with `per_target` populated; default callers get `per_target = vec![]` to keep payloads small.

---

## 5. Cross-entity invariants

- **I-1**: `RuleStats.per_target.len() == Rule.targets.len()` whenever `per_target` is requested.
- **I-2**: `target_failovers_total` is monotone non-decreasing for the lifetime of a rule activation; resets only on rule replace / client restart.
- **I-3**: A rule with `targets.len() == 1` MUST have `per_target.len() == 0` in default RuleStats responses (no per-target state allocated; preserves the v0.6.0 statistics shape exactly).
- **I-4**: A rule with `targets.len() >= 2` MUST emit a structured log line `event = "rule.target.health_changed"` on every state transition. This ride on the existing `tracing` JSON pipeline; no new sink, no new schema beyond the `event` discriminator.

---

## 6. Notes on what is NOT modeled

- **Weight, distribution, hash-ring, least-connections** — out of scope per spec assumptions. Selection is strict priority + Healthy-only (with all-Failed fallback).
- **Cross-rule failover / rule groups** — out of scope. Each rule is independent.
- **L7 (HTTP / gRPC) health checks** — out of scope. TCP-connect only.
- **Per-target connection caps / circuit breakers** — out of scope; future work.
- **Persistent health across restarts** — out of scope (assumption: ephemeral).
