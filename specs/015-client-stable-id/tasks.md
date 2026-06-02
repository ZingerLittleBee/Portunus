---
description: "Task list for 015-client-stable-id"
---

# Tasks: Client Stable Identifier (name as display field)

**Input**: Design documents from `/specs/015-client-stable-id/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/

**Tests**: REQUIRED. Constitution III (Test-First, NON-NEGOTIABLE) mandates wire
contract tests, real-socket integration tests, and tests-before-implementation. Test
tasks below are written to FAIL first.

**Organization**: This is a horizontal refactor. The identity/label plumbing has no
standalone user value, so it lives in **Phase 2 Foundational** (blocks every story).
User stories are the thin user-facing slices on top. Foundational MUST leave
`cargo test --workspace` green so each story is independently testable.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: parallelizable (different files, no incomplete-task dependency)
- **[Story]**: US1/US2/US3/US4; Setup/Foundational/Polish carry no story label

## Path Conventions

Rust workspace under `crates/`; proto at `proto/portunus.proto`; SPA under `webui/src/`.

---

## Phase 1: Setup

**Purpose**: Record baselines before the refactor.

- [ ] T001 Record green baseline + perf baseline: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace` passes and `cargo bench -p portunus-client --bench data_plane` captured for later flatness comparison
- [ ] T002 [P] Add a `V010`-DB seed helper module (≥2 clients, each with ≥1 rule, owner rate-limit, traffic quota, minute + hour usage, enrollment row) in `crates/portunus-server/tests/support/seed_v010.rs` for reuse by migration + e2e tests

---

## Phase 2: Foundational (Blocking Prerequisites)

**⚠️ CRITICAL**: No user story can begin until this phase is complete and the workspace is green.

### Core identity types (`portunus-core`)

- [ ] T003 [P] Rewrite `ClientName` validation unit tests in `crates/portunus-core/src/id.rs` to the relaxed contract (reject empty/whitespace-only, control chars, >255 bytes; accept uppercase/space/`.`/`_`/`-`/Unicode, verbatim) — tests MUST FAIL first
- [ ] T004 Relax `ClientName::new` and replace `ClientNameError` variants with `Empty`/`TooLong(usize)`/`ControlChar` in `crates/portunus-core/src/id.rs` to pass T003
- [ ] T005 [P] Add `ClientId(Ulid)` unit tests (Display, `FromStr` parse+validate, serde transparent round-trip, `Ord` sort) in `crates/portunus-core/src/id.rs` — MUST FAIL first
- [ ] T006 Implement `ClientId(Ulid)` newtype (`Copy`, `Eq`, `Hash`, `Ord`, `Serialize`/`Deserialize`, `Display`, `FromStr`) in `crates/portunus-core/src/id.rs` to pass T005

### Auth seam (`portunus-auth`)

- [ ] T007 Add `pub client_id: ClientId` to `ClientIdentity` (keep `client_name`) in `crates/portunus-auth/src/lib.rs`

### Wire contract (`proto` + `portunus-proto`)

- [ ] T008 [P] Wire contract tests: `client_id` round-trips in `CredentialBundle`/`OwnerRateLimitUpdate`/`TrafficQuotaUpdate`, and a message WITHOUT `client_id` (legacy) still decodes — in `crates/portunus-server/tests/wire_client_id.rs` — MUST FAIL first
- [ ] T009 Add additive fields to `proto/portunus.proto` (`CredentialBundle.client_id = 7`, `OwnerRateLimitUpdate.client_id = 5`, `TrafficQuotaUpdate.client_id = 6`; `Hello`/`Welcome` unchanged) and rebuild to regenerate tonic-prost types; pass T008

### Persistence migration (`V011`)

- [ ] T010 [P] Migration test in `crates/portunus-server/tests/migration_v011.rs`: from the T002 seed, run the refinery runner; assert every client has a `client_id`, every dependent row backfilled (zero orphans), no table retains a `client_name` PK/UNIQUE, and a second runner pass is a no-op (idempotent) — MUST FAIL first
- [ ] T011 Implement `crates/portunus-server/src/store/migrations/V011__client_id.sql` plus the Rust-side ULID assignment step (mint a ULID per `client_tokens` row, then table-rebuild + name-join backfill for `client_tokens`, `rules`, `rate_limit_owner`, `traffic_quotas`, `traffic_usage_minute`, `traffic_usage_hour`, `client_enrollments`; recreate `rules_client_idx(client_id, listen_port)`) wired into `crates/portunus-server/src/store/mod.rs`; pass T010

### Store layer re-key (all on top of T011)

- [ ] T012 Re-key `token_store`: key by `ClientId`, `verify(token) -> ClientIdentity { client_id, client_name }`, issue/revoke/delete by `client_id`, keep `client_name` as mutable display column; rewrite its unit tests in `crates/portunus-server/src/store/token_store.rs`
- [ ] T013 [P] Re-key `OwnerCap` + all SQL to `client_id` (`WHERE client_id = ?`, PK `(client_id, owner_id)`) in `crates/portunus-server/src/store/owner_cap_store.rs`
- [ ] T014 [P] Re-key `client_enrollments` access to `client_id` in `crates/portunus-server/src/store/enrollment_store.rs`
- [ ] T015 [P] Re-key rule rows to `client_id` (column + index) in `crates/portunus-server/src/store/operator_store.rs`
- [ ] T016 [P] Re-key traffic-quota stores (`traffic_quotas`, `traffic_usage_minute`, `traffic_usage_hour`) to `(user_id, client_id, …)` in the traffic-quota store module(s) under `crates/portunus-server/src/store/`

### Server runtime + control plane

- [ ] T017 Switch `ConnectedClients` to `HashMap<ClientId, ConnectedClient>` (register/unregister/get/set_supported_protocols) in `crates/portunus-server/src/clients.rs`
- [ ] T018 Use `identity.client_id` for registry + rule lookups (keep `client_name` for log fields/display) across `crates/portunus-server/src/grpc/service.rs`
- [ ] T019 Update `crates/portunus-server/src/metrics.rs`: correlate internally by `client_id`, keep the Prometheus `client` label VALUE as the display name
- [ ] T020 Re-path client-scoped operator HTTP routes to `/v1/clients/{client_id}/...` and switch CLI subcommands to `--client-id` across `crates/portunus-server/src/operator/` (owner-cap, rule, quota CLIs + handlers) — keep workspace compiling
- [ ] T021 Update client-reference error messages to use the id (with name for display) in `crates/portunus-server/src/rules.rs` and operator error paths

### Client bundle

- [ ] T022 Add `client_id` to `CredentialBundle` (load/save) with legacy-tolerant parsing (absent id = pre-upgrade bundle) in `crates/portunus-client/src/bundle.rs`

### Checkpoint

- [ ] T023 Foundational green gate: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check` all pass

---

## Phase 3: User Story 1 — Friendly client name (Priority: P1) 🎯 MVP

**Goal**: An operator can create a client with a human-friendly name (uppercase, spaces,
dots, underscores, Unicode); it is stored/shown verbatim and gets a distinct id.

**Independent Test**: Create `Acme Prod – East` and `北京边缘节点`; both succeed, list shows
them verbatim with distinct ids; empty/256-byte/control-char names are rejected with a
specific message.

- [ ] T024 [P] [US1] Integration test (real sockets / operator API): friendly names accepted and round-trip verbatim; bad names rejected with rule-specific messages — `crates/portunus-server/tests/client_friendly_name.rs` — MUST FAIL first
- [ ] T025 [US1] Surface relaxed creation in the operator create/enroll handler and return clear field-specific validation errors (FR-011) in `crates/portunus-server/src/operator/`
- [ ] T026 [P] [US1] Web UI: client provisioning form accepts free-form names (drop any DNS-label restriction) and renders server validation errors — `webui/src/components/` (client provisioning dialog). DO NOT touch `webui/src/components/UserCreateForm.tsx:44` (that regex is for **userId**)
- [ ] T027 [P] [US1] Web UI: `ClientsList` / `ClientDetail` render the display name verbatim plus a short id form — `webui/src/pages/` + `webui/src/components/`
- [ ] T028 [US1] Checkpoint: create a friendly-named client via API and UI; verify verbatim display + distinct id

---

## Phase 4: User Story 2 — Identity survives a rename (Priority: P1)

**Goal**: Renaming a client keeps its id and all rules/tokens/quotas/history, and does not
drop a live session.

**Independent Test**: Client with rule+quota+traffic, connected; rename it (and to a name
another client already uses); id unchanged, all records still resolve, session keeps
forwarding, duplicate rename accepted.

- [ ] T029 [P] [US2] Integration test: rename preserves `client_id` and every dependent row; live gRPC session uninterrupted across rename; rename to a duplicate name succeeds — `crates/portunus-server/tests/client_rename.rs` — MUST FAIL first
- [ ] T030 [US2] Add rename endpoint `PATCH /v1/clients/{client_id}` (`UPDATE client_tokens SET client_name=? WHERE client_id=?`, relaxed validation, audit-grade record) in `crates/portunus-server/src/operator/`
- [ ] T031 [P] [US2] Add CLI `client rename --client-id <ULID> --name "<display>"` subcommand in `crates/portunus-server/src/operator/`
- [ ] T032 [P] [US2] Web UI: rename control on `ClientDetail` calling `PATCH /v1/clients/{id}` — `webui/src/pages/` + `webui/src/components/`
- [ ] T033 [US2] Checkpoint: rename a connected client; confirm session survives and records persist

---

## Phase 5: User Story 3 — Stable links and references (Priority: P2)

**Goal**: Client detail links and id-based operator/CLI calls keep resolving after a rename;
unknown id returns a clean not-found.

**Independent Test**: Open `/clients/<id>`, copy URL, rename, reload → same client; a
`--client-id` command keeps working after rename; an unknown id → 404, not 5xx.

- [ ] T034 [P] [US3] Tests: id-based detail route + `--client-id` command resolve after rename; unknown `client_id` → 404 (FR-012) — `crates/portunus-server/tests/client_addressing.rs` (+ a webui route smoke test) — MUST FAIL first
- [ ] T035 [US3] Web UI: change route to `/clients/:clientId` (was `:clientName`) and key all client links/requests on the id — `webui/src/App.tsx:231` + callers
- [ ] T036 [P] [US3] Web UI: disambiguate duplicate display names with a short id in all client listings — `webui/src/pages/` + `webui/src/components/`
- [ ] T037 [US3] Ensure all client-scoped operator routes return 404 for unknown `client_id` without leaking colliding-name existence (Constitution V) in `crates/portunus-server/src/operator/`
- [ ] T038 [US3] Checkpoint: bookmark a client, rename it, reopen bookmark → same client

---

## Phase 6: User Story 4 — Existing deployment upgrades cleanly (Priority: P1)

**Goal**: An upgrade assigns ids to all existing clients, re-associates every record with no
loss, and pre-enrolled clients keep working with no re-enrollment.

**Independent Test**: Populated `V010` DB → upgrade → every client has an id, zero orphans,
re-run is a no-op; a legacy bundle (no id) reconnects and forwards.

- [ ] T039 [P] [US4] e2e test: seed a `V010`-era client + legacy bundle (token, no `client_id`), start the upgraded server, reconnect with the legacy bundle, assert traffic forwards (SC-005) — `crates/portunus-e2e/tests/legacy_client_reconnect.rs` — MUST FAIL first
- [ ] T040 [US4] Confirm the token→`client_id` resolution path makes a legacy bundle connect transparently (no re-enroll) — `crates/portunus-server/src/store/token_store.rs` + `crates/portunus-server/src/grpc/service.rs`
- [ ] T041 [US4] Add an e2e idempotency assertion: restart the upgraded server twice, no re-migration, data unchanged (complements T010) — `crates/portunus-e2e/`
- [ ] T042 [US4] Checkpoint: populated `V010` DB upgrades cleanly and a legacy client reconnects

---

## Phase 7: Polish & Cross-Cutting Concerns

- [ ] T043 [P] Add a `CHANGELOG.md` entry: new `client_id` wire field, relaxed client-name rules, client rename (Constitution: user-visible change requires a CHANGELOG note)
- [ ] T044 [P] Update docs for id-based addressing + rename + free-form names in `docs/content/`
- [ ] T045 Run `cargo bench -p portunus-client --bench data_plane`; confirm flat vs the v0.1.0 baseline (Constitution II)
- [ ] T046 [P] Web UI build budget: `cd webui && pnpm install --frozen-lockfile && pnpm build` (tsc + vite + size-limit ≤500 KB gz) green
- [ ] T047 Run the full `quickstart.md` acceptance walkthrough (SC-001..SC-007)
- [ ] T048 Final gate: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`

---

## Dependencies & Execution Order

### Phase dependencies

- **Setup (P1)**: no dependencies.
- **Foundational (P2)**: depends on Setup; **blocks all user stories**. Internal order:
  core types (T003–T006) → auth (T007) → proto (T008–T009) → migration (T010–T011) → store
  re-key (T012–T016) → runtime/control-plane (T017–T021) → bundle (T022) → green gate (T023).
- **User Stories (P3–P6)**: all require Foundational (T023). They are then largely
  independent and may proceed in parallel; US3's id-based routes assume the Foundational
  re-path (T020) is done.
- **Polish (P7)**: after all targeted stories.

### Within a story

- The story's test task (`MUST FAIL first`) precedes its implementation tasks.
- Backend handler before its Web UI consumer where a story spans both.

### Parallel opportunities

- T003 ∥ T005 (same file but independent test blocks — coordinate edits) ; T008, T010 are
  independent test files and run in parallel.
- Store re-key T013 ∥ T014 ∥ T015 ∥ T016 (different files), after T011/T012.
- Within US1: T026 ∥ T027 ; within US2: T031 ∥ T032 ; within US3: T036 alongside T035.
- Polish: T043 ∥ T044 ∥ T046.

---

## Implementation Strategy

### MVP

Setup → Foundational (T001–T023) → **US1** (T024–T028). This delivers the headline value
(friendly names on a stable id) on a fully-migrated, green backend. Stop and validate.

### Incremental delivery

1. Foundational green (T023) — backend fully re-keyed, workspace passes.
2. US1 friendly name → demo (MVP).
3. US2 rename → demo.
4. US3 stable links → demo.
5. US4 upgrade verification → release gate (seeded migration + legacy reconnect).
6. Polish → CHANGELOG, docs, bench flatness, bundle budget, final gate.

### Notes

- Foundational is large by design (horizontal refactor); keep it green at T023 before any
  story so stories stay independently testable.
- `Hello`/`Welcome` need no wire change — identity is token-resolved (research R-004/R-005).
- `portunus-forwarder` (data-plane lib) is untouched.
- Do not edit historical `specs/*/contracts/` snapshots.
- Commit after each task or logical group (local only; do not push unless asked).
