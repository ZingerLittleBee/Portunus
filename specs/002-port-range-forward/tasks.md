---
description: "Tasks for 002-port-range-forward"
---

# Tasks: Port-Range Forwarding Rules

**Input**: Design documents from `/specs/002-port-range-forward/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/
**Tests**: REQUIRED. Constitution III (Test-First Discipline) is non-
negotiable; the plan's Constitution Check entry for III enumerates the
contract / unit / integration tests this feature must ship.

**Organization**: Tasks are grouped by user story so each story is
independently completable and shippable. Range work extends an existing
v0.1.0 codebase (spec 001-tcp-forward-mvp) — every change is additive.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: parallelizable (different files, no dependencies on
  incomplete tasks).
- **[Story]**: maps to spec.md user stories (US1…US4) for traceability.
- File paths are absolute-style (relative to repo root).

## Path Conventions

Cargo workspace at repo root. All paths relative to repo root.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Workspace prep. Minimal — this is an extension on top of
v0.1.0, not a new project.

- [X] T001 Bump workspace version to `0.2.0-dev` in `Cargo.toml` (root `[workspace.package]` if shared, else each crate's `version` in `crates/forward-{core,proto,auth,server,client,e2e}/Cargo.toml`)
- [X] T002 [P] Add `criterion = "0.5"` (or current pin) as a dev-dependency in `crates/portunus-client/Cargo.toml` and add `[[bench]]` entry for `range_install` (used by T055)
- [X] T003 [P] Add a CHANGELOG.md `[Unreleased]` section noting "port-range forwarding rules (additive)" in `CHANGELOG.md`

**Checkpoint**: Workspace builds (`cargo check --workspace`) with the version bump.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The shared types, proto changes, and server config field
that EVERY user story depends on. No story work can begin until this
phase is complete.

⚠️ **CRITICAL**: Foundational tasks block US1–US4.

### Wire / proto

- [X] T004 Extend `proto/portunus.proto`: add fields `uint32 listen_port_end = 6;` and `uint32 target_port_end = 7;` to `message Rule` (additive, see `specs/002-port-range-forward/contracts/portunus.proto`). Verify `cargo build -p portunus-proto` regenerates bindings without breaking existing imports.
- [X] T005 Extend `proto/portunus.proto`: add `repeated PerPortStats per_port = 5;` to `message RuleStats`, plus a new `message PerPortStats { uint32 listen_port = 1; uint64 bytes_in = 2; uint64 bytes_out = 3; uint32 active_connections = 4; }`. Same proto file as T004 — sequential.
- [X] T006 [P] Contract test in `crates/portunus-proto/tests/range_wire_compat.rs` (NEW): assert that a v0.1.0-shaped `Rule` (no range fields set, defaults to 0) round-trips to the same on-wire bytes as it did pre-change (read a fixed encoded blob from `tests/fixtures/legacy_rule.bin`). Also assert a range rule with `listen_port_end=30050` round-trips through `prost::Message::encode`/`decode`. This test MUST exist and FAIL before T004/T005 land (TDD), but since proto codegen needs the field, write the test first using `cargo expand` / hand-encoded bytes for the assertion.

### Core types

- [X] T007 [P] Create `crates/portunus-core/src/port_range.rs`: `PortRange { start: u16, end: u16 }` newtype with `single`, `new`, `pair`, `len() -> u32`, `contains`, `overlaps`, `iter`, `target_for` per `data-model.md` § PortRange. Add `PortRangeError { Inverted, LengthMismatch { listen_len, target_len }, OutOfBounds, ExceedsCap { requested, cap } }` here.
- [X] T008 [P] Unit tests in `crates/portunus-core/src/port_range.rs` (`#[cfg(test)] mod tests`): cover `new` rejects `start > end`, `new` rejects `start == 0`, `pair` rejects length mismatch, `len()` correct at edges (1-1 → 1, 1-65535 → 65535, 65535-65535 → 1), `overlaps` across symmetric/disjoint/adjacent/nested cases, `target_for` correctness across the full range.
- [X] T009 Export `PortRange` and `PortRangeError` from `crates/portunus-core/src/lib.rs` so other crates can `use portunus_core::PortRange;`. Sequential after T007.

### Server config

- [X] T010 [P] Add `range_rule_max_ports: u32` field with `#[serde(default = "default_range_cap")]` (default `1024`) to `ServerConfig` in `crates/portunus-core/src/config.rs`. Reject load if value is `0` (return a config error).
- [X] T011 [P] Add a unit test in `crates/portunus-core/src/config.rs` `tests` module: a `server.toml` snippet WITHOUT `range_rule_max_ports` loads with `range_rule_max_ports == 1024`; a snippet WITH `range_rule_max_ports = 256` loads with that value; a snippet with `range_rule_max_ports = 0` fails to load with a clear error.

### Server rule struct

- [X] T012 Extend `Rule` in `crates/portunus-server/src/rules.rs` with `pub listen_port_end: Option<u16>` and `pub target_port_end: Option<u16>` (both `#[serde(default, skip_serializing_if = "Option::is_none")]`). Add helpers `listen_range()`, `target_range()`, `range_size() -> u32`, `is_range()` per `data-model.md` § Rule. Sequential — same file as T013.
- [X] T013 Extend `RuleStoreError` in `crates/portunus-server/src/rules.rs` with `PortInUse { offending_port: u16 }` (replace the unit `PortInUse`) AND `ExceedsCap { requested: u32, cap: u32 }` AND `RangeInvalid(PortRangeError)`. Update existing call sites in the same file (the existing tests use `RuleStoreError::PortInUse` — match on the new tuple variant).

### Client rule struct

- [X] T014 Extend `ClientRule` in `crates/portunus-client/src/forwarder/mod.rs`: replace `listen_port: u16, target_port: u16` with `listen_range: PortRange, target_range: PortRange` (using `portunus_core::PortRange`). Update the existing single-port construction sites (in `crates/portunus-client/src/control.rs` — see T032) to lift single-port rules via `PortRange::single(port)`. Sequential — same file as T027, T030, T031, T035, T040.

**Checkpoint**: `cargo test --workspace` is green for the foundational layer (proto, core, server config, rule struct compiles, client struct compiles). Existing v0.1.0 unit tests still pass (single-port behavior unchanged).

---

## Phase 3: User Story 1 — Push a port-range rule in a single operation (Priority: P1) 🎯 MVP

**Goal**: Operator pushes one rule for a contiguous port range; the
client binds every port in the range and forwards traffic with the
documented same-offset mapping.

**Independent Test**: With a fresh server + connected client (using
the existing `portunus-e2e` harness), push a rule for 100 contiguous
ports, confirm `list-rules` shows exactly one entry, drive a TCP
connection through any port in the range, verify it reaches the
corresponding target port (same offset).

### Tests for User Story 1 ⚠️

- [X] T015 [P] [US1] Unit tests in `crates/portunus-server/src/rules.rs` (extend the existing `tests` module): `push_range_rule_returns_single_id`, `push_range_assigns_pending_state`, `push_inverted_range_rejected_with_RangeInvalid`, `push_length_mismatch_rejected`, `push_exceeds_cap_rejected_with_named_limit`, `push_range_size_1_behaves_like_single_port` (degenerate case). Tests MUST fail before T020 lands.
- [X] T016 [P] [US1] Unit test in `crates/portunus-server/src/operator/cli.rs` `tests` module: `parse_listen_arg` accepts `"30000-30050"` returning `PortRange::new(30000, 30050)`, accepts `"18080"` returning `PortRange::single(18080)`, rejects `"30050-30000"` with `Inverted`, rejects `"abc-def"`. Same for `parse_target` extending the existing helper to accept `host:start-end`.
- [X] T017 [P] [US1] HTTP handler test in `crates/portunus-server/src/operator/http.rs` (extend existing test module if present, else NEW `crates/portunus-server/tests/range_http.rs`): POST `/v1/rules` with `listen_port_end` + `target_port_end` succeeds and returns 201 + `rule_id`; POST with only one of the two fields returns 400 `mismatched_range`; POST with `listen_port_end < listen_port` returns 400 `range_inverted`; POST with size > cap returns 400 `exceeds_cap`.
- [X] T018 [P] [US1] Forwarder bind-fan-out unit test in `crates/portunus-client/src/forwarder/range.rs` (NEW file with `#[cfg(test)] mod tests`): `bind_all` succeeds for a range of 50 OS-chosen free ports and returns 50 `TcpListener`s; `bind_all` rolls back ALL successful binds and returns the offending port when one bind fails (deliberately occupy a port mid-range and verify other ports in the range are released).
- [X] T019 [P] [US1] Integration test in `crates/portunus-e2e/tests/range_smoke.rs` (NEW): start a real server + client (reuse the existing `portunus-e2e` harness), push a 100-port range rule via the loopback HTTP API, verify `GET /v1/rules` returns one entry with `range_size = 100`, drive a TCP connection through ports `30000`, `30050`, and `30099`, verify each reaches the corresponding target port via the existing echo upstream. This test corresponds directly to SC-001 / US1 acceptance scenarios 1–3.

### Implementation for User Story 1

- [X] T020 [US1] Implement range-aware push in `ServerRuleStore::push` in `crates/portunus-server/src/rules.rs`: change signature to accept `listen: PortRange, target: PortRange` (or accept the existing single-port form via overload — pick whichever stays caller-compatible). Validation: `PortRange::pair` invariants → `RangeInvalid`; range size > `range_rule_max_ports` → `ExceedsCap`; conflict check via the new `by_client_listen: HashMap<ClientName, BTreeMap<u16, RuleId>>` index per `data-model.md` § ServerRuleStore (R-002). Build the `Rule` with `listen_port_end = Some(listen.end)` (or `None` if size 1) and same for target. Sequential — same file as T013, T021.
- [X] T021 [US1] Update `ServerRuleStore::remove` in `crates/portunus-server/src/rules.rs` to drop the entry from the new `by_client_listen` index. Same file as T020.
- [X] T022 [P] [US1] Add `parse_listen` helper in `crates/portunus-server/src/operator/cli.rs` that returns `PortRange`. Update `parse_target` to optionally return `(host, PortRange)` (host plus port range). Both helpers handle the legacy single-port form by returning `PortRange::single(...)`. Sequential — same file as T029.
- [X] T023 [US1] Update CLI `push-rule` handler in `crates/portunus-server/src/operator/rule_cli.rs` to accept `<listen>` of either single or range form, build the request body with optional `listen_port_end`/`target_port_end`, and surface the new HTTP error code strings (`exceeds_cap`, `range_inverted`, `mismatched_range`) via `code_to_exit` (all map to existing exit `3` family per contracts/operator-api.md).
- [X] T024 [P] [US1] Update `PushRuleBody` in `crates/portunus-server/src/operator/http.rs` with optional `listen_port_end: Option<u16>` and `target_port_end: Option<u16>`. In `post_rules`, build a `(PortRange, PortRange)` and call the extended store push. Translate `RuleStoreError::PortInUse { offending_port }` to HTTP 409 with body `{"error":{"code":"port_in_use","message":"port {offending_port} already in use by rule {rule_id} on {client_name}"}}`. Translate `ExceedsCap` to HTTP 400 `exceeds_cap`. Translate `RangeInvalid` to HTTP 400 `range_inverted` or `mismatched_range`.
- [X] T025 [P] [US1] Update `PushRuleResponse` (and `GET /v1/rules` rendering) in `crates/portunus-server/src/operator/http.rs` so list output includes `listen_port_end`, `target_port_end` (omitted via `serde(skip_serializing_if = "Option::is_none")`) plus a derived `range_size` (always present). Same file as T024 — sequential.
- [X] T026 [US1] Implement `crates/portunus-client/src/forwarder/range.rs` (NEW): `pub async fn bind_all(listen: &PortRange) -> Result<Vec<TcpListener>, BindFailure>` per R-001. On any bind error, drop all successful listeners and return `BindFailure { offending_port: u16, reason: &'static str }` (reason from `classify_bind_error`).
- [X] T027 [US1] Refactor `crates/portunus-client/src/forwarder/mod.rs::run`: replace the single `TcpListener::bind` with `range::bind_all`. On failure, emit one `RuleStatusEvent::Failed { reason: format!("{reason}:{offending_port}") }`. On success, spawn one accept loop per `(port, listener)` pair into the SAME existing `JoinSet` and SAME `proxy_cancel`. Each accept loop computes the per-connection `target_port = rule.target_range.target_for(listen_port, ...)`. Emit one `RuleStatusEvent::Activated` after all binds succeed and before the accept loops start. Removed event semantics unchanged (one per rule). Same file as T014, T035, T037.
- [X] T028 [US1] Wire single-port → range lift in `crates/portunus-client/src/control.rs`: when consuming a `RuleUpdate.PUSH`, build `ClientRule { listen_range: PortRange::new(rule.listen_port as u16, rule.listen_port_end.map(|e| e as u16).unwrap_or(rule.listen_port as u16))?, target_range: ... }`. Reject malformed range upstream with the same `RuleStatusEvent::Failed { reason: "range_invalid" }` path (defensive — the server should have validated already).
- [X] T029 [US1] Update `portunus-server` clap command-line parser in `crates/portunus-server/src/operator/cli.rs` (or wherever the subcommand definitions live) so `push-rule <client> <listen> <target>` argument parsing tolerates the `start-end` syntax. Same file as T022.

**Checkpoint**: T019 passes end-to-end. A 100-port range rule pushes, lists as one entry, forwards same-offset traffic. Existing v0.1.0 single-port rules still work unchanged (regression check via the existing `portunus-e2e` smoke test).

---

## Phase 4: User Story 2 — Remove a port-range rule cleanly (Priority: P1)

**Goal**: Removing a range rule releases every listener in the range
within the existing drain window; in-flight connections drain; ports
become free for re-use.

**Independent Test**: Push a range rule, drive traffic, remove it.
Verify (a) every port in the range is no longer bound on the client
within the drain window, (b) `list-rules` no longer shows the entry,
(c) re-pushing a rule that uses any subset of those same ports
succeeds.

### Tests for User Story 2 ⚠️

- [X] T030 [P] [US2] Forwarder unit test in `crates/portunus-client/src/forwarder/mod.rs` `tests` module: `range_remove_releases_all_listeners` — push a 10-port range rule, verify all 10 ports are bound (TcpStream connect succeeds to each), trigger `cancel`, after drain verify all 10 ports refuse new connects within 1 s (mirrors existing `cancel_stops_accept_within_one_second` for the range case).
- [X] T031 [P] [US2] Forwarder unit test in `crates/portunus-client/src/forwarder/mod.rs` `tests` module: `range_in_flight_connection_drains` — push a range rule, open a connection on one port in the range, write/read warmup, cancel, verify the in-flight connection still echoes after cancel (parallels existing `cancel_drains_in_flight_connection`).
- [X] T032 [P] [US2] Integration test in `crates/portunus-e2e/tests/range_smoke.rs` (extend T019's file with a second `#[tokio::test]`): push a 50-port range, drive traffic on 3 ports, `DELETE /v1/rules/{id}`, after drain verify (a) `GET /v1/rules` returns empty, (b) on the client host, all 50 ports accept fresh `TcpListener::bind` from a sibling task. Maps to US2 acceptance scenarios 1 & 2.

### Implementation for User Story 2

- [X] T033 [US2] Verify the existing `JoinSet` + `proxy_cancel` drain loop in `forwarder::run` correctly handles N-listener cleanup. The drop of the `JoinSet` on function exit reaps every accept task; the shared `proxy_cancel` cancels all in-flight proxies after `drain_timeout`. If T030/T031 fail, fix here. Same file as T014, T027.
- [X] T034 [US2] Confirm the rule store conflict check correctly releases the `(client, listen_port_start)` entry on remove (from T021) so re-push of overlapping ports succeeds. Add a regression test `re_push_after_remove_succeeds` in `crates/portunus-server/src/rules.rs` `tests` module.

**Checkpoint**: Range removal is observably equivalent to single-port removal. T030, T031, T032 all pass.

---

## Phase 5: User Story 3 — Per-rule observability without label explosion (Priority: P2)

**Goal**: `rule-stats <id>` returns one aggregate row regardless of
range size; `/metrics` exposes one Prometheus series per per-rule
collector per rule, never per port; an opt-in `--per-port` CLI flag
exposes per-port detail without affecting Prometheus.

**Independent Test**: Push a 50-port range, drive bytes through three
distinct ports, run `rule-stats <id>` — one row whose counters sum
across all three ports. Run `rule-stats <id> --per-port` — see the
three ports with non-zero traffic. `curl /metrics | grep
portunus_rule_bytes_in_total | wc -l` returns the same count as before
the range push.

### Tests for User Story 3 ⚠️

- [X] T035 [P] [US3] Forwarder unit test in `crates/portunus-client/src/forwarder/mod.rs` `tests` module: `range_aggregate_stats_sum_across_ports` — push a 5-port range, drive 1 KB through ports `[0]`, `[2]`, `[4]`, sleep one stats-tick equivalent, verify the rule's aggregate `bytes_in == 3 * 1024` and that `stats.per_port` has 5 entries with the expected non-zero distribution.
- [X] T036 [P] [US3] Metrics cardinality test in `crates/portunus-server/tests/range_metrics_cardinality.rs` (NEW): start a real server, push a 100-port range rule (mock the client side via `portunus-e2e` harness), scrape `/metrics`, assert `portunus_rule_bytes_in_total{client_name="…",rule_id="…"}` appears exactly once for that `rule_id` (NOT 100 times). Direct test of SC-002.
- [X] T037 [P] [US3] HTTP per-port test in `crates/portunus-server/tests/range_per_port_http.rs` (NEW or extend T017): push a 10-port range, GET `/v1/rules/{id}/stats?per_port=true`, verify response includes `per_port` array of length 10; GET without the query param returns the v0.1.0 shape (no `per_port` key).
- [X] T038 [P] [US3] CLI test in `crates/portunus-server/src/operator/rule_cli.rs` `tests` module (or NEW `tests/range_per_port_cli.rs`): `rule-stats <id> --per-port` invokes the HTTP endpoint with `?per_port=true` and renders the per-port table format documented in `contracts/operator-api.md`.

### Implementation for User Story 3

- [X] T039 [P] [US3] Extend `RuleStats` in `crates/portunus-client/src/forwarder/stats.rs` with `per_port: BTreeMap<u16, PerPortCounters>` per `data-model.md` § RuleStats. Initialize with one entry per port in the listen range at forwarder startup. Add `record_in(port, n)` / `record_out(port, n)` / `inc_active(port)` / `dec_active(port)` methods that update BOTH the aggregate atomics AND the per-port atomics.
- [X] T040 [US3] Update `forwarder::run` accept loops (in `crates/portunus-client/src/forwarder/mod.rs`) to thread the per-port `listen_port` through to `proxy::proxy` so per-port counters increment correctly. The aggregate counters keep their existing call sites. Same file as T014, T027, T033.
- [X] T041 [US3] Update `proxy::proxy` in `crates/portunus-client/src/forwarder/proxy.rs` to accept the optional `listen_port: Option<u16>` (or always `Some` for ranges). Increment per-port counters via the new `RuleStats` methods.
- [X] T042 [P] [US3] Extend `StatsReport` packing in the client's stats reporter (search for the existing `StatsReport` build site, likely in `crates/portunus-client/src/control.rs` or a stats subscriber): emit `per_port` repeated entries from `RuleStats.per_port` snapshots. For single-port rules, populate the single-element per-port slot too (graceful degradation per `data-model.md`).
- [X] T043 [US3] Create `crates/portunus-server/src/operator/per_port_stats.rs` (NEW): `pub struct PerPortStatsCache { inner: Arc<RwLock<HashMap<RuleId, BTreeMap<u16, PerPortSnapshot>>>> }` with `update(rule_id, snapshots)` and `get(rule_id) -> Option<…>`. Wire into `AppState` (sequential — same file as T044).
- [X] T044 [US3] Update `crates/portunus-server/src/state.rs` to embed `PerPortStatsCache`. Same file as T043 (sequential).
- [X] T045 [US3] Update server gRPC `StatsReport` handler in `crates/portunus-server/src/grpc/service.rs` (around line 193 — `Some(Payload::StatsReport(report))`) to extract `per_port` from each `RuleStats` and call `state.per_port_stats.update(...)`. Aggregate counters keep their existing path.
- [X] T046 [US3] Update HTTP `GET /v1/rules/{rule_id}/stats` in `crates/portunus-server/src/operator/http.rs` to honor `?per_port=true`: read from `state.per_port_stats.get(rule_id)` and append a `per_port` array to the response. Default behavior (no query param) is unchanged. Same file as T024, T025.
- [X] T047 [US3] Add `--per-port` flag to `rule-stats` subcommand parser in `crates/portunus-server/src/operator/cli.rs` (or wherever subcommand defs live) and pass through to `crates/portunus-server/src/operator/rule_cli.rs::stats`, which appends `?per_port=true` to the URL and renders the per-port table.
- [X] T048 [US3] Confirm `crates/portunus-server/src/metrics.rs` does NOT add per-port labels (no diff expected — exists to satisfy SC-002 explicitly). Add a comment in that file: `// Range rules deliberately reuse the (client_name, rule_id) labels — see specs/002-port-range-forward/contracts/operator-api.md and SC-002.`

**Checkpoint**: T036 passes (Prometheus cardinality is range-size invariant). T037, T038 pass (per-port available via `--per-port` only). T035 passes (aggregate counters sum correctly).

---

## Phase 6: User Story 4 — Range conflicts rejected with useful error (Priority: P2)

**Goal**: A range that overlaps any active rule's listen port (single
or range) is rejected with an error naming at least one offending
port; the existing rule and any unrelated ports are unaffected.

**Independent Test**: Push rule A on `30000-30010`, then push rule B
on `30005-30015`. Confirm rule B is rejected with exit `5` whose
stderr names port `30005` (or the overlap range), no port in
`30011-30015` is bound on the client, and rule A's status is unchanged.

### Tests for User Story 4 ⚠️

- [X] T049 [P] [US4] Rule store overlap unit tests in `crates/portunus-server/src/rules.rs` (extend the `tests` module): `range_overlapping_existing_range_returns_PortInUse_with_offending_port`, `range_overlapping_existing_single_port_returns_PortInUse`, `range_adjacent_no_overlap_succeeds` (covers `30000-30010` then `30011-30020`), `range_against_pre_existing_external_listener_at_push_time` (cannot fully test — that's the client-side bind failure path covered by T018).
- [X] T050 [P] [US4] HTTP conflict test in `crates/portunus-server/tests/range_conflict_http.rs` (NEW or extend T017): push a range, then push an overlapping range, verify the second response is HTTP 409 with body `{"error":{"code":"port_in_use","message":"port {N} already in use by rule {ID} on {client}"}}`.
- [X] T051 [P] [US4] Integration test in `crates/portunus-e2e/tests/range_smoke.rs` (third test in same file): full overlap scenario per US4 acceptance — push, conflict, verify exit `5`, verify `list-rules` shows only the first rule, verify a fresh `TcpListener::bind` on `30011` (outside the second rule's overlap region but inside its requested range) succeeds (proves no partial bind).

### Implementation for User Story 4

- [X] T052 [US4] Implement the per-client interval check in `ServerRuleStore::push` (already partly built in T020). Walk `by_client_listen[client].range(..=candidate.end)`, for each entry load the rule, compute `existing.listen_range()`, and if it `overlaps(candidate.listen)` AND state is `Active | Failed`, return `RuleStoreError::PortInUse { offending_port: max(existing.start, candidate.start) }`. Same file as T020 — sequential.
- [X] T053 [US4] Update HTTP error formatting in `crates/portunus-server/src/operator/http.rs` (already partly done in T024) to render `PortInUse { offending_port }` into the human message naming the port AND the colliding `rule_id` and `client_name` (look up the existing rule via `store.get` for the `rule_id`). Same file as T024, T025, T046.
- [X] T054 [US4] Confirm CLI exit-code mapping in `crates/portunus-server/src/operator/rule_cli.rs::code_to_exit` — `port_in_use → 5` is already wired; add a regression test asserting the message format (the message field is human, but operators may grep for it; freezing the format helps downstream tooling).

**Checkpoint**: T049–T051 pass. Range overlap detection is symmetric (single↔range, range↔range, range↔single).

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Performance benchmark (Constitution II), docs, deploy
files, end-to-end SC verification.

- [X] T055 [P] Create `crates/portunus-client/benches/range_install.rs` (NEW): criterion benchmark for `forwarder::range::bind_all` across range sizes `[1, 10, 100, 1024]`. Record results in the PR description per Constitution II. Verify the 1024-port case completes well under the SC-001 5-second push budget.
- [X] T056 [P] Update `README.md` Layout / Quickstart sections to mention range rules (one paragraph + one example, link to `specs/002-port-range-forward/quickstart.md`).
- [X] T057 [P] Update `CHANGELOG.md`: move the `[Unreleased]` entry from T003 into a `## [0.2.0] - 2026-MM-DD` block when the feature ships, listing additive proto fields, new CLI flag `--per-port`, new server config `range_rule_max_ports`, and the SC-002 cardinality guarantee.
- [X] T058 [P] Update `deploy/server.toml.example`: add a commented-out `# range_rule_max_ports = 1024` line with a comment explaining the relationship to `LimitNOFILE`.
- [X] T059 [P] Update `deploy/systemd/portunus-server.service` (and `portunus-client.service` if relevant): if `LimitNOFILE` is currently capped, raise to leave headroom for the default `range_rule_max_ports = 1024` plus per-connection overhead. Document the math in a comment.
- [X] T060 [P] Update `docs/runbook.md` § Day-2 ops with a "managing range rules" section: cap config, conflict semantics, downgrade safety from `quickstart.md`.
- [X] T061 Run `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings` and fix any lints introduced.
- [X] T062 Run `cargo test --workspace --release` to confirm everything passes including the slower integration tests (`range_smoke.rs`).
- [X] T063 Execute the SC-001 100-port verification recipe from `specs/002-port-range-forward/quickstart.md` § "Verifying SC-001 on a fresh host pair" against the production deployment fixtures (the `example-edge` Debian host already wired up for spec 001 SC-001). Capture wall-clock numbers in `CHANGELOG.md` under the `0.2.0` entry.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no deps; can start immediately.
- **Foundational (Phase 2)**: depends on Setup. **BLOCKS US1–US4.**
- **US1 (Phase 3)**: depends on Foundational complete.
- **US2 (Phase 4)**: depends on Foundational + US1 (uses US1's range push to set up the remove scenarios).
- **US3 (Phase 5)**: depends on Foundational + US1 (needs an active range rule to observe).
- **US4 (Phase 6)**: depends on Foundational + US1 (uses US1's range push to construct the conflict).
- **Polish (Phase 7)**: depends on US1–US4 (the bench + docs reflect final behavior).

### User Story Dependencies

- **US1 (P1)**: foundational only.
- **US2 (P1)**: depends on US1's range push code paths existing.
- **US3 (P2)**: depends on US1; orthogonal to US2 (can run in parallel with US2 on different files).
- **US4 (P2)**: depends on US1; orthogonal to US2 + US3 (different files: rules.rs conflict path).

### Within Each User Story

- Tests (T015–T019, T030–T032, T035–T038, T049–T051) MUST be written FIRST and observed to FAIL before the implementation tasks land (Constitution III).
- Models / data structures before services / handlers.
- Implementation before integration tests.

### Parallel Opportunities

- T002, T003 in Setup.
- T006, T007/T008, T010/T011 in Foundational (different files).
- T015–T019 in US1 (different files; tests).
- T024, T025 in US1 implementation are SAME file (sequential).
- T030, T031, T032 in US2 (different files; tests).
- T035–T038 in US3 (different files; tests).
- T039, T043, T046, T047 in US3 implementation can run in parallel pairs (different files); T040+T041 share `forwarder/mod.rs` and `proxy.rs` so are sequential within their pair.
- T049, T050, T051 in US4 (different files; tests).
- T055–T060 in Polish (different files).
- US2 + US3 + US4 can be staffed by three developers simultaneously after US1 completes.

---

## Parallel Example: User Story 1

```bash
# After Foundational completes, launch US1 tests in parallel:
Task: "T015 Unit tests in crates/portunus-server/src/rules.rs"
Task: "T016 Unit tests in crates/portunus-server/src/operator/cli.rs"
Task: "T017 HTTP handler tests in crates/portunus-server/tests/range_http.rs"
Task: "T018 Forwarder unit tests in crates/portunus-client/src/forwarder/range.rs"
Task: "T019 Integration test in crates/portunus-e2e/tests/range_smoke.rs"

# Then implementation (some sequential because same-file):
Task: "T020 + T021 (ServerRuleStore push + remove, sequential same file)"
Task: "T022 + T029 (operator/cli.rs parser, sequential same file)"
Task: "T024 + T025 (operator/http.rs handler + list, sequential same file)"
Task: "T026 (forwarder/range.rs NEW file, parallel)"
Task: "T027 (forwarder/mod.rs run refactor, sequential after T026)"
Task: "T028 (control.rs lift, parallel)"
```

---

## Implementation Strategy

### MVP First (US1 only)

1. Phase 1 (Setup) — small.
2. Phase 2 (Foundational) — proto + `PortRange` + config + struct extensions.
3. Phase 3 (US1) — operator can push a range and traffic flows.
4. **STOP and VALIDATE**: run T019 (the 100-port integration test).
5. Demo / merge to a feature branch.

### Incremental Delivery

1. Setup + Foundational → branch builds, no behavior change.
2. + US1 → operators can push range rules. **This alone is shippable.**
3. + US2 → range removal hardened (most of it works automatically; tests prove it).
4. + US3 → operators get aggregate stats + opt-in `--per-port`.
5. + US4 → conflict errors are useful + tested.
6. Polish → bench, docs, deploy, SC-001 100-port run.

### Parallel Team Strategy

- Solo developer: do phases sequentially as listed.
- Two developers: after Foundational, one takes US1+US2 (lifecycle / forwarder), other takes US3+US4 (observability / conflict checks); merge before Polish.
- Three developers: after US1 lands, US2 / US3 / US4 fan out in parallel.

---

## Notes

- [P] = different files, no deps on incomplete tasks.
- [Story] label = US1/US2/US3/US4, maps directly to spec.md.
- Every code change is additive — no v0.1.0 single-port API surface
  changes. The existing `portunus-e2e` smoke test for the single-port
  flow is the regression net.
- Constitution II requires the bench (T055) before merging the
  implementation. Don't skip it.
- Constitution III requires the contract test (T006) before the proto
  field changes (T004/T005) land in a release; in practice the proto
  field changes are required for the contract test to compile, so the
  ordering becomes "land the proto change in the same commit as the
  contract test, with the test verifying both the legacy and the new
  shapes round-trip".
- After each task: `cargo test -p <crate>` for the affected crate.
  After each phase checkpoint: full `cargo test --workspace`.
- Commit after each task or logical group; the `auto-commit` hooks
  will fire on `/speckit-implement` boundaries.
