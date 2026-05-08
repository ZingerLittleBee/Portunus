# Implementation Plan: Unified Embedded SQL Store for Server Persistent State

**Branch**: `008-sqlite-storage` | **Date**: 2026-05-08 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/008-sqlite-storage/spec.md`

## Summary

v0.8 collapses every server-side persistent JSON file (`tokens.json`,
`identity.json`, `rules.json`) plus the in-memory audit ring buffer into
one embedded SQLite database at `<data-dir>/state.db`. The single-binary
deployment posture stays intact; no external DB process is introduced.
Schema-version metadata + forward-only migrations + a single-file
backup/restore CLI close the constitution-level `TODO(STORAGE_CHOICE)`.
Forwarding-plane code paths (TCP/UDP fast paths, wire protocol, control-
plane RPC) are not touched. The operator-API HTTP shapes are byte-stable
for v0.7 callers; only additive query parameters land on `GET /v1/audit`.

A new `--data-dir` flag splits daemon-managed state from
`--config-dir`-rooted admin material (`server.toml`, `server.{crt,key}`),
aligning with FHS / XDG conventions used by Grafana, step-ca, Authelia,
Vaultwarden, syncthing, mosquitto, and Tailscale. The `forward-client`
binary picks up one ergonomic change: `--bundle` becomes optional with a
documented search order; the client itself remains stateless to disk.

The audit hot path is decoupled from the durable writer via a small
bounded async hand-off queue: typical-case durability ≤100 ms, sustained
burst ≤1 s; queue overflow drops oldest pending entries (incrementing
the existing `forward_audit_buffer_drops_total` counter) rather than
back-pressuring `auth_layer`.

## Technical Context

**Language/Version**: Rust 1.88 (workspace MSRV; pinned by `tonic`'s own MSRV — Cargo.toml:rust-version)
**Primary Dependencies**:
- New: `rusqlite` (bundled SQLite, see R-001) + `r2d2`/`r2d2_sqlite` for connection pooling + `refinery` for embedded forward-only schema migrations (see R-005)
- Retained: `tokio`, `tokio-rustls`, `tonic 0.14`, `axum` (operator HTTP), `clap`, `serde`/`serde_json`, `chrono`, `tracing`
- Retired: the JSON-file persistence layer (`forward_auth::file_store::FileTokenStore`, `forward_auth::operator_store::FileOperatorStore`, `forward_server::rules::*` JSON read/write) — replaced behind the same trait seams (`Authenticator`, `OperatorAuthenticator`, rules CRUD)
**Storage**: SQLite, journal_mode=WAL, synchronous=NORMAL, foreign_keys=ON, busy_timeout configured (see R-002, R-003). Single file at `<data-dir>/state.db` with sidecars (`*-wal`, `*-shm`) auto-managed by SQLite.
**Testing**: `cargo test` workspace-wide; tiered as today —
- Unit: per-module Rust tests (e.g., schema migration round-trips, query plan stability)
- Contract: independent tests against the new CLI subcommands (backup/restore/reset) and the additive `/v1/audit` query parameters; v0.7-shape regression tests pin existing endpoints
- Integration: real-socket end-to-end tests in `crates/forward-e2e` (audit-survives-restart smoke, multi-entity atomic mutation under fault injection)
- Bench: criterion benches for the operator API hot path + the forwarding hot path (hold v0.7 baseline within the spec's SC envelopes)
**Target Platform**: Linux x86_64 + aarch64 (primary); macOS for development. Windows out of scope.
**Project Type**: Cargo workspace, six crates (`forward-server`, `forward-client`, `forward-auth`, `forward-core`, `forward-proto`, `forward-e2e`). v0.8 changes are concentrated in `forward-server` + `forward-auth`; `forward-client` gets only the bundle search-path tweak.
**Performance Goals**:
- Operator API p50 / p99 within 10 % of the v0.7 baseline (SC-004)
- `/v1/audit` page load < 2 s for a store of 100 000 entries with time-range filter (SC-005)
- Backup of 10 000 audit + 100 rules + 50 users + 50 tokens ≤ 5 minutes including copy to another host (SC-002)
- Forwarding-plane throughput / p99 within 5 % of the v0.7 numbers (SC-007 → Constitution Principle II hot-path budget)
**Constraints**:
- Audit durability window ≤ 100 ms typical, ≤ 1 s sustained burst (FR-005, SC-001)
- Multi-entity mutation atomicity 100 % under fault-injection (SC-003)
- No allocation in the per-byte / per-packet path (FR-010 + Principle II)
- `--data-dir` MUST refuse NFS / tmpfs at startup (FR-019, edge case)
**Scale/Scope**:
- Up to 100 connected clients (carry-over from v0.1.0 SC-004a); RBAC scale: ≤ 1 000 users, ≤ 10 000 credentials, ≤ 10 000 grants (no current production pressure pushing these)
- Audit table grows unbounded by default; `forward-server audit prune --before <ts>` provides the operator-managed retention path

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution version: `2.0.1` (TLS + bearer token; data-plane userspace; SQLite explicitly closes `TODO(STORAGE_CHOICE)`).

| Principle | Status | Justification |
|-----------|--------|---------------|
| **I. Security by Default** | ✅ | Auth seam unchanged (FR-009): the new `Authenticator` / `OperatorAuthenticator` impls sit behind the same `auth_middleware` and trait boundary — no read or write path bypasses it. Bearer tokens still stored only as blake3 hashes (FR-001 carries the `tokens` table, hashing logic is unchanged from `forward_auth::token`). Audit emit sites unchanged. Data file mode bits restrict to daemon user (FR-019). No new wire protocol fields touch crypto. |
| **II. Performance Is a Feature** | ✅ (with bench gate) | Forwarding hot path (`forwarder/`) is not touched (FR-010 + SC-007). Operator-API path is allowed up to 10 % regression by SC-004; we will gate this with a criterion bench in `crates/forward-server/benches/operator_api.rs` (new) plus the existing `crates/forward-client/benches/data_plane.rs` to verify the data plane is byte-identical. Audit writes are async-decoupled (FR-006), so the auth path acquires no DB handle. |
| **III. Test-First Discipline** | ✅ | TDD applies: contract tests for backup/restore/reset CLI and `/v1/audit` query-param expansion are authored before implementation. Schema-migration round-trip tests pin v0.8 → v0.9-style upgrade behaviour (forward-only, FR-014). Integration tests in `forward-e2e` cover audit-survives-restart, multi-entity atomic mutation under SIGKILL fault injection. Mocks are not used for the SQLite seam — real on-disk DBs in `tempfile::tempdir()`. |
| **IV. Observability & Operability** | ✅ | Structured logs retained (no fewer emit sites than v0.7). New Prometheus series: `forward_audit_durable_writer_lag_seconds` (gauge, hand-off queue depth + last-flush age) — _additive only, no rename of existing series_. The existing `forward_audit_buffer_drops_total` is reused for hand-off queue overflow (semantically identical: "we lost an audit entry due to backpressure"). Graceful reload preserved: store handle is `Arc<Pool>` shared with the operator stack; rule push/remove work without restart. Shutdown drain extended by one final audit-flush + WAL checkpoint before listeners stop. |
| **V. Multi-Tenant Isolation** | ✅ | Authorisation checks remain `(user, resource)` keyed in `auth_layer`. The new SQL queries express the same join shape (`grants` × `rules`) — no global table is exposed. Error messages and timing characteristics for grant lookups stay consistent across users (deterministic prepared statement; we will add an integration test that verifies query latency is independent of user-id presence vs absence). |

**Constitution gate (initial): PASS.** No Complexity Tracking entries yet. The bench-gate cell above is the only conditional commitment; it is enforced by tasks T-XX in the next phase.

**Constitution gate (post-Phase 1, after `research.md`, `data-model.md`, `contracts/*`, `quickstart.md` written): PASS.** No new violations surfaced from the Phase 1 design:

- Principle I: `contracts/operator-api.md` confirms the auth envelope is unchanged; the new `/v1/audit` query params live behind the existing `auth_middleware` and a contract test pins this. `data-model.md` keeps token storage as blake3 hex hashes (`client_tokens.token_hash`) — same shape as v0.7.
- Principle II: data-plane benches retained; the forwarding hot path is not touched (no DB handle anywhere in `crates/forward-client/src/forwarder/` per the structure decision). Operator-API bench gate is enforced before merge per `cli.md` test plan.
- Principle III: every new endpoint and CLI subcommand has a contract test enumerated in `contracts/operator-api.md` §"Quick contract test plan" and `contracts/cli.md`.
- Principle IV: `contracts/operator-api.md` lists the additive Prometheus series; existing series are reused without rename.
- Principle V: `data-model.md` `grants` table shape is the v0.5 RBAC envelope; the SQL queries for grants × rules are written so query latency is independent of user-id presence (verified by an integration test enumerated in `forward-auth/tests/multi_entity_atomic.rs`).

## Project Structure

### Documentation (this feature)

```text
specs/008-sqlite-storage/
├── plan.md                                  # this file
├── spec.md                                  # /speckit-specify (with /speckit-clarify edits)
├── research.md                              # Phase 0 — R-001..R-015 decisions
├── data-model.md                            # Phase 1 — schema (DDL semantics + ERD)
├── quickstart.md                            # Phase 1 — operator + dev walkthrough
├── contracts/
│   ├── persistence.md                       # data-dir layout, backup format, schema-version handshake
│   ├── operator-api.md                      # additive /v1/audit query params + unchanged endpoints
│   └── cli.md                               # new `backup`, `restore`, `reset`, `audit prune` subcommands
├── checklists/
│   └── requirements.md                      # /speckit-specify quality checklist
└── tasks.md                                 # /speckit-tasks output (NOT created here)
```

### Source Code (repository root)

```text
crates/
├── forward-auth/
│   ├── src/
│   │   ├── lib.rs                           # public traits unchanged: Authenticator / OperatorAuthenticator
│   │   ├── token.rs                         # blake3 hashing — unchanged
│   │   ├── sqlite_store.rs                  # NEW — Authenticator impl backed by SQLite (replaces file_store.rs)
│   │   ├── sqlite_operator_store.rs         # NEW — OperatorAuthenticator impl backed by SQLite (replaces operator_store.rs)
│   │   └── identity.rs                      # entity types unchanged (User, Credential, Grant)
│   └── tests/
│       ├── sqlite_store_contract.rs         # NEW — contract tests for the trait impls
│       └── multi_entity_atomic.rs           # NEW — fault-injection integration test
│
├── forward-server/
│   ├── src/
│   │   ├── main.rs                          # `--data-dir` resolution + new subcommands wired
│   │   ├── data_dir.rs                      # NEW — data-dir resolution + filesystem-class probe
│   │   ├── store/
│   │   │   ├── mod.rs                       # NEW — Pool, transaction wrappers, Arc handle
│   │   │   ├── migrations/                  # refinery embedded migrations
│   │   │   │   └── V001__initial_schema.sql
│   │   │   ├── audit_writer.rs              # NEW — async hand-off queue → durable writer
│   │   │   └── backup.rs                    # NEW — Online Backup API wrapper
│   │   ├── rules.rs                         # rule CRUD now goes through `store`; struct shapes unchanged
│   │   ├── operator/
│   │   │   ├── audit.rs                     # rewires emit-site → audit_writer; ring buffer struct retired
│   │   │   ├── audit_http.rs                # adds `since`/`until`/`limit`/`cursor` parameter parsing
│   │   │   └── cli.rs                       # `backup`, `restore`, `reset`, `audit prune` subcommands
│   │   ├── serve.rs                         # boot path: open store → run migrations → bootstrap check
│   │   └── state.rs                         # `AppState` now holds `Arc<Store>`; Mutex<VecDeque> retired
│   ├── benches/
│   │   └── operator_api.rs                  # NEW — criterion bench for SC-004 enforcement
│   └── tests/
│       ├── audit_persists_across_restart.rs  # NEW — SC-001 integration
│       ├── backup_restore_roundtrip.rs       # NEW — SC-002 integration
│       └── data_dir_unsupported_fs.rs        # NEW — NFS / tmpfs refusal smoke
│
├── forward-client/
│   ├── src/
│   │   ├── main.rs                          # bundle search order added; `--bundle` becomes optional
│   │   └── bundle.rs                        # search-path resolver
│   └── tests/
│       └── bundle_search_path.rs            # NEW — contract test for FR-020
│
├── forward-core/                            # unchanged
├── forward-proto/                           # unchanged (FR-011 wire protocol untouched)
└── forward-e2e/
    └── tests/
        ├── audit_persists_e2e.rs             # NEW — full server↔client smoke including audit retention
        └── multi_target_unchanged.rs         # NEW — confirms v0.7 multi-target behaviour byte-identical
```

**Structure Decision**: Cargo workspace, in-place evolution. New code lives
in `crates/forward-server/src/store/`, `crates/forward-auth/src/sqlite_*`,
and `crates/forward-server/src/data_dir.rs`. The retired modules
(`forward_auth::file_store::FileTokenStore`, `forward_auth::operator_store::FileOperatorStore`,
the JSON read/write halves of `forward_server::rules`) are deleted in
the same release per the Assumptions section of `spec.md` ("no
migration"). The `Authenticator` and `OperatorAuthenticator` trait
seams stay byte-identical so call sites in `forward_server::auth_layer`
and `forward_server::serve` change only in their constructor wiring.

## Complexity Tracking

> No Constitution Check violations are present in the initial gate. The
> next gate (post-Phase 1) will be re-evaluated below once the data
> model and contracts are written. The candidate complexity item to
> watch is the new third-party dependency (`rusqlite` bundled SQLite —
> static-linked, ~1.5 MB binary growth) which is necessary to honour
> Constitution `Deployment` ("no required runtime dependencies beyond
> libc and the kernel") — see R-001 for the alternative comparison.
>
> If post-Phase 1 design surfaces a violation that needs a justified
> exception, this table is filled in at that point.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| _none yet_ | _none_ | _none_ |

---

## Phase 0: Outline & Research → `research.md`

Resolved 15 decisions (R-001..R-015):

- R-001: SQLite Rust driver — **`rusqlite` (bundled)**; alternatives: `sqlx` (async, but heavier and forces tokio runtime into store layer), `libsql`, `limbo`. Bundled feature avoids a system-libsqlite3 dependency, preserving single-binary deployment.
- R-002: Journal mode — **WAL** (concurrent reader / single writer; checkpoint on close).
- R-003: `synchronous=NORMAL` (paired with WAL — durability ≥ FR-005 budget; FULL would oversubscribe operator-API latency).
- R-004: Connection model — **`r2d2_sqlite` pool**, write-serialised via SQLite's own internal lock; size = `min(cpu_count, 8)`. The audit writer takes one dedicated connection.
- R-005: Schema migration tooling — **`refinery` with embedded SQL migrations** (`V001__initial_schema.sql`, etc.); forward-only.
- R-006: Audit hand-off queue — **bounded `tokio::sync::mpsc` (capacity 1024) + drop-oldest policy**, mirroring the existing ring buffer's drop semantic. Counter reused: `forward_audit_buffer_drops_total`.
- R-007: Backup mechanism — **SQLite Online Backup API** (`sqlite3_backup_*` via `rusqlite::backup`); produces a clean single-file artefact regardless of WAL state. Restore = file-replace + open + run pending migrations.
- R-008: Filesystem class probe — **`statfs(2)` `f_type` check** on Linux (reject `NFS_SUPER_MAGIC`, `TMPFS_MAGIC`, `RAMFS_MAGIC`); macOS uses `statfs::f_fstypename` string match. CI matrix already covers both.
- R-009: Pre-v0.8 JSON cleanup — **warn-and-ignore**; the server emits a one-line tracing warning per legacy file found in `<config-dir>` and proceeds with the SQLite store. No automatic delete (avoid accidental data loss in dev environments). Operators are pointed at `forward-server reset` for clean-slate.
- R-010: Bootstrap superadmin on empty store — **same CLI subcommand as v0.5** (`forward-server bootstrap-superadmin`); the path now writes through the store transactionally rather than to JSON.
- R-011: Reset CLI — **new `forward-server reset --confirm` subcommand** that closes the store, deletes `state.db` + `state.db-wal` + `state.db-shm` atomically (rename-to-temp then unlink, or unlink with retry on Windows-emulating filesystems), and exits.
- R-012: Audit table indexes — composite `(ts DESC)`, `(outcome, ts DESC)`, `(user_id, ts DESC)`. Time-range queries use the first; outcome filter the second; per-user reports the third.
- R-013: Connection sharing — `AppState` holds `Arc<Pool>`; per-request handler checks out a connection from the pool. The audit writer holds a long-lived dedicated connection for batched writes.
- R-014: Write serialisation — implicit via SQLite's writer lock + the connection pool. `BEGIN IMMEDIATE` for any multi-table mutation to avoid SQLITE_BUSY mid-transaction.
- R-015: Error mapping — `rusqlite::Error::SqliteFailure(SQLITE_BUSY, ...)` → existing `ForwardError::Transient`; constraint violations → `ForwardError::Conflict`; corruption → fail-fast at boot, do not run.

Full prose, alternatives considered, and rejected options live in `research.md` (Phase 0 artefact).

## Phase 1: Design & Contracts

Outputs in this directory:

- `data-model.md` — concrete table list, columns, indexes, foreign-key chains, schema-migrations meta table layout, and an entity-relationship diagram. Each row maps to the spec's "Key Entities" list.
- `contracts/persistence.md` — `<data-dir>/state.db` filesystem layout; backup file format & schema-version envelope; migration handshake at boot; refusal modes for newer-than-binary and unsupported filesystem.
- `contracts/operator-api.md` — additive query-param shape on `GET /v1/audit?since=&until=&limit=&cursor=&outcome=`; unchanged response envelope; v0.7-call regression coverage.
- `contracts/cli.md` — new subcommands: `backup --out <path>`, `restore --in <path> [--force]`, `reset --confirm`, `audit prune --before <RFC3339>`. Unchanged subcommands listed for completeness.
- `quickstart.md` — operator walkthrough (production + dev) + developer recipes (run integration tests, regenerate fixtures, take a backup).

Agent context update: `CLAUDE.md` head pointer flipped from
`007-multi-target-failover` to `008-sqlite-storage`.

## Phase 2 (next: `/speckit-tasks`)

Not run by `/speckit-plan`. The expected slice ordering once `tasks.md`
is generated:

1. Scaffolding: `store/` module, `data_dir.rs`, refinery migrations
   for `V001__initial_schema.sql`. Bootstrap-superadmin path rewired.
2. Authenticator + OperatorAuthenticator SQLite impls; the
   trait-seam swap; deletion of the JSON-file modules; legacy-file
   warn-and-ignore wiring.
3. Rules CRUD swap; multi-entity atomic mutation under
   `BEGIN IMMEDIATE`; v0.7 multi-target byte-stable proof.
4. Audit hand-off queue; durable writer; drop-counter rewire;
   `/v1/audit` query-param expansion.
5. Backup / restore / reset / audit prune CLI surface.
6. Bench harness for SC-004 + SC-005; data-plane bench parity for
   SC-007.
7. Documentation: `quickstart.md` validated end-to-end; CLAUDE.md +
   README updates; CHANGELOG entry; constitution
   `TODO(STORAGE_CHOICE)` closure note.
