---
description: "Task list for 003-domain-name-forward"
---

# Tasks: Domain-name forwarding targets

**Input**: Design documents from `/specs/003-domain-name-forward/`
**Prerequisites**: plan.md (required), spec.md (required for user
                  stories), research.md, data-model.md, contracts/

**Tests**: TDD, contract, and integration tests are mandatory per
            Constitution Principle III. Test tasks are interleaved
            with implementation tasks below — write the test first,
            confirm it fails, then implement.

**Organization**: Tasks are grouped by user story so each story can be
                  implemented and validated independently.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2)
- Include exact file paths in descriptions

## Path Conventions

Multi-crate Cargo workspace at repo root: `crates/forward-core`,
`crates/forward-proto`, `crates/forward-auth`, `crates/forward-server`,
`crates/forward-client`, `crates/forward-e2e`. The repo-root
`proto/forward.proto` is the canonical wire schema; `forward-proto`'s
`build.rs` consumes it.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Workspace-level prerequisites for the v0.3.0 work

- [X] T001 Bump workspace version to `0.3.0-dev` in `Cargo.toml` (workspace package metadata) and re-sync each member crate's `version = "0.3.0-dev"` line in `crates/*/Cargo.toml`
- [X] T002 [P] Add `hickory-resolver = "0.24"` (or current stable) with features `["tokio-runtime", "system-config"]` to `crates/forward-client/Cargo.toml`; deny default features to keep static-musl builds clean (no `dns-over-tls` / `dns-over-https` per spec § OOS)
- [X] T003 [P] Add `[Unreleased]` section to `CHANGELOG.md` referencing `003-domain-name-forward`; leave the body empty until T053 fills it post-quickstart

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Wire schema, hostname/Target validation, and persistence
seam — every user story consumes these.

**⚠️ CRITICAL**: No user story work can begin until this phase is
complete (US1 needs the proto field, US2 needs Target classification,
etc.).

- [X] T004 [P] Create RFC 1123 strict-hostname validator + `Hostname` newtype in `crates/forward-core/src/hostname.rs` (R-005, FR-001). Pure function, zero deps; rejects underscores, IDN unicode, SRV-style names, whitespace, label > 63 octets, total > 253 octets, leading/trailing hyphen
- [X] T005 [P] Unit tests for the hostname validator in `crates/forward-core/src/hostname.rs` (`#[cfg(test)] mod tests`): happy cases (`api.example.com`, `single`, trailing-dot FQDN) + sad cases (each rejection rule above) + ASCII case-insensitive normalization
- [X] T006 Create `Target` enum + `Target::parse(&str) -> Result<Target, TargetError>` in `crates/forward-core/src/target.rs` per data-model.md disambiguation order: try IPv4 → bracketed IPv6 → hostname; bare unbracketed IPv6 with port rejected
- [X] T007 [P] Unit tests for `Target::parse` in `crates/forward-core/src/target.rs`: IPv4 literal / bracketed IPv6 / valid hostname / hostname classified before all-numeric / unbracketed IPv6 rejection
- [X] T008 Re-export `Hostname` and `Target` from `crates/forward-core/src/lib.rs` (one-line `pub use`)
- [X] T009 Update `proto/forward.proto`: add `optional bool prefer_ipv6 = 8;` to `message Rule` and `uint64 dns_failures = 6;` to `message RuleStats` per `contracts/forward.proto`
- [X] T010 Contract test `crates/forward-proto/tests/dns_wire_compat.rs` (NEW): construct a `Rule` and a `RuleStats` populated only with v0.2.0 fields; serialize via prost; assert byte-identical encoding before/after the additive proto change (use a hand-rolled v0.2.0-shaped reference vec for the comparison). Then construct one with `prefer_ipv6 = Some(true)` / `dns_failures = 7` and assert the new bytes round-trip cleanly
- [X] T011 Extend `PersistedRule` in `crates/forward-server/src/rules.rs`: add `prefer_ipv6: Option<bool>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`; on load, classify `target_host` via `forward_core::Target::parse` and reject entries whose `target_host` fails parsing (data corruption — distinct from forward-compat unknown serde fields, which proto3/serde already tolerate) with a startup error naming the offending `rule_id`

**Checkpoint**: Foundation ready — proto delta in, hostname seam in,
persistence accepts hostnames. User story implementation can proceed.

---

## Phase 3: User Story 1 - DNS-target rule, traffic flows (Priority: P1) 🎯 MVP

**Goal**: An operator pushes a single rule whose `target_host` is a
DNS name; an end-user TCP connection through the rule reaches the
upstream and round-trips bytes. IP-target rules continue to behave
byte-identically to v0.2.0 (FR-010).

**Independent Test**: With a hosts-file entry mapping `echo.test →
127.0.0.1`, push `8080 → echo.test:41000`, then `ncat 127.0.0.1 8080`
echoes back through the proxy. Same recipe with `127.0.0.1:41000` as
literal target produces byte-identical observable behavior (latency
profile, log shape, persistence shape).

### Tests for User Story 1 ⚠️

> Write these tests FIRST, ensure they FAIL, then implement.

- [ ] T012 [P] [US1] Resolver-trait scaffold test in `crates/forward-client/src/resolver/mod.rs` `#[cfg(test)] mod tests`: an IP-target call to `connect_target(Target::Ip(_), port, _)` MUST short-circuit to direct `TcpStream::connect` (no resolver call) — proven by passing a `MockResolver` whose `resolve()` panics if invoked
- [ ] T013 [P] [US1] Cache happy-path unit test in `crates/forward-client/src/resolver/cache.rs` `#[cfg(test)] mod tests`: cold lookup → resolver invoked once → result stored → hot lookup returns cached addrs without invoking resolver
- [ ] T014 [P] [US1] Server CLI integration test in `crates/forward-server/tests/cli_push_rule.rs` (NEW or extend existing): `push-rule edge-01 8080 echo.test:41000` succeeds; `push-rule edge-01 8080 'foo_bar.example:80'` exits non-zero with `invalid_target_host` reason
- [ ] T015 [P] [US1] e2e smoke skeleton in `crates/forward-e2e/tests/dns_smoke.rs` (NEW): `test_dns_us1_happy_path` writes a temp hosts-file mapping (or uses a localhost mini-resolver — see T024), pushes one DNS-target rule, drives bytes through, asserts round-trip + asserts IP-target rule on a second port still works byte-identically

### Implementation for User Story 1

- [ ] T016 [P] [US1] Define `Resolver` trait + `ResolverError` types + `Target` re-import in `crates/forward-client/src/resolver/mod.rs` per R-006 signature; trait is `async_trait`-free (use AFIT given Rust 1.88) or `#[async_trait]` if needed for object safety
- [ ] T017 [P] [US1] Define `ResolverConfig` defaults struct in `crates/forward-client/src/resolver/mod.rs`: cache_floor=5s, cache_ceiling=5min, stale_while_error_grace=30s, attempt_timeout=3s, negative_cache_retry=3s, max_concurrent_resolves=64. Doc-comment that all values are spec-fixed in v0.3.0 (no operator/server tunability wired in this feature) and that `stale_while_error_grace` is a fixed spec budget per FR-005 — not a runtime knob even when future work exposes the cache floor/ceiling
- [ ] T018 [US1] Build cache module in `crates/forward-client/src/resolver/cache.rs`: `Arc<Mutex<HashMap<Hostname, CacheEntry>>>`; only `Pending` and `Resolved` variants in this phase (StaleAfterFailedRefresh + Failed land in US2). Provide `get_or_resolve(name, &impl Resolve) -> Result<Vec<IpAddr>, ResolverError>` (depends on T016)
- [ ] T019 [US1] Implement `LiveResolver` in `crates/forward-client/src/resolver/mod.rs`: hickory-backed `Resolve` impl reading `/etc/resolv.conf` via `system-config`; for `Target::Ip` short-circuit; for `Target::Dns` call `cache.get_or_resolve(name)` then sequential dial with `attempt_timeout` per address. Family preference is hard-coded IPv4-first here; US3 adds the flip (depends on T016, T017, T018)
- [ ] T020 [US1] Wire `LiveResolver` into the proxy hot path in `crates/forward-client/src/forwarder/proxy.rs`: replace the `format!("{host}:{port}") + TcpStream::connect` line with a `self.resolver.connect_target(target, port, prefer_ipv6=false)` call. Forwarder owns one `Arc<LiveResolver>` (constructed in `crates/forward-client/src/main.rs` at startup) shared across all rules. `ClientRule` grows a `target: Target` field populated from the proto `target_host` string at rule-receive time in `crates/forward-client/src/control.rs` (depends on T019)
- [ ] T021 [US1] Server-side `target_host` validation seam: in `crates/forward-server/src/operator/rule_cli.rs` and `crates/forward-server/src/operator/http.rs`, route `target_host` through `forward_core::Target::parse` and surface `OperatorError::InvalidTargetHost { code, message }` per `contracts/operator-api.md` (depends on T011)
- [ ] T021a [P] [US1] Port-range × DNS coalescing test in `crates/forward-client/src/forwarder/mod.rs` (or dedicated `tests/range_dns.rs`): construct a `ClientRule` with a 4-port range (e.g. `8080-8083 → echo.test:41000-41003`) backed by a `MockResolver`, drive 4 concurrent end-user connections — one per listen port — and assert `MockResolver::resolve()` is invoked **exactly once** for `echo.test`. Proves FR-011 ("port-range rules with DNS targets share one resolution per range") holds via the existing `Hostname`-keyed cache (depends on T018, T020)

**Checkpoint**: US1 fully functional — DNS targets work end-to-end,
IP targets unchanged. Run T015 to validate.

---

## Phase 4: User Story 2 - Graceful failure + recovery (Priority: P2)

**Goal**: When DNS for a rule's target fails (NXDOMAIN, SERVFAIL,
timeout, …), the rule stays Active, end-user connections fail fast
with a structured reason within 3 s, and recovery happens
automatically when DNS comes back. Cached answers serve traffic
during transient resolver outages (stale-while-error grace, FR-005).

**Independent Test**: Drive one connection to prime the cache, then
break the resolver; assert subsequent connections fail with
`dns_resolution_failed` within 3 s while the rule's status remains
Active. Restore the resolver; assert next connection succeeds
without operator action.

### Tests for User Story 2 ⚠️

- [ ] T022 [P] [US2] `MockResolver` test fixture in `crates/forward-client/src/resolver/tests/mock.rs` (NEW) implementing the `Resolve` trait: returns canned answers, can be configured to fail with each `ResolveFailReason` variant, accepts a `MockClock` for time advancement
- [ ] T023 [P] [US2] Single-flight unit test in `crates/forward-client/src/resolver/cache.rs`: spawn N concurrent `get_or_resolve(name)` tasks during a slow `Pending` state, assert exactly one resolver call happens (FR-012). Uses MockResolver + MockClock
- [ ] T024 [P] [US2] TTL clamp unit test in `crates/forward-client/src/resolver/cache.rs`: assert TTL=0 from resolver clamps to 5 s; TTL=24h clamps to 5 min; TTL=30 s passes through (FR-003)
- [ ] T025 [P] [US2] Stale-while-error unit test in `crates/forward-client/src/resolver/cache.rs`: prime cache (success), advance clock past TTL, configure resolver to fail on refresh, assert subsequent lookups in next 30 s return stale addrs AND record the failure for counter bumps; assert lookup at TTL+30s+1ms returns ResolverError (FR-005)
- [ ] T026 [P] [US2] Failed-state retry-backoff unit test in `crates/forward-client/src/resolver/cache.rs`: after grace expiry, assert `negative_cache_retry` window prevents back-to-back resolver calls
- [ ] T027 [P] [US2] e2e test `test_dns_us2_failure_and_recovery` in `crates/forward-e2e/tests/dns_smoke.rs`: prime cache via T015 setup; rewrite hosts file to remove the entry; assert connect fails within 3 s and `list-rules` still shows status `Active`; restore hosts entry; assert next connect succeeds

### Implementation for User Story 2

- [ ] T028 [US2] Inject a `Clock` trait (`now() -> Instant`) into `crates/forward-client/src/resolver/cache.rs` so tests can advance time without sleeping; production impl wraps `tokio::time::Instant::now`
- [ ] T029 [US2] Extend `CacheEntry` in `crates/forward-client/src/resolver/cache.rs`: add `StaleAfterFailedRefresh { stale_addrs, fail_grace_until }` and `Failed { retry_after, last_reason }` variants per data-model state machine
- [ ] T030 [US2] Implement single-flight in `crates/forward-client/src/resolver/cache.rs`: when an in-flight Pending exists, concurrent waiters await its `Arc<Notify>` instead of spawning new resolver calls (FR-012)
- [ ] T031 [US2] Apply TTL clamp `[ResolverConfig::cache_floor, cache_ceiling]` to resolver-reported TTL when transitioning `Pending → Resolved` in `crates/forward-client/src/resolver/cache.rs` (FR-003)
- [ ] T032 [US2] Implement stale-while-error grace in `crates/forward-client/src/resolver/cache.rs`: when refresh attempt for an expired-but-cached name fails, transition to `StaleAfterFailedRefresh` and serve `stale_addrs` until `fail_grace_until` then transition to `Failed` (FR-005)
- [ ] T033 [US2] Define `ResolveFailReason` enum + classifier in `crates/forward-client/src/resolver/mod.rs`: `NxDomain`, `ServFail`, `AttemptTimeout`, `AllAddrsUnreachable`, `Other`. Classify hickory's error variants into these (depends on T029, T030, T032)
- [ ] T033a [P] [US2] Multi-A dial-fallback unit test in `crates/forward-client/src/resolver/mod.rs` `#[cfg(test)] mod tests`: MockResolver returns two A records where the first points at a closed/RST-ing port (bind+drop a TcpListener; or use `127.0.0.1:1` which RSTs fast on Linux/macOS) and the second points at a live echo. Assert `connect_target` walks the list and the resulting connection succeeds against the second address (FR-006 + spec § Edge Cases L204-209). Pair with a "both addresses fail" variant that asserts `dns_resolution_failed` after both attempts time out (depends on T033)
- [ ] T034 [US2] On `dns_resolution_failed` from the resolver layer in `crates/forward-client/src/forwarder/proxy.rs`: refuse the end-user connection (close inbound socket; do NOT half-open the proxy), and emit a structured log event `rule.dns_failed` with `{rule_id, hostname, reason}` (depends on T033)
- [ ] T035 [US2] Audit-grade resolution-success log: in `crates/forward-client/src/resolver/mod.rs` emit one `rule.dns_resolved` event per successful resolution (NOT per cache hit) with `{rule_id, hostname, chosen_addr, ttl_applied}` at INFO level — must NOT log resolved addresses on every connection (Constitution IV / R-008)

**Checkpoint**: US2 done — flaky DNS no longer brings rules down; e2e
test T027 passes.

---

## Phase 5: User Story 3 - Per-rule IPv6 opt-in (Priority: P3)

**Goal**: An operator can flip a single rule from IPv4-first (default)
to IPv6-first via `--prefer-ipv6` on `push-rule` or `prefer_ipv6:
true` on the HTTP body. Other rules pointed at the same hostname
remain unaffected.

**Independent Test**: Add a dual-stack hosts entry; push two rules to
the same hostname (one default, one `--prefer-ipv6`); inspect logs
to confirm the first dials the IPv4 address, the second the IPv6.

### Tests for User Story 3 ⚠️

- [ ] T036 [P] [US3] Family-ordering unit test in `crates/forward-client/src/resolver/mod.rs`: MockResolver returns mixed A+AAAA; `connect_target(_, _, prefer_ipv6=false)` dials the A first; with `prefer_ipv6=true` dials the AAAA first; only-A dataset works under both flags; only-AAAA dataset works under both flags (FR-007 acceptance scenarios)
- [ ] T037 [P] [US3] HTTP round-trip test in `crates/forward-server/tests/http_push_rule.rs` (NEW or extend): POST `{ "prefer_ipv6": true }` returns the field in the response body; GET `/v1/rules` lists it; absent in body decodes as default `false`
- [ ] T038 [P] [US3] e2e test `test_dns_us3_ipv6_optin` in `crates/forward-e2e/tests/dns_smoke.rs`: dual-stack hosts mapping; two rules to same hostname (one default, one `--prefer-ipv6`); for each rule open an end-user connection and parse the structured `rule.dns_resolved.chosen_addr` log line to confirm v4 vs v6 family; cover the "only-A available + prefer_ipv6=true falls back to A" case from US3 acceptance scenario 3

### Implementation for User Story 3

- [ ] T039 [US3] `ClientRule` in `crates/forward-client/src/forwarder/mod.rs` grows `prefer_ipv6: bool` populated from the proto `Option<bool>` in `crates/forward-client/src/control.rs` (default `false` when absent)
- [ ] T040 [US3] `LiveResolver` family-preference logic in `crates/forward-client/src/resolver/mod.rs`: split addrs into A-list + AAAA-list, concatenate preferred-first per `prefer_ipv6` (R-003); pass per-rule `prefer_ipv6` from proxy through `connect_target` (depends on T039)
- [ ] T041 [P] [US3] Server CLI: add `--prefer-ipv6` boolean flag to `push-rule` in `crates/forward-server/src/operator/cli.rs` and route it through to the rule struct in `crates/forward-server/src/operator/rule_cli.rs`
- [ ] T042 [P] [US3] Server HTTP: `PushRuleBody` accepts optional `prefer_ipv6: Option<bool>` in `crates/forward-server/src/operator/http.rs`; response body always echoes the field (per `contracts/operator-api.md`); `list-rules` JSON includes it; `--wide` text mode adds a column

**Checkpoint**: US3 done — IPv6 opt-in works end-to-end with per-rule
isolation; T038 passes.

---

## Phase 6: User Story 4 - Per-rule DNS-failure observability (Priority: P4)

**Goal**: Operators see DNS-failure rate per rule on the metrics
dashboard (`forward_rule_dns_failures_total{client,rule}`) and on
`rule-stats` / HTTP `GET /v1/rules/{id}/stats`. Cardinality stays at
one row per rule (SC-006).

**Independent Test**: Push N rules with deliberately broken DNS,
drive K connections through each, scrape `/metrics`, assert exactly
N rows and per-row counts equal K.

### Tests for User Story 4 ⚠️

- [ ] T043 [P] [US4] Counter-increment unit test in `crates/forward-client/src/resolver/mod.rs`: drive M end-user connections through a name that always NXDOMAINs, assert the per-rule `dns_failures` accumulator equals M; drive M connections during a stale-while-error window where refresh fails, assert the accumulator also gains M (FR-005 + FR-008 increment rule)
- [ ] T044 [P] [US4] Cardinality unit test in `crates/forward-server/src/metrics.rs` `#[cfg(test)] mod tests`: register the new collector, simulate StatsReports for N rules, scrape the registry text format, `grep ^forward_rule_dns_failures_total | wc -l` equals N (SC-006)
- [ ] T045 [P] [US4] e2e test `test_dns_us4_metric_cardinality` in `crates/forward-e2e/tests/dns_smoke.rs`: N broken DNS rules, K connections each, wait one StatsReport tick (5 s default), curl `/metrics`, assert exactly N rows and `value(rule=i) == K_i`

### Implementation for User Story 4

- [ ] T046 [US4] Add `dns_failures: AtomicU64` to per-rule stats in `crates/forward-client/src/forwarder/stats.rs` `RuleStats`; `inc_dns_failure(&self)` helper
- [ ] T047 [US4] Resolver layer bumps the counter in `crates/forward-client/src/resolver/mod.rs`: increment on (a) every connection that ultimately reports `dns_resolution_failed`, AND (b) every cache-hit that succeeds via `StaleAfterFailedRefresh` (because the underlying refresh attempt failed — FR-005). The Resolver gets a `&Arc<RuleStats>` reference passed in by the proxy at `connect_target` call time (depends on T046)
- [ ] T048 [US4] Carry `dns_failures` on the existing `StatsReport` tick: extend the per-rule snapshot serialization in `crates/forward-client/src/forwarder/stats.rs` to populate the new proto field 6 (depends on T009, T046)
- [ ] T049 [US4] Server: register `forward_rule_dns_failures_total` `IntCounterVec` in `crates/forward-server/src/metrics.rs` with labels `["client", "rule"]`; expose a `record_dns_failures(&self, client, rule, delta)` helper
- [ ] T050 [US4] Server: accumulate per-rule `dns_failures` from incoming `StatsReport` in `crates/forward-server/src/grpc/service.rs` (where v0.2.0 already accumulates `bytes_in/bytes_out`), and update the per-rule stats cache in `crates/forward-server/src/operator/per_port_stats.rs` (or wherever the rule-stats snapshot lives — verify path) (depends on T049)
- [ ] T051 [US4] Operator HTTP `GET /v1/rules/{id}/stats` returns `dns_failures` field in the body in `crates/forward-server/src/operator/http.rs` (depends on T050)
- [ ] T052 [P] [US4] Operator CLI `rule-stats <id>` prints `dns_failures` row in `crates/forward-server/src/operator/rule_cli.rs` (text mode + `--json` mode) (depends on T051)

**Checkpoint**: US4 done — operators see DNS failures on dashboards
without grepping logs; T045 passes.

---

## Phase 7: Polish & Cross-Cutting Concerns

**Purpose**: Performance gates, docs, and end-to-end quickstart
verification.

- [ ] T053 [P] Add criterion bench `dns_resolver_cache_hit` in `crates/forward-client/benches/dns_resolver.rs` (NEW): single LiveResolver, prime cache, measure `connect_target(Target::Dns(_), _, _)` median when answer is cached. Must show ≪ network connect cost (SC-004)
- [ ] T054 [P] Add criterion bench `dns_resolver_singleflight` in `crates/forward-client/benches/dns_resolver.rs`: 100 concurrent first-connect attempts to a slow MockResolver; assert resolver call count == 1 (proves FR-012); measure median per-task latency overhead. Doc-comment that SC-005 ("≤1 query per rule per cache window across 100 mixed rules") follows by composition: per-rule single-flight (this bench) × per-rule cache lifetime (T013/T024) — no separate fleet-scale bench is run because the bound is structural, not statistical
- [ ] T054a [P] Re-run the existing `cargo bench --bench data_plane` from v0.2.0 against the v0.3.0 binary; record before/after p99 + throughput in the PR description and assert no >5% regression (Constitution II hot-path gate). The proxy hot path itself is unchanged for IP-target rules and adds only one cache-hit lookup for warm-cache DNS rules; this bench confirms it
- [ ] T055 [P] Update `README.md` with a v0.3.0 DNS-target push example block (matches `quickstart.md` § "Walkthrough" step 4)
- [ ] T056 [P] Update `docs/runbook.md`: remove "no domain forwarding" from the Limitations section if present; add a "Domain-name forwarding (v0.3.0+)" subsection covering the 30 s stale-while-error and 5 min cache-ceiling operator expectations
- [ ] T057 [P] Update `deploy/server.toml.example` and the systemd units in `deploy/systemd/` if any new server config keys land for resolver tunables (currently none — defaults from `ResolverConfig` are baked in; this task is to confirm that and add a `# Reserved for future resolver tunables` comment if useful)
- [ ] T058 Run `quickstart.md` walkthrough end-to-end on a Linux host pair (or single-host loopback per the v0.2.0 SC-001 recipe pattern). Capture wall-clock numbers for SC-001 (push → first byte), SC-002 (DNS-change propagation within ceiling), SC-006 (one row per rule). Record measurements in `CHANGELOG.md` `### Verified` block under `[Unreleased]`
- [ ] T059 Final `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --release` — all green before opening PR

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately
- **Foundational (Phase 2)**: Depends on Setup; **BLOCKS** all user stories
- **US1, US2, US3, US4 (Phases 3–6)**: Each depends on Foundational; in
  practice they have light cross-story coupling:
  - US2 depends on US1's resolver scaffold (T016–T019) being in place
    before adding state-machine variants (T029, T032).
  - US3's family-preference (T040) refines the same `LiveResolver`
    that US1 built; safe to land after US1 / in parallel with US2.
  - US4's `RuleStats.dns_failures` is consumed by US2's resolver
    failure path (T047 references T046); US4 should land after the
    resolver state machine is solid, but its server-side wiring
    (T049–T052) is independent of the client-side state machine.
- **Polish (Phase 7)**: Depends on all desired user stories being
  complete (benches in T053/T054 measure code shipped in US1/US2)

### User Story Dependencies

- **US1 (P1)**: Strict prereq for US2's state machine and US3's
  preference flag (both extend the resolver US1 builds). Independent
  of US4 in code shape.
- **US2 (P2)**: Builds on US1's `Resolver` trait and `cache.rs`
  scaffold. Independent of US3 (family preference is orthogonal to
  failure handling) and US4 (counter wiring lives in stats, not
  cache).
- **US3 (P3)**: Builds on US1's `LiveResolver`. Independent of US2
  and US4.
- **US4 (P4)**: Hooks into US2's failure-classifier path (T047 needs
  T033). Server-side wiring (T049–T052) independent of all client-side
  US2/US3 work — can land in parallel.

### Within Each User Story

- Tests written first (each `T0XX [P] [USx]` test task precedes its
  matching implementation task in this file's order)
- Models / types before services (Resolver trait + ResolverConfig
  before LiveResolver; CacheEntry variants before single-flight loop)
- Internal logic before external surfaces (resolver before proxy
  wire-up; client-side counter before server-side metric)

### Parallel Opportunities

- Phase 1: T002 + T003 in parallel (different files, no deps)
- Phase 2: T004 + T005 + T009 + T010 in parallel; T006 + T007 in
  parallel after T004 (Target depends on Hostname)
- Phase 3: T012/T013/T014/T015 (tests) all parallel; T016/T017
  parallel; T018 → T019 → T020 sequential within client-side; T021
  parallel with the client-side chain; T021a runs after T020 (proves
  FR-011 port-range × DNS coalescing)
- Phase 4: T022–T027 (tests) all parallel; T028 → T029 → T030/T031/T032
  sequential within cache; T033/T034/T035 parallel after the cache
  state-machine lands; T033a (FR-006 multi-A dial fallback) runs after
  T033
- Phase 5: T036/T037/T038 (tests) parallel; T039 → T040 sequential;
  T041/T042 parallel with the client-side chain
- Phase 6: T043/T044/T045 (tests) parallel; T046 → T047 → T048
  sequential client-side; T049 → T050 → T051 sequential server-side;
  T052 parallel after T051
- Phase 7: T053/T054/T054a/T055/T056/T057 all parallel; T058 → T059
  sequential at the end

### Parallel Example: User Story 1 tests

```bash
# Launch all US1 tests in parallel:
Task: "Resolver-trait scaffold test in crates/forward-client/src/resolver/mod.rs"          # T012
Task: "Cache happy-path unit test in crates/forward-client/src/resolver/cache.rs"          # T013
Task: "Server CLI integration test in crates/forward-server/tests/cli_push_rule.rs"        # T014
Task: "e2e smoke skeleton in crates/forward-e2e/tests/dns_smoke.rs"                        # T015
```

---

## Implementation Strategy

### MVP First (US1 only)

1. Phase 1 (Setup) → 3 tasks
2. Phase 2 (Foundational) → 8 tasks
3. Phase 3 (US1) → 10 tasks (4 tests + 6 impl)
4. **STOP and VALIDATE**: drive `quickstart.md` step 4–5 against a
   real binary; assert hostname target proxies traffic
5. Optionally tag a `v0.3.0-rc.1` for early operator feedback

### Incremental Delivery

1. MVP (US1) → demo: "rules now point at hostnames"
2. + US2 → demo: "DNS failures no longer break rules"
3. + US3 → demo: "operators can stage IPv6 per rule"
4. + US4 → demo: "DNS failures visible on the dashboard"
5. Polish + quickstart-verify → tag `v0.3.0`, ship release notes

### Suggested MVP scope

US1 alone is shippable. Without US2, DNS failures cause the same
hang as you'd see resolving manually — but rules still work and the
operator surface is unchanged. US2/US3/US4 are independently valuable
follow-ups.

---

## Notes

- [P] = different files, no incomplete-task deps. Task IDs are in
  total execution order, but [P] groups inside a phase can be
  worked in any order or in parallel.
- Test tasks come BEFORE their matching implementation tasks per
  Constitution III. Verify the test fails before writing the
  implementation it covers.
- Commit after each task or each `[P]` group, with a message that
  mentions the task ID for traceability.
- Stop at any "Checkpoint" line to validate the just-completed
  story end-to-end.
