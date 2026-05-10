# Implementation Plan: Single-Tenant Token-Authenticated Control Plane with Single-Port TCP Forwarding (MVP)

**Branch**: `001-tcp-forward-mvp` | **Date**: 2026-05-06 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `specs/001-tcp-forward-mvp/spec.md`

## Summary

The MVP delivers an end-to-end vertical slice of Portunus: an operator runs a server, provisions clients (each gets a TLS fingerprint + bearer token bundle), starts client processes that connect over TLS, then pushes TCP forwarding rules and observes traffic. The architecture is a Rust workspace with five crates (`forward-proto`, `forward-core`, `forward-auth`, `forward-server`, `forward-client`) plus an end-to-end test crate. The control channel uses gRPC over TLS via Tonic with bidirectional streaming for server→client rule push and client→server stat reporting; per-client authentication is enforced by a Tonic interceptor that calls into a single `forward-auth` trait (FR-023's seam, so a future mTLS swap touches one crate). The data plane uses `tokio::io::copy_bidirectional` for zero-copy bidirectional proxying. Token store and TLS material are persisted as JSON / PEM files at operator-configurable paths (no SQL store in MVP). Structured logging is `tracing` with the JSON layer; a Prometheus-format metrics endpoint exposes per-rule counters (added beyond what the spec literally requires, to honour Constitution Principle IV — see Constitution Check).

## Technical Context

**Language/Version**: Rust 1.88 (stable, MSRV). Edition 2024. Pinned in workspace `Cargo.toml`. (MSRV is driven by `tonic`'s own MSRV of 1.88; edition 2024 stabilised in 1.85 so it is available to us.)

**Primary Dependencies** (all versions LOCKED for the duration of this MVP — no hedges):

| Crate | Version | Features | Role |
|---|---|---|---|
| `tokio` | `1` | `["full"]` (or narrowed in implementation: `rt-multi-thread`, `net`, `signal`, `sync`, `time`, `macros`) | Async runtime |
| `tokio-util` | `0.7` | `["rt"]` for `CancellationToken` | Graceful shutdown |
| `tonic` | `0.14` | `["transport", "tls-aws-lc"]` | gRPC server/client over TLS. Note: `tls-roots` is intentionally NOT enabled — we use a custom `PinnedVerifier`, not the system trust store. |
| `tonic-prost` | `0.14` | default | Prost runtime integration (separate crate in 0.14, was bundled in earlier tonic) |
| `prost` | `0.14` | default | Generated message types |
| `tonic-prost-build` | `0.14` | (build-dep) | Code generation in `forward-proto`'s `build.rs` |
| `rustls` | `0.23` | `["aws-lc-rs"]` (provider) | TLS; `aws-lc-rs` provider chosen over `ring` for modern defaults and easier FIPS path |
| `tokio-rustls` | `0.26` | default | TLS adapter (used directly only by the client's `PinnedVerifier`; tonic owns the server side) |
| `rcgen` | `0.13` | default | First-launch self-signed server cert generation |
| `axum` | `0.8` | `["tokio", "macros"]` | Operator HTTP interface, loopback-only (no `hyper` fallback) |
| `clap` | `4` | `["derive"]` | CLI for both binaries |
| `tracing` | `0.1` | default | Structured logging |
| `tracing-subscriber` | `0.3` | `["json", "env-filter"]` | JSON layer + `EnvFilter` (`RUST_LOG`-driven) |
| `serde` | `1` | `["derive"]` | (de)serialisation |
| `serde_json` | `1` | default | Token store, bundle, JSON logs |
| `toml` | `0.8` | default | Config files (`server.toml`, `client.toml`) |
| `rand` | `0.8` | `["std", "std_rng"]` | Token generation via `OsRng` |
| `blake3` | `1` | default | Token hash (input is ≥128-bit random — salt-less fast hash is appropriate) |
| `ulid` | `1` | `["serde"]` | `RequestId` correlation IDs |
| `prometheus` | `0.13` | default | Metrics text encoding for `/metrics` |
| `criterion` | `0.5` | (dev-dep) | Benchmark harness in `forward-client/benches/` |
| `proptest` | `1` | (dev-dep) | Property tests for `FileTokenStore` atomicity |
| `tempfile` | `3` | (dev-dep) | Test scratch dirs |
| `assert_cmd` | `2` | (dev-dep) | E2E tests spawning binaries |
| `tokio-test` | `0.4` | (dev-dep) | `tokio::test` helpers and `block_on` for non-async tests |

**Storage**:
- Token store: `<config_dir>/tokens.json` (atomic write: tmp-file + rename). Schema in `data-model.md`. ≤100 entries — JSON is well within scale.
- Server TLS material: `<config_dir>/server.crt` and `<config_dir>/server.key` (PEM).
- Rule state and connection state: in-memory only (per spec Assumption).
- Resolves Constitution `TODO(STORAGE_CHOICE)` for MVP. SQL store deferred to whichever future spec first needs persistent rules / audit-log persistence / multi-tenant identity.

**Testing**:
- `cargo test` for unit + per-crate integration tests.
- Per-crate `tests/` directories use real sockets (loopback) per Constitution III.
- Workspace-level `crates/forward-e2e/` boots an in-process server + client and exercises the protocol end-to-end (this is the contract-test layer required by Constitution III).
- `criterion` benches in `crates/forward-client/benches/` (data-plane throughput / p99 latency on loopback). No regression threshold yet (no baseline); harness exists from day one so the next hot-path spec can lock in numbers.

**Target Platform**: Linux x86_64 + aarch64 (primary). macOS supported for development (CI matrix). Windows out of scope.
**Project Type**: Cargo workspace, multi-binary (`forward-server`, `forward-client`) + supporting libraries.
**Performance Goals** (informational, not gated for this MVP):
- ≥1 Gbps sustained throughput on a single rule, loopback, 1 connection
- <2 ms added p99 latency vs direct connection, loopback
- ≤512 KB steady-state memory per active forwarded connection (combined direction buffers)

**Constraints**:
- Userspace-only data plane (Constitution; resolves `TODO(KERNEL_OFFLOAD)` for MVP — no eBPF / `splice` / `SO_REUSEPORT` in v1).
- ≤100 concurrently connected clients per server (SC-004a).
- Single static binary per role; only runtime dependency is `libc` and the kernel.
- No GUI; operator surface is CLI + loopback HTTP.
- Auth layer behind a single trait (FR-023).

**Scale/Scope**:
- 100 connected clients × 5 rules per client × 100 connections per rule = 50,000 concurrent forwarded sockets at the upper bound. Tokio scales to this comfortably on a modern host.
- Token store: ≤100 entries, mutated only on `provision` / `revoke` (rare).
- Rule store: ≤500 rules across all clients, mutated only on `push` / `remove` / `failed`-marking.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution v2.0.0 was loaded. Each principle is checked below.

### I. Security by Default (NON-NEGOTIABLE) — PASS

| Requirement | Plan satisfies via |
|---|---|
| TLS for all control-plane traffic | Tonic server bound to a `rustls`-backed `ServerTlsConfig`; client uses `rustls`-backed `ClientTlsConfig` with custom `ServerCertVerifier` enforcing fingerprint pinning. Plain-TCP listener never opened. |
| Server cert pinning by client | `ServerCertVerifier` impl in `forward-client` compares the leaf cert SHA-256 fingerprint against the value in the bundle; mismatch returns `webpki::Error`. No system trust store consulted for the control endpoint. |
| Per-client bearer token | Tonic `Interceptor` reads `authorization: Bearer <token>` metadata, hashes with `blake3`, looks up against `forward-auth` trait. Missing/invalid/revoked → `Status::unauthenticated(reason)`. |
| ≥128 bits token entropy | `OsRng` → 32 random bytes → URL-safe base64 (43 chars). Generated in `forward-auth::generate_token`. |
| Hash-only storage | `TokenStore::insert(name, hash, ...)`; raw token never persisted; surfaced once in the bundle returned to operator. |
| No secrets in logs | `tracing` field redaction via a custom `tracing` layer that elides any field named `token`, `secret`, `private_key`, etc. |
| Audit events | Server emits `audit.provision`, `audit.revoke`, `audit.rule_push`, `audit.rule_remove` events with operator-action / client / outcome fields. |
| Single auth seam | All auth lives in `forward-auth` crate behind `trait Authenticator` (`verify(token) -> Result<ClientIdentity, AuthError>`). Server only depends on this trait, not on token specifics. Future mTLS = new `Authenticator` impl, no changes to rule logic. |

### II. Performance Is a Feature — PASS WITH NOTE

The constitution mandates a benchmark for any hot-path change. The spec's
Assumption claiming a "v1 carve-out" overstates the constitution — there is no
such carve-out documented. The plan therefore tightens the spec by including
a `criterion` benchmark harness in `forward-client/benches/` from day one
(data-plane throughput + p99 latency over loopback). No regression threshold
is enforced yet — there is no baseline. The benchmark exists so the next
hot-path-touching spec can lock in numbers and CI gates.

The data plane uses `tokio::io::copy_bidirectional`, which performs ring-buffer
copies via `splice`-style operations on Linux when both ends are TCP — the
canonical zero-allocation steady-state copy. Buffers are owned by Tokio (no
per-byte heap allocation in the hot path).

### III. Test-First Discipline (NON-NEGOTIABLE) — PASS

- Wire protocol contract tests live in `forward-e2e/` and exercise a real Tonic server + real Tonic client, both backed by the actual implementation crates (no mocks). This satisfies "contract tests independent of either implementation" because the contract surface is the `.proto` file plus the auth seam, not the implementation classes.
- Forwarding integration tests use real loopback TCP sockets (`forward-client/tests/`).
- Pure-unit tests cover `forward-auth` (token gen, hash, store roundtrip) and `forward-core` (config parsing, error mapping).
- TDD discipline: each task in `/speckit-tasks` will be authored test-first per the constitution; the executor (you/me) is bound to that during `/speckit-implement`.

### IV. Observability & Operability — PASS WITH NOTE

The spec's Assumption claiming "no metrics endpoint in MVP" contradicts
Constitution Principle IV which makes the Prometheus endpoint a MUST. The
plan resolves this by including a small Prometheus exporter in
`forward-server` (per-rule `bytes_in`, `bytes_out`, `active_connections`,
plus `clients_connected` gauge). Cost: ~50 LOC + the `prometheus` crate.
The spec text remains accurate as a *minimum* (operator-facing on-demand
queries also work); the metrics endpoint is an addition.

- Structured logs: `tracing-subscriber` JSON layer to stderr.
- Correlation IDs: every operator action generates a `request_id`; rule-push messages carry it through to the client and back; data-plane connection logs include the parent `rule_id`.
- Graceful shutdown: `tokio_util::sync::CancellationToken` rooted at process; signal handler triggers cancel; rule listeners stop accepting; in-flight `copy_bidirectional` futures drain up to the configured timeout (default 30 s, FR-020).

### V. Multi-Tenant Isolation — N/A FOR MVP, ARCHITECTURE PRESERVES OPTION

MVP is explicitly single-tenant (spec Assumption). However:
- All `forward-auth` identities are typed `ClientIdentity`, never globally reused. When multi-tenancy lands, `ClientIdentity` will gain a `tenant_id` field without restructuring downstream code.
- The Tonic interceptor passes the `ClientIdentity` into request extensions; rule handlers already receive it and check `client_name == identity.client_name`. Adding `tenant_id` checks later is additive.

### Gate Decision: PASS

No unjustified violations. Two "PASS WITH NOTE" items (benchmarks harness, Prometheus endpoint) tighten the plan above what the spec literally describes; both are constitution-mandated and cheap to include now. Recorded but **not** entered in Complexity Tracking because they are not violations — they are alignments to the constitution.

## Project Structure

### Documentation (this feature)

```text
specs/001-tcp-forward-mvp/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── forward.proto    # Canonical wire protocol
│   ├── operator-api.md  # Operator CLI + HTTP surface
│   └── persistence.md   # On-disk formats
├── checklists/
│   └── requirements.md  # Spec quality checklist (already exists)
└── tasks.md             # Phase 2 output (/speckit-tasks command)
```

### Source Code (repository root)

```text
Portunus/
├── Cargo.toml                       # [workspace], members = [...]
├── proto/
│   └── forward.proto                # Single source of truth for wire protocol
├── crates/
│   ├── forward-proto/               # build.rs runs tonic-build over ../proto
│   │   ├── build.rs
│   │   ├── Cargo.toml
│   │   └── src/lib.rs               # `pub mod forward { tonic::include_proto!(...); }`
│   ├── forward-core/                # shared types, errors, config types, fingerprint helpers
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── config.rs            # ServerConfig, ClientConfig, ConfigDir resolution
│   │       ├── error.rs             # ForwardError taxonomy
│   │       ├── id.rs                # ClientName, RuleId, RequestId newtypes
│   │       └── fingerprint.rs       # cert SHA-256 helpers
│   ├── forward-auth/                # auth seam — the FR-023 single-seam crate
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs               # `trait Authenticator`, `ClientIdentity`, `AuthError`
│   │       ├── token.rs             # generate_token, hash_token (blake3)
│   │       └── file_store.rs        # FileTokenStore: atomic-write JSON
│   ├── forward-server/              # binary
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       ├── grpc/                # Tonic service impl
│   │       │   ├── mod.rs
│   │       │   ├── interceptor.rs   # bearer token interceptor
│   │       │   └── service.rs       # ControlService impl
│   │       ├── operator/            # operator interface
│   │       │   ├── mod.rs
│   │       │   ├── cli.rs           # `forward-server provision-client …`
│   │       │   └── http.rs          # axum router on loopback
│   │       ├── rules.rs             # ServerRuleStore (in-mem) + state machine
│   │       ├── clients.rs           # ConnectedClients (in-mem)
│   │       ├── tls.rs               # rustls server config builder
│   │       ├── metrics.rs           # Prometheus collectors + exporter
│   │       └── shutdown.rs          # CancellationToken plumbing
│   │   └── tests/
│   │       ├── auth_flow.rs
│   │       ├── rule_lifecycle.rs
│   │       └── persistence_roundtrip.rs
│   ├── forward-client/              # binary
│   │   ├── Cargo.toml
│   │   ├── src/
│   │   │   ├── main.rs
│   │   │   ├── control.rs           # connect to server, maintain stream, reconnect/backoff
│   │   │   ├── pinned_verifier.rs   # rustls ServerCertVerifier impl
│   │   │   ├── forwarder/
│   │   │   │   ├── mod.rs           # Listener per rule, accept loop
│   │   │   │   ├── proxy.rs         # copy_bidirectional driver
│   │   │   │   └── stats.rs         # atomic counters per rule
│   │   │   └── shutdown.rs
│   │   ├── benches/
│   │   │   └── data_plane.rs        # criterion: throughput, p99 latency
│   │   └── tests/
│   │       └── tcp_forward_loopback.rs       # data-plane integration; pin-mismatch lives in forward-e2e
│   └── forward-e2e/                 # workspace-level end-to-end / contract tests
│       ├── Cargo.toml
│       └── tests/
│           ├── happy_path.rs        # Story 1+2 e2e
│           ├── observability.rs     # Story 3 e2e
│           ├── auth_failures.rs     # invalid / revoked / cert-mismatch
│           └── rule_failure_lifecycle.rs # port_in_use stays `failed` until removed
└── specs/                           # spec dirs (already exists)
```

**Structure Decision**: Cargo workspace with five production crates plus a workspace-level e2e test crate. The five-crate split is driven by Constitution Principle I's "single auth seam" requirement (`forward-auth` is its own crate so the trait surface is small and reviewable) and by the desire to keep the proto code-gen out of the binaries' compile-graph hot reload (it lives in `forward-proto`). `forward-e2e` is its own crate so the tests live with the contract (the proto + the auth trait) rather than inside either binary, which matches Constitution III's "tests independent of either implementation" wording.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified.**

No violations. The plan over-delivers vs the spec on two dimensions
(benchmark harness, Prometheus endpoint) to align with the constitution; this
is documented in the Constitution Check section above and is NOT a violation
requiring entry in this table.
