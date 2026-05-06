<!--
SYNC IMPACT REPORT
==================
Version change: 2.0.0 → 2.0.1 (PATCH — clarifications only)
  - Added explicit MSRV anchor (1.88, driven by tonic) under Technology
    & Operational Constraints. Not a new constraint, just a concrete number.
  - Added TODO(WEB_UI) under Deferred / TODOs to make the future web-UI
    scope discoverable from the constitution (it was previously only in the
    spec's Assumptions section). Surfacing pre-existing scope, not adding it.

Earlier version change: 1.0.0 → 2.0.0
Bump rationale: MAJOR. Principle I (Security by Default) is redefined in a
backward-incompatible way: the control-plane authentication scheme moves from
mTLS (mutual X.509) to TLS-with-per-client-bearer-token. This was driven by an
explicit operability/complexity trade-off — see project memory for context. All
downstream specs and plans referencing mTLS, CA, or client-cert provisioning
must be reconciled.

Modified principles:
  - I. Security by Default (NON-NEGOTIABLE) — auth model: mTLS → TLS+token,
    server cert pinning instead of CA chain trust, secret hygiene unchanged.

Modified sections:
  - Technology & Operational Constraints — TLS bullet rewritten; PKI removed.
  - Development Workflow & Quality Gates — second-reviewer trigger updated
    (was "certificate handling", now "credential / token handling").

Added sections: none
Removed sections: none

Templates requiring updates:
  - ✅ .specify/templates/plan-template.md — Constitution Check gate is
       generic; no structural change required.
  - ✅ .specify/templates/spec-template.md — generic; no change required.
  - ✅ .specify/templates/tasks-template.md — generic; no change required.
  - ✅ .specify/templates/checklist-template.md — generic; no change required.
  - ⚠ specs/001-tcp-forward-mvp/spec.md — IN-FLIGHT spec was authored against
       v1.0.0 (mTLS). MUST be rewritten in this same change to align: FR-001
       through FR-005, FR-015, SC-003, edge cases, key entities, assumptions.
  - ⚠ CLAUDE.md — auto-generated stub; re-check at first /speckit-plan.

Deferred / TODOs:
  - TODO(STORAGE_CHOICE): server-side persistent store (SQLite vs Postgres) is
    not yet decided; finalize in first /speckit-plan run.
  - TODO(KERNEL_OFFLOAD): whether the data path uses pure-userspace Tokio or
    kernel offload (eBPF / splice / SO_REUSEPORT) is left to plan-level research.
  - TODO(MTLS_REVISIT): if a future deployment surfaces a compliance requirement
    for mutual X.509 (e.g., regulated industries), reopen Principle I; design
    SHOULD allow swapping the auth layer without touching the data path.
  - TODO(WEB_UI): the original project goal includes a server-side management
    web UI ("管理页面") that is explicitly OUT of scope for the MVP (spec
    001-tcp-forward-mvp Assumptions). A future spec MUST decide the frontend
    technology stack (recommend brainstorming via `/speckit-brainstorm` before
    `/speckit-specify`); the MVP's loopback HTTP operator API on `/v1/...` is
    the contract that frontend will consume.
-->

# forward-rs Constitution

## Core Principles

### I. Security by Default (NON-NEGOTIABLE)

All control-plane communication between server and client MUST be transported
over TLS with server-certificate verification; plaintext control channels are
forbidden, even in development. Clients MUST pin the server certificate (by
fingerprint or by an operator-supplied trust anchor) — opportunistic /
trust-on-first-use over an untrusted network is forbidden. Each client MUST
authenticate to the server with a per-client bearer token issued at
provisioning time; tokens MUST be high-entropy (≥128 bits of randomness),
stored only as a non-reversible hash on the server, and never logged, echoed
to stdout, or committed to the repository. Every user-facing action MUST run
under the least privilege required (e.g., a tenant with a port-range
allocation cannot bind ports outside that range, regardless of client-side
request). Authentication failures, token issuance, token revocation, and
permission denials MUST emit audit-grade log records. The authentication
layer MUST be a single seam in the codebase so that swapping it for mTLS or
another scheme later does not require touching the data path.

**Rationale**: A traffic-forwarding daemon sits on the network path and is a
prime escalation target. We chose TLS + bearer token over mTLS because the
operator runs both ends and the operational cost of a private CA outweighs
the marginal security gain in this threat model (the same machine compromise
that leaks a token would also leak a private key). The "single seam"
constraint preserves the option to add mTLS for compliance-driven deployments
without rearchitecting.

### II. Performance Is a Feature

The forwarding hot path (per-packet / per-connection logic on the client) MUST
be designed for zero-allocation steady state where the language and runtime
allow, and MUST avoid synchronous blocking calls. Any change to a hot-path
function MUST ship with a reproducible benchmark (criterion or equivalent)
showing throughput and p99 latency before/after. Regressions >5% on either
metric MUST be either justified (with the rationale recorded in the PR) or
fixed before merge.

**Rationale**: This is a forwarding tool — performance *is* the product.
Without measurement gates, perf rots invisibly across refactors.

### III. Test-First Discipline (NON-NEGOTIABLE)

TDD applies to all production code paths: tests are written, reviewed, and
fail, *then* implementation makes them pass. The server↔client wire protocol
MUST have contract tests independent of either implementation; the
forwarding engine MUST have integration tests that exercise real sockets
(loopback is acceptable, mocks are not). Pure-unit tests are encouraged for
algorithmic code but never substitute for the contract/integration layer.

**Rationale**: Forwarding bugs corrupt user traffic silently. Tests against
real sockets and a versioned wire protocol are the only credible safety net.

### IV. Observability & Operability

Every production binary MUST emit structured logs (JSON or equivalent
machine-parseable format) with correlation IDs tying a control-plane request
to its resulting data-plane effect. Per-port, per-user, and per-rule traffic
metrics MUST be exposed on a metrics endpoint (Prometheus-compatible by
default). Configuration changes MUST be applicable without dropping in-flight
connections (graceful reload), and shutdown MUST drain connections before
terminating.

**Rationale**: Operators of a multi-tenant forwarder need to attribute load,
diagnose tenant-specific issues, and roll out config without outages.

### V. Multi-Tenant Isolation

Every authorization check MUST be expressed in terms of (tenant, resource)
pairs, never globally. A tenant's allowed port range, protocol whitelist, and
machine allocation are policy inputs to the forwarding engine, not
client-trusted hints. No data structure shared across tenants may expose a
tenant's rules, traffic counters, or connection metadata to another tenant —
including via error messages or timing side channels where feasible.

**Rationale**: The server explicitly supports multiple users with bounded
quotas. Isolation must be enforced server-side; clients cannot be trusted to
self-restrict.

## Technology & Operational Constraints

- **Language**: Rust, stable toolchain (MSRV pinned in `Cargo.toml`; bumps
  require a PR note). Current MSRV is 1.88, driven by `tonic`'s own MSRV;
  the next forced bump is the next `tonic` MSRV move.
- **Async runtime**: Tokio. Custom executors require constitutional amendment.
- **TLS**: `rustls` (no OpenSSL dependency in production binaries). Server
  presents a TLS certificate (self-signed by default, operator-supplied
  optional); clients pin the server certificate fingerprint. mTLS is NOT
  used in v2.x — see Principle I.
- **Authentication**: per-client bearer tokens, stored server-side as
  hashes only. Token rotation = re-provision in the MVP; automatic rotation
  is a future amendment.
- **Wire protocol**: A single canonical schema (gRPC, QUIC, or
  length-prefixed framed protocol — chosen at first `/speckit-plan`) MUST be
  versioned; breaking changes require a major-version protocol bump and a
  documented migration path.
- **Persistence (server)**: Embedded or external SQL store —
  TODO(STORAGE_CHOICE), to be decided in first plan iteration.
- **Data path implementation**: Userspace Tokio is the default. Kernel
  offload (eBPF / `splice` / `SO_REUSEPORT`) is permitted as an optimization
  but MUST NOT become a hard dependency unless this constitution is amended —
  TODO(KERNEL_OFFLOAD).
- **Deployment**: Single-static-binary distribution per role (`forward-server`,
  `forward-client`). No required runtime dependencies beyond `libc` and the
  kernel.
- **Configuration reload**: Zero-downtime; in-flight connections drain.
- **Supported platforms**: Linux (primary). macOS for development. Windows
  is out of scope for v1 unless escalated.

## Development Workflow & Quality Gates

- **Spec-driven flow**: All non-trivial work follows
  `/speckit-specify` → (`/speckit-clarify`) → `/speckit-plan` →
  `/speckit-tasks` → `/speckit-implement`. Hotfixes (security, P0 outages)
  may skip ahead but MUST retroactively produce the spec within one week.
- **Constitution Check gate**: Every `plan.md` MUST pass the Constitution
  Check section before Phase 0 research and again after Phase 1 design.
  Violations go in the Complexity Tracking table with a justified rationale.
- **Reviews**: All merges to the default branch require code review.
  Changes touching crypto, credential / token handling, authentication, the
  wire protocol, or the forwarding hot path require a *second* reviewer with
  named domain context (security or performance).
- **Benchmarks**: Hot-path PRs include criterion (or equivalent) results in
  the PR description. CI MAY enforce regression thresholds.
- **Audit log**: A human-readable CHANGELOG entry is required for any
  user-visible change (new permission, new protocol field, default change).

## Governance

This constitution supersedes ad-hoc conventions, README guidance, and prior
verbal agreements. Amendments require:

1. A PR modifying `.specify/memory/constitution.md` with a Sync Impact Report
   (see top of file) and a bumped version per semantic versioning:
   - **MAJOR**: removed or redefined a principle in a backward-incompatible way.
   - **MINOR**: added a principle or materially expanded an existing one.
   - **PATCH**: clarifications, typos, non-semantic edits.
2. Approval from at least one maintainer not authoring the change.
3. A migration note in the PR if templates, plans, or in-flight specs need
   reconciling.

Compliance is verified at every Constitution Check gate. Unjustified violations
block merge. Justified violations are recorded in the relevant `plan.md`
Complexity Tracking table and revisited at the next constitutional review.

Runtime / per-task agent guidance lives in `CLAUDE.md` and the relevant
`.specify/templates/*.md` files; those documents MUST stay consistent with
the principles above.

**Version**: 2.0.1 | **Ratified**: 2026-05-06 | **Last Amended**: 2026-05-06
