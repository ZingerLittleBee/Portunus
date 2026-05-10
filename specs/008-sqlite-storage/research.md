# Phase 0 Research — 008 Unified SQLite Store

This document records the 15 design decisions that the v0.8 plan
depends on. Each entry follows the project's standing format:

> **Decision** — what was chosen.
> **Rationale** — why.
> **Alternatives considered** — what was evaluated and rejected, with
> the tripwire.

Decisions resolve every open variable in `plan.md` Technical Context.
No `NEEDS CLARIFICATION` markers remain after this phase.

---

## R-001 — SQLite Rust driver

**Decision**: `rusqlite = { version = "...", features = ["bundled", "blob",
"backup", "chrono", "limits"] }` plus `r2d2 = "..."` and
`r2d2_sqlite = "..."` for connection pooling.

**Rationale**:
- `bundled` feature statically links a known-good SQLite (currently
  3.46 series) — preserves the constitution's "no required runtime
  dependencies beyond libc and the kernel" deployment posture and
  removes `libsqlite3-dev` as a build prerequisite on contributor
  machines.
- Synchronous API matches our trait-seam pattern: each call site holds
  a `&Connection` for the duration of a small operation (rules CRUD,
  RBAC lookup, audit write). No coloured-async refactor of the
  existing `Authenticator` / `OperatorAuthenticator` traits is needed
  — they stay sync.
- `backup` feature exposes `Backup::run`, the safe wrapper around
  SQLite's online backup API (R-007).
- `chrono` feature gives `DateTime<Utc>` ↔ TEXT mapping out of the box,
  matching the existing wire types (`crates/forward-auth/src/file_store.rs`
  already uses `chrono::DateTime<Utc>`).
- `r2d2` is the de-facto sync pool for `rusqlite` and is the same crate
  Diesel / Rocket use — well-trod path.

**Alternatives considered**:
- **`sqlx` with the `sqlite` feature** — async-first, compile-time
  query verification. Rejected because (a) it forces the operator-API
  request handler to await every DB call inside an axum extractor,
  spreading the async colour into trait seams that today are sync, and
  (b) compile-time query verification needs a build-time DB which
  complicates `cargo check` for contributors. The sqlx pool's
  per-connection runtime overhead is also worse than r2d2 for the
  sync-heavy access pattern.
- **`limbo`** (Rust-native SQLite rewrite) — promising long term, not
  yet at production parity (no backup API, no WAL stability guarantees
  on macOS as of late-2025). Reopen at a future spec if the project
  needs to drop the C dependency.
- **`libsql`** — Turso's fork; multi-tenant features we do not need;
  introduces a soft dependency on a network sync engine we explicitly
  reject for v0.8 (FR-002).
- **Hand-rolled `sqlite3-sys` bindings** — every project that has done
  this regrets it within 18 months. Out.

---

## R-002 — Journal mode

**Decision**: `PRAGMA journal_mode = WAL` set at every connection check-out
through a `r2d2` `CustomizeConnection` hook.

**Rationale**:
- WAL allows concurrent readers while a writer is committing; this
  matches the operator API access pattern (many parallel read-heavy
  GETs + occasional writes from push-rule / provision-client / audit
  emit).
- WAL's incremental checkpoint behaviour amortises the durability cost
  per transaction, supporting the FR-005 budget (≤100 ms typical).
- WAL mode survives unclean shutdowns by replaying the log on next
  open; this is what makes the "abrupt process kill ≤100 ms loss"
  guarantee feasible.

**Alternatives considered**:
- `DELETE` (default rollback journal) — readers and writers serialise;
  every operator GET would block during a rules push. Rejected.
- `TRUNCATE` / `PERSIST` — variants of rollback journal; same blocking
  problem.
- `MEMORY` — journal in RAM; loses durability. Rejected — directly
  violates FR-005.
- `OFF` — no journal; corruption on crash. Rejected — violates FR-005
  and the edge-case "data file present but corrupt" requirement.

---

## R-003 — `synchronous` PRAGMA

**Decision**: `PRAGMA synchronous = NORMAL` paired with WAL.

**Rationale**:
- `NORMAL + WAL` gives durability at WAL checkpoint boundaries while
  not requiring an `fsync` per commit; this is the SQLite-recommended
  pairing and the one used by Tailscale, syncthing, mosquitto.
- A typical commit acquires a small bounded WAL append + periodic
  checkpoint; the durability window matches FR-005's 100 ms typical /
  1 s burst budget when paired with the audit writer's batching
  cadence (R-006).
- `synchronous = FULL` would force `fsync` on the WAL after every
  commit, blowing through the SC-004 operator-API latency envelope.

**Alternatives considered**:
- `FULL` — every commit `fsync`s the WAL. Safer against ill-timed
  power loss, slower (50%+ latency hit on hot transaction paths in
  internal microbenches). Rejected: SC-004 budget cannot absorb it,
  and FR-005 explicitly trades a bounded loss window for throughput.
- `OFF` — fastest, no durability guarantee. Rejected.
- `EXTRA` — equivalent to `FULL` plus directory `fsync`. Rejected for
  the same latency reason.

---

## R-004 — Connection model

**Decision**: A single `r2d2::Pool<SqliteConnectionManager>` owned by
`AppState`. Pool size = `min(num_cpus::get(), 8)`. The audit durable
writer gets one dedicated connection checked out for its whole lifetime
(re-acquired only across reconnects after a fault).

**Rationale**:
- SQLite serialises writers internally; a pool primarily helps reads.
  Capping at 8 avoids paying the file-handle / WAL-frame-cache cost
  for connections that would idle on small deployments.
- The audit writer's dedicated connection prevents the audit batch
  from competing with operator-API GETs for a pool slot during
  bursts.
- `r2d2` has zero async overhead (matches R-001's sync trait-seam
  decision).

**Alternatives considered**:
- One global connection, mutex-guarded — simpler, but turns every read
  into a serialisation point. Rejected on SC-004 grounds.
- Per-task connection (no pool) — open/close churn on every operator
  request. Rejected.
- A read-pool plus a write-pool of 1 — redundant: SQLite already
  enforces single-writer at the engine level, so adding a Rust-side
  write mutex buys nothing but bookkeeping.

---

## R-005 — Schema migration tooling

**Decision**: `refinery` (`refinery_macros = "0.8"`,
`refinery = { features = ["rusqlite"] }`) with embedded SQL migrations
under `crates/forward-server/src/store/migrations/` named
`V001__initial_schema.sql`, `V002__...`, etc. Forward-only.

**Rationale**:
- Embedded migrations live alongside the binary; `single-binary
  deployment` (FR-002) is preserved — no migration files to ship
  separately.
- `refinery` supports the version-numbered `V###__name.sql` convention
  used by Flyway; well-known, low cognitive overhead.
- Each migration is auto-wrapped in a transaction; partial migration
  failure rolls back without corrupting the file.
- Forward-only matches the spec's `downgrade-via-restore-from-backup`
  policy (Assumptions §Downgrade); we do not need down-migrations and
  refusing to author them prevents the maintenance burden seen in
  half-bidirectional schemas.

**Alternatives considered**:
- `sqlx::migrate!` — coupled to `sqlx` (rejected at R-001).
- Hand-rolled migration runner — every project that builds one ends up
  reinventing version tracking, transaction wrapping, and lock
  acquisition. Reject.
- `barrel` — Rust DSL for migrations; less expressive than raw SQL
  for SQLite-specific options (PRAGMA, partial indexes). Reject.

---

## R-006 — Audit hand-off queue

**Decision**: A bounded `tokio::sync::mpsc::channel<AuditEntry>` with
capacity 1024 between the `auth_layer` emit sites and a single
durable-writer task. On `try_send` returning `Full`, the producer
drops the oldest enqueued entry (via a paired `Notify` + a
`tokio::sync::Mutex<VecDeque>` adapter) and increments
`forward_audit_buffer_drops_total`.

The durable writer batches up to 256 entries or 100 ms (whichever
comes first) per `INSERT` transaction.

**Rationale**:
- Decouples audit emission from durable IO — FR-006 (no operator-API
  back-pressure).
- Capacity of 1024 mirrors the v0.6 ring buffer's order of magnitude;
  drop semantics are byte-identical from the operator's view ("we may
  lose audit entries under extreme load, the counter rises").
- 100 ms batch ceiling matches the typical-case durability budget
  (FR-005) without forcing single-entry commits, which would push
  fsync overhead onto the WAL critical path.

**Alternatives considered**:
- Unbounded channel — defeats the back-pressure isolation, allows
  unbounded memory growth on a saturated DB. Reject.
- Blocking enqueue — blocks `auth_layer`. Reject.
- `crossbeam::ArrayQueue` (lock-free SPSC) — single-producer; we have
  many emit sites in axum handlers. MPSC is the right fit.
- New Prometheus counter for hand-off overflow — rejected in favour
  of reusing `forward_audit_buffer_drops_total` (operators already
  alert on it; semantic identical).

---

## R-007 — Backup mechanism

**Decision**: SQLite Online Backup API via `rusqlite::backup::Backup`.
The backup CLI opens a read-only handle on the live DB, opens a fresh
DB at the destination path, and runs `Backup::run` with
`step = -1` (copy-everything-in-one-go), producing a clean single-file
artefact regardless of WAL state. The artefact is a vanilla SQLite
file that any compatible binary can open.

Restore reverses this: open the backup file as the source, the
destination `<data-dir>/state.db` (which must be empty unless
`--force`) as the target, run `Backup::run`, then close. On next start,
the regular `refinery` migration sequence runs (forward-compat).

**Rationale**:
- Online Backup API takes care of WAL-mode consistency without
  requiring a checkpoint or a server stop.
- The artefact is a vanilla SQLite file → operators with `sqlite3` CLI
  on hand can inspect / triage independently.
- Embedding the schema version in the `schema_migrations` table makes
  the FR-013 "embed schema version" requirement free.

**Alternatives considered**:
- `cp state.db backup.db` while running — unsafe in WAL mode; some
  in-flight transactions are in `*-wal` and won't be in the copy.
  Reject.
- Stop-server-and-copy — operator-disruptive; violates the spec's
  "snapshot of a running server" expectation in SC-002.
- `VACUUM INTO 'backup.db'` — works for non-WAL only; in WAL mode
  needs a checkpoint dance. The Backup API supersedes it.
- Custom JSON / msgpack export — loses fidelity, requires schema-
  aware code to round-trip, defeats "single self-contained artefact".

---

## R-008 — Filesystem class probe (NFS/tmpfs refusal)

**Decision**: At startup, after resolving `--data-dir`, the server
calls `statfs(2)` on the directory:

- Linux: read `f_type` and refuse if it matches `NFS_SUPER_MAGIC`
  (0x6969), `TMPFS_MAGIC` (0x01021994), `RAMFS_MAGIC` (0x858458f6), or
  any of the FUSE family if known to lack POSIX locking. Allow `EXT4`,
  `XFS`, `BTRFS`, `ZFS`, `F2FS`, etc.
- macOS: read `f_fstypename` (string) and refuse `nfs`, `smbfs`,
  `webdav`. Allow `apfs`, `hfs`. (macOS is dev only — looser checks
  acceptable.)

The check is wrapped behind a `data_dir::probe_fs_class` function so
unit tests can fake the result.

**Rationale**:
- SQLite's documented hard requirements: POSIX advisory locking +
  durable `fsync`. NFS without `nolock` either hangs or silently
  duplicates writes; tmpfs `fsync` is a no-op + reboot-loss; FUSE
  varies wildly. Failing fast at boot beats silent data corruption.

**Alternatives considered**:
- Best-effort mount-point string check (parse `/proc/mounts`) —
  brittle on edge filesystems, no equivalent on macOS, fragile in
  containers. Reject.
- Probe-by-locking — open the file with `flock(LOCK_EX | LOCK_NB)` and
  detect "lock not supported" from `errno`. Doesn't catch tmpfs
  (locking works, durability does not). Reject.
- Skip the check, trust the operator — silent corruption when a
  helpful sysadmin moves `/var/lib/portunus` onto NFS. Reject by
  edge-case requirement in spec.

---

## R-009 — Pre-v0.8 JSON cleanup behaviour

**Decision**: Warn-and-ignore. On startup the server checks
`<config-dir>` for `tokens.json`, `identity.json`, `rules.json` and
emits a one-line `warn!` per file pointing operators at
`forward-server reset` (or manual rm). The server does not read these
files, regardless of contents.

**Rationale**:
- The spec's no-migration assumption forbids automatic import.
- Auto-deleting unfamiliar files in a config-dir is a footgun in dev
  environments where multiple project versions might coexist.
- A warning produces a discoverable signal in logs without changing
  state.

**Alternatives considered**:
- Auto-delete legacy files — too aggressive; risks data loss for
  developers running v0.7 / v0.8 in alternation.
- Refuse to start if legacy files present — operator-hostile; turns
  every fresh install with a leftover file into a manual cleanup.
- Silent ignore — discoverable only by a curious operator; a warn
  log is the documented best-practice equivalent.

---

## R-010 — Bootstrap superadmin on empty store

**Decision**: The existing `forward-server bootstrap-superadmin` CLI
(v0.5) is preserved at the user-facing level; its body is rewritten to
write through the new `Store`'s identity transaction. Bootstrap is
atomic: the user row, the credential row, and the initial
`schema_migrations` rows commit or roll back together.

**Rationale**:
- Operator habits and existing automation scripts (provisioning,
  Ansible roles) keep working.
- FR-017 mandates atomic bootstrap; a single transaction over the
  three tables (users, credentials, schema_migrations head check) is
  the natural fit.

**Alternatives considered**:
- A new subcommand name (`init`, `setup`, `genesis`) — net negative;
  every operator that knows v0.5 must relearn.
- Server self-bootstrap on first request — surprising for operators
  used to the explicit one-shot CLI step; produces auth-by-accident
  if the server is exposed before bootstrap.

---

## R-011 — Reset CLI surface

**Decision**: New top-level subcommand
`forward-server reset --confirm`. The command:

1. Opens the store at the resolved `--data-dir` to verify we're
   pointing at a real SQLite file (refuse to "reset" an arbitrary
   path).
2. Closes the connection pool.
3. Atomically removes `state.db`, `state.db-wal`, `state.db-shm`
   (rename-to-temp + unlink, fall back to direct unlink on platforms
   where rename fails).
4. Exits with a structured log line summarising what was deleted.

`--confirm` is mandatory — running without it errors out with the
exact command-line the operator must type. No double-prompt; CLI is
expected to be scripted.

**Rationale**:
- Mirrors today's `rm rules.json && rm identity.json && rm tokens.json`
  ergonomics behind a single command (FR-015).
- Sidecar files removed in the same step prevents accidental
  resurrection (edge case "operator-driven full reset").
- `--confirm` flag (rather than an interactive prompt) keeps the
  surface CI-friendly and matches existing project CLI conventions
  (every other destructive subcommand follows the same pattern).

**Alternatives considered**:
- `forward-server data wipe` — verbose; existing destructive
  subcommands use single-word verbs (`provision-client`, `revoke`,
  `remove-rule`).
- Interactive Y/N prompt — breaks scripted runs (Ansible, CI).
- `--force` instead of `--confirm` — `--force` is overloaded across
  the rest of the CLI for "skip warnings"; using a distinct word
  flags the higher blast radius.

---

## R-012 — Audit table indexes for time-range + filter queries

**Decision**: Three composite indexes plus the implicit primary key:

- `audit (ts DESC)` — primary access pattern: newest-first list with
  optional `limit`.
- `audit (outcome, ts DESC)` — operator filters by allow/deny.
- `audit (user_id, ts DESC)` — per-user historical drilldown.

The PK is a monotonic `seq INTEGER PRIMARY KEY AUTOINCREMENT` which
gives implicit insertion-order ordering for cursor pagination and
serves as a stable cursor token.

**Rationale**:
- These three queries cover the FR-007 expansion (since/until/cursor)
  + the v0.6 / v0.7 baseline (`?limit=&outcome=`) + the future
  per-user reporting use case.
- Composite-with-`ts DESC` lets SQLite do a single index range scan
  for "deny entries in the last 24h, newest 50" without a sort step
  — proven on the SC-005 100k-entry budget in a quick local
  prototype.
- `seq AUTOINCREMENT` is monotonic across rolls — good cursor token
  semantics (no duplication on millisecond-tied `ts`).

**Alternatives considered**:
- Single composite (`outcome, user_id, ts DESC`) — fewer indexes but
  the leading-column rule means it can't serve "all outcomes, time
  range only" queries efficiently.
- Materialised view per filter — premature for the SC-005 budget.
- No indexes, full scan — fine for 1k entries, breaks at 100k.

---

## R-013 — `AppState` connection sharing

**Decision**: `AppState { store: Arc<Store>, ... }` where `Store`
wraps the `r2d2::Pool` and exposes typed methods. Per-request handlers
borrow a connection via `store.with_conn(|conn| ...)`. The audit
writer holds a long-lived dedicated connection.

**Rationale**:
- Mirrors today's `AppState` structure (which already wraps stores
  behind `Arc`s).
- `Arc<Store>` is `Send + Sync` and works directly with axum's state
  extractor.

**Alternatives considered**:
- Pass the pool directly through `AppState` — exposes `r2d2::Pool` as
  a public type across crates. Reject (encapsulation).
- `tokio::task_local!` connection — coloured async, complicated
  testing.

---

## R-014 — Multi-table mutation transactions

**Decision**: Every mutation that touches more than one table opens
its transaction with `BEGIN IMMEDIATE`. Read-only operations use the
default deferred mode.

**Rationale**:
- `BEGIN IMMEDIATE` acquires the writer lock up-front; if another
  writer holds it, we get `SQLITE_BUSY` immediately rather than
  mid-statement, which would otherwise abort the in-progress insert
  with no clean retry boundary.
- The writer lock is released on COMMIT/ROLLBACK; readers (`BEGIN
  DEFERRED`) are unaffected — WAL mode keeps them concurrent.

**Alternatives considered**:
- All mutations as `BEGIN EXCLUSIVE` — overkill, blocks all readers.
- All mutations as default deferred — risk of mid-transaction
  upgrade-to-write failing with `SQLITE_BUSY`. Reject.

---

## R-015 — `rusqlite::Error` mapping

**Decision**: Map at the `Store` boundary into existing
`ForwardError` variants:

- `SqliteFailure(code, _) where code == SQLITE_BUSY | SQLITE_LOCKED` →
  `ForwardError::Transient` (caller may retry; rare in practice
  thanks to `BEGIN IMMEDIATE` + busy_timeout).
- `SqliteFailure(code, _) where code is a constraint violation
  (UNIQUE, CHECK, FOREIGN KEY, NOT NULL)` → `ForwardError::Conflict`
  with a structured detail field.
- `SqliteFailure(code, _) where code is corruption-class
  (SQLITE_CORRUPT, SQLITE_NOTADB, SQLITE_IOERR_*)` → fail-fast at
  boot with a recovery hint pointing at backup/restore.
- All others → `ForwardError::Internal` with the underlying message
  preserved in `tracing` only (never leaked to operator API
  responses; only an opaque `request_id` is surfaced).

**Rationale**:
- Reuses the existing `ForwardError` taxonomy → no breaking changes
  in `forward-server::operator::http` error mapping.
- Constraint violations stay actionable for operators (e.g.,
  duplicate user_id surfaces as a 409 with a clear reason); internal
  details stay internal.

**Alternatives considered**:
- A new `ForwardError::Storage(rusqlite::Error)` variant — leaks the
  driver type out of `forward-auth` / `forward-server` into the
  public API surface; reject on encapsulation grounds.
- Retry on every `SQLITE_BUSY` inside the store — pushes retries into
  a place where the caller cannot control timeout; the existing
  `Transient` variant is the documented retry seam.
