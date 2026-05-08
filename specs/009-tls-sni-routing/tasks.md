---

description: "Task list for 009 — TLS SNI Routing"
---

# Tasks: TLS SNI-Based Routing for Forwarded Connections

**Input**: Design documents from `/specs/009-tls-sni-routing/`
**Prerequisites**: plan.md, spec.md, design.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: REQUIRED. Constitution Principle III is non-negotiable; every contract surface in `contracts/*` and every SC- in `spec.md` ships with a contract / integration test authored before its implementation.

**Organization**: Tasks are grouped by user story so each is independently completable. Phase 2 (Foundational) is the single blocking prerequisite — once it lands, US1..US5 can be worked in priority order or in parallel.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Different file, no dependency on incomplete tasks → can run in parallel
- **[Story]**: US1..US5 maps to spec.md user stories
- File paths are absolute relative to repo root

## Path conventions

Cargo workspace, six crates. v0.9 changes land in:

- `crates/forward-proto/proto/forward.proto` (additive fields + new message)
- `crates/forward-server/src/{rules,operator,grpc,metrics,store/migrations,main}.rs` (control plane)
- `crates/forward-client/src/forwarder/{mod,proxy,stats}.rs`,
  `crates/forward-client/src/forwarder/sni/`, `crates/forward-client/src/port_groups.rs`,
  `crates/forward-client/src/control.rs` (data plane)

Test homes:
- Per-crate `tests/` (Cargo integration test convention)
- Real packet captures under `crates/forward-client/tests/fixtures/tls/`

Bench home:
- `crates/forward-client/benches/sni_route.rs` (NEW)
- `crates/forward-client/benches/data_plane.rs` (existing v0.7 baseline; legacy byte-stability gate)
- `crates/forward-server/benches/operator_api.rs` (existing v0.8; rule-push regression gate)

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Empty scaffolding + proto field reservations. **No new workspace deps** (R-006).

- [x] T001 [P] Reserve proto field numbers in `/Users/zingerbee/Documents/forward-rs/proto/forward.proto`: add a comment block above `message Rule` documenting that field 11 is now `sni_pattern` (009-tls-sni-routing); above `message RuleStats` documenting that fields 13/14/15 are now SNI hit counters; above `message StatsReport` documenting that field 3 is `sni_listener_stats`. Body of each field is added in T015..T018. Reference: contracts/wire.md §1.
- [x] T002 [P] Create empty module tree under `/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/sni/`: `mod.rs` (empty `pub use`), `client_hello.rs` (`pub fn parse() -> _ { unimplemented!() }` stub), `route_table.rs` (empty), `peek.rs` (empty), `listener.rs` (empty). Register `pub mod sni;` in `crates/forward-client/src/forwarder/mod.rs`.
- [x] T003 [P] Create empty module file `/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/port_groups.rs` (stub `pub struct PortGroupManager;` with `unimplemented!()` constructors); register `pub mod port_groups;` in `crates/forward-client/src/lib.rs`.
- [x] T004 [P] Create empty migration file `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/store/migrations/V002__add_sni_pattern.sql` (header comment only); confirm `refinery::embed_migrations!` picks it up at boot (boot output shows "applied migration V002" or similar — body filled in T013).
- [x] T005 [P] Create empty bench scaffold `/Users/zingerbee/Documents/forward-rs/crates/forward-client/benches/sni_route.rs` and register it under `[[bench]]` in `crates/forward-client/Cargo.toml`. Body for now: `criterion_main!(()); fn _placeholder() {}`. Real benches added in T080 / T081.
- [x] T006 [P] Create test fixtures directory `/Users/zingerbee/Documents/forward-rs/crates/forward-client/tests/fixtures/tls/` (empty); fixtures land in T020..T024.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Wire delta + SQLite migration + server-side rule store + capability gate. Until this phase is complete no user story above the wire can be implemented because every store / dispatcher seam (`SniRoutingTable`, `PortGroupManager`, capability gate) needs a real `Rule.sni_pattern` and `RuleStats.sni_route_*_total`.

**⚠️ CRITICAL**: All US-labelled tasks (Phase 3+) are blocked by this phase.

### Tests for Foundational (TDD per Constitution III)

> Write these tests FIRST and confirm they fail before implementing T015..T018, T013..T014, T026..T028.

- [x] T007 [P] Wire-compat round-trip — Rule field 11 — in `/Users/zingerbee/Documents/forward-rs/crates/forward-proto/tests/sni_wire_compat.rs::roundtrip_rule_field_11`. Build a `Rule` with `sni_pattern = Some("api.example.com")`, encode → decode → assert equal; assert that `sni_pattern = None` encoding is byte-identical to the v0.8 encoding of the same logical rule. Reference contracts/wire.md §5.
- [x] T008 [P] Wire-compat round-trip — RuleStats fields 13/14/15 — in `crates/forward-proto/tests/sni_wire_compat.rs::roundtrip_rule_stats_13_14_15`. Build a `RuleStats` with all three SNI counters set; round-trip; assert default-zero encoding equals v0.8.
- [x] T009 [P] Wire-compat round-trip — `SniListenerStats` + `StatsReport.sni_listener_stats = 3` — in `crates/forward-proto/tests/sni_wire_compat.rs::roundtrip_sni_listener_stats`. Round-trip a `StatsReport` carrying one populated `SniListenerStats`; assert empty list encoding equals v0.8.
- [x] T010 [P] **Negative** wire-compat — `RuleStats` fields 11/12 untouched — in `crates/forward-proto/tests/sni_wire_compat.rs::negative_rule_stats_11_12_unchanged`. Build a `RuleStats` with v0.7 fields 11 (`target_failovers_total = 9`) and 12 (`per_target = […]`) set, **no SNI fields** set; assert encoded bytes are identical to v0.8 encoding of the same logical content. Reference: design.md HIGH-1 from round-3 review.
- [x] T011 [P] Operator-API contract test — `POST /v1/rules` accepts `sni_pattern` — in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/sni_rule_validation.rs`. Each malformed input from contracts/operator-api.md §1.2 → asserted status + `error.code` (UDP/range → 400 `validation.sni_on_unsupported_rule`; malformed grammar → 400 `validation.sni_pattern_malformed`).
- [x] T012 [P] Capability-gate contract — `crates/forward-server/tests/sni_capability_gate.rs`. Connect a forward-client whose `Hello.client_version = "0.8.0"`; push a rule with `sni_pattern = Some("api.example.com")`; assert HTTP 422 with `error.code = "sni_unsupported_by_client"` and that the rule is absent from `GET /v1/rules` and no `RuleUpdate` reaches the bidi channel.

### Implementation for Foundational

- [x] T013 Author `V002__add_sni_pattern.sql` in `/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/store/migrations/V002__add_sni_pattern.sql`: `ALTER TABLE rules ADD COLUMN sni_pattern TEXT;` plus partial helper index `CREATE INDEX rules_sni_lookup ON rules(listen_port, sni_pattern) WHERE sni_pattern IS NOT NULL;`. NO SQL `UNIQUE` (R-003). Verify boot logs "applied migration V002".
- [x] T014 Bump schema-version range in the v0.8 store handshake (search for `[1, 1]` / `range = 1..=1` in `crates/forward-server/src/store/mod.rs`) → `[1, 2]`. Add a regression test in `crates/forward-server/tests/store_schema_handshake.rs` (already exists from v0.8) covering "v0.9 binary opens v0.8 state.db, runs V002, schema version becomes 2".
- [x] T015 Add `optional string sni_pattern = 11;` to `message Rule` in `proto/forward.proto`; add the documentation block from contracts/wire.md §1.1 above the field. Re-run `cargo build` — auto-generates the prost types. Make T007 pass.
- [x] T016 Add `uint64 sni_route_exact_total = 13; uint64 sni_route_wildcard_total = 14; uint64 sni_route_fallback_total = 15;` to `message RuleStats` in `proto/forward.proto` with the doc block from contracts/wire.md §1.2. Make T008 pass; verify T010 still passes (no field 11/12 disturbance).
- [x] T017 Add new `message SniListenerStats { uint32 listen_port = 1; uint64 sni_route_miss_total = 2; uint64 client_hello_parse_failures_total = 3; }` to `proto/forward.proto`.
- [x] T018 Add `repeated SniListenerStats sni_listener_stats = 3;` to `message StatsReport` in `proto/forward.proto`. Make T009 pass.
- [x] T019 Wire-layout drift guard — `crates/forward-proto/tests/sni_wire_compat.rs::wire_layout_documented`: parse `proto/forward.proto` (regex / line scan) and assert the field-number registry table at the bottom of contracts/wire.md §4 matches the actual proto declarations. Prevents future spec drift.
- [ ] T020 [P] Capture TLS 1.0 ClientHello fixture into `crates/forward-client/tests/fixtures/tls/client_hello_tls10.bin` using `openssl s_client -tls1 -connect localhost:9999 -servername example.com` against an `openssl s_server -tls1`. Document the capture command in `crates/forward-client/tests/fixtures/tls/README.md` so future maintainers can reproduce.
- [ ] T021 [P] Capture TLS 1.1 ClientHello fixture into `crates/forward-client/tests/fixtures/tls/client_hello_tls11.bin` (same procedure as T020 with `-tls1_1`).
- [ ] T022 [P] Capture TLS 1.2 ClientHello fixture into `crates/forward-client/tests/fixtures/tls/client_hello_tls12.bin` (`-tls1_2`).
- [ ] T023 [P] Capture TLS 1.3 ClientHello fixture into `crates/forward-client/tests/fixtures/tls/client_hello_tls13.bin` (`-tls1_3`). Include one with PQ-hybrid `X25519MLKEM768` group if openssl-provider is available, else stick to `X25519`.
- [ ] T024 [P] Synthesise a fragmented ClientHello fixture into `crates/forward-client/tests/fixtures/tls/client_hello_fragmented.bin` by hand-splitting one of T020..T023 across two TLS records (RFC 8446 §5.1). Document the split point in the README.
- [x] T025 [P] Add `sni_pattern: Option<String>` to `crates/forward-server/src/rules.rs::Rule`. Update every constructor / SQL row mapper. The store reads/writes the column directly; persistence test in `crates/forward-server/tests/rule_store_round_trip.rs` (existing) gains a SNI variant.
- [x] T026 Rewrite `ServerRuleStore::push` overlap check in `crates/forward-server/src/rules.rs` per data-model.md §Overlap matrix. Steps:
  1. Replace `by_client_listen_start: BTreeMap<u16, RuleId>` with `BTreeMap<u16, Vec<RuleId>>` (no new dep).
  2. For TCP single-port candidates, walk the existing list at `(client, listen_port)` and apply the overlap matrix. Distinct `sni_pattern` siblings → accept; matching pattern → 409 `conflict.sni_route_duplicate`; legacy + SNI candidate → 409 `conflict.legacy_to_sni_unsupported`; SNI + duplicate fallback → 409 `conflict.sni_fallback_duplicate`.
  3. UDP and port-range candidates retain v0.7 overlap rules unchanged.
  4. Update `RuleStoreError` variants and `From<RuleStoreError> for OperatorError` to surface the new conflict codes.
- [x] T027 Add `version_at_least_0_9(v: &str) -> bool` next to `version_at_least_0_7` in `crates/forward-server/src/operator/http.rs` (search for `version_at_least_0_7` around line 353). Same parser; gate on minor ≥ 9.
- [x] T028 In the same `operator/http.rs::push_rule` handler, after the v0.7 multi-target gate, add the v0.9 capability check: if `body.sni_pattern.is_some()` and the targeted client's last `Hello.client_version` is below 0.9.0, return `OperatorError::SniUnsupportedByClient { client_name, client_version }` (new variant) → HTTP 422 with `error.code = "sni_unsupported_by_client"`. Make T012 pass.
- [x] T029 Validate `sni_pattern` grammar (data-model.md §V-3) in `crates/forward-server/src/operator/http.rs::validate_rule` (or its delegated helper). Reject UDP / port-range with `sni_pattern.is_some()` → 400 `validation.sni_on_unsupported_rule`. Reject malformed pattern → 400 `validation.sni_pattern_malformed`. Lowercase the pattern on accept (data-model.md §V-4). Make T011 pass.
- [x] T030 Overlap matrix integration test — `crates/forward-server/tests/sni_overlap_matrix.rs`. Drive every row of the §Overlap table through `POST /v1/rules` against a real server; assert the documented status / error code for each.
- [x] T031 Active legacy → SNI conversion test — `crates/forward-server/tests/sni_legacy_to_sni_unsupported.rs`. Push a legacy plain-TCP rule on port P; push an SNI sibling → assert 409 `conflict.legacy_to_sni_unsupported`; remove the legacy rule; push the SNI rule again → assert success.

**Checkpoint**: Foundation ready. From here on, each US can be implemented and tested in isolation against the now-stable wire / store seams.

---

## Phase 3: User Story 1 — Route Multiple TLS Hostnames Through One Port (Priority: P1) 🎯 MVP

**Goal**: Two TCP rules on `:443` with distinct exact `sni_pattern` route a real TLS client to the correct upstream by hostname; bytes pass through unchanged.

**Independent Test**: Push two rules with `sni_pattern = "api.example.com"` and `sni_pattern = "web.example.com"` on `:443` → connect with each SNI from `openssl s_client -servername` → assert each lands on the correct backend (SC-001).

### Tests for User Story 1 (TDD)

> Write these tests FIRST and confirm they fail before implementing T036..T046.

- [x] T032 [P] [US1] Unit test — `client_hello::parse` happy path — in `/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/sni/client_hello.rs::tests::parse_tls12_extracts_sni`. Load `tests/fixtures/tls/client_hello_tls12.bin`; assert `parse(bytes) == Ok(Some(ServerName("example.com")))`.
- [x] T033 [P] [US1] Unit test — incremental feed (Truncated → Ok) — in the same file `::tests::parse_truncated_then_complete`. Feed bytes one at a time; assert `Truncated` then `Ok(Some(...))`.
- [x] T034 [P] [US1] Unit test — exact match priority — in `/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/sni/route_table.rs::tests::exact_beats_fallback`. Build a table with one exact rule and one fallback; assert exact wins.
- [x] T035 [P] [US1] Integration test — two SNI rules → two upstreams — in `/Users/zingerbee/Documents/forward-rs/crates/forward-client/tests/sni_route_e2e_exact.rs`. Spin up two TCP backends + a server + a v0.9 client; push two SNI rules on `:443`; rustls clients with each SNI; assert each lands on the right backend.

### Implementation for User Story 1

- [x] T036 [P] [US1] Implement `client_hello::parse(bytes: &[u8]) -> Result<ParseOutcome, ParseError>` in `crates/forward-client/src/forwarder/sni/client_hello.rs`. Outcomes: `Truncated`, `Ok(Some(host))`, `Ok(None)`. Errors: `NotTls`, `Malformed`. Single record only — first handshake message must be ClientHello (R-015). Read only `server_name` extension; skip everything else by length. Make T032..T033 pass.
- [x] T037 [P] [US1] Implement `SniRoutingTable` in `crates/forward-client/src/forwarder/sni/route_table.rs` with the layout from data-model.md §2.2 — `exact: HashMap<String, RuleId>`, `wildcards: Vec<…>` (empty for now — populated in US2), `fallback: Option<RuleId>` (empty for now — populated in US3). `lookup` implements Exact-then-Fallback only at this stage. Make T034 pass.
- [x] T038 [P] [US1] Implement `peek::read_client_hello(stream, 3 s, 64 KiB) -> Result<(Vec<u8>, Option<ServerName>), PeekError>` in `crates/forward-client/src/forwarder/sni/peek.rs`. Re-invokes `client_hello::parse` after each `read`; returns the captured buffer + parsed SNI. Errors map cleanly to the five tracing event names (R-009).
- [x] T039 [P] [US1] Add `pub sni_pattern: Option<String>` to `ClientRule` in `crates/forward-client/src/forwarder/mod.rs`. Update every constructor (search call sites); `crates/forward-client/src/control.rs::handle_rule_update` plumbs it from the wire `Rule.sni_pattern`.
- [x] T040 [US1] Implement `SniListener` in `crates/forward-client/src/forwarder/sni/listener.rs` (data-model.md §2.3): owns the bound `TcpListener`, the `tokio::sync::watch::Receiver<Arc<SniRoutingTable>>`, the cancellation token, and an `Arc<SniListenerCounters>`. Per-connection accept handler:
  1. peek ClientHello (T038),
  2. snapshot table via `borrow().clone()`,
  3. lookup, hit → dispatch to `proxy::proxy(stream, preread, rule)`; miss/error → close + bump counter + tracing event.
- [x] T041 [US1] Extend `proxy::proxy` in `crates/forward-client/src/forwarder/proxy.rs` to accept an optional preread buffer. When present, write it to the upstream before splicing. The hot path on the legacy plain-TCP code path stays byte-identical (`preread = None`).
- [x] T042 [US1] Implement `PortGroupManager` skeleton in `crates/forward-client/src/port_groups.rs` (data-model.md §2.4) — `groups: HashMap<u16, GroupState>` + `rule_to_port: HashMap<RuleId, u16>`. Operations: `apply_push(rule)` and `apply_remove(rule_id)` returning `Result<(), PortGroupError>`. For US1 only the SNI mode (≥1 SNI member) needs to be functional; the Legacy variant can carry a stub that delegates to the existing v0.7 forwarder spawn for backward compat (US4 will exercise it).
- [x] T043 [US1] Wire `PortGroupManager` into the control loop at `crates/forward-client/src/control.rs::handle_rule_update`: `RuleUpdate(PUSH | REMOVE)` flows through the manager instead of the per-rule `forwarder::ClientRule -> task` path. Old per-rule path is removed only after all five user stories activate it (kept side-by-side until US4 verifies byte-stability).
- [x] T044 [US1] CLI flag — add optional `--sni <PATTERN>` to `forward-server push-rule` in `crates/forward-server/src/main.rs` per contracts/cli.md §1. Pre-API rejections (UDP / port-range / malformed) exit 2 with the documented stderr.
- [x] T045 [US1] Operator API response shape — `POST /v1/rules` and `GET /v1/rules` echo `sni_pattern` when present, omit when absent (`#[serde(skip_serializing_if = "Option::is_none")]`).
- [x] T046 [US1] Make `sni_route_e2e_exact.rs` (T035) pass: end-to-end with two SNI rules + two upstreams + rustls clients with two SNIs.

**Checkpoint**: US1 functional. The MVP — operators can fan out two TLS hostnames on one port — is shippable here.

---

## Phase 4: User Story 2 — Wildcard SNI Routing (Priority: P1)

**Goal**: A rule with `sni_pattern = "*.app.example.com"` matches `tenantA.app.example.com` and `other.app.example.com` but not `app.example.com` and not `a.b.app.example.com`.

**Independent Test**: Push one wildcard rule on `:443` → four rustls clients (matching, matching, no-leading-label, two-level) → assert two pass, two are rejected (SC-001 + edge cases).

### Tests for User Story 2 (TDD)

- [x] T047 [P] [US2] Unit test — wildcard match + single-label remainder — in `crates/forward-client/src/forwarder/sni/route_table.rs::tests::wildcard_single_label_only`. Assert `*.example.com` matches `foo.example.com`; rejects `example.com` (no left label) and `a.b.example.com` (extra label).
- [x] T048 [P] [US2] Unit test — wildcard specificity — `::tests::longest_wildcard_wins`. Two rules `*.team.example.com` and `*.example.com`; assert `x.team.example.com` matches the more specific one.
- [x] T049 [P] [US2] Unit test — exact vs. wildcard priority — `::tests::exact_beats_wildcard`. One exact `api.example.com`, one wildcard `*.example.com`; assert `api.example.com` matches exact.
- [x] T050 [P] [US2] Validation test — wildcard grammar — extend `crates/forward-server/tests/sni_rule_validation.rs::wildcard_grammar` to include rejections for `*.com` (single-label suffix), `*.*.example.com` (multi-`*`), `foo*.example.com` (`*` not at leftmost), `*example.com` (no dot after `*`).
- [x] T051 [P] [US2] Integration test — wildcard end-to-end — `crates/forward-client/tests/sni_route_e2e_wildcard.rs`. Push `*.web.example.com`; rustls clients with three SNIs; assert correct routing decisions.

### Implementation for User Story 2

- [x] T052 [US2] Extend `SniRoutingTable::from_members` in `crates/forward-client/src/forwarder/sni/route_table.rs` to populate `wildcards: Vec<(String, RuleId)>` sorted by suffix length descending. The suffix stored is the part **after** `*.`.
- [x] T053 [US2] Extend `SniRoutingTable::lookup` to walk `wildcards` after `exact` miss; for each `(suffix, rule_id)` apply the data-model.md §2.2 algorithm: `host.ends_with("." + suffix) && prefix_before_suffix.contains('.') == false`. Make T047..T049 pass.
- [x] T054 [US2] Tighten the grammar check in `crates/forward-server/src/operator/http.rs::validate_rule` per data-model.md §V-3 (T029 covers the basics; this task adds wildcard-specific checks: `*` must be the first label; suffix must have ≥ 2 labels; no other `*`). Make T050 pass.
- [x] T055 [US2] Make `sni_route_e2e_wildcard.rs` (T051) pass.

**Checkpoint**: US2 functional. Operators get production-grade subdomain fan-out.

---

## Phase 5: User Story 3 — TLS-Only Fallback (Priority: P2)

**Goal**: A `sni_pattern = NULL` rule on a port with SNI siblings catches valid TLS connections whose SNI is missing or unmatched.

**Independent Test**: Push one exact + one fallback rule → no-SNI client lands on fallback; without fallback → connection reset + `tls.no_sni` event (SC-001 + spec §2 edge cases).

### Tests for User Story 3 (TDD)

- [x] T056 [P] [US3] Unit test — fallback only when exact + wildcard miss — `crates/forward-client/src/forwarder/sni/route_table.rs::tests::fallback_only_on_miss`.
- [x] T057 [P] [US3] Unit test — duplicate fallback rejected at table build — `::tests::duplicate_fallback_panics_or_errors`. Pass two None members to `from_members`; assert error / panic with explicit message (data-model.md INV-1 indirectly).
- [x] T058 [P] [US3] Integration test — fallback present — `crates/forward-client/tests/sni_route_fallback.rs::with_fallback_routes_no_sni_client`. No-SNI rustls client on a port with a fallback rule → lands on the fallback upstream.
- [x] T059 [P] [US3] Integration test — fallback absent — `…::without_fallback_resets_connection`. Same setup minus the None rule → connection reset; `tls.no_sni` tracing event observed.

### Implementation for User Story 3

- [x] T060 [US3] Populate `fallback: Option<RuleId>` in `SniRoutingTable::from_members` when a member has `sni_pattern = None`. Reject duplicate fallbacks at build time (panic with a clear message; the server's overlap matrix prevents this in normal flow but the in-memory check is a backstop). Make T056..T057 pass.
- [x] T061 [US3] Verify `SniListener` lookup falls through to `fallback` when both `exact` and `wildcard` miss (already implied by T053; spot-check with T056 fixture).
- [x] T062 [US3] Make T058 + T059 pass.

**Checkpoint**: US3 functional. Operators get a TLS-only catch-all that does not silently accept plain-TCP traffic.

---

## Phase 6: User Story 4 — Existing Plain-TCP Rules Unchanged (Priority: P2)

**Goal**: Every v0.8 rule without `sni_pattern` continues to forward byte-for-byte; v0.8 e2e suite passes unchanged on v0.9.

**Independent Test**: Run v0.8's existing test suite against a v0.9 build; capture v0.8 control-plane wire trace and replay; assert byte-identical responses (SC-002, SC-004).

### Tests for User Story 4 (TDD)

- [x] T063 [P] [US4] Integration test — legacy plain-TCP byte-stability — `crates/forward-client/tests/legacy_plain_tcp_unchanged.rs`. Push a legacy rule on `:9000`; send a non-TLS payload (e.g. raw bytes / HTTP); assert the upstream receives the bytes byte-identically (sha256 match) AND assert via `tracing::subscriber` that no `target = "tls_sni"` event fires (data plane never enters the SNI path).
- [ ] T064 [P] [US4] Wire-replay test — `crates/forward-server/tests/v07_v08_wire_replay.rs`. Capture an existing v0.8 RuleUpdate trace (from the `forward-e2e` integration suite or a synthetic capture) and replay against a v0.9 server; assert the response stream is byte-identical to the v0.8 baseline.
- [x] T065 [P] [US4] Reuse the existing v0.7 byte-passthrough test as a regression — `crates/forward-client/tests/sni_byte_passthrough.rs`. Two upstream paths (legacy + SNI listener); for both, assert sha256(upstream-received) == sha256(client-sent).
- [ ] T066 [P] [US4] Bench gate — confirm legacy data plane path is unchanged — `crates/forward-client/benches/data_plane.rs` (existing). Compare v0.9 numbers to the v0.7 / v0.8 baseline checked in under `specs/008-sqlite-storage/baselines/` (or capture a fresh baseline if none exists). Allow ≤ 5 % regression per Constitution II hot-path budget.

### Implementation for User Story 4

- [x] T067 [US4] Confirm `PortGroupManager::Legacy` arm dispatches to the existing v0.7 forwarder spawn (the one in `crates/forward-client/src/forwarder/mod.rs::spawn_for_rule`) without touching `proxy::proxy`'s preread path. The compile check + T063 + T064 + T065 + T066 jointly verify byte-stability.
- [x] T068 [US4] Add an explicit assertion in `SniListener::handle_accept`: if the route group's mode is Legacy, the listener task should not exist — Legacy listeners run a different task type. Catch any developer footgun with `debug_assert!(matches!(self.mode, ListenerMode::Sni))` early in the function.
- [x] T069 [US4] Remove the dual-spawn fallback (the side-by-side path kept in T043) once T067..T068 are green; control loop now goes through `PortGroupManager` for all TCP single-port rules. Other rule shapes (UDP, port-range) remain on the v0.7 spawn path unchanged.

**Checkpoint**: v0.7 / v0.8 deployments upgrade to v0.9 without functional change.

---

## Phase 7: User Story 5 — Operator Diagnostics (Priority: P3)

**Goal**: After mixed traffic, operators read `/metrics` and the structured tracing log to triage SNI listener health (exact / wildcard / fallback / miss / parse-failure).

**Independent Test**: Run mixed traffic for 5 min → scrape `/metrics` → see counters matching expected proportions; structured log contains one event per failure case (SC-007).

### Tests for User Story 5 (TDD)

- [x] T070 [P] [US5] Per-rule counter emission test — `crates/forward-client/tests/sni_stats_emitted.rs::per_rule_hit_counters`. Run 10 exact-hit / 5 wildcard / 3 fallback connections; assert that the next `StatsReport` carries the expected `RuleStats.sni_route_*_total` values (fields 13/14/15) per rule.
- [x] T071 [P] [US5] Listener-level counter emission test — `…::listener_miss_and_parse`. Drive 4 SNI miss + 2 plain-HTTP + 1 timeout connections; assert the next `StatsReport.sni_listener_stats[port=443]` carries `sni_route_miss_total = 4` and `client_hello_parse_failures_total = 3`.
- [ ] T072 [P] [US5] Hot-reload preserves in-flight test — `crates/forward-client/tests/sni_hot_reload.rs`. Open a long-running SNI connection; mutate the route group; assert the open connection completes its bytes; new connections see the new table.
- [x] T073 [P] [US5] REMOVE-by-rule_id consistency — `crates/forward-client/tests/sni_remove_by_rule_id.rs`. Push two SNI rules on `:443`; REMOVE the second by rule_id (no port hint); assert the listener still has the first rule and the reverse index is consistent (data-model.md INV-2).
- [ ] T074 [P] [US5] Server-side metrics surface — `crates/forward-server/tests/sni_metrics_surface.rs`. Same scenario as T070..T071; scrape `/metrics`; assert `forward_tls_sni_route_total{client,rule,owner,result}` and `forward_tls_sni_listener_miss_total{client,port}` are present with expected values.
- [ ] T075 [P] [US5] Audit-ring isolation — `crates/forward-server/tests/sni_audit_ring_isolation.rs`. Drive every tracing event listed in contracts/operator-api.md §5; assert `GET /v1/audit` returns the same result before and after.
- [x] T076 [P] [US5] Timeout / not-TLS rejection events — `crates/forward-client/tests/sni_route_timeout.rs` and `…/sni_route_not_tls.rs`. Open TCP without sending bytes (3 s) → connection reset + `tls.client_hello_timeout`. Send `GET / HTTP/1.1\r\n\r\n` → reset + `tls.parse_failed`.

### Implementation for User Story 5

- [x] T077 [US5] Implement per-rule SNI counters in `crates/forward-client/src/forwarder/stats.rs::RuleStats`: three new monotonic `AtomicU64` fields (exact / wildcard / fallback) bumped from `SniListener::handle_accept`. Aggregator that builds `proto::RuleStats` reads them into fields 13/14/15.
- [x] T078 [US5] Implement listener-level counters in `crates/forward-client/src/forwarder/sni/listener.rs::SniListenerCounters` (miss + parse-failure) and aggregate them into `proto::SniListenerStats` in `stats.rs`. Wire into `proto::StatsReport.sni_listener_stats` in the existing 5 s tick.
- [x] T079 [US5] Register five new Prometheus collectors in `crates/forward-server/src/metrics.rs` (alongside existing v0.5+ patterns — `client/rule/owner/result` for per-rule, `client/port` for per-listener). Reuse the `Registry` already present.
- [x] T080 [US5] Extend `crates/forward-server/src/grpc/service.rs::handle_stats_report` (around `:317`) to fold the three `RuleStats.sni_route_*_total` fields into the per-rule collectors and the new `StatsReport.sni_listener_stats` rows into the per-listener collectors.
- [x] T081 [US5] Implement the gauge `forward_tls_sni_routes_active` server-side: count rules with `sni_pattern.is_some()` from `ServerRuleStore`; refresh in the existing metrics tick (search for `metrics_tick` or equivalent in `crates/forward-server/src/metrics.rs`).
- [x] T082 [US5] Add the five `tracing` events with `target = "tls_sni"` in `crates/forward-client/src/forwarder/sni/listener.rs` and `peek.rs`: `tls.client_hello_timeout` (WARN), `tls.parse_failed` (WARN), `tls.no_sni` (INFO + `fallback_used: bool`), `tls.sni_no_match` (WARN), `tls.sni_routed` (INFO + `server_name`, `match_kind`).

**Checkpoint**: Full observability surface; operators can run v0.9 in production with their existing Prometheus / log infrastructure.

---

## Phase 8: Polish & Cross-Cutting

**Purpose**: Web-UI, CLI ergonomics, performance verification, release prep.

- [x] T083 [P] Web UI — rules list — add `SNI` column to the rules table in `crates/forward-web/src/pages/Rules.tsx` (or v0.6's equivalent path). `—` rendered when `sni_pattern` is absent. Update the Rules page snapshot test if one exists.
- [x] T084 [P] Web UI — rule editor — add an optional `SNI Pattern` input to the new/edit form, gated on Protocol = TCP and Port mode = Single. Helper text: "Exact host (api.example.com) or wildcard (*.example.com); leave blank for fallback / plain TCP". Wire client-side validation to mirror the API grammar.
- [x] T085 [P] CLI help — `forward-server push-rule --help` output gains the SNI section per contracts/cli.md §4. `list-rules` human output adds the `SNI` column.
- [x] T086 [P] Quickstart validation — run through `quickstart.md` end-to-end on a clean checkout; capture the actual `/metrics` output into the doc as the "expected" snippet (replace placeholders if numbers shifted).
- [ ] T087 [P] Bench — `SniRoutingTable::lookup` ns/op — `crates/forward-client/benches/sni_route.rs::lookup_at_scale`. Build tables with 100 / 1 000 / 10 000 routes; criterion runs hits + misses; assert p99 < 100 µs at 100 routes (SC-006).
- [ ] T088 [P] Bench — connection-setup latency — `crates/forward-client/benches/sni_route.rs::setup_latency_vs_baseline`. Compare SNI listener vs. v0.7 plain-TCP listener; assert SNI within +5 ms p99 (SC-003).
- [ ] T089 [P] Operator API regression bench — `crates/forward-server/benches/operator_api.rs` (existing). Assert the rule-push path is within 5 % of the v0.8 baseline despite the new overlap-matrix walk.
- [x] T090 CHANGELOG entry under `# 0.9.0` documenting: SNI routing, capability gate, additive proto fields, additive SQL migration, no breaking changes. Reference the spec / plan file paths.
- [x] T091 Bump workspace version in `Cargo.toml` and `crates/*/Cargo.toml` from `0.8.0` to `0.9.0`. Validate `CHANGELOG.md` entry exists (use the project's `release-version` skill if available).
- [x] T092 Final audit — run `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`, `cargo fmt --check`, `cargo bench --workspace -- --quick` — record the green run in the PR description before merging to `main`.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1 (Setup)**: No dependencies — can start immediately.
- **Phase 2 (Foundational)**: Depends on Phase 1. **Blocks all user stories.**
- **Phase 3 (US1 — MVP)**: Depends on Phase 2.
- **Phase 4 (US2)**: Depends on US1's `SniRoutingTable` skeleton (T037, T052..T053 layer cleanly on top).
- **Phase 5 (US3)**: Depends on US1 (T060..T062 extend the same lookup path).
- **Phase 6 (US4)**: Depends on US1's `PortGroupManager` (T067 verifies the Legacy arm; can run in parallel with US3 once US1 is done).
- **Phase 7 (US5)**: Depends on US1..US4 functional (counters need real hits to assert).
- **Phase 8 (Polish)**: Depends on all desired user stories complete.

### User Story Dependencies (within Phase 2 done)

- US1 → blocks US2, US3, US4 (they extend US1's modules)
- US2, US3 are independent of each other (different table slots)
- US4 verifies byte-stability of the legacy code path that US1 already touched
- US5 depends on US1..US4 because counters are built from real traffic

### Parallel Opportunities

- All `[P]` Setup tasks (T001..T006) — different files, no dependencies.
- All `[P]` Foundational tests (T007..T012) — different test files.
- Within US1 tests (T032..T035) and US1 implementation parallel parts (T036..T039) — different files.
- US2..US5 can be staffed in parallel once US1 lands.
- All `[P]` Polish tasks (T083..T089) parallelise.

---

## Parallel Example: User Story 1

```bash
# Tests (write together, all FAIL initially):
Task: T032 — sni/client_hello.rs::tests::parse_tls12_extracts_sni
Task: T033 — sni/client_hello.rs::tests::parse_truncated_then_complete
Task: T034 — sni/route_table.rs::tests::exact_beats_fallback
Task: T035 — tests/sni_route_e2e_exact.rs

# Implementation (T036..T039 are different files — parallel):
Task: T036 — sni/client_hello.rs (parser)
Task: T037 — sni/route_table.rs (table skeleton)
Task: T038 — sni/peek.rs (async peek loop)
Task: T039 — forwarder/mod.rs (ClientRule.sni_pattern + control plumbing)
# Then sequentially:
Task: T040 — sni/listener.rs (depends on T037 + T038)
Task: T041 — proxy.rs (preread plumbing)
Task: T042 — port_groups.rs (depends on T040)
Task: T043 — control.rs (wires it together)
Task: T044 — main.rs (CLI flag)
Task: T045 — operator/http.rs (response shape)
Task: T046 — make e2e exact pass
```

---

## Implementation Strategy

### MVP First (US1 only)

1. Phase 1 + Phase 2 → wire + storage + capability gate ready.
2. Phase 3 (US1) → ship the MVP. Tag a pre-release.
3. **STOP and validate**: real `openssl s_client -servername` against a staging environment.

### Incremental Delivery

1. Foundation (Phase 1+2) + US1 → MVP.
2. + US2 (wildcard) → production-ready for multi-tenant subdomain fan-out.
3. + US3 (fallback) → catch-all + diagnostics-friendlier rejection.
4. + US4 (legacy unchanged) → upgrade-in-place safe.
5. + US5 (observability) → operator-friendly.
6. Polish (Phase 8) → web UI, benches, release.

### Parallel Team Strategy

If multiple engineers are available:

1. Whole team finishes Phase 1+2 together (the wire-compat tests in particular benefit from a second pair of eyes).
2. US1 → 1-2 engineers (it touches a lot of crates).
3. After US1: US2 + US3 + US4 split across three engineers; US5 waits on real traffic from the others.
4. Polish phase: parallel by file (web-UI, benches, docs are independent).

---

## Notes

- `[P]` = different files, no dependency on incomplete tasks.
- `[Story]` label maps each task to the spec.md user story it serves.
- Tests MUST fail before implementation per Constitution III.
- Commit after each task or logical group; the project memory at
  `~/.claude/projects/-Users-zingerbee-Documents-forward-rs/memory/` notes
  the user opted in to auto-running the optional `/speckit-git-commit`
  hook so commits land on each speckit step.
- Avoid: vague tasks, same-file conflicts in `[P]` groups, cross-story
  dependencies that break independent testability.
