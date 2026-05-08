# Implementation Plan: Multi-target failover

**Branch**: `007-multi-target-failover` | **Date**: 2026-05-08 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification at `specs/007-multi-target-failover/spec.md`

## Summary

A forwarding rule today carries a single `(target_host, target_port)`. v0.7 extends that to an ordered list of targets with priority-ordered failover and per-target health tracking, all client-side. The control plane only carries the list — selection happens inside the forwarder. The data-plane hot path for single-target rules stays byte-identical to v0.6.0.

The technical approach: add `repeated Target targets = 9` (an additive proto field) plus new optional health-check tunables on `Rule`; add per-target fields to `RuleStats`; introduce a small per-target health-state machine in `forward-client/src/forwarder` that runs only when `targets.len() > 1`. The 005 RBAC envelope is unchanged (targets are not part of the grant). The 006 Web UI gains a "targets" rendering on the rule push form and rule detail page.

## Technical Context

**Language/Version**: Rust 1.88 (workspace MSRV, driven by tonic).
**Primary Dependencies**: same as v0.6.0 — tokio (async runtime), tonic (gRPC), axum (HTTP), rust-embed (SPA embed), prometheus (metrics), tracing + tracing-subscriber (structured logs). No new crate adds.
**Storage**: server-side `rules.json` (atomic-write, mode 0600) gains a `targets[]` per rule. Back-compat: any v0.6.0 rule (single `target_host`/`target_port`) is read as a one-element targets list at load time. Per-target health state is in-memory only on the client (FR-016 says it survives queries, not restarts).
**Testing**: `cargo test --workspace --tests`, criterion benchmark gate (`cargo bench -p forward-client --bench data_plane`), Vitest + Playwright in `webui/`. Existing `forward-e2e` + `forward-server/tests/*` patterns extend.
**Target Platform**: Linux (primary), macOS (development). Single-static-binary deploy preserved.
**Project Type**: workspace of 6 Rust crates + 1 Vite/React frontend. Identical to v0.6.0.
**Performance Goals**: SC-003 — single-target hot path ≤ 1% regression on the existing TCP forwarder data-plane benchmark vs the v0.6.0 baseline. SC-002 — primary recovery picked up on the next new connection (no extra latency on the in-flight connection).
**Constraints**:
- Constitution Principle II — single-target hot path byte-identical to v0.6.0. Multi-target lives in a separate code path entered via `match targets.len() { 1 => fast_path, _ => failover_path }`.
- Constitution Principle V — RBAC envelope unchanged. Grants gate `(client, listen-port range, protocol)` only; targets are operator's free choice within their grant (FR-021).
- v0.3.0 DNS resolver — applies per-target unchanged. The shared resolver layer doesn't need a code change; the failover loop just calls `connect_target(t)` per attempt.
- v0.4.0 UDP — failover applies on the first inbound packet of a new flow. Once a flow is bound to a target, it sticks until idle-evict.
**Scale/Scope**: a typical operator deploys 1–50 multi-target rules per client. Per-target byte counters cost O(targets) memory per rule (~64 B per target) — negligible at expected scale. Active probes (opt-in) cost one tokio task per rule with `health_check_interval_secs`-set, sleeping most of the time.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|---|---|---|
| I. Security by Default | ✅ pass | No new auth surface. Targets list flows through the existing TLS+bearer seam on both `Channel` (gRPC) and `POST /v1/rules` (operator HTTP). Active probes are plain TCP-connect to operator-supplied targets — no new credential, no new trust anchor. |
| II. Performance Is a Feature | ✅ pass (with bench gate) | Single-target hot path stays byte-identical via `targets.len() == 1` branch at rule activation. SC-003 codifies ≤ 1% bench regression as a release gate; the existing CI bench-regression workflow (`.github/workflows/bench.yml`) already enforces 25% gates and will catch any drift. |
| III. Test-First Discipline | ✅ pass | Phase 1 contracts include: `targets-wire-compat.rs` (proto round-trip), `rules_multi_target_contract.rs` (HTTP body validation), `failover_state_contract.rs` (health-state transitions). These ship before the implementation per the project pattern. |
| IV. Observability & Operability | ✅ pass | New Prometheus counter `forward_rule_target_failovers_total{client, rule}` (rule-level, not per-target — keeps cardinality bounded; FR-018). Per-target counters surface only on demand via `rule-stats --per-target` (FR-017) and the Web UI rule detail page (FR-019). All health-state transitions emit a structured `event = "rule.target.health_changed"` log line for ops correlation. Graceful drain unchanged. |
| V. Multi-Tenant Isolation | ✅ pass | RBAC envelope unchanged (FR-021). Existing `enforce_grant` path doesn't inspect targets; `enforce_read` and `enforce_write` continue to gate on `(client, listen-port range, protocol)` only. The new targets field is part of the rule body the owner controls — peers see it via the existing rule-projection that already filters by ownership. |

**Post-Phase-1 re-check**: still passes. The data-model + contract design preserves all five principles. No items in Complexity Tracking.

## Project Structure

### Documentation (this feature)

```text
specs/007-multi-target-failover/
├── plan.md                         # This file
├── research.md                     # Phase 0 — decisions on health-state machine, wire shape, persistence
├── data-model.md                   # Phase 1 — Target entity, Rule extension, health-state machine
├── contracts/
│   ├── proto-rule-extension.md     # Wire-level Rule + RuleStats + Target additions (proto3)
│   ├── operator-api.md             # POST /v1/rules body shape + GET /v1/rules/{id}/stats response
│   └── ui-routes.md                # Web UI form + rule-detail rendering deltas
├── quickstart.md                   # Two-host walkthrough — push multi-target rule, kill primary, observe failover, recover
├── checklists/requirements.md      # Already created by /speckit-specify
└── tasks.md                        # /speckit-tasks output
```

### Source Code (repository root)

```text
crates/
├── forward-proto/
│   ├── proto/forward.proto         # +Target message, +repeated Target targets, +RuleStats per-target fields, +Rule.health_check_interval_secs
│   └── tests/targets_wire_compat.rs # NEW — round-trip + back-compat assertions for v0.6.0 Rule shape
├── forward-core/src/
│   ├── target.rs                   # NEW — Target struct, validation (host, port, priority)
│   └── rules.rs                    # extend Rule with `targets: Vec<Target>` (Default::default keeps single-target back-compat)
├── forward-server/src/
│   ├── operator/http.rs            # POST /v1/rules accepts BOTH legacy + new shape; GET response carries targets[] + per-target health
│   ├── operator/rule_cli.rs        # CLI push-rule gains repeatable --target / --targets-json
│   └── persistence.rs              # rules.json read path tolerates v0.6.0 single-target rules; write path emits targets[]
└── forward-client/src/
    ├── forwarder/
    │   ├── mod.rs                  # branch on targets.len() — single-target fast path unchanged; multi-target enters failover module
    │   ├── failover.rs             # NEW — per-target health state machine, target selection, passive failure tracking
    │   └── probe.rs                # NEW — opt-in active TCP-connect prober (skipped when health_check_interval_secs unset)
    └── stats.rs                    # extend RuleStats with target_failovers_total + per-target counters

webui/src/
├── pages/RulePush.tsx              # form gains "Add another target" with priority slot
├── pages/RuleDetail.tsx            # render targets list with health badges + per-target byte counters
└── api/types.ts                    # Target, TargetHealth, RuleWithTargets shape

specs/007-multi-target-failover/
├── plan.md                         # This file (already linked above)
└── ...
```

**Structure Decision**: extend the existing 6-crate workspace + `webui/` SPA. No new crate; no new top-level directory. The `failover.rs` and `probe.rs` modules live inside `forward-client/src/forwarder/` so the single-target hot path doesn't even pull them into compilation linkage decisions (they're behind a `match` arm).

## Complexity Tracking

> **No violations.** Constitution Check passes both pre- and post-Phase-1.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| (none) | — | — |
