# Implementation Plan: Connection Rate Limiting & QoS

**Branch**: `011-rate-limiting-qos` | **Date**: 2026-05-09 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/011-rate-limiting-qos/spec.md`

## Summary

v0.11 adds **per-rule** and **per-owner** quality-of-service caps on top of
the v0.10 codebase. Four cap dimensions per scope: bandwidth (bytes/sec
ingress and egress), new-connection rate (TCP conn/sec or UDP flow/sec), and
concurrent connection / flow count. Each cap is independently optional;
absent fields preserve v0.10 wire and behavioural semantics byte-for-byte.

Enforcement lives in the data plane on `forward-client`. Bandwidth caps
**throttle** in-flight reads/writes via a token bucket; connection-rate and
concurrent caps **reject** new connections (TCP RST after accept; UDP packet
drop before NAT binding) before the v0.7 multi-target selection step. The
rate limiter never closes existing connections — including when a hot-reload
lowers a concurrent cap below the live count, where the excess drains
gracefully (Q4).

Per-owner caps add a second token-bucket layer keyed `(client, owner)` whose
ceiling binds before per-rule caps (Q1). The control surface for per-owner
envelopes is a nested operator API resource at
`/clients/{id}/owners/{owner_id}/rate-limit`, exposed in the Web UI as an
"Owner quotas" tab on the client detail page (Q5).

Token-bucket model is hand-rolled `{rate, burst}` over `tokio` primitives —
zero new workspace dependencies (Constitution II). Burst defaults to one
second of `rate` and is hidden from common UI; an optional `burst_*` field
on every cap lets advanced operators override (Q2). Listener policy is
**accept-then-RST** rather than listener-pause to avoid penalising other
rules sharing a v0.7/v0.9 listener (Q3).

The wire delta is additive: `Rule.rate_limit = 12` (next free after v0.9),
`Target` unchanged, `RuleStats.rate_limit = 16` (next free after v0.9),
`StatsReport.owner_rate_limit_stats = 4` (next free after v0.9). Capability
gate (`rate_limit_unsupported_by_client`) refuses pushing any rule with
cap fields — or any owner envelope mutation that would target — a client
whose `Hello.client_version < 0.11.0`. SQLite schema gains migration V005
adding optional cap columns to `rules` and a new `rate_limit_owner` table
keyed `(client_name, owner_id)`; schema-version range shifts `[1,3] → [1,4]`.

## Technical Context

**Language/Version**: Rust 1.88 (workspace MSRV)

**Primary Dependencies**:
- New workspace deps: **none**
- Existing crates touched: `tokio` (timers + atomics for token buckets),
  `prost`, `tonic`, `serde`, `prometheus`, `rusqlite`, `tracing`,
  `refinery` (one new migration)
- Existing codepaths touched: `proto/forward.proto`, `forward-core`
  (per-rule and per-owner cap envelope types), `forward-server`
  (rule + owner-cap persistence, validation, capability gate, operator
  HTTP API, metrics fold for owner stats), `forward-client` (token-bucket
  enforcement layer in front of the v0.7 multi-target dial path and in
  the bidirectional copy loop), Web UI (rules table + editor + new
  Owner quotas tab)

**Storage**: SQLite migration V005 adds eight optional columns to `rules`
(`rl_bandwidth_in_bps`, `rl_bandwidth_out_bps`,
`rl_new_connections_per_sec`, `rl_concurrent_connections`, plus four
companion `rl_burst_*` overrides) and creates a new
`rate_limit_owner(client_name TEXT, owner_id TEXT, …)` table with the
same eight columns plus a composite primary key. All cap columns are
nullable; null = unlimited.

**Testing**:
- `cargo test` — unit + integration + contract coverage
- `forward-proto` wire-compat tests for the new `RateLimit` and
  `RateLimitStats` messages and the `Rule.rate_limit` / `RuleStats.rate_limit`
  / `StatsReport.owner_rate_limit_stats` field tags
- `forward-server` contract tests for validation (cap = 0 rejected,
  burst override range, capability gate, owner envelope CRUD)
- `forward-client` integration tests for token-bucket convergence,
  reject path (TCP RST, UDP drop), graceful drain under hot-reload,
  and per-owner ceiling binding before per-rule
- `forward-e2e` two-host scenario covering owner A vs owner B
  starvation isolation
- `criterion` benches: data-plane regression for the no-cap path
  (Constitution II) and token-bucket overhead at the 1 MB/s, 10 MB/s,
  100 MB/s anchor points

**Target Platform**: Linux primary, macOS development
**Project Type**: Cargo workspace with server/client binaries + embedded Web UI

**Performance Goals**:
- A rule with no cap fields set is byte-identical to v0.10 on the wire
  and ≤ 2% throughput / ≤ 5% per-connection setup-latency regression
  vs v0.10 on the existing data-plane bench harness (SC-004).
- Bandwidth-cap convergence: ±10% of target rate over a 20s window for
  caps in {100 KB/s, 1 MB/s, 10 MB/s, 100 MB/s} (SC-001).
- Concurrent cap is exact (±0) and rejection latency ≤ 50ms (SC-002).
- New-conn-rate cap is ±10% over a 60s window for R ∈ {10, 100, 1000}
  conn/sec (SC-003).
- Hot-reload propagation ≤ 2 s end-to-end without RST (SC-005).

**Constraints**:
- No new workspace dependencies (Constitution II).
- Auth seam unchanged (Constitution I); per-owner cap mutations require
  the existing operator-token RBAC check that already gates rule mutation.
- Data-plane reject / throttle events are tracing-only and MUST NOT
  enter the SQLite operator audit ring (mirrors v0.9 D13 / v0.10 invariant).
- Token-bucket math MUST be lock-free on the hot path (atomic counters
  per bucket); cap update is a pointer swap of an `Arc<RateLimitConfig>`,
  no allocation per packet.
- Capability gate refuses any cap-bearing rule or any owner-envelope
  mutation aimed at a forward-client whose self-reported
  `client_version < 0.11.0`.
- Concurrent-cap behaviour on hot-reload below live count is **graceful
  drain** — no connection MAY be closed by the rate limiter (Q4).

**Scale/Scope**:
- Per-rule limiter overhead bounded to one atomic CAS per packet on
  capped flows; one cap × four dims = up to 4 buckets per rule.
- Per-owner limiter overhead bounded to one atomic CAS per packet on
  capped flows under that owner; up to 4 buckets per (client, owner).
- Reject reasons enumerated to 6 values (per-rule × {concurrent, conn-rate,
  udp-flow-rate} + per-owner × same), so metric cardinality grows by
  `rules × owners × 6` for `rate_limit_reject_total` and `rules × 2`
  + `owners × 2` for throttle / active-connection series — same envelope
  as the v0.10 metrics surface.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution version loaded: `.specify/memory/constitution.md` v2.0.2.

| Principle | Status | Justification |
|---|---|---|
| I. Security by Default | PASS | Auth seam unchanged. Per-owner cap mutations reuse the existing operator RBAC check. Token-bucket carries no secret; capability gate keeps cap-bearing rules off pre-v0.11 clients. |
| II. Performance Is a Feature | PASS | No new deps. Legacy no-cap path stays byte-identical; capped path adds one lock-free atomic-CAS per packet per active bucket. Existing data-plane bench is extended (SC-004 gate). |
| III. Test-First Discipline | PASS | Wire, validation, and data-plane behaviour all have concrete contract / integration tests authored before implementation tasks (mirrors v0.9 / v0.10). |
| IV. Observability & Operability | PASS | Adds `rate_limit_reject_total`, `rate_limit_throttle_seconds_total`, `rate_limit_active_connections` (per-rule and per-owner). Hot-reload preserves in-flight forwarding (FR-011). Existing drain-on-shutdown unchanged. |
| V. Multi-Tenant Isolation | PASS | Per-owner cap layer is the explicit tenant-isolation primitive (Q1). Per-owner ceilings bind before per-rule caps (FR-013). Reject / throttle metrics carry an `owner` label distinct from per-rule events (FR-014). No cross-owner data exposure. |

Initial gate: **PASS**.

Post-design gate: **PASS**. No constitutional violations surfaced by the
Phase 0 / Phase 1 artifacts below.

## Project Structure

### Documentation (this feature)

```text
specs/011-rate-limiting-qos/
├── plan.md
├── spec.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── operator-api.md
│   └── wire.md
├── checklists/
│   └── requirements.md
└── tasks.md          # /speckit-tasks output (NOT created here)
```

### Source Code (repository root)

```text
proto/
└── forward.proto

crates/
├── forward-core/
│   ├── src/rate_limit.rs              # NEW: RateLimit envelope + validation
│   └── tests/
├── forward-proto/
│   └── tests/
├── forward-server/
│   ├── src/
│   │   ├── operator/http.rs           # capability gate + owner-cap routes
│   │   ├── operator/owner_cap.rs      # NEW: owner-cap REST handlers
│   │   ├── operator/rule_cli.rs       # CLI rule-cap fields
│   │   ├── rules.rs                   # rule cap persistence + push gating
│   │   ├── owner.rs                   # NEW: owner cap aggregation + GC
│   │   ├── grpc/service.rs            # capability check vs Hello version
│   │   ├── metrics.rs                 # owner stats fold
│   │   └── store/migrations/
│   │       └── V005__add_rate_limit_columns.sql   # NEW
│   └── tests/
├── forward-client/
│   ├── src/
│   │   ├── control.rs                 # absorb owner caps from rule push
│   │   ├── forwarder/
│   │   │   ├── rate_limit/            # NEW: token bucket + scope manager
│   │   │   │   ├── mod.rs
│   │   │   │   ├── bucket.rs
│   │   │   │   └── scope.rs
│   │   │   ├── failover.rs            # gate dial behind concurrent / rate
│   │   │   ├── failover_path.rs       # bandwidth throttle on copy loop
│   │   │   ├── stats.rs               # report rate-limit counters
│   │   │   └── udp/                   # gate NAT-bind behind UDP flow rate
│   │   └── tests/
├── forward-e2e/
│   └── tests/                         # owner A vs B starvation scenario
└── webui/
    ├── src/
    │   ├── pages/RuleEditor.tsx       # cap inputs + advanced burst
    │   ├── pages/ClientDetail.tsx     # NEW Owner quotas tab
    │   └── components/RateLimitForm.tsx  # NEW
    └── ...
```

**Structure Decision**: The feature lands inside the existing crates plus
the embedded Web UI. Per-rule cap data lives next to existing rule state
in `forward-server`; per-owner cap data gets its own table and a thin
`owner.rs` aggregation surface. On `forward-client`, the rate limiter is
a new sibling module under `forwarder/` so the v0.7 failover and v0.10
PROXY paths remain untouched on no-cap rules. The Web UI extends the
v0.6 rule editor and adds one new tab on the client detail page.

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| none      | n/a        | n/a                                  |
