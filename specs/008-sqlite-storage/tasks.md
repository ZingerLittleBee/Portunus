---

description: "Task list for 008 — Unified Embedded SQLite Store"
---

# Tasks: Unified Embedded SQLite Store for Server Persistent State

**Input**: Design documents from `/specs/008-sqlite-storage/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: REQUIRED. Constitution Principle III (Test-First Discipline) is non-negotiable; every contract surface listed in `contracts/` and every SC- in `spec.md` ships with a contract / integration test authored before its implementation.

**Organization**: Tasks are grouped by user story so each story is independently completable. Phase 2 (Foundational) is the only blocking prerequisite — once it lands, US1..US4 can be tackled in priority order or in parallel by different contributors.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Different file, no dependency on incomplete tasks → can run in parallel
- **[Story]**: US1..US4 maps to spec.md user stories (P1..P3); setup / foundational / polish carry no story label
- File paths are absolute relative to the repo root

## Path conventions

Cargo workspace, six crates. New code lands in:

- `crates/forward-server/src/{store,operator,data_dir.rs}`
- `crates/forward-auth/src/sqlite_*.rs`
- `crates/forward-client/src/{main.rs,bundle.rs}` (only the optional-bundle change)

Test homes:

- Per-crate `tests/` directories (Cargo integration test convention)
- Cross-crate end-to-end in `crates/forward-e2e/tests/`

Bench homes:

- `crates/forward-server/benches/operator_api.rs` (new)
- `crates/forward-client/benches/data_plane.rs` (existing v0.7 baseline; reused)

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Pull in the new dependencies and stand up the empty scaffolding required by every later phase. No business logic.

- [x] T001 Add new workspace dependencies to `/Users/zingerbee/Documents/forward-rs/Cargo.toml`: `rusqlite = { version = "0.32", features = ["bundled", "blob", "backup", "chrono", "limits"] }`, `r2d2 = "0.8"`, `r2d2_sqlite = "0.25"`, `refinery = { version = "0.8", features = ["rusqlite"] }`, `refinery_macros = "0.8"`, `num_cpus = "1"`. Reference per crate via `workspace = true` in `crates/forward-server/Cargo.toml` and `crates/forward-auth/Cargo.toml` (R-001, R-005).
- [x] T002 [P] Create empty bench scaffold `/Users/zingerbee/Documents/forward-rs/crates/forward-server/benches/operator_api.rs` and register it under `[[bench]]` in `crates/forward-server/Cargo.toml`. Body for now: `criterion_main!(()); fn _placeholder() {}`. Real benches added in T027 / T071.
- [x] T003 [P] Add the `--data-dir <PATH>` global flag to the `Cli` struct in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/main.rs`. Stub the resolution function `resolve_data_dir(opt: Option<PathBuf>) -> PathBuf` to `unimplemented!()`; it is filled in T004. Wire the flag into every subcommand that opens the store (`serve`, `bootstrap-superadmin`, `provision-client`, `revoke`, `list-clients`, `push-rule`, `remove-rule`, `list-rules`, `rule-stats`).
- [x] T004 [P] Create empty module file `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/data_dir.rs` and register it in `lib.rs` / `main.rs` mod tree. Body: a stub `pub fn resolve(...)` and `pub fn probe_fs_class(...)`.
- [x] T005 Create directory tree under `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/store/`: `mod.rs` (empty), `migrations/` (empty), `audit_writer.rs` (empty), `backup.rs` (empty). Register `mod store;` in the parent module.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Make `state.db` openable, migrated, transactional, and discoverable. Until this phase is complete, no user-story-level code can be implemented because the trait seams (`Authenticator`, `OperatorAuthenticator`, rule CRUD, audit emit) all need a real `Arc<Store>`.

**⚠️ CRITICAL**: All US-labelled tasks (Phase 3 onward) are blocked by this phase.

### Tests for Foundational (TDD per Constitution III)

> Write these tests FIRST and confirm they fail before implementing T011..T020.

- [x] T006 [P] Contract test for `--data-dir` resolution order in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/data_dir_resolution.rs`: cover (a) `--data-dir` explicit override; (b) `$STATE_DIRECTORY` env var; (c) `$XDG_STATE_HOME/forward-rs`; (d) `$HOME/.local/state/forward-rs`; (e) cwd `./forward-rs.state` fallback. Asserts the resolved path matches FR-019.
- [x] T007 [P] Contract test for unsupported-filesystem refusal in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/data_dir_unsupported_fs.rs`: mock `probe_fs_class` to return NFS / TMPFS / RAMFS magic and assert the boot path returns the structured error documented in `contracts/persistence.md` §4 (exit 78, event `startup.unsupported_filesystem`).
- [x] T008 [P] Contract test for store-in-use refusal in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/store_in_use.rs`: open the store, then attempt a second `Store::open` against the same path, assert exit 75 + event `startup.store_in_use`.
- [x] T009 [P] Contract test for schema-too-new refusal in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/store_schema_handshake.rs`: hand-craft a `state.db` with `schema_migrations` rows beyond the binary's supported range, assert refuses to start with event `startup.schema_version_too_new`, exit 78.
- [x] T010 [P] Contract test for corrupt-store refusal in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/store_corrupt.rs`: write garbage bytes to a file named `state.db`, assert refuses to start with event `startup.store_corrupt`, exit 78.
- [x] T011 [P] Contract test for legacy JSON warn-and-ignore in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/legacy_json_ignored.rs`: drop `tokens.json` / `identity.json` / `rules.json` into the resolved `--config-dir`, assert one structured `event=startup.legacy_persistence_file_ignored` warning per file and that the server still proceeds (R-009).
- [x] T012 [P] Contract test for required PRAGMAs in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/store_pragmas.rs`: open a fresh store and assert every connection in the pool has WAL, NORMAL, foreign_keys=ON, busy_timeout=5000, temp_store=MEMORY, cache_size=-8000 (`contracts/persistence.md` §5).

### Implementation for Foundational

- [x] T013 Implement `data_dir::resolve` and `data_dir::probe_fs_class` in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/data_dir.rs` per FR-019 + R-008. Linux uses `nix::sys::statfs::statfs` `f_type` matched against `NFS_SUPER_MAGIC` (0x6969), `TMPFS_MAGIC` (0x01021994), `RAMFS_MAGIC` (0x858458f6). macOS uses `f_fstypename` string match. Make T006 + T007 pass.
- [x] T014 Author the initial schema migration `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/store/migrations/V001__initial_schema.sql` per `data-model.md` §Tables (every column, every CHECK, every index, STRICT mode, all FKs `ON DELETE CASCADE` where specified). Embed via `refinery::embed_migrations!` in `store/mod.rs`.
- [x] T015 Implement `Store` in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/store/mod.rs`: wraps `r2d2::Pool<SqliteConnectionManager>`, sets PRAGMAs through `with_init`, exposes `with_conn`, `with_write_tx` (BEGIN IMMEDIATE), and a `migrate()` boot helper. Pool size = `min(num_cpus::get(), 8)`. Make T012 pass.
- [x] T016 Wire `refinery::embed_migrations!` and the schema-version handshake in `Store::open` per `contracts/persistence.md` §2. Includes: detect `read > target` and bubble a structured error mapped to exit 78 in main.rs. Make T009 pass.
- [x] T017 Implement file-lock-based store-in-use detection in `Store::open`: acquire `flock(LOCK_EX | LOCK_NB)` on `state.db` for the lifetime of the pool; release on `Drop`. Make T008 pass.
- [x] T018 Map `rusqlite::Error` to `forward_core::ForwardError` per R-015 in a new `store::error` submodule. `SQLITE_BUSY` / `SQLITE_LOCKED` → `Transient`; constraint family → `Conflict { detail }`; corruption → fail-fast at boot. Add unit tests inline.
- [x] T019 Add `pub store: Arc<Store>` field to `AppState` in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/state.rs`. Update every `AppState::new(...)` call site (build_offline_state, build_online_state) to construct the store from the resolved data-dir.
- [x] T020 In `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/serve.rs`, add the legacy-file warn-and-ignore boot step BEFORE opening the store (R-009). Emit one `warn!(event = "startup.legacy_persistence_file_ignored", path = ...)` per file found. Make T011 pass.
- [x] T021 In `crates/forward-server/src/serve.rs` boot sequence, the order MUST be: (1) `data_dir::resolve` → (2) `data_dir::probe_fs_class` (refuse if unsupported) → (3) legacy-file warn → (4) `Store::open` → (5) `store.migrate()` → (6) bootstrap completeness check → (7) listener bind. Document this in a header comment block.
- [x] T022 Provide `Store::checkpoint_for_clean_shutdown()` (issues `PRAGMA wal_checkpoint(TRUNCATE)` on a dedicated handle) and call it from the existing graceful-shutdown drain in `serve.rs` so an orderly shutdown leaves a quiesced WAL.

**Checkpoint**: Foundation ready. From here on, each US can be implemented and tested in isolation against the existing `Arc<Store>`.

---

## Phase 3: User Story 1 — Audit Log Survives Server Restart (Priority: P1) 🎯 MVP

**Goal**: Persist every audit event into the `audit` table; surface them through the existing `GET /v1/audit` endpoint with byte-stable v0.7 shape; tolerate process kill within FR-005's 100 ms typical / 1 s burst window.

**Independent Test**: bring a fresh server up, perform a documented mix of operator allow / deny calls, kill -9 the server, restart, query `GET /v1/audit?limit=N` — every pre-kill entry except those in the last sub-second window is present and ordered newest-first (SC-001).

### Tests for User Story 1 (TDD)

- [x] T023 [P] [US1] Integration test: audit persists across clean shutdown in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/audit_persists_clean_restart.rs`. Performs 50 audit events → graceful stop → restart → assert all 50 retrievable in correct order (SC-001).
- [ ] T024 [P] [US1] Integration test: audit persists across SIGKILL in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/audit_persists_kill_restart.rs`. Same as T023 but forks a child server, kills it with SIGKILL after a fixed delay; asserts at most 1 s of activity is lost (FR-005 + SC-001).
- [ ] T025 [P] [US1] Contract test: `forward_audit_buffer_drops_total` increments on hand-off queue overflow, oldest entry dropped, in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/audit_overflow_drop.rs`. Inject 2× the queue capacity in a tight loop, assert the counter reflects the surplus and the durable rows are the most recent.
- [ ] T026 [P] [US1] Contract test: `GET /v1/audit?limit=&outcome=` returns the v0.7 JSON-array root in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/audit_v07_array_root.rs`. Snapshot a v0.7 response, replay against the v0.8 server, assert byte-equal modulo seeded timestamps.
- [ ] T027 [P] [US1] Bench: operator API p50 / p99 within 10 % of v0.7 baseline in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/benches/operator_api.rs`. Run `GET /v1/users` + `GET /v1/rules` + audit-emit-bearing `POST /v1/users/{id}/credentials` under a representative load (criterion). Compare to the saved v0.7 baseline checked in under `specs/008-sqlite-storage/baselines/operator_api_v07.json` (T071 will refresh / verify).
- [ ] T028 [P] [US1] End-to-end test: full server↔client smoke including audit retention in `/Users/zingerbee/Documents/forward-rs/crates/forward-e2e/tests/audit_persists_e2e.rs`.

### Implementation for User Story 1

- [x] T029 [P] [US1] Implement audit-write SQL in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/store/mod.rs`: a typed method `Store::insert_audit_batch(&[AuditEntry])` using a single prepared statement inside one transaction. Honours the `audit` table shape from `data-model.md` (seq AUTOINCREMENT, ts, user_id, outcome, action, resource_kind, resource_value, correlation_id, details_json).
- [x] T030 [US1] Implement `audit_writer` in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/store/audit_writer.rs` per R-006: bounded `tokio::sync::mpsc::channel<AuditEntry>` capacity 1024, drop-oldest on `Full`, single durable-writer task that holds a long-lived dedicated connection and batches up to 256 entries or 100 ms (whichever first) per `BEGIN IMMEDIATE` transaction. Increments `forward_audit_buffer_drops_total` on overflow and updates `forward_audit_durable_writer_lag_seconds`.
- [x] T031 [US1] Add gauge `forward_audit_durable_writer_lag_seconds` and counter passthrough for `forward_store_busy_total` to `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/metrics.rs`. (Counter `forward_audit_buffer_drops_total` already exists in v0.6; reuse — semantic identical per `contracts/operator-api.md` §Prometheus.)
- [x] T032 [US1] Rewire emit sites in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/operator/audit.rs`: replace `Mutex<VecDeque<AuditEntry>>` push with a non-blocking `audit_writer::Handle::record(entry)`. Remove the in-memory ring buffer struct. Keep the public `record(...)` signature byte-compatible so `auth_layer` call sites do not change.
- [x] T033 [US1] Update `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/operator/audit_http.rs` so `GET /v1/audit` reads from the `audit` table via `Store`. When NEITHER `since`, `until`, nor `cursor` are passed, emit the v0.7 JSON-array-root shape; envelope mode lands in US4. Make T026 pass.
- [x] T034 [US1] Wire `audit_writer::spawn(store_handle, drain_token)` into `serve.rs` boot, and call its `flush_and_close()` on graceful shutdown so the durable writer drains before the listener stops.

**Checkpoint**: User Story 1 is independently shippable. Audit survives clean and abrupt restart; `/v1/audit` works; v0.7 callers are byte-stable.

---

## Phase 4: User Story 2 — Unified, Atomic Control-Plane State (Priority: P1)

**Goal**: Replace `FileTokenStore` and `FileOperatorStore` with SQLite-backed `Authenticator` / `OperatorAuthenticator` impls; route every rule CRUD through `Store`; guarantee atomic multi-entity mutations under fault injection (SC-003); preserve byte-stable HTTP shapes for `/v1/users`, `/v1/rules`, `/v1/grants`, etc.

**Independent Test**: remove a user with N rules, M credentials, K grants; SIGKILL the server during the multi-table delete; on restart, the post-kill state is either fully pre-mutation or fully post-mutation (FR-004 + SC-003).

### Tests for User Story 2 (TDD)

- [x] T035 [P] [US2] Contract test: SQLite-backed `Authenticator` parity in `/Users/zingerbee/Documents/forward-rs/crates/forward-auth/tests/sqlite_store_contract.rs`. Replays the v0.5 / v0.7 `file_store::tests` battery (issue / verify / revoke / persist + reload / reject duplicates / mode-bits / blake3 hash round-trip) against the new impl.
- [x] T036 [P] [US2] Contract test: SQLite-backed `OperatorAuthenticator` parity in `/Users/zingerbee/Documents/forward-rs/crates/forward-auth/tests/sqlite_operator_store_contract.rs`. Replays the v0.5 `operator_store::tests` battery (users / credentials / grants invariants).
- [x] T037 [P] [US2] Integration test: multi-entity atomic delete under fault injection in `/Users/zingerbee/Documents/forward-rs/crates/forward-auth/tests/multi_entity_atomic.rs`. Forks a child process, calls `DELETE /v1/users/{id}` mid-transaction (a hook in `Store::with_write_tx` that panics after the second statement), restart, assert state is one of the two valid endpoints — never a hybrid (SC-003). Repeats 100× to expose timing variance.
- [ ] T038 [P] [US2] Contract test: `GET /v1/users` byte-stable in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/operator_api_v07_compat.rs::users`. Same byte-equality framework as T026.
- [ ] T039 [P] [US2] Contract test: `GET /v1/rules` byte-stable in same file as T038, separate `#[test]`.
- [ ] T040 [P] [US2] Contract test: `GET /v1/users/me` and `GET /v1/users/{id}` byte-stable in same file.
- [ ] T041 [P] [US2] Contract test: rule CRUD round-trip including v0.7 multi-target shape in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/rules_crud_sqlite.rs`. Push a multi-target rule, list, get, remove; assert v0.7 wire shape preserved.
- [ ] T042 [P] [US2] End-to-end: v0.7 multi-target failover behaviour byte-identical in `/Users/zingerbee/Documents/forward-rs/crates/forward-e2e/tests/multi_target_unchanged.rs`.

### Implementation for User Story 2

- [x] T043 [P] [US2] Implement `Authenticator` for `SqliteTokenStore` in `/Users/zingerbee/Documents/forward-rs/crates/forward-auth/src/sqlite_store.rs`: `verify`, `issue`, `revoke`, plus a `list()` surface; statements use the `client_tokens` table; blake3 hashing reuses `crate::token::hash_token`. Make T035 pass.
- [x] T044 [P] [US2] Implement `OperatorAuthenticator` for `SqliteOperatorStore` in `/Users/zingerbee/Documents/forward-rs/crates/forward-auth/src/sqlite_operator_store.rs`: `bootstrap_pair`, `bootstrap_legacy_superadmin`, `add_user`, `remove_user` (cascading), `issue_credential`, `revoke_credential`, `rotate_credential`, `add_grant`, `revoke_grant`, all read-side surfaces. All multi-table mutations go through `Store::with_write_tx` (BEGIN IMMEDIATE per R-014). Make T036 + T037 pass.
- [ ] T045 [US2] Swap rules CRUD in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/rules.rs`: read / write paths now use `Store`; the in-memory `Vec<Rule>` cache (today's pattern) is rebuilt from a single `SELECT` on demand or kept invalidated on every mutation. The public `Rule` struct shape stays byte-identical with v0.7 (FR-008). Make T041 pass.
- [x] T046 [US2] Cascade-delete-on-user-removal: `DELETE FROM users WHERE user_id = ?` triggers FK cascades for `credentials` / `grants` / `rules` / `rule_targets`. Wrap the call in a single `BEGIN IMMEDIATE` transaction. Make T037 pass.
- [ ] T047 [US2] Constraint-violation mapping: in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/operator/http.rs` error-converter, surface `ForwardError::Conflict { detail }` as HTTP 409 with the body shape from `contracts/operator-api.md` §Error response envelope. Add an inline test for duplicate `client_name` and duplicate `user_id`.
- [x] T048 [US2] Rewrite `forward-server bootstrap-superadmin` body in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/main.rs` (and any helper in `crates/forward-server/src/operator/cli.rs`) to call `SqliteOperatorStore::bootstrap_pair` inside one transaction (R-010 + FR-017). Add an inline test that asserts no row exists if the credential insert fails.
- [ ] T049 [US2] Delete `/Users/zingerbee/Documents/forward-rs/crates/forward-auth/src/file_store.rs` and remove every `use ::file_store::*` import. Update `crates/forward-auth/src/lib.rs` `mod` declarations. Confirm `cargo check` clean.
- [ ] T050 [US2] Delete `/Users/zingerbee/Documents/forward-rs/crates/forward-auth/src/operator_store.rs` and remove its imports. Update `lib.rs` exports.
- [ ] T051 [US2] Delete the JSON read/write helpers in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/rules.rs` (the `load_from_disk` / `persist` style functions); the trait surface for in-memory state stays.
- [x] T052 [US2] Update `crates/forward-server/src/serve.rs` + `crates/forward-server/src/main.rs` constructor sites: replace `FileTokenStore::open(...)` and `FileOperatorStore::open(...)` with the new `SqliteTokenStore::new(store.clone())` and `SqliteOperatorStore::new(store.clone())`.
- [ ] T053 [US2] Add an integration test that proves grant lookup latency is independent of user-id presence vs absence in `/Users/zingerbee/Documents/forward-rs/crates/forward-auth/tests/grant_lookup_timing.rs` (Constitution Principle V verification).

**Checkpoint**: User Stories 1 + 2 are functionally complete. The codebase no longer reads or writes `tokens.json`, `identity.json`, `rules.json`. v0.7 HTTP / wire / forwarding-plane behaviour is byte-stable.

---

## Phase 5: User Story 3 — Backup / Restore as a Single Operation (Priority: P2)

**Goal**: Single-file backup of a live server, single-file restore on a fresh installation; full state recoverable in ≤ 5 minutes for the SC-002 budget; restore from older schema auto-migrates; restore from newer schema is refused.

**Independent Test**: populate a server with 10 000 audit + 100 rules + 50 users + 50 tokens, run `forward-server backup --out /tmp/x.db`, copy to a fresh host, run `forward-server restore --in /tmp/x.db && forward-server serve`, verify all entities present (SC-002).

### Tests for User Story 3 (TDD)

- [ ] T054 [P] [US3] Integration: backup → restore roundtrip in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/backup_restore_roundtrip.rs`. Seeds the store, runs backup, opens the artefact with vanilla `rusqlite::Connection::open_with_flags(... READ_ONLY)`, asserts every table count matches; then runs restore on a fresh data-dir and re-asserts (SC-002).
- [ ] T055 [P] [US3] Contract: `restore` refuses non-empty data-dir without `--force` in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/cli_restore.rs::refuses_non_empty`.
- [ ] T056 [P] [US3] Contract: `restore` from older schema runs forward migration in `crates/forward-server/tests/cli_restore.rs::forward_migrates`. Uses a hand-crafted backup at fake schema version 0 and asserts the restored DB is at the binary's target version.
- [ ] T057 [P] [US3] Contract: `restore` refuses newer-than-binary schema in `crates/forward-server/tests/cli_restore.rs::refuses_too_new`. Same fixture but with schema version = target+1; asserts exit 78 with event `startup.schema_version_too_new` (also covers FR-014's refusal mode).
- [ ] T058 [P] [US3] Contract: `cli_backup` happy path + destination-already-exists refusal in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/cli_backup.rs`.
- [ ] T059 [P] [US3] Contract: `cli_reset` happy path + sidecar cleanup + signature-check refusal of typo'd `--data-dir` in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/cli_reset.rs`. (`reset` ships in this phase because lifecycle CLIs are co-located.)

### Implementation for User Story 3

- [ ] T060 [US3] Implement `store::backup::run_backup(src_path, dst_path)` in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/store/backup.rs` using `rusqlite::backup::Backup::run(-1)` per R-007. Opens source read-only, destination as fresh empty DB, copies, closes. Returns the absolute destination path.
- [ ] T061 [US3] Implement `store::backup::run_restore(src_artefact, dst_data_dir, force: bool)` in same file. Validates source signature, refuses non-empty dst unless `--force`, copies, then runs the regular schema-version handshake. On migration failure, removes the half-written destination.
- [ ] T062 [US3] Add `forward-server backup --out <PATH>` subcommand to `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/main.rs` and `crates/forward-server/src/operator/cli.rs`. Resolves `--out` (file or directory; if directory, use `forward-state-<RFC3339>.db`). Refuses to overwrite an existing file. Make T058 pass.
- [ ] T063 [US3] Add `forward-server restore --in <PATH> [--force]` subcommand at the same call sites. Make T055 + T056 + T057 pass.
- [ ] T064 [US3] Add `forward-server reset --confirm` subcommand at the same call sites. Closes the pool, removes `state.db` + `state.db-wal` + `state.db-shm` via rename-then-unlink. Refuses to operate if the file does not look like SQLite (signature check) — protects against typo'd `--data-dir` (R-011). Make T059 pass.
- [ ] T065 [US3] Add a structured-log event `event=cli.backup_complete` (and the equivalents for restore + reset) so operators can grep for outcomes. The events are listed alongside existing CLI events in `crates/forward-server/src/operator/cli.rs`.

**Checkpoint**: User Stories 1 + 2 + 3 functional. Operators can disaster-recover with two CLI commands.

---

## Phase 6: User Story 4 — Historic Audit Query With Pagination & Time Range (Priority: P3)

**Goal**: Extend `GET /v1/audit` with opt-in `since` / `until` / `cursor` parameters; expose the envelope shape only when at least one is passed; provide `audit prune` for operator-managed retention; SC-005 page query under 2 s for 100 k entries.

**Independent Test**: load 100 k audit entries spread across multiple days, query with `since` / `until` / `limit`, paginate via `cursor`, assert each page is correct, no duplicates / omissions, p99 page latency < 2 s (SC-005).

### Tests for User Story 4 (TDD)

- [ ] T066 [P] [US4] Contract: `GET /v1/audit?since=...` returns the envelope shape in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/audit_v08_envelope.rs::since_returns_envelope`.
- [ ] T067 [P] [US4] Contract: cursor pagination round-trip across N pages reaches every entry exactly once in `crates/forward-server/tests/audit_v08_envelope.rs::pagination_round_trip`.
- [ ] T068 [P] [US4] Contract: invalid cursor → HTTP 400 `invalid_cursor` in same file.
- [ ] T069 [P] [US4] Contract: `since` after `until` → HTTP 400 `invalid_time_range` in same file.
- [ ] T070 [P] [US4] Contract: invalid RFC3339 in `since` / `until` → HTTP 400 `invalid_timestamp` with the offending field name in same file.
- [ ] T071 [P] [US4] Performance: page query under 2 s at 100 k entries in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/audit_query_scaling.rs`. Seeds 100 k rows via the `audit_writer` fast path; runs three representative queries (no filter, outcome filter, since-until-cursor); asserts p99 < 2 s on a developer-class machine (SC-005). Marked `#[ignore]` by default; CI runs it nightly.
- [ ] T072 [P] [US4] Contract: `audit prune --before` deletes only matching rows + `--dry-run` does not modify the DB in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/cli_audit_prune.rs`.

### Implementation for User Story 4

- [ ] T073 [US4] Add `since`, `until`, `cursor` query parameter parsing in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/operator/audit_http.rs`. Per `contracts/operator-api.md`: any of the three present → switch to envelope mode; none present → keep v0.7 array root.
- [ ] T074 [US4] Implement opaque cursor encoding (base64-encoded `seq` integer) in `audit_http.rs`. Validation rejects malformed cursors with HTTP 400. Make T068 pass.
- [ ] T075 [US4] Expand `Store::query_audit` to take `(since, until, outcome, cursor, limit)` and return `(rows, next_cursor)`. Uses the indexes from `data-model.md` §audit (`audit_ts_idx`, `audit_outcome_ts_idx`); confirms with `EXPLAIN QUERY PLAN` in the test.
- [ ] T076 [US4] Add `forward-server audit prune --before <RFC3339> [--dry-run]` subcommand wired in `crates/forward-server/src/main.rs` + `crates/forward-server/src/operator/cli.rs`. Implementation runs `DELETE FROM audit WHERE ts < ?` inside `BEGIN IMMEDIATE`, then `PRAGMA incremental_vacuum;`. `--dry-run` returns the count via `SELECT COUNT(*)` only. Make T072 pass.
- [ ] T077 [US4] Update the operator Web UI's audit page (`web/src/pages/AuditPage.tsx` or equivalent under `crates/forward-server/web/`) to consume the envelope when scrolling back. v0.7 default load (no params) keeps the existing array-root code path.

**Checkpoint**: All four user stories functional and independently testable.

---

## Phase 7: Polish & Cross-Cutting Concerns

- [ ] T078 [P] Make `--bundle` optional on `forward-client` per FR-020 in `/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/main.rs` and `crates/forward-client/src/bundle.rs`. Resolution order: `--bundle` > `$FORWARD_CLIENT_BUNDLE` > `$XDG_CONFIG_HOME/forward-rs/client.bundle.json` > `$HOME/.config/forward-rs/client.bundle.json` > `./client.bundle.json`. Exit 1 with all attempted paths listed when none resolve.
- [ ] T079 [P] Contract test: bundle search-path resolution in `/Users/zingerbee/Documents/forward-rs/crates/forward-client/tests/bundle_search_path.rs`. Cover all 4 fallbacks + explicit override + the not-found error message format.
- [ ] T080 [P] Update `/Users/zingerbee/Documents/forward-rs/CHANGELOG.md` with a new `## [0.8.0]` section: SQLite store, `--data-dir`, backup / restore / reset / audit prune CLIs, additive audit query params, FR-020 client bundle search, deletion of `tokens.json` / `identity.json` / `rules.json` legacy persistence layer, closure of constitution `TODO(STORAGE_CHOICE)`.
- [ ] T081 [P] Update `/Users/zingerbee/Documents/forward-rs/.specify/memory/constitution.md` Sync Impact Report with `TODO(STORAGE_CHOICE)` resolved-at note pointing to `specs/008-sqlite-storage/` (PATCH bump of constitution; or fold into v0.8 release prep — whichever your release process prefers).
- [ ] T082 Bench parity: forwarding-plane throughput / p99 within 5 % of the v0.7 baseline, asserted via `/Users/zingerbee/Documents/forward-rs/crates/forward-client/benches/data_plane.rs` (existing v0.7 baseline; run on the v0.8 binary, compare to `target/criterion/...` saved baselines). SC-007.
- [ ] T083 Run `cargo clippy --workspace --all-targets -- -D warnings` and resolve every warning before tagging.
- [ ] T084 Run `cargo test --workspace --release` final pass and confirm every contract / integration test introduced in T006..T079 passes.
- [ ] T085 Walk through `quickstart.md` §1 (production cold start), §2 (dev cold start with legacy file warn-and-ignore), §3 (backup / restore), §4 (client bundle resolution), §5 (audit historic query) on a clean checkout. Capture any drift between the doc and behaviour and fix in-place before tagging.
- [ ] T086 Tag `v0.8.0` in git, push, and update `Cargo.toml` workspace `version = "0.8.0"`.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No deps. Start immediately.
- **Foundational (Phase 2)**: Depends on Phase 1. **BLOCKS all user stories.**
- **User Stories (Phase 3..6)**: All depend on Phase 2. Within Phase 2 + 3, no further blocker.
- **Polish (Phase 7)**: Depends on US1..US4 being complete (or at least the ones in scope for the cut).

### User Story Dependencies

- **US1** (audit persistence): No dependency on other US.
- **US2** (atomic control-plane): No dependency on US1; touches different tables (`users`, `credentials`, `grants`, `rules`, `rule_targets`, `client_tokens`) and different trait seams.
- **US3** (backup / restore): Logically depends on US1 + US2 having populated tables to back up; in practice, the `backup.rs` API is independent and US3 can be implemented in parallel — its tests need a populated store, which the `Store` from Phase 2 already provides.
- **US4** (historic audit query): Depends on US1 (audit table populated). Implementation of `since` / `until` / `cursor` is independent of US2 / US3.

### Within Each User Story (TDD per Constitution III)

- All `T0xx [P]` test tasks within a story MUST be authored and observed failing before any non-test implementation task in the same story is started.
- Models / migrations exist in Phase 2 → no per-story model task; per-story implementation goes straight to services + endpoints.

### Parallel Opportunities

- All `[P]` tasks in **Phase 1** (T002, T003, T004) can run in parallel.
- All `[P]` tests in **Phase 2** (T006..T012) can be authored in parallel.
- All `[P]` tests in **Phase 3** (T023..T028) can run in parallel; same for `[P]` impl tasks (T029) where they exist.
- All `[P]` tests in **Phase 4** (T035..T042) and `[P]` impls (T043, T044) can run in parallel by different developers.
- All `[P]` tests in **Phase 5** (T054..T059) and **Phase 6** (T066..T072) can run in parallel.
- All `[P]` Polish tasks (T078..T081) can run in parallel.
- Cross-story: once Phase 2 is done, US1, US2, US3, US4 can each be staffed to a different developer.

---

## Parallel Example: User Story 1

```bash
# Author all US1 tests in parallel (different files, no deps):
Task: "Integration test audit_persists_clean_restart in crates/forward-server/tests/audit_persists_clean_restart.rs"
Task: "Integration test audit_persists_kill_restart in crates/forward-server/tests/audit_persists_kill_restart.rs"
Task: "Contract test audit_overflow_drop in crates/forward-server/tests/audit_overflow_drop.rs"
Task: "Contract test audit_v07_array_root in crates/forward-server/tests/audit_v07_array_root.rs"
Task: "Bench operator_api in crates/forward-server/benches/operator_api.rs"
Task: "End-to-end audit_persists_e2e in crates/forward-e2e/tests/audit_persists_e2e.rs"
```

## Parallel Example: User Story 2

```bash
# Author all US2 tests in parallel:
Task: "Authenticator parity test in crates/forward-auth/tests/sqlite_store_contract.rs"
Task: "OperatorAuthenticator parity test in crates/forward-auth/tests/sqlite_operator_store_contract.rs"
Task: "Multi-entity atomic delete fault injection in crates/forward-auth/tests/multi_entity_atomic.rs"
Task: "GET /v1/users byte-stable in crates/forward-server/tests/operator_api_v07_compat.rs (users)"
Task: "GET /v1/rules byte-stable in same file (rules)"
Task: "Rule CRUD round-trip in crates/forward-server/tests/rules_crud_sqlite.rs"
Task: "Multi-target byte-identical e2e in crates/forward-e2e/tests/multi_target_unchanged.rs"

# Then in parallel (still US2):
Task: "Implement SqliteTokenStore in crates/forward-auth/src/sqlite_store.rs"
Task: "Implement SqliteOperatorStore in crates/forward-auth/src/sqlite_operator_store.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 only)

1. Phase 1 → Phase 2 → Phase 3.
2. **STOP and VALIDATE**: SC-001, SC-004 hold in isolation; the audit page survives restart; `/v1/audit` v0.7 callers byte-stable.
3. Demo / merge the MVP slice. The codebase still has both the SQLite store *and* the legacy JSON files at this point — the JSON files are read by no one (T011 wired warn-and-ignore in Phase 2) but the modules are not yet deleted. That is acceptable for the MVP cut.

### Incremental Delivery

1. MVP (US1) → ship.
2. Add US2 (T035..T053): the legacy JSON modules are deleted in this phase. Demo / merge.
3. Add US3 (backup / restore / reset). Demo / merge.
4. Add US4 (envelope query + audit prune + Web UI). Demo / merge.
5. Polish + tag v0.8.0.

### Parallel Team Strategy

- One developer drives Phase 1 + 2 to the checkpoint.
- Then split: A on US1, B on US2, C on US3 + US4 (US3 + US4 are smaller).
- Polish phase is one developer's last cut: docs, changelog, bench parity, tag.

---

## Notes

- `[P]` tasks touch different files, no in-flight dependencies → safe to parallelise.
- `[Story]` label maps a task to its user story for traceability.
- Every contract / integration test (T0xx where description starts with "Contract" / "Integration" / "End-to-end" / "Performance") MUST be observed failing before its implementation task is merged. CI enforces this via `cargo test --workspace`'s pass-fail signal across the PR series.
- Commit per task or per logical group; the `before_*` hook chain auto-commits if you opt in.
- The `[P]` markers on Polish tasks (T078..T081) are correct because they touch different files (`forward-client/src/main.rs`, `forward-client/tests/bundle_search_path.rs`, `CHANGELOG.md`, `constitution.md`).
- Avoid: editing `crates/forward-server/src/operator/cli.rs` from two parallel CLIs at once — T062, T063, T064, T076 all touch this file and MUST be sequential within their story (the `[P]` marker is intentionally absent on the implementation tasks that share `cli.rs`).
