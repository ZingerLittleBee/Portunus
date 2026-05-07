# Implementation Plan: Port-Range Forwarding Rules

**Branch**: `002-port-range-forward` | **Date**: 2026-05-07 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/002-port-range-forward/spec.md`

## Summary

Add a single `Rule` shape that names a contiguous listen-port range mapped
same-offset onto a contiguous target-port range on one upstream host
(e.g., `30000–30050 → upstream:30000–30050`). The wire / persistence /
HTTP / CLI surfaces evolve **additively**: the existing `Rule` gains two
optional fields `listen_port_end` and `target_port_end`; absent = today's
single-port behavior, present = range. Internally the v0.1.0 single-port
rule becomes the degenerate `range_size = 1` case so the forwarder, the
rule store, and persistence all share one code path. Per-rule
observability (`rule-stats`, Prometheus) stays aggregate (matching today's
cardinality) with a new opt-in CLI `rule-stats <id> --per-port` for
on-demand diagnosis.

The change touches: `proto/forward.proto` (additive fields), the server
rule store and conflict checker (range-aware port-collision check),
operator HTTP + CLI (`push-rule` accepts range syntax, `rule-stats
--per-port` flag), the client forwarder (binds N listeners under one
`RuleId`, atomic all-or-nothing activation, aggregated stats), and the
persistence layer (existing rules.json keeps loading because the new
fields are optional).

## Technical Context

**Language/Version**: Rust 1.88 (constitution-pinned MSRV via `tonic`).
**Primary Dependencies**: `tokio` (async runtime, current-thread + `JoinSet`
                          for the per-port listeners), `tonic` 0.14
                          (gRPC), `prost` (proto codegen), `rustls`,
                          `prometheus` (registry already wired), `axum`
                          (operator HTTP).
**Storage**: existing `tokens.json` and (already-present-or-extended)
             rules persistence layer; range fields are optional so v0.1.0
             rule files load unmodified (per FR-005 / spec § Clarifications).
**Testing**: `cargo test` per crate (unit + integration); `forward-e2e`
             integration crate exercises server + client + real sockets;
             contract tests live under `tests/contract/` in
             `forward-proto`.
**Target Platform**: Linux primary (musl static binary), macOS for
                     development. No Windows.
**Project Type**: Multi-crate Cargo workspace (`forward-core`,
                  `forward-proto`, `forward-auth`, `forward-server`,
                  `forward-client`, `forward-e2e`). No frontend.
**Performance Goals**: A 100-port range MUST achieve `push → all 100
                       listeners bound → first byte through any port` in
                       the same wall-clock budget as the single-port
                       quickstart (SC-001 ≤ 5 minutes from zero on a
                       fresh host pair, ≤ 5 seconds for the push step on
                       a running pair). Per-port forwarding throughput
                       MUST NOT regress versus v0.1.0 single-port
                       (Constitution II).
**Constraints**: All-or-nothing activation (no partial bind on failure);
                 default range cap **1024 ports** (FR-008, matches Linux
                 default soft `RLIMIT_NOFILE`); operator-configurable;
                 Prometheus cardinality MUST be independent of range size
                 (FR-009 / SC-002).
**Scale/Scope**: Cap of 1024 ports per single rule means a worst-case
                 rule consumes ≤ 1024 listening sockets + ≤ 2 fds per
                 in-flight connection. With the existing fd-headroom
                 assumption (Tokio + accepted connections), a single
                 client should comfortably run a handful of max-size
                 range rules concurrently.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Pass? | Notes |
|---|---|---|
| I. Security by Default (TLS + bearer token, no plaintext) | ✅ | No transport changes. The existing TLS + per-client bearer-token control plane carries range rule updates exactly the same way as single-port. No new credentials, no new auth surface. |
| II. Performance Is a Feature | ✅ | Forwarder hot path is unchanged: each port in the range still runs the same `accept → copy_bidirectional` loop as today. New work is at install time only (bind N sockets serially under one rule). Will ship a criterion benchmark for the bind-fan-out path under `forward-client/benches/range_install.rs` showing the cost scales linearly with range size and stays under the SC-001 budget. |
| III. Test-First Discipline | ✅ | Will write contract tests for the additive proto shape (proto round-trips with absent / present range fields produce identical wire bytes for the legacy case), unit tests for range validation + range-vs-range overlap, and integration tests in `forward-e2e` for "push a 100-port range, drive bytes through 3 random ports, observe one row in stats, remove, all bound ports release". |
| IV. Observability & Operability | ✅ | Per-rule labels (`client_name`, `rule_id`) are unchanged → Prometheus cardinality matches single-port (SC-002). Aggregate counters cover the range. `rule-stats --per-port` is a separate CLI path that reads server-side per-port detail snapshots over the same loopback HTTP API; no Prometheus exposure of per-port series. Range install / removal emits the existing `rule.activated` / `rule.removed` audit events (one per range rule, not one per port). |
| V. Multi-Tenant Isolation | ✅ | Range rules are still owned by one `(client_name, rule_id)` pair. Per-tenant policy (allowed port ranges per spec 001 / future tenant work) gets enforced at push time against the entire range, not just the start port. The conflict check extends today's `(client_name, listen_port)` index into a `(client_name, listen_port_range)` interval check — see Phase 1. |

**Gate result**: PASS. No constitutional violations; nothing to track in
the Complexity Tracking table.

## Project Structure

### Documentation (this feature)

```text
specs/002-port-range-forward/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/
│   ├── forward.proto    # Phase 1: additive proto diff (overlay vs MVP)
│   ├── operator-api.md  # Phase 1: HTTP + CLI surface deltas
│   └── persistence.md   # Phase 1: rules persistence schema deltas
├── checklists/
│   └── requirements.md  # Already produced by /speckit-specify
└── tasks.md             # /speckit-tasks output (not created here)
```

### Source Code (repository root)

```text
proto/
└── forward.proto                         # add optional listen_port_end, target_port_end

crates/
├── forward-core/src/
│   └── port_range.rs                     # NEW: PortRange newtype + helpers
├── forward-proto/                        # codegen consumes the additive .proto
├── forward-server/src/
│   ├── rules.rs                          # extend Rule + ServerRuleStore for ranges + interval conflict check
│   ├── operator/
│   │   ├── cli.rs                        # parse listen[--end] / target[--end] flags
│   │   ├── http.rs                       # accept + return optional *_port_end fields
│   │   └── rule_cli.rs                   # accept range syntax; pass --per-port through
│   ├── operator/per_port_stats.rs        # NEW: cache the client's per-port detail report
│   └── metrics.rs                        # unchanged labels (proves SC-002)
├── forward-client/src/
│   ├── forwarder/
│   │   ├── mod.rs                        # extend ClientRule with optional range, fan-out under one task
│   │   ├── range.rs                      # NEW: bind-all-or-nothing helper used by mod.rs
│   │   └── stats.rs                      # add per-port atomic counters; aggregate exposed via existing API
│   └── control.rs                        # marshal RangeRule from RuleUpdate; no behavior change for single-port
├── forward-e2e/tests/
│   └── range_smoke.rs                    # NEW: 100-port range smoke (SC-001), reuses MVP harness
└── forward-client/benches/
    └── range_install.rs                  # NEW: criterion benchmark, install fan-out cost
```

**Structure Decision**: Continue the existing multi-crate Cargo workspace
laid out in spec 001-tcp-forward-mvp. No new crates — every change lives
in an existing crate (extension, not addition). The two named NEW
modules (`forward-core/port_range.rs`,
`forward-client/forwarder/range.rs`,
`forward-server/operator/per_port_stats.rs`) keep range-specific code
isolated so the v0.1.0 single-port path stays readable.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

(Empty — Constitution Check passed without violations.)
