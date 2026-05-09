# Implementation Plan: PROXY-Protocol Injection & SNI Peek-Duration Histogram

**Branch**: `010-proxy-protocol-and-peek-histogram` | **Date**: 2026-05-09 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/010-proxy-protocol-and-peek-histogram/spec.md`

## Summary

v0.10 adds two missing pieces on top of the existing v0.9 codebase:

1. Per-target PROXY protocol injection on the forward-client upstream dial path.
2. A Prometheus histogram for TLS ClientHello peek duration on SNI-mode listeners.

The design stays additive. A target may opt into PROXY v1 or v2 independently of
its siblings in the same rule. The forward-client writes exactly one PROXY header
to the upstream socket before any forwarded bytes when the selected target opts in,
and writes nothing otherwise. The header always reports the original client source
address and the accepted listener's concrete local address, not the wildcard bind.

Peek duration is observed only on SNI listeners, for every peek outcome
(`success`, `timeout`, `parse_error`). The client aggregates fixed-bucket classic
histogram counts in-process and reports them to the server over the existing
`StatsReport` channel. The server folds those counts into Prometheus collectors.

The feature is additive to the existing wire/storage shapes, preserves the pure L4
posture, introduces no new workspace dependencies, and reuses the existing v0.9
SNI listener, multi-target failover, metrics pipeline, and version capability gate
patterns.

## Technical Context

**Language/Version**: Rust 1.88 (workspace MSRV)

**Primary Dependencies**:
- New workspace deps: none
- Existing crates touched: `tokio`, `prost`, `tonic`, `serde`, `prometheus`, `rusqlite`, `tracing`
- Existing codepaths touched: `proto/forward.proto`, `forward-core` target model, `forward-server` rule persistence / operator API / metrics fold, `forward-client` failover dial path, SNI listener stats

**Storage**: Existing SQLite store. One additive migration on `rules` target persistence to record per-target PROXY protocol mode. Existing proto / JSON shapes remain backward-compatible when the field is absent.

**Testing**:
- `cargo test` for unit + integration + contract coverage
- `forward-proto` wire-compat tests for new target / stats fields
- `forward-server` contract tests for validation and capability gating
- `forward-client` integration tests for PROXY v1/v2 prelude emission and SNI histogram reporting
- `forward-e2e` observability / mixed-target scenarios where useful

**Target Platform**: Linux primary, macOS development
**Project Type**: Cargo workspace with server/client binaries

**Performance Goals**:
- PROXY-protocol opt-in adds no more than 1 ms median setup latency versus the same target without PROXY
- Legacy non-opted-in targets remain byte-identical to v0.9
- Peek histogram covers sub-ms through the 3 s deadline and supports `histogram_quantile()` queries

**Constraints**:
- No TLS termination or L7 parsing beyond the existing ClientHello peek
- No new workspace dependencies
- Auth seam unchanged
- Data-plane diagnostics remain tracing + Prometheus only, not SQLite audit
- Capability gate must refuse PROXY-enabled rules to clients older than v0.10

**Scale/Scope**:
- Reuses current per-rule multi-target fanout (`<= 8` targets)
- Histogram emitted only for SNI listeners, labelled by `client, port`
- PROXY applies only to TCP targets, never UDP

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution version loaded: `.specify/memory/constitution.md` v2.0.1.

| Principle | Status | Justification |
|---|---|---|
| I. Security by Default | PASS | Auth/token/TLS seam unchanged. PROXY protocol carries peer metadata only; no new secret handling or TLS behaviour. |
| II. Performance Is a Feature | PASS | No new deps. Legacy non-PROXY path stays structurally unchanged. New hot-path work is bounded to one optional upstream prelude write and one histogram observation on SNI listeners. Existing benches can be extended. |
| III. Test-First Discipline | PASS | Wire, validation, and data-plane behaviour all have concrete contract/integration tests before implementation tasks. |
| IV. Observability & Operability | PASS | Feature adds metrics without altering existing drains/shutdown. Histogram flows through existing Prometheus surface; structured logs cover PROXY write failures. |
| V. Multi-Tenant Isolation | PASS | Setting is per target and per rule; server-side validation and metrics remain labelled by client/rule ownership, no cross-tenant leakage introduced. |

Initial gate: PASS.

Post-design gate: PASS. No constitutional violations surfaced by the Phase 0/1 artifacts below.

## Project Structure

### Documentation (this feature)

```text
specs/010-proxy-protocol-and-peek-histogram/
├── plan.md
├── spec.md
├── research.md
├── data-model.md
├── quickstart.md
├── contracts/
│   ├── operator-api.md
│   └── wire.md
└── tasks.md
```

### Source Code (repository root)

```text
proto/
└── forward.proto

crates/
├── forward-core/
│   ├── src/rule_target.rs
│   └── tests/
├── forward-proto/
│   └── tests/
├── forward-server/
│   ├── src/
│   │   ├── operator/http.rs
│   │   ├── operator/rule_cli.rs
│   │   ├── rules.rs
│   │   ├── grpc/service.rs
│   │   ├── metrics.rs
│   │   └── store/migrations/
│   └── tests/
├── forward-client/
│   ├── src/
│   │   ├── control.rs
│   │   ├── forwarder/failover.rs
│   │   ├── forwarder/failover_path.rs
│   │   ├── forwarder/proxy.rs
│   │   ├── forwarder/sni/listener.rs
│   │   ├── forwarder/sni/peek.rs
│   │   └── forwarder/stats.rs
│   └── tests/
└── forward-e2e/
    └── tests/
```

**Structure Decision**: The feature lands inside the existing crates. Server-side
shape changes live where v0.7/v0.9 already handle target validation, capability
gating, persistence, and metrics folding. Client-side behaviour is split between
the failover dial path (for PROXY prelude injection) and the SNI listener/stats
path (for peek-duration histogram reporting).

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| none | n/a | n/a |
