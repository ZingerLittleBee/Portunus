# Implementation Plan: Client Stable Identifier (name as display field)

**Branch**: `015-client-stable-id` | **Date**: 2026-06-02 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/015-client-stable-id/spec.md`

## Summary

Separate a client's **identity** from its **label**. Introduce a system-generated,
stable, opaque `ClientId` (ULID, reusing the workspace `ulid` crate) that becomes the
canonical key across every layer — persistence primary keys, in-memory connected-client
tracking, operator API/CLI/Web-UI addressing, and internal correlation. Demote
`client_name` to a free-form display field with relaxed validation, and make display-name
changes identity-safe (rules/tokens/quotas/history and live sessions survive a rename).

Technical approach: additive gRPC wire change (`client_id` added, `client_name` kept for
display); a refinery `V011` migration that re-keys seven `client_name`-keyed tables to
`client_id` (build-new-table + copy + rename, with `client_tokens` as the source of truth
and backfill by name-join); transparent upgrade for already-enrolled clients because client
identity is resolved from the bearer token, not the wire name. The forwarding hot path is
untouched.

## Technical Context

**Language/Version**: Rust edition 2024, MSRV 1.88 (workspace); Web UI is
TypeScript + React + Vite SPA.
**Primary Dependencies**: `tonic`/`tonic-prost` (gRPC), `rusqlite` + `refinery` (embedded
migrations) + `r2d2` (pool), `ulid`, `serde`, `prometheus`, `axum`-style operator HTTP;
React Router on the frontend.
**Storage**: Bundled SQLite at `<data-dir>/state.db` (WAL, `BEGIN IMMEDIATE`, refinery).
Adds migration `V011`. Current head is `V010`.
**Testing**: `cargo test --workspace`; wire contract tests; integration tests over real
loopback sockets (Constitution III); `portunus-e2e` process-level tests; `pnpm build`
(`tsc -b && vite build && size-limit ≤500 KB gz`) for the Web UI.
**Target Platform**: Linux (primary, static `musl` release artifacts); macOS for dev.
**Project Type**: Web application — Rust backend workspace (8 crates) + embedded React SPA.
**Performance Goals**: No data-plane change. The forwarding hot path is not modified, so the
`v0.1.0` data-plane benchmark gate (>25% CI fail / >5% Constitution gate) is expected to be
flat. `ClientId` is a 128-bit `Copy` value; connected-client map lookups stay O(1).
**Constraints**: Additive wire schema (running clients must not break); migration MUST be
idempotent and crash-safe; auth model unchanged (TLS + bearer token, single seam);
multi-tenant isolation preserved across re-keying.
**Scale/Scope**: Tens–hundreds of clients per server; seven persisted tables re-keyed; one
new core type; ~5 proto fields added; operator HTTP/CLI + Web UI surfaces re-pathed.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

- **I. Security by Default (NON-NEGOTIABLE)** — PASS. Auth scheme unchanged: per-client
  bearer tokens, hashed server-side, never logged. `ClientIdentity` remains the single auth
  seam; it gains a `ClientId` alongside `ClientName`, and the token store resolves a token to
  the client's `ClientId`. Token issuance/revocation paths keep emitting audit records;
  **rename is a new operator mutation and MUST emit an audit-grade record**. Relaxing
  `ClientName` does not weaken any security boundary — the name was never an authz input
  (authz keys on tenant/owner). `ClientId` is opaque and non-guessable-by-design (ULID).
- **II. Performance Is a Feature** — PASS. The per-packet/per-connection forwarding hot path
  is not touched; this is control-plane + persistence work. No new hot-path allocations.
  `ConnectedClients` switches its key from `ClientName` (heap `String`) to `ClientId`
  (128-bit `Copy`), which is neutral-to-favorable. No new benchmark is required by the
  hot-path rule; the existing data-plane bench is run to confirm flatness.
- **III. Test-First Discipline (NON-NEGOTIABLE)** — PASS (enforced in tasks). Wire contract
  tests assert the additive `client_id` field round-trips and that a name-only (legacy)
  bundle still authenticates. A migration test seeds a `V010` database with multiple clients
  (each with rules/quota/history) and asserts id assignment, zero orphans, and idempotent
  re-run. Integration tests over real loopback sockets cover rename-keeps-session and
  legacy-client-reconnect. Tests are written before implementation.
- **IV. Observability & Operability** — PASS. Correlation is strengthened: a renamed client
  remains one logical entity keyed by `ClientId`. Prometheus `client` label keeps the
  human-readable name for dashboard readability (documented decision); internal correlation
  uses the id. Rename applies without dropping in-flight connections (graceful). A CHANGELOG
  entry is required (new protocol field + relaxed validation + rename capability).
- **V. Multi-Tenant Isolation** — PASS. Owner/quota/rate-limit authorization remains keyed on
  (tenant/owner, client); re-keying the client side from name to id preserves the (tenant,
  resource) pairing. Not-found responses for unknown `ClientId` MUST avoid leaking whether
  another tenant owns a colliding name (names are now non-unique, which actually reduces a
  pre-existing enumeration signal).

**Result**: No violations. Complexity Tracking table is empty.

## Project Structure

### Documentation (this feature)

```text
specs/015-client-stable-id/
├── plan.md              # This file (/speckit-plan output)
├── spec.md              # Feature spec (/speckit-specify output)
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output (wire + HTTP + CLI + UI deltas)
│   ├── proto-delta.md
│   ├── operator-http.md
│   └── migration-v011.md
├── checklists/
│   └── requirements.md  # Spec quality checklist (/speckit-specify output)
└── tasks.md             # /speckit-tasks output (NOT created here)
```

### Source Code (repository root)

```text
crates/
├── portunus-core/
│   └── src/id.rs                    # +ClientId(Ulid) newtype; relax ClientName validation
├── portunus-proto/
│   └── (generated from proto/portunus.proto)
├── portunus-auth/
│   └── src/lib.rs                   # ClientIdentity gains client_id
├── portunus-server/
│   └── src/
│       ├── store/
│       │   ├── migrations/V011__client_id.sql   # NEW refinery migration
│       │   ├── token_store.rs       # key by ClientId; token→ClientId resolution
│       │   ├── owner_cap_store.rs   # OwnerCap re-keyed to client_id
│       │   ├── enrollment_store.rs  # client_enrollments re-keyed
│       │   ├── operator_store.rs    # rules carry client_id
│       │   └── (traffic quota stores)            # V008 tables re-keyed
│       ├── grpc/service.rs          # identity.client_id; Hello/Welcome unchanged on wire
│       ├── operator/                # HTTP routes /v1/clients/{id}/...; CLI args; rename endpoint
│       ├── clients.rs               # ConnectedClients: HashMap<ClientId, _>
│       ├── rules.rs                 # error messages reference id (+ name for display)
│       └── metrics.rs               # label stays name; internal key id
├── portunus-client/
│   └── src/bundle.rs                # CredentialBundle gains client_id; relaxed name parse
└── portunus-e2e/                    # process-level: rename, legacy reconnect

proto/portunus.proto                 # +client_id in Enroll/CredentialBundle/OwnerRateLimitUpdate/TrafficQuotaUpdate

webui/src/
├── App.tsx                          # route /clients/:clientId (was :clientName)
├── pages/ + components/             # ClientsList/ClientDetail/EnrollmentInstallGuide/Traffic/UserQuota
└── (rename UI affordance)
```

**Structure Decision**: Existing Rust workspace (8 crates) + embedded React SPA — no new
crates or top-level dirs. Changes are concentrated in `portunus-core` (new type), the
`portunus-server` store/operator/grpc layers, the proto schema, the client bundle, and the
Web UI client surfaces. The shared `portunus-forwarder` data-plane library is **not** touched
(it is proto-free and name-agnostic).

## Complexity Tracking

> No Constitution violations. Table intentionally empty.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|--------------------------------------|
| — | — | — |
