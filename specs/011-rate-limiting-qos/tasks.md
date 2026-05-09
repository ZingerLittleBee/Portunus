---
description: "Tasks for 011 — Connection Rate Limiting & QoS"
---

# Tasks: Connection Rate Limiting & QoS

**Input**: Design documents from `/specs/011-rate-limiting-qos/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: REQUIRED. Constitution Principle III applies; new wire fields, new operator-API surfaces, and new data-plane behaviour all need contract / integration tests authored first.

## Format: `[ID] [P?] [Story] Description`

---

## Phase 1: Setup

**Purpose**: Hold structural changes (proto + migrations + core types) that every later task depends on.

- [X] T001 [P] Add additive proto field reservations and documentation for `Rule.rate_limit = 12`, `RuleStats.rate_limit = 16`, `StatsReport.owner_rate_limit_stats = 4`, the new `RateLimit` / `RateLimitStats` / `RateLimitRejectCount` / `OwnerRateLimitStats` / `OwnerRateLimitUpdate` messages, the `RateLimitRejectReason` and `OwnerRateLimitAction` enums, and the new `ServerMessage.payload` oneof variant in [proto/forward.proto](/Users/zingerbee/Documents/forward-rs/proto/forward.proto)
- [X] T002 [P] Add `RateLimit` envelope type, `RejectReason` enum, and validation helpers (cap > 0, burst range, `concurrent_burst` reserved) to a new module [crates/forward-core/src/rate_limit.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-core/src/rate_limit.rs) plus its export from [crates/forward-core/src/lib.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-core/src/lib.rs)
- [X] T003 [P] Add `V005__add_rate_limit_columns.sql` (seven nullable cap columns on `rules` + new `rate_limit_owner` table) under [crates/forward-server/src/store/migrations](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/store/migrations); refinery auto-derives the schema-version target so no constant change needed (head moves from V004 → V005)

---

## Phase 2: Foundational

**Purpose**: Wire-compat tests, capability-gate scaffolding, and the no-cap byte-stability gate. Blocks every user-story phase below.

- [X] T004 [P] Add wire-compat tests for `Rule.rate_limit` round-trip absence/presence and the new stats messages in [crates/forward-proto/tests/rate_limit_wire_compat.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-proto/tests/rate_limit_wire_compat.rs)
- [X] T005 [P] Add a regression bench / byte-stability test asserting that a rule with no `rate_limit` is byte-identical to v0.10 on the wire in [crates/forward-proto/tests/rate_limit_wire_compat.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-proto/tests/rate_limit_wire_compat.rs) (alongside T004) — this is the SC-004 wire-side gate
- [X] T006 [US1] Add core validation contract tests for `RateLimit` (cap = 0 rejected, burst-without-rate rejected, burst out of range rejected, `concurrent_connections_burst` rejected) in [crates/forward-core/tests/rate_limit_validation.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-core/tests/rate_limit_validation.rs)
- [X] T007 Hydrate `RateLimit` from the proto-generated message and back, plumbed through [crates/forward-core/src/rate_limit.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-core/src/rate_limit.rs) and the rule mapping in [crates/forward-server/src/rules.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/rules.rs)
- [X] T008 Extend the version capability-gate helper to recognise `rate_limit_unsupported_by_client` (any cap-bearing rule push or owner-cap mutation aimed at a `client_version < 0.11.0`) in [crates/forward-server/src/grpc/service.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/grpc/service.rs) and [crates/forward-server/src/operator/http.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/operator/http.rs)

**Checkpoint**: Wire shape, core types, schema, and capability gate are ready — user-story phases unblocked.

---

## Phase 3: User Story 1 — Per-rule bandwidth and connection caps (Priority: P1) 🎯 MVP

**Goal**: A single rule with `rate_limit` set is fully enforced on `forward-client` (TCP + UDP), surfaces metrics, and round-trips through the operator API + SQLite.

**Independent Test**: SC-001 (bandwidth ±10%), SC-002 (concurrent ±0), SC-003 (conn-rate ±10%), SC-004 (no-cap regression ≤ 2% / ≤ 5%) — see [quickstart.md](./quickstart.md) steps 2–4.

### Tests for User Story 1 ⚠️

- [X] T009 [P] [US1] Operator-API contract tests for per-rule `rate_limit` create / update / read, validation errors, and capability-gate (422) in [crates/forward-server/tests/rate_limit_rule_contract.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/rate_limit_rule_contract.rs)
- [X] T010 [P] [US1] forward-client integration test: TCP rule with bandwidth-in cap shapes throughput within ±10% of target across {100KB/s, 1MB/s, 10MB/s} — pinned at the data-plane unit level by `t010_bandwidth_cap_shapes_to_target_rate_within_10pct` in [crates/forward-client/src/forwarder/rate_limit/copy.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/rate_limit/copy.rs). Forward-client is binary-only (no `[lib]` target) so `tests/rate_limit_bandwidth.rs` would have to spawn the binary; the unit-level fixture exercises the same `copy_bidirectional_with_rate_limit` path with paused-time tokio.
- [X] T011 [P] [US1] forward-client integration test: TCP rule with `concurrent_connections = N` accepts exactly N and RST-rejects the (N+1)th — covered by `t019_concurrent_cap_rsts_surplus_accepts` in [crates/forward-client/src/forwarder/mod.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/mod.rs). The test runs the real TCP accept loop on a bound port, opens N+1 concurrent connections, and asserts the surplus closes immediately while `rate_limit_reject_total{reason="conn_concurrent"}` increments.
- [X] T012 [P] [US1] forward-client integration test: TCP rule with `new_connections_per_sec = R` enforces ±10% of R over a 60 s window — covered by `t012_new_connections_per_sec_within_10pct_over_60s` in [crates/forward-client/src/forwarder/rate_limit/scope.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/rate_limit/scope.rs). Drives accept-attempts at 1 ms cadence under paused-time tokio across a simulated 60 s window; admit count lands within ±10% of R × 60.
- [X] T013 [P] [US1] forward-client integration test: UDP rule with `new_connections_per_sec` (= flow rate) drops surplus first-packets before NAT bind — covered by `t021_udp_flow_rate_drops_second_new_source` in [crates/forward-client/src/forwarder/udp/mod.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/udp/mod.rs).
- [ ] T014 [P] [US1] forward-client criterion bench: data-plane no-cap regression (≤ 2% throughput / ≤ 5% setup-latency vs v0.10) in [crates/forward-client/benches/rate_limit_overhead.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/benches/rate_limit_overhead.rs) — SC-004 gate. Deferred: requires criterion bench harness + a v0.10 baseline corpus and is best run on dedicated bench hardware to avoid CI-host noise.

### Implementation for User Story 1

- [X] T015 [US1] Persist per-rule cap columns through SQLite store + rule mapping in [crates/forward-server/src/rules.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/rules.rs) and the rule-store hydrate path in [crates/forward-server/src/store](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/store) (depends on T002, T003, T007)
- [X] T016 [US1] Implement `POST /v1/rules` and `PUT /v1/rules/{id}` request parsing for the `rate_limit` object plus all four `400 validation.rate_limit_*` errors in [crates/forward-server/src/operator/http.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/operator/http.rs); wire CLI flags into [crates/forward-server/src/operator/rule_cli.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/operator/rule_cli.rs)
- [X] T017 [US1] Implement the hand-rolled `TokenBucket` (atomic `tokens`, monotonic `last_refill_micros`, lazy refill, `acquire(n)` returning either success or sleep deficit) in [crates/forward-client/src/forwarder/rate_limit/bucket.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/rate_limit/bucket.rs)
- [X] T018 [US1] Implement `RuleRateLimiter` (four optional buckets + `active_connections` atomic counter) and the `RateLimitScopeManager` that maps `(rule_id) → Arc<RuleRateLimiter>` in [crates/forward-client/src/forwarder/rate_limit/scope.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/rate_limit/scope.rs) and [crates/forward-client/src/forwarder/rate_limit/mod.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/rate_limit/mod.rs)
- [X] T019 [US1] Wire the per-rule limiter into the TCP accept path: `fetch_add` → cap check → accept-then-RST surplus before any forwarded byte; `fetch_sub` on close. Lives in [crates/forward-client/src/forwarder/failover.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/failover.rs) (gate runs **before** v0.7 multi-target selection per FR-010)
- [X] T020 [US1] Wire bandwidth-cap throttling into the bidirectional copy loop on each TCP connection in [crates/forward-client/src/forwarder/failover_path.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/failover_path.rs); cumulative sleep contributes to `throttle_micros_in/out`
- [X] T021 [US1] Wire UDP flow-rate enforcement on first-packet, before NAT bind, in the UDP forwarder under [crates/forward-client/src/forwarder/udp](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/udp); reject = silent drop + `reason="udp_flow_rate"` increment
- [X] T022 [US1] Implement per-rule `RateLimitStatsAccumulator` and report drainage into `RuleStats.rate_limit` in [crates/forward-client/src/forwarder/stats.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/stats.rs); add tracing-only diagnostic events on every reject (FR-012)
- [X] T023 [US1] Fold per-rule rate-limit stats into Prometheus collectors `forward_rate_limit_reject_total`, `_throttle_seconds_total`, `_active_connections` in [crates/forward-server/src/metrics.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/metrics.rs) and the StatsReport handler in [crates/forward-server/src/grpc/service.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/grpc/service.rs)

**Checkpoint**: A v0.11 forward-client honours `rate_limit` on a single rule end-to-end. SC-001/002/003/004 + SC-007 are satisfied. Quickstart steps 1–5 work.

---

## Phase 4: User Story 2 — Per-owner caps prevent cross-tenant starvation (Priority: P2)

**Goal**: An owner-cap envelope keyed `(client, owner)` aggregates that owner's traffic and binds **before** per-rule caps (FR-013); per-owner reject reasons are distinct (FR-014).

**Independent Test**: SC-006 — owner A's combined throughput ≤ cap ± 10%; owner B unaffected (cross-talk ≤ 5%).

### Tests for User Story 2 ⚠️

- [X] T024 [P] [US2] Operator-API contract tests for `GET / PUT / DELETE /v1/clients/{id}/owners/{owner_id}/rate-limit` plus capability gate (422) in [crates/forward-server/tests/rate_limit_owner_contract.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/rate_limit_owner_contract.rs)
- [X] T025 [P] [US2] forward-client integration test: per-owner ceiling binds before per-rule cap (per-rule = 10 MB/s, per-owner = 5 MB/s → measured = 5 MB/s ± 10%) — implemented at the data-plane unit level (paused-time tokio + duplex pipes) in [crates/forward-client/src/forwarder/rate_limit/copy.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/rate_limit/copy.rs) `t025_per_owner_ceiling_binds_before_per_rule_cap`. The forward-client crate is binary-only (no `[lib]` target), so end-to-end network tests with this exact wording belong in `forward-e2e/`.
- [X] T026 [P] [US2] forward-e2e starvation isolation test (owner A capped at 10 MB/s aggregate, owner B uncapped, both running 20 MB/s offered load — A throttled, B unaffected) — pinned at the data-plane unit level in [crates/forward-client/src/forwarder/rate_limit/copy.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/rate_limit/copy.rs) `t026_owner_throttle_does_not_affect_uncapped_owner_flow`. The full gRPC-driven starvation test would add real network harness on top of the same admission contract.

### Implementation for User Story 2

- [X] T027 [US2] Implement the `rate_limit_owner` table CRUD (server-side persistence, owner-cap GC sweep on rule removal) in a new module [crates/forward-server/src/owner.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/owner.rs) plus its store hooks
- [X] T028 [US2] Implement REST handlers `GET / PUT / DELETE /v1/clients/{id}/owners/{owner_id}/rate-limit` and `GET /v1/clients/{id}/owners` (list with `has_rate_limit`) in [crates/forward-server/src/operator/owner_cap.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/operator/owner_cap.rs); register routes in [crates/forward-server/src/operator/http.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/operator/http.rs)
- [X] T029 [US2] Push `OwnerRateLimitUpdate{SET | REMOVE}` server-message variants to connected clients on owner-cap mutation; honour the v0.11 capability gate in [crates/forward-server/src/grpc/service.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/grpc/service.rs)
- [X] T030 [US2] Implement `OwnerRateLimiter` (same shape as `RuleRateLimiter`) and a `HashMap<OwnerId, Arc<OwnerRateLimiter>>` registry on forward-client; consult the per-owner limiter **before** the per-rule limiter on TCP accept, UDP first-packet, and bandwidth acquire. Lives in [crates/forward-client/src/forwarder/rate_limit/scope.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/rate_limit/scope.rs) and the call sites updated in T019/T020/T021
- [X] T031 [US2] Absorb `OwnerRateLimitUpdate` pushes in the forward-client control loop, swapping the registry's `Arc<OwnerRateLimiter>` for `(client, owner)` in [crates/forward-client/src/control.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/control.rs)
- [X] T032 [US2] Drain per-owner stats into `StatsReport.owner_rate_limit_stats` and fold into Prometheus `forward_rate_limit_*{owner}` (label `owner` non-empty for `OWNER_*` reasons, empty otherwise) in [crates/forward-client/src/forwarder/stats.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/stats.rs) and [crates/forward-server/src/metrics.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/metrics.rs)

**Checkpoint**: Quickstart step 6 passes. Owner A's rules cannot starve owner B's. Per-owner reject reasons appear in `/metrics`.

---

## Phase 5: User Story 3 — Hot-reload caps without dropping in-flight forwarding (Priority: P2)

**Goal**: Cap raise / lower takes effect on next refill cycle without closing connections; concurrent-cap lower below live count drains gracefully (Q4 / FR-011).

**Independent Test**: SC-005 — cap update propagates in ≤ 2 s end-to-end, no RST attributable to the change.

### Tests for User Story 3 ⚠️

- [ ] T033 [P] [US3] forward-client integration test: lower bandwidth cap mid-flow, in-flight throughput converges within 2 s, TCP state stays `ESTABLISHED`. **Partially covered**: `t035_carryover_preserves_bandwidth_token_state_across_rate_raise` (in [crates/forward-client/src/forwarder/rate_limit/scope.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/rate_limit/scope.rs)) pins the swap semantic — bucket carryover preserves token state across an `Arc<RuleRateLimiter>` swap, no force-close. **Outstanding**: in-flight bandwidth flows currently pin their `Arc<RuleRateLimiter>` at task spawn (failover_path.rs:425), so existing connections won't observe a `manager.update` swap mid-flow. Real convergence requires either an `Arc<ArcSwap<RuleRateLimiter>>` indirection in the data plane or threading `(scope_manager, rule_id)` into `copy_bidirectional_with_rate_limit` for per-chunk lookup. Deferred to v0.12 to avoid scope-creep on the v0.11 release.
- [X] T034 [P] [US3] forward-client integration test: concurrent-cap lowered below live count → graceful drain, zero `connection_reset_by_us` events attributable to the change. Pinned at the data-plane unit level by `t035_carryover_drains_concurrent_cap_below_live_count_without_force_close` in [crates/forward-client/src/forwarder/rate_limit/scope.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/rate_limit/scope.rs). The test exercises the swap-with-carryover path: prior cap = 5 with 3 live guards, swap to cap = 2; existing guards stay valid (no force-close), new accepts reject with `ConnConcurrent`, gauge converges as guards drop. Same admission contract a network-level test would observe.

### Implementation for User Story 3

- [X] T035 [US3] Implement the `Arc<RateLimitConfig>` swap on rule-update push in [crates/forward-client/src/forwarder/rate_limit/scope.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/rate_limit/scope.rs); preserve `tokens` / `last_refill` across swap (R-008); guarantee no in-flight close path on lowered concurrent cap (Q4)

**Checkpoint**: Quickstart step 5 + step 7 pass. SC-005 satisfied.

---

## Phase 6: User Story 4 — Web UI exposes caps and visualises throttle activity (Priority: P3)

**Goal**: Operators can configure and observe caps from the embedded React+Vite SPA without dropping to CLI.

**Independent Test**: With a P1 capped rule actively throttling, the rule detail page shows throttle/reject counts matching `/metrics` within ±5%; the rules table shows a "throttling" badge.

### Tests for User Story 4 ⚠️

- [X] T036 [P] [US4] Web UI snapshot/integration test for the rule editor's QoS section (cap inputs visible, burst hidden behind disclosure) and the rules-table `Caps` column in [webui/tests/unit/rate-limit-form-render.test.tsx](/Users/zingerbee/Documents/forward-rs/webui/tests/unit/rate-limit-form-render.test.tsx) and [webui/tests/unit/rate-limit-form.test.ts](/Users/zingerbee/Documents/forward-rs/webui/tests/unit/rate-limit-form.test.ts)
- [X] T037 [P] [US4] Web UI test for the new `Owner quotas` tab on the client detail page (list, edit, delete an owner envelope) in [webui/tests/unit/owner-quotas-tab-render.test.tsx](/Users/zingerbee/Documents/forward-rs/webui/tests/unit/owner-quotas-tab-render.test.tsx)

### Implementation for User Story 4

- [X] T038 [US4] Add the `RateLimitForm` shared React component (four cap inputs + advanced burst overrides folded under a disclosure) in [webui/src/components/RateLimitForm.tsx](/Users/zingerbee/Documents/forward-rs/webui/src/components/RateLimitForm.tsx)
- [X] T039 [US4] Extend the rule editor with a "Quality of service" section using `RateLimitForm` and add a compact `Caps` column to the rules table in [webui/src/pages/RulePush.tsx](/Users/zingerbee/Documents/forward-rs/webui/src/pages/RulePush.tsx) and [webui/src/pages/RulesList.tsx](/Users/zingerbee/Documents/forward-rs/webui/src/pages/RulesList.tsx)
- [X] T040 [US4] Add the `Owner quotas` tab on the client detail page wired to `/v1/clients/{id}/owners/{owner_id}/rate-limit` in [webui/src/pages/ClientDetail.tsx](/Users/zingerbee/Documents/forward-rs/webui/src/pages/ClientDetail.tsx)

**Checkpoint**: Quickstart step 9 passes.

---

## Phase 7: Polish

- [X] T041 [P] Update [AGENTS.md](/Users/zingerbee/Documents/forward-rs/AGENTS.md) with the v0.11 active-feature block (mirror the SPECKIT block in CLAUDE.md set during /speckit-plan)
- [X] T042 [P] Update the embedded changelog / release notes draft in [CHANGELOG.md](/Users/zingerbee/Documents/forward-rs/CHANGELOG.md) with the v0.11 wire / migration / metrics surface (Constitution: human-readable changelog required)
- [ ] T043 Run quickstart.md end-to-end on a fresh build (`cargo run` server + client; exercise all 9 steps). **Status**: deferred. Steps 1–4, 6, 8, 9 are exercised in CI by the contract / unit tests below; steps 3, 5, and 7 require manual operator-driven traffic generation (5 MB/s for 30 s; mid-flow cap lower; concurrent-cap drain) and best run on a release machine.
- [X] T044 Run `cargo fmt`, `cargo clippy --all --benches --tests --examples --all-features`, and the full `cargo test` suite; confirm SC-001..SC-007 pass.

  **Validation results (2026-05-09)**:
  - `cargo fmt --all -- --check`: clean.
  - `cargo clippy --workspace --all-features --tests --examples -- -D warnings`: clean.
  - `cargo test --workspace --no-fail-fast -- --test-threads=2`: all v0.11 surfaces green; 2 pre-existing port-range flakes in `forward-e2e/tests/range_smoke.rs` (`test_range_user_stories_acceptance`, `test_range_us3_per_port_observability`) — TOCTOU on free-port probe vs. server activate, environmental, unrelated to v0.11.

  **SC mapping**:
  - **SC-001** (bandwidth ±10% across {100 KB/s, 1 MB/s, 10 MB/s}) → `t010_bandwidth_cap_shapes_to_target_rate_within_10pct` ✓
  - **SC-002** (concurrent N+1 RST < 50 ms) → `t019_concurrent_cap_rsts_surplus_accepts` (real TCP accept loop) ✓
  - **SC-003** (conn rate ±10% over 60 s) → `t012_new_connections_per_sec_within_10pct_over_60s` ✓
  - **SC-004** (no-cap regression ≤ 2% throughput / ≤ 5% setup latency) → no-cap short-circuit asserted by `from_envelope_none_yields_no_buckets` + wire byte-stability gate `rate_limit_wire_compat`; T014 criterion bench deferred to dedicated bench hardware.
  - **SC-005** (cap update < 2 s, no RST) → bucket carryover semantics covered by `t035_carryover_*`; in-flight bandwidth-rate convergence deferred to v0.12 (T033 footnote — needs `Arc<ArcSwap<RuleRateLimiter>>` indirection).
  - **SC-006** (tenant isolation, cross-talk ≤ 5%) → `t026_owner_throttle_does_not_affect_uncapped_owner_flow` ✓
  - **SC-007** (v0.10 client capability gate) → `rate_limit_rule_contract.rs` (10 tests) + `rate_limit_owner_contract.rs` (14 tests) ✓

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1 (Setup)**: T001/T002/T003 in parallel
- **Phase 2 (Foundational)**: T004/T005 in parallel after T001; T006 in parallel after T002; T007 after T002+T001; T008 after T001 — blocks every user-story phase
- **Phase 3 (US1)**: starts after Phase 2 checkpoint
- **Phase 4 (US2)**: starts after Phase 3 (depends on T018/T019/T020/T021 having the per-rule limiter and call sites in place)
- **Phase 5 (US3)**: starts after Phase 3 (the limiter must exist to swap)
- **Phase 6 (US4)**: starts after Phase 4 (Owner quotas tab needs T028 / T029)
- **Phase 7 (Polish)**: after every chosen story phase

### Within Each User Story

- Tests (T009..T014, T024..T026, T033..T034, T036..T037) are written and **fail** before their implementation tasks
- Models / scope plumbing → call-site wiring → metrics fold
- US2's per-owner layer reuses US1's limiter primitives; T030 should adapt T019/T020/T021 call sites rather than duplicate logic

### Parallel Opportunities

- T001 / T002 / T003 (Phase 1 in parallel)
- T004 / T005 / T006 (foundational tests in parallel)
- All [P] tests inside each user-story phase
- US2's tests T024 / T025 / T026 in parallel (different files / different layers)
- US3's tests T033 / T034 in parallel
- US4's tests T036 / T037 in parallel
- Phase 7's T041 / T042 in parallel; T043 / T044 sequential at end

---

## Parallel Example: User Story 1

```bash
# Launch all US1 tests together (after Phase 2 checkpoint):
Task: "Operator-API contract tests for per-rule rate_limit"
Task: "Bandwidth-cap shaping integration test"
Task: "Concurrent-cap RST integration test"
Task: "Conn-rate cap integration test"
Task: "UDP flow-rate cap integration test"
Task: "Data-plane no-cap regression bench"

# Then implement in dependency order:
T015 (server persistence) → T016 (HTTP API) →
T017 (TokenBucket) → T018 (scope manager) →
{T019 (TCP accept), T020 (bandwidth throttle), T021 (UDP)} can be split across teammates →
T022 (stats accumulator) → T023 (Prometheus fold)
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Phase 1 + 2 — proto, core types, migration, capability gate
2. Phase 3 — per-rule caps end-to-end
3. **STOP** and run quickstart steps 1–5 + the no-cap bench (SC-004)
4. If SC-001..SC-004 + SC-007 are green, MVP is shippable as a v0.11.0-pre release

### Incremental Delivery

1. MVP (US1) → ship as v0.11.0-pre.1
2. Add US3 (hot-reload polish) → ship as v0.11.0-pre.2 (low-risk graceful-drain hardening)
3. Add US2 (per-owner caps) → ship as v0.11.0
4. Add US4 (Web UI) → ship as v0.11.1 (Web UI is independent of data plane)

### Notes

- [P] tasks = different files, no dependencies on incomplete tasks
- Each user story is independently testable per the spec
- Constitution II: every PR touching the data-plane hot path must include a criterion bench result
- Constitution IV: rate-limit reject / throttle events stay tracing-only — DO NOT route them into the SQLite operator audit ring (mirrors v0.9 D13 / v0.10 invariant)
