---

description: "Task list for 007-multi-target-failover (v0.7.0)"
---

# Tasks: Multi-target failover

**Input**: Design documents from `/specs/007-multi-target-failover/`
**Prerequisites**: plan.md ✓, spec.md ✓, research.md ✓, data-model.md ✓, contracts/ ✓, quickstart.md ✓

**Tests**: Test-First Discipline (Constitution Principle III) is non-negotiable for this feature — contract tests ship BEFORE implementation. Integration tests gate each user story's checkpoint.

**Organization**: Tasks are grouped by user story (US1 P1, US2 P2, US3 P3, US4 P3). The single-target hot path stays byte-identical to v0.6.0 — multi-target lives in a separate code path entered via `match targets.len() { 1 => fast_path, _ => failover_path }`.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: Which user story this task belongs to (US1, US2, US3, US4)

## Path Conventions

This is a 6-crate Rust workspace + `webui/` Vite SPA. Paths are repo-root-relative. Per `plan.md` Project Structure:

- `crates/forward-proto/` — wire protocol
- `crates/forward-core/` — shared types
- `crates/forward-server/` — operator HTTP, CLI, persistence, gRPC server
- `crates/forward-client/` — gRPC client, forwarder, health/failover
- `webui/` — React+Vite SPA embedded via rust-embed

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Workspace version bump and changelog stub. No new crate adds.

- [X] T001 Bump workspace version `0.6.0` → `0.7.0-dev` in `Cargo.toml`
- [X] T002 [P] Open `## [Unreleased]` section in `CHANGELOG.md` with placeholders for `### Added` (multi-target rules + active probe + per-target stats), `### Changed` (Rule shape additive), `### Fixed` (none expected at start)
- [X] T003 [P] Verify Rust toolchain MSRV `1.88` (constitution pins this in `Cargo.toml` `[workspace.package].rust-version`; no separate `rust-toolchain.toml` exists in this repo)

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Wire-shape extensions + core entity types + persistence read-tolerance + server-side validation. Every user story builds on these. Tests in this phase are CONTRACT tests (Constitution Principle III) and MUST land before any implementation in this phase.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

### Wire protocol + contract tests (Phase 2 — first)

- [X] T004 Write proto contract test `crates/forward-proto/tests/targets_wire_compat.rs` covering W-1..W-6 from `contracts/proto-rule-extension.md` (legacy round-trip, v0.6 reader drops field 9 silently, byte-eq for single-target, RuleStats back-compat). MUST FAIL before T005/T006.
- [X] T005 Extend `proto/forward.proto`: add `message Target`, `repeated Target targets = 9` and `uint32 health_check_interval_secs = 10` on `Rule`, `uint64 target_failovers_total = 11` and `repeated PerTargetStats per_target = 12` on `RuleStats`, and new `message PerTargetStats` per `contracts/proto-rule-extension.md` §1–§4
- [X] T006 Re-run `cargo build -p forward-proto` to regenerate the tonic types and confirm T004 passes (6/6 wire-compat tests green; back-compat shimmed in 3 downstream sites with `targets: vec![]` / `health_check_interval_secs: 0` and `target_failovers_total: 0` / `per_target: vec![]` defaults)

### Core entity types

- [X] T007 [P] Create `crates/forward-core/src/rule_target.rs` with `pub struct RuleTarget { host: String, port: u16, priority: u32 }` plus `pub fn rule_target::validate(targets: &[RuleTarget]) -> Result<(), RuleTargetError>` enforcing V-T1..V-T4 + V-R5. (NB: name is `RuleTarget` not `Target` — `forward_core::Target` is already taken by the existing host-classifier module.)
- [X] T008 [P] Add `crates/forward-core/tests/rule_target_validation.rs` with 13 cases: empty/single/max accepted, 9-target rejected, empty-host, invalid-host syntax, port 0, dup (host,port), same-host-different-port accepted, same-priority-value accepted, IPv4/bracketed-IPv6 accepted, unbracketed IPv6 rejected
- [X] T009 Extend `crates/forward-server/src/rules.rs` (canonical `Rule` lives here, NOT in forward-core): add `targets: Vec<RuleTarget>` and `health_check_interval_secs: Option<u32>` with `#[serde(default, skip_serializing_if = ...)]` so legacy on-disk shapes round-trip; add `targets_view()` and `is_multi_target()` helpers
- [X] T010 Re-export `RuleTarget`, `RuleTargetError`, `MAX_TARGETS_PER_RULE` from `crates/forward-core/src/lib.rs`

### Persistence read-tolerance

- [~] T011 ~~Update `crates/forward-server/src/persistence.rs` write path~~ **N/A**: rules are in-memory only in this codebase ("future work" per `crates/forward-server/src/rules.rs:42`). The new `Rule.targets` and `Rule.health_check_interval_secs` fields carry serde-default + skip-if-empty attributes so back-compat is automatic if persistence is ever added.
- [~] T012 ~~Update read path to promote legacy single-target rules~~ **N/A**: see T011. The `targets_view()` helper performs the equivalent in-memory promotion at the consumer-side instead.
- [~] T013 ~~Persistence back-compat test~~ **N/A**: see T011. The serde back-compat is exercised via the proto wire-compat tests (T004) which round-trip the same shape.

### Server-side push validation

- [X] T013a Add `crates/forward-server/tests/rules_multi_target_contract.rs` (11 tests, all green): accept legacy shape, accept new shape parses, reject both/neither shapes, reject empty/too-many/duplicate targets, reject `health_check_interval_secs` out of range, reject multi-target to v0.6 client (422), single-target to v0.6 client passes guard, FR-021 targets-not-in-RBAC. Test landed RED before T014/T015/T016 — Constitution Principle III preserved.
- [X] T014 Extended `crates/forward-server/src/operator/http.rs` `PushRuleBody` with optional `target_host`/`target_port` plus new optional `targets: Vec<TargetBody>` and `health_check_interval_secs: Option<u32>`. Shape dispatch in `post_rules` branches on which fields are present (mutually-exclusive enforced via `RuleShapeConflict` / `RuleShapeMissing`).
- [X] T015 Wired all validation per `contracts/operator-api.md` §1 via new `OperatorError` variants (`RuleShapeConflict`, `RuleShapeMissing`, `TargetsInvalid(RuleTargetError)`, `HealthCheckIntervalOutOfRange`, `MultiTargetUnsupportedByClient`). Each maps to the contract code in `OperatorError::code()` and the right HTTP status (400 for shape/targets/hci, 422 for version guard). Targets list NOT subject to RBAC — `enforce_push` only inspects the existing `(client, listen-port range, protocol)` envelope (FR-021).
- [X] T016 Server-side client-version guard (R-007): `ConnectedClient` gains `client_version: Option<String>` populated from `Hello.client_version` in `grpc/service.rs`; new `ConnectedClients::set_client_version` + `client_version_of` methods. The new `push_multi_target` helper in `http.rs` runs `version_at_least_0_7` against the cached version when `targets.len() >= 2` and returns 422 `multi_target_unsupported_by_client` BEFORE any rule mutation.

**Checkpoint**: Foundation ready — wire protocol, core types, persistence, and server-side validation accept both shapes. User story implementation can begin.

---

## Phase 3: User Story 1 — Primary failure transparently shifts to secondary (P1) 🎯 MVP

**Goal**: A multi-target rule whose primary becomes unreachable shifts new TCP connections / new UDP flows onto the secondary without operator intervention.

**Independent Test**: Push a rule with `[primary_unreachable, secondary_working]`, open a TCP connection from a third host to the rule's listen port, confirm bytes round-trip via the secondary. Verify `target_failovers_total >= 1`. (Spec US1 acceptance scenarios 1–3.)

### Tests for User Story 1 (write FIRST, ensure they FAIL before T020+)

- [X] T017 [P] [US1] State machine contract tests live INLINE in `crates/forward-client/src/forwarder/failover.rs` `mod tests` (7 cases): cold state Healthy, 3-fails-in-window flips Failed (`target_failovers_total += 1`), 2-fails-in-window stays Healthy, fail-then-window-elapses-then-fail-stays-Healthy, recover after Failed needs 2 successes, mixed-failure resets recovery counter, last_failure_at populated.
- [X] T018 [P] [US1] `crates/forward-e2e/tests/multi_target_passive_failover.rs`: spawns `forward-server` + `forward-client`, pushes a 2-target rule whose primary port was just released (refused on connect), confirms a TCP connection to the listen port round-trips bytes via the secondary echo.
- [X] T019 [P] [US1] Selection algorithm tests live INLINE in `crates/forward-client/src/forwarder/failover.rs` `mod tests` (5 cases on `select` + 2 on `sort_priority`): FR-006 priority-ordered Healthy preferred, single Healthy among Failed picked, all-Healthy picks index 0, all-Failed falls back to index 0 (FR-007), staggered priority + tie-breaks.

### Implementation for User Story 1

- [X] T020 [P] [US1] `crates/forward-client/src/forwarder/failover.rs`: `pub struct HealthState` per `data-model.md` §3 fields (consecutive_failures, consecutive_successes, failure_window_start, last_failure_at, last_success_at, atomic byte/connection counters), `record_failure(now, wall, &target_failovers_total)` + `record_success(now, wall, &target_failovers_total)`. Transitions emit `tracing::warn!/info!(event = "rule.target.health_changed", ...)` and increment the atomic counter on Healthy↔Failed flips.
- [X] T021 [P] [US1] `pub fn select(states: &[HealthState]) -> usize` in `failover.rs`: walks `states` in row order returning the first `Healthy` index; returns `0` (strict highest-priority fallback per FR-007) when every state is Failed.
- [X] T022 [US1] `crates/forward-client/src/forwarder/mod.rs` activation entry dispatches on `!rule.targets.is_empty()` BEFORE the legacy bind path — single-target rules never allocate a `HealthState`, never enter `failover_path`, never branch through `failover.rs` (Constitution Principle II byte-identity guarantee). Multi-target lives in `crates/forward-client/src/forwarder/failover_path.rs`. `crates/forward-client/src/control.rs` was wired to populate `ClientRule.targets` from the proto `Rule.targets[]` (with per-target host parse → `forward_core::Target`).
- [X] T023 [US1] `failover_path::handle_connection` + `dial_with_failover`: snapshots per-target health, tries the first Healthy target then walks the rest in row order on connect failure, records successes/failures into the per-target `HealthState`, and `tokio::io::copy_bidirectional`s on the chosen target. `target_failovers_total` increments via the HealthState transitions. Wired into `forward-server` via the new `cli::push_rule_multi_target` helper which emits the multi-target `RuleUpdate`. Verified end-to-end by T018.
- [X] T024 [US1] `crates/forward-client/src/forwarder/udp/mod.rs` `pub async fn run_listener_multi_target`: existing UDP flows take the byte-identical fast path; new flows trigger per-target select-and-walk on first packet, attributing resolve/bind outcomes to the chosen target's `HealthState`. Subsequent packets on the same flow stick to the chosen target (FR-012 — failover applies only to NEW flows). Dispatched from `forwarder::run_udp` when `!rule.targets.is_empty()`.
- [X] T025 [US1] DNS-resolution failure attribution: `dial_with_failover` (TCP) and `handle_inbound_multi_target` (UDP) both treat resolver errors and empty-resolved-address-set as a connect failure for the attempted target's `HealthState` (FR-008 / quickstart §3 attribution table).

**Checkpoint**: A multi-target TCP rule passively fails over to the secondary on primary failure. Single-target rules still go through the v0.6.0 fast path. T017+T018+T019 pass.

---

## Phase 4: User Story 2 — Primary recovery resumes traffic to primary (P2)

**Goal**: When the primary recovers, the next new connection lands on the primary again. In-flight connections on the secondary continue undisturbed.

**Independent Test**: Same setup as US1; after secondary takes over, restart the primary, confirm next new connection lands on primary while the in-flight connection on secondary keeps flowing. Verify `target_failovers_total == 2`.

### Tests for User Story 2

- [ ] T026 [P] [US2] Extend `crates/forward-client/tests/failover_state_contract.rs` with the Failed→Healthy path: 2 consecutive successes flip state, `target_failovers_total += 1` on the transition, mixed failure resets `consecutive_successes`. MUST FAIL before T028
- [ ] T027 [P] [US2] Add `crates/forward-e2e/tests/multi_target_recovery.rs`: spawn fixture with active probe enabled (`health_check_interval_secs: 1` for fast tests), kill primary, observe failover, then restart primary and assert next new connection lands on primary while a long-lived in-flight connection on secondary keeps round-tripping bytes. MUST FAIL before T028+T029+T030

### Implementation for User Story 2

- [ ] T028 [US2] Extend `HealthState::record_success` in `crates/forward-client/src/forwarder/failover.rs` with the Failed→Healthy transition (2 consecutive successes), emitting the same `target_failovers_total += 1` and `tracing::info!(event = "rule.target.health_changed", ...)` log
- [ ] T029 [P] [US2] Create `crates/forward-client/src/forwarder/probe.rs`: opt-in active TCP-connect prober. Skipped when `health_check_interval_secs.is_none()`. Spawns one tokio task per multi-target rule with `health_check_interval_secs.is_some()`; the task probes each target round-robin at the configured cadence using `tokio::net::TcpStream::connect` with the same connect timeout the data plane uses (FR-014). No probe-overlap per target — defer if in-flight (R-008)
- [ ] T030 [US2] Confirm in-flight TCP connections never migrate (FR-011): the multi-target arm in `forwarder/mod.rs` only calls `select()` on the accept path, never on existing per-connection state. Add a focused unit test in `forwarder/mod.rs` that opens a connection, swaps the rule's per-target health to Failed, and asserts the existing connection still completes its data path on the originally-chosen target.

**Checkpoint**: Recovery works automatically (passive + active). T026+T027 pass. US1 still passes.

---

## Phase 5: User Story 3 — Operator observes which target traffic is using and why (P3)

**Goal**: Per-target byte counters, last-failure / last-success timestamps, and the rule-level `target_failovers_total` are visible via CLI `rule-stats --per-target`, HTTP `GET /v1/rules/{id}/stats?per_target=true`, the SSE stream, and Prometheus.

**Independent Test**: With a rule that has experienced N failovers, query each surface and confirm they all agree on the targets list, current health, per-target byte counters, and the failover count.

### Tests for User Story 3

- [ ] T031 [P] [US3] Add `crates/forward-server/tests/rules_per_target_stats.rs` covering: default `/v1/rules/{id}/stats` carries `target_failovers_total` (0 for single-target), `?per_target=true` populates `per_target[]` for multi-target rules and returns `per_target: []` for single-target (invariant I-3). MUST FAIL before T033+T034
- [ ] T032 [P] [US3] Add `crates/forward-server/tests/metrics_cardinality.rs` proving SC-006: `/metrics` adds exactly 1 new series (`forward_rule_target_failovers_total{client,rule}`) per multi-target rule; per-target counters are NOT exported as default series. MUST FAIL before T035

### Implementation for User Story 3

- [ ] T033 [US3] Extend `crates/forward-client/src/stats.rs` `RuleStats` builder: include `target_failovers_total` from the per-rule `AtomicU64`, populate `per_target: Vec<PerTargetStats>` from each `HealthState` (bytes_in/bytes_out/connections_accepted/health/timestamps). Single-target rules emit `target_failovers_total = 0` and `per_target: vec![]`
- [ ] T034 [US3] Wire per-target byte accumulation in the multi-target TCP and UDP data paths: every byte forwarded credits the chosen target's `HealthState.bytes_in/bytes_out`; every accepted TCP connection / new UDP flow credits `connections_accepted`
- [ ] T035 [US3] Register a new Prometheus collector in `crates/forward-server/src/metrics.rs`: `forward_rule_target_failovers_total{client, rule}` counter. Sourced from `RuleStats.target_failovers_total` on the StatsReport tick. NO per-target series exposed (FR-018 / SC-006)
- [ ] T036 [US3] Extend `crates/forward-server/src/operator/http.rs` `GET /v1/rules/{id}/stats` to support `?per_target=true` query param: when set, include `per_target[]` in the response per `contracts/operator-api.md` §4. Default response unchanged shape, only the new `target_failovers_total` field added.
- [ ] T037 [US3] Extend the SSE stream `GET /v1/rules/{id}/stats/stream` (from spec 006) to honour `?per_target=true` on subscribe — each tick payload mirrors the §4 shape with or without `per_target[]`. Single-target rules continue to emit `per_target: []` regardless
- [ ] T038 [US3] Extend `GET /v1/rules/{id}` and `GET /v1/rules` responses to carry the `targets[]` list with each target's `health` snapshot per `contracts/operator-api.md` §2 (single-target rules emit one element with `health: null`)
- [ ] T039 [P] [US3] Add `--per-target` flag to `crates/forward-server/src/operator/rule_cli.rs` `rule-stats` subcommand. Default output gains the `target_failovers_total: N` line; `--per-target` appends the per-target detail block per `contracts/operator-api.md` §8. Single-target rules with `--per-target` print the `(single-target rule, no per-target state)` note and exit 0

**Checkpoint**: Per-target observability is live across CLI, HTTP, SSE, and Prometheus. T031+T032 pass. US1+US2 still pass.

---

## Phase 6: User Story 4 — Operator builds, edits, and removes multi-target rules through the same surfaces (P3)

**Goal**: The operator pushes multi-target rules via the same `push-rule` CLI subcommand, the same `POST /v1/rules` endpoint, and the same Web UI form they used in v0.6.0. Single-target push surfaces are unchanged.

**Independent Test**: Push two equivalent rules — one via the legacy positional `push-rule edge-01 8080 example.com:80`, one via the new `--target example.com:80`. Both are accepted; the data plane behaviour is byte-identical.

### Tests for User Story 4

- [ ] T040 [P] [US4] Add Phase 6 wire-through-CLI integration coverage in `crates/forward-server/tests/rules_multi_target_e2e.rs`: end-to-end push via the CLI subcommand round-trips back through `GET /v1/rules/{id}` with the targets list intact (validates the surfaces from T043 hang together). The shape validation itself is already covered by T013a in Phase 2.
- [ ] T041 [P] [US4] Add `crates/forward-server/tests/push_rule_cli.rs`: legacy positional form, repeatable `--target` form, `--targets-json` form, mutually-exclusive form combinations rejected before HTTP issue. MUST FAIL before T043
- [ ] T042 [P] [US4] Add `webui/tests/e2e/us1-multi-target-push.spec.ts`, `us3-target-detail-render.spec.ts`, `us4-single-target-back-compat.spec.ts` per `contracts/ui-routes.md` §7. MUST FAIL before T046+T047

### Implementation for User Story 4

- [ ] T043 [US4] Extend `crates/forward-server/src/operator/rule_cli.rs` `push-rule` subcommand: gain repeatable `--target host:port[@priority]` and `--targets-json '[…]'`. Legacy positional still works. Mutually-exclusive group enforced by clap. Builds the same `RulePushBody` shape `POST /v1/rules` accepts
- [ ] T044 [US4] Update `webui/src/api/types.ts` per `contracts/ui-routes.md` §5: add `Target`, `TargetHealth`, `TargetWithHealth`, `PerTargetStats`, `RuleWithTargets`; extend `RuleStats` with `target_failovers_total: number` and optional `per_target?: PerTargetStats[]`
- [ ] T045 [US4] Update `webui/src/api/rules.ts` and `webui/src/api/stats.ts` hooks: `useRule(id)` returns `RuleWithTargets`; `useRuleStatsStream(id, { perTarget })` adds the optional `per_target=true` query param
- [ ] T046 [US4] Extend `webui/src/pages/RulePush.tsx` per `contracts/ui-routes.md` §2: targets list builder with "Add another target" button, per-row host/port/priority/remove controls, optional collapsible "Active health check" with `health_check_interval_secs` field, client-side validation mirror of server rules. Form ALWAYS submits the new `targets[]` shape (server folds length-1 to legacy on the wire)
- [ ] T047 [US4] Extend `webui/src/pages/RuleDetail.tsx` per `contracts/ui-routes.md` §3: render Targets section below the live stats panel — table with rows per target, health badge (Healthy / Degraded / Failed), last-failure / last-success timestamps, per-target byte counters. Subscribe to `/stats/stream?per_target=true`. Single-target rules render the "single-target rule — no failover state" note instead
- [ ] T048 [P] [US4] Add new i18n keys per `contracts/ui-routes.md` §6 to `webui/src/i18n/en.json` and `webui/src/i18n/zh-CN.json`
- [ ] T049 [P] [US4] Optional: add small `MT` pill on `webui/src/pages/RulesList.tsx` rows where `targets.length > 1` — drop if it slips

**Checkpoint**: Operators can push, view, and delete multi-target rules via every surface they already know. T040+T041+T042 pass. US1+US2+US3 still pass.

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Bench gate, quickstart validation, and v0.7.0 release prep.

- [ ] T050 Run `cargo bench -p forward-client --bench data_plane -- --baseline v0.6.0` and confirm the single-target hot path regresses ≤ 1% (SC-003). Update `crates/forward-client/benches/data_plane.rs` if a `_multi_target` variant is needed for completeness — single-target benchmark is the gate
- [ ] T051 Walk through every step of `specs/007-multi-target-failover/quickstart.md` end-to-end on a two-host setup (or local docker-compose). Note any drift between docs and behaviour and fix in the same PR
- [ ] T052 [P] Refresh `crates/forward-server/src/audit.rs` correlation: confirm health-state transition log lines (`event = "rule.target.health_changed"`) flow through the existing `tracing` JSON pipeline. NOT part of the operator HTTP audit ring (which gates on operator actions); add a brief comment in the audit module pointing readers at the failover module
- [ ] T053 [P] Update `deploy/server.toml.example` header to v0.7.0; no new config keys are introduced (active probe is a per-rule field, not a server config knob)
- [ ] T054 Bump workspace version `0.7.0-dev` → `0.7.0` in `Cargo.toml`. Seal the `## [Unreleased]` section in `CHANGELOG.md` into `## [0.7.0] — YYYY-MM-DD` per the project's release cadence
- [ ] T055 [P] Update `webui/package.json` version to `0.7.0`
- [ ] T056 Run the full test gate: `cargo test --workspace --tests`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, `pnpm --filter webui test`, `pnpm --filter webui run e2e`. All MUST be green
- [ ] T057 Tag `v0.7.0` and push branch + tag (operator confirmation required before push — see `Operating actions with care` instructions)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no dependencies. T002+T003 can run parallel to T001.
- **Foundational (Phase 2)**: depends on Setup. Within Phase 2:
  - T004 (contract test) BEFORE T005+T006 (proto extension + regen)
  - T007+T008 parallel; T009 depends on T007 (uses Target type)
  - T011+T012 sequential (same file); T013 after T012
  - T014+T015+T016 sequential (same file `operator/http.rs`)
- **User Story 1 (Phase 3)**: depends on Phase 2 complete. Tests T017+T018+T019 land before implementation T020+. Within implementation:
  - T020+T021 parallel (different concerns in `failover.rs` — splittable; can also be one task if convenient)
  - T022 (forwarder activation branch) gates T023+T024+T025
  - T023 and T024 parallel after T022 (TCP vs UDP arms — independent files)
- **User Story 2 (Phase 4)**: depends on US1 complete (extends the same `failover.rs` and `forwarder/mod.rs`).
- **User Story 3 (Phase 5)**: depends on US1 complete (needs HealthState fields). T031+T032 land before implementation. T036+T037+T038 sequential (same `operator/http.rs` file). T033+T034+T035 parallel-ish but credit-on-data-path (T034) interacts with T033's reading.
- **User Story 4 (Phase 6)**: depends on Phase 2 (HTTP shape) + US3 (`per_target=true` query exists, types exist). T044+T045 must precede T046+T047.
- **Polish (Phase 7)**: depends on US1+US2+US3+US4 complete.

### Within each user story

- Tests MUST be written and FAIL before implementation tasks (Constitution Principle III).
- Models / state machine before consumers.
- Single-target hot path stays untouched at every step (Constitution Principle II).

### Parallel opportunities

- **Phase 1**: T002+T003 parallel to T001
- **Phase 2**: T007+T008 parallel; T013 after T012; everything else mostly sequential by file conflict
- **Phase 3 tests**: T017+T018+T019 parallel
- **Phase 3 impl**: T020+T021 parallel; T023+T024 parallel after T022
- **Phase 4 tests**: T026+T027 parallel
- **Phase 5 tests**: T031+T032 parallel; T035+T036+T037+T038 parallel-ish (one server file, one collector)
- **Phase 6 tests**: T040+T041+T042 parallel
- **Phase 7**: T052+T053+T055 parallel; T050+T051+T056 sequential

### Story independence (testable separately)

- US1 alone delivers MVP failover (passive only — no recovery, no UI affordance). Operator pushes via raw HTTP; observes failover via Prometheus once T035 is delivered.
- US2 adds recovery; depends on US1 only because they share the state machine module.
- US3 adds observability surfaces; testable independently with a static rule (no failover required to verify the surfaces render).
- US4 adds operator UX (CLI + UI); testable independently by exercising surfaces against a single-target rule.

---

## Parallel Example: Phase 3 (User Story 1) tests

```bash
# Land all three US1 contract / integration tests first; they MUST fail before T020+:
Task: "Write Healthy→Failed state machine contract test in crates/forward-client/tests/failover_state_contract.rs"
Task: "Write passive-failover e2e in crates/forward-e2e/tests/multi_target_passive_failover.rs"
Task: "Write selection algorithm tests in crates/forward-client/tests/selection_algorithm.rs"
```

```bash
# Then land US1 implementation in two parallel arms after T022:
Task: "Implement TCP failover loop in crates/forward-client/src/forwarder/mod.rs (multi-target arm)"
Task: "Implement UDP first-packet binding (FR-012) in the multi-target arm"
```

---

## Implementation Strategy

### MVP First (US1 only — passive failover)

1. Phase 1: Setup
2. Phase 2: Foundational (CRITICAL — wire shape + persistence + server validation)
3. Phase 3: US1 — passive failover lands. Operator pushes rules via raw HTTP; observes failover via tracing logs.
4. **STOP and VALIDATE**: integration test passes; quickstart §1–§3 passes
5. Optional demo / dogfood

### Incremental Delivery

1. Setup + Foundational → foundation ready
2. + US1 (P1) → passive failover MVP
3. + US2 (P2) → automatic recovery (active probe)
4. + US3 (P3) → operator can see what's happening (CLI / HTTP / Prometheus / SSE)
5. + US4 (P3) → operator UX in CLI + Web UI matches single-target ergonomics
6. + Polish (Phase 7) → bench gate, quickstart, release cut → v0.7.0

### Parallel team strategy

1. Team finishes Setup + Foundational together (T001..T016)
2. Once Phase 2 done:
   - Developer A: US1 (Phase 3)
   - Developer B: US3 surfaces that don't need the state machine yet (T036+T037+T038 against stub stats)
   - Developer C: US4 Web UI scaffolding (T044+T045+T046)
3. US2 (Phase 4) starts once US1 is at the checkpoint
4. Polish (Phase 7) runs after every story is at its checkpoint

---

## Notes

- [P] tasks = different files, no dependencies on incomplete tasks
- [Story] label maps each task to its user story for traceability and independent shipping
- Constitution Principle II is the hard guarantee: any task that touches the single-target hot path MUST keep the v0.6.0 byte path intact (T022 is the explicit branch point; everything after lives behind that branch)
- Constitution Principle III is the test discipline: contract / integration tests MUST fail before the implementation that satisfies them lands
- Per-target byte counters are query-only on `/metrics` (FR-018, SC-006) — never accidentally promote them to a default Prometheus series
- Targets are NOT part of the RBAC envelope (FR-021) — every server-side validation site that touches `targets[]` does NOT consult `enforce_grant` for them; the listen-port-range / protocol envelope unchanged
- Commit after each task (or each [P] group); the project runs auto-commit after-tasks via the speckit hook
