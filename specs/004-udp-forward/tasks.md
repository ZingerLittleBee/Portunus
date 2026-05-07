---
description: "Task list for 004-udp-forward"
---

# Tasks: UDP forwarding

**Input**: Design documents from `/specs/004-udp-forward/`
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

**Purpose**: Workspace-level prerequisites for the v0.4.0 work

- [ ] T001 Bump workspace version to `0.4.0-dev` in `Cargo.toml` (workspace package metadata) and re-sync each member crate's `version = "0.4.0-dev"` line in `crates/*/Cargo.toml`
- [ ] T002 [P] Add `[Unreleased]` section to `CHANGELOG.md` referencing `004-udp-forward`; leave the body empty until T069 fills it post-quickstart
- [ ] T003 [P] Confirm no new external dependencies are needed (UDP forwarding uses `tokio::net::UdpSocket` from the existing `tokio` `net` feature; record this finding as a one-line comment in `crates/forward-client/Cargo.toml` near the `tokio` dependency line)

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Wire schema, capability negotiation, and resolver split —
every user story consumes these. The wire deltas land first because
both server-side capability storage (HIGH-1 from review) and the
metric/stats roll-up depend on them.

**⚠️ CRITICAL**: No user story work can begin until this phase is
complete. US1 needs `Protocol::UDP` on the wire AND server-side
capability gating; US2 needs the `resolve_target` API; US4 needs the
`Welcome` tunables.

### Wire schema

- [ ] T004 [P] Update `proto/forward.proto`: add `UDP = 2` to `enum Protocol`; add `repeated Protocol supported_protocols = 3;` to `message Hello`; add `uint32 udp_flow_idle_secs = 3;` and `uint32 udp_max_flows_per_rule = 4;` to `message Welcome`; add `uint64 datagrams_in = 7;`, `uint64 datagrams_out = 8;`, `uint32 active_flows = 9;`, `uint64 flows_dropped_overflow = 10;` to `message RuleStats`; add `uint64 datagrams_in = 4;`, `uint64 datagrams_out = 5;` to `message PerPortStats`. Match the comment vocabulary in `specs/004-udp-forward/contracts/forward.proto` (R-009)
- [ ] T005 Contract test `crates/forward-proto/tests/udp_wire_compat.rs` (NEW): construct a TCP-only `Rule` (no `UDP` value, no UDP fields touched) and a TCP-only `RuleStats` (all UDP fields default-zero) populated only with v0.3.0-shaped data; serialize via prost; assert byte-identical encoding before/after the additive proto change. Then construct a UDP `Rule` (`protocol = UDP`) plus a `RuleStats` with all four UDP fields set and assert the new bytes round-trip cleanly through prost. Then construct a `Hello` with `supported_protocols = [TCP, UDP]` and assert the v0.3.0-shaped Hello (no field 3) round-trips byte-identical via the wire-omit path

### Capability negotiation (HIGH-1)

- [ ] T006 In `crates/forward-server/src/clients.rs`, add `pub supported_protocols: HashSet<forward_proto::v1::Protocol>` field to `ConnectedClient`. Default to `{Protocol::Tcp}` when no Hello has been observed (back-compat for v0.3 clients per `data-model.md` § Capability negotiation step 2). Provide `pub fn supports(&self, p: Protocol) -> bool` accessor used by push validation
- [ ] T007 [P] Unit test in `crates/forward-server/src/clients.rs` `#[cfg(test)] mod tests`: a fresh `ConnectedClient` defaults to `{Tcp}` only; adding UDP to `supported_protocols` makes `supports(Udp)` true; `supports(ProtocolUnspecified)` always false
- [ ] T008 In `crates/forward-server/src/grpc/service.rs::control`, replace the "send Welcome immediately on register" sequence with: (a) await the first inbound `ClientMessage`; (b) if it is a `Hello`, copy `hello.supported_protocols` (mapped from `i32` enum values, dropping `PROTOCOL_UNSPECIFIED`) into the `ConnectedClient` row created by `ClientRegistry::register`; (c) THEN send Welcome populated with `udp_flow_idle_secs` and `udp_max_flows_per_rule` from server config; (d) if the first message is anything other than Hello (e.g. v0.3 client that goes straight to RuleStatus or StatsReport), still register, leave `supported_protocols = {Tcp}`, send Welcome with default tunables, and feed the original message into the existing handler. Update the existing log line `event = "client.connected"` to include `supported_protocols` as a JSON-encodable list
- [ ] T009 [P] Unit test for the Hello-gated capability path in `crates/forward-server/src/grpc/service.rs` `#[cfg(test)] mod tests` (or extend existing service tests): drive a fake inbound stream that sends Hello with `[TCP, UDP]` first; assert `ConnectedClient.supports(Udp)` becomes true. Drive a stream that sends StatsReport first (no Hello); assert capability defaults to `{Tcp}` only. Drive a stream that sends Hello with an unknown enum value (e.g. wire integer 99); assert it is silently dropped from the set, never coerced
- [ ] T010 [P] In `crates/forward-client/src/control.rs`, populate `Hello.supported_protocols` with `[Protocol::Tcp as i32, Protocol::Udp as i32]` on the existing Hello send path. Existing `Hello.protocol_version` and `Hello.client_version` fields are unchanged

### Server config + Welcome plumbing

- [ ] T011 [P] In `crates/forward-server/src/config.rs`, add two new optional `ServerConfig` fields: `udp_flow_idle_secs: Option<u32>` (default 60, validated range `30..=300`) and `udp_max_flows_per_rule: Option<u32>` (default 1024, validated range `1..=65535`). Out-of-range values return a typed `ConfigError::OutOfRange { key, value, range }` with the same shape as the existing `range_rule_max_ports` validation. Re-export both via `ServerConfig` accessors `udp_flow_idle_secs()` / `udp_max_flows_per_rule()` returning the resolved (default-applied) values
- [ ] T012 [P] Unit test in `crates/forward-server/src/config.rs` `#[cfg(test)] mod tests`: defaults are 60 and 1024; out-of-range values rejected (29, 301, 0, 65536); valid edge values (30, 300, 1, 65535) accepted
- [ ] T013 In `crates/forward-server/src/serve.rs` (or wherever Welcome is constructed in T008), pass the resolved `ServerConfig.udp_flow_idle_secs()` / `udp_max_flows_per_rule()` values into the `Welcome` message. Both fields use the wire convention "0 = client uses compile-time default" — but server config defaults are non-zero, so the Welcome always carries explicit positive integers

### Resolver split for UDP reuse (HIGH-2)

- [ ] T014 In `crates/forward-client/src/resolver/mod.rs`, add `pub async fn resolve_target(&self, rule_id: RuleId, target: &Target, port: u16, prefer_ipv6: bool) -> Result<(Vec<SocketAddr>, AnswerSource), ConnectError>` that performs the resolution-and-ordering portion of today's `connect_target` and returns the reachable address candidates without dialing. For `Target::Ip(ip)` this is `Ok((vec![SocketAddr::new(*ip, port)], AnswerSource::Fresh))`. For `Target::Dns` it consults the cache via `LiveResolver::cache.get_or_resolve` and orders the result via the existing `order_by_family` helper. Returns `ConnectError::Resolution` on resolver failure (so UDP path can bump `dns_failures` for the same condition)
- [ ] T015 Refactor `LiveResolver::connect_target` in `crates/forward-client/src/resolver/mod.rs` to consume `resolve_target`: call it, then iterate the returned `Vec<SocketAddr>` with the existing per-attempt timeout + multi-A fallback loop, returning `(TcpStream, AnswerSource)` on the first successful dial or `ConnectError::AllAddrsUnreachable` after exhausting candidates. **Behaviour MUST be byte-identical** to the pre-refactor TCP path — every existing `connect_target` test in this file MUST still pass with no test changes. The refactor is mechanical: extract the resolve step into a method call
- [ ] T016 [P] Unit test in `crates/forward-client/src/resolver/mod.rs` `#[cfg(test)] mod tests`: `resolve_target(Target::Ip(127.0.0.1), 9999, false)` returns exactly `[127.0.0.1:9999]` with `AnswerSource::Fresh` and **does not invoke the resolver** (use a panicking `MockResolver`). `resolve_target(Target::Dns("dual.example"), 9999, prefer_ipv6=false)` against a `MockResolver` returning `[v6, v4]` returns `[v4_addr_with_port, v6_addr_with_port]` (v4-first order). Same with `prefer_ipv6=true` returns `[v6_addr_with_port, v4_addr_with_port]`. These tests cover R-006

### Push-rule capability gating

- [ ] T017 In `crates/forward-server/src/rules.rs`, extend `RuleStore::push_*` validation: after rule construction but before persisting, look up the target client via `ClientRegistry`, and if the requested rule's `protocol` is `UDP` and the client's `supported_protocols.supports(Udp)` is false, return `RuleStoreError::UnsupportedProtocol { client_name, protocol: "udp" }`. The error variant maps to error code `unsupported_protocol`, HTTP status 422, exit code 3 (per `contracts/operator-api.md`)
- [ ] T018 [P] In `crates/forward-server/src/operator/cli.rs`, map the new error code in `code_to_exit`: add `"unsupported_protocol" => 3` to the input-validation arm (FR-011, alongside the v0.3.0 `invalid_target_host*` codes)
- [ ] T019 [P] In `crates/forward-server/src/operator/http.rs`, map `RuleStoreError::UnsupportedProtocol` to HTTP 422 with body `{"error":"unsupported_protocol","message":"client {name} does not support protocol {protocol}"}`. Include the new code in the existing operator-error → status mapping function
- [ ] T020 [P] Integration test in `crates/forward-server/tests/cli_push_rule.rs` (extend existing): `push-rule edge-01 6000 127.0.0.1:9999 --protocol udp` against a registered v0.3-style client (no UDP capability) exits 3 and stderr contains `unsupported_protocol`. Same call against a UDP-capable client succeeds (will be wired up by US1 implementation; this test is initially `#[ignore]` and unignored after T030)

**Checkpoint**: Foundation ready — wire deltas in, capability gating
in, resolver split in. User story implementation can proceed.

---

## Phase 3: User Story 1 - Single-port UDP forwarding (Priority: P1) 🎯 MVP

**Goal**: An operator pushes a single UDP rule with an IP-literal
target. End-user UDP datagrams reach the upstream and replies route
back to the original `(addr, port)`. Two end-users on different
source ports stay isolated.

**Independent Test**: With an upstream UDP echo on `127.0.0.1:9999`,
push `edge-01 6000 127.0.0.1:9999 --protocol udp`. Send a UDP datagram
to `:6000` from source port 50000; assert the same payload arrives
back at source port 50000. Send from source port 50001; assert each
gets only its own reply. (Quickstart steps 3-4.)

### Tests for User Story 1 ⚠️

> Write these tests FIRST, ensure they FAIL, then implement.

- [ ] T021 [P] [US1] Operator CLI test in `crates/forward-server/tests/cli_push_rule.rs`: `push-rule` with `--protocol udp` against a UDP-capable client succeeds and the response body's `protocol` field is `"udp"`. Same form without `--protocol` defaults to `"tcp"` (FR-014, byte-compat with v0.3 push-rule call shape)
- [ ] T022 [P] [US1] FlowTable cap unit test in `crates/forward-client/src/forwarder/udp/table.rs` `#[cfg(test)] mod tests` (NEW): construct a `UdpFlowTable` with `cap = 2`, insert flows for 3 distinct source addresses; assert the third returns the typed `OverflowDropped` outcome and `dropped_overflow` counter advanced by 1; existing two flows untouched (data-model § UdpFlowTable)
- [ ] T023 [P] [US1] FlowTable lookup-or-insert unit test in `crates/forward-client/src/forwarder/udp/table.rs`: same source `(addr, port)` returns the same `Arc<UdpFlow>` instance across calls; different sources get different instances; `len()` reflects the count
- [ ] T024 [P] [US1] UdpListener round-trip integration test in `crates/forward-client/src/forwarder/udp/mod.rs` `#[cfg(test)] mod tests`: bind a real `tokio::net::UdpSocket` as the upstream echo on a loopback port; spawn a `UdpListener` task targeting that port; from a third real `UdpSocket` send a datagram to the listener and assert the same bytes come back to the sender. Use real sockets per Constitution III "no socket mocks"
- [ ] T025 [P] [US1] UdpListener flow isolation integration test in same file as T024: drive two concurrent senders from distinct source `(addr, port)` pairs; assert each receives only its own reply (no cross-routing) — the SC-003 hard-zero invariant
- [ ] T026 [P] [US1] e2e smoke test `crates/forward-e2e/tests/udp_smoke.rs::test_udp_us1_happy_path` (NEW): start a real `forward-server` + `forward-client`, push a UDP rule via HTTP, run a UDP echo on a localhost port, drive a datagram through the proxy, assert byte-identical round-trip. Add a helper `push_rule_http_with_protocol` in `crates/forward-e2e/tests/common/mod.rs` matching the v0.3 helper shape

### Implementation for User Story 1

- [ ] T027 [P] [US1] Define `UdpFlow` struct in `crates/forward-client/src/forwarder/udp/flow.rs` (NEW) with the field set from `data-model.md` § UdpFlow: `source_addr`, `upstream_socket: Arc<UdpSocket>`, `upstream_addr`, `last_seen: Mutex<Instant>` (or `ArcSwap<Instant>`), per-flow byte/datagram `AtomicU64` counters, `cancel: CancellationToken`. Provide `bump_last_seen(&self)` and `bump_inbound(&self, bytes)` / `bump_outbound(&self, bytes)` mutation helpers
- [ ] T028 [US1] Build `UdpFlowTable` in `crates/forward-client/src/forwarder/udp/table.rs` (NEW): `tokio::sync::Mutex<HashMap<SocketAddr, Arc<UdpFlow>>>`, `cap: usize`, `dropped_overflow: AtomicU64`. Provide `lookup_or_insert<F>(source, build: F) -> Result<Arc<UdpFlow>, OverflowDropped>` where `build` is a closure invoked under the mutex only when the entry is absent; if the table is at cap, return `OverflowDropped` and bump the counter. `len()` returns the live entry count for the `active_flows` gauge tick. `evict(source)` cancels the flow and removes it. `drain()` cancels everything and clears the map. Reaper task is added in US4 (T056); for v0.4.0 US1 the table starts without an active reaper (idle eviction is US4 — until then flows accumulate up to the cap and overflow drops keep memory bounded)
- [ ] T029 [P] [US1] Add UDP-specific atomic counters to `crates/forward-client/src/forwarder/stats.rs::RuleStats`: `pub datagrams_in: AtomicU64`, `pub datagrams_out: AtomicU64`, `pub active_flows: AtomicU32`, `pub flows_dropped_overflow: AtomicU64`. Helpers: `inc_datagram_in(&self, bytes: u64)`, `inc_datagram_out(&self, bytes: u64)`, `set_active_flows(&self, n: u32)`, `inc_flow_dropped_overflow(&self)`, plus `snapshot_*` readers for each
- [ ] T030 [US1] Build `UdpListener` in `crates/forward-client/src/forwarder/udp/mod.rs` (NEW): bind a `UdpSocket` on the rule's `listen_port`, allocate one heap `vec![0u8; 65535]` buffer, loop `socket.recv_from(&mut buf)` → `flow_table.lookup_or_insert(source, || build_flow(source))`. `build_flow` resolves the IP target as `SocketAddr::new(target_ip, target_port)` (US1 is IP-only), `bind(0)` an upstream `UdpSocket`, spawn a per-flow reply task that loops `upstream.recv_from(&mut reply_buf)` → `listener.send_to(reply_buf, source)` and bumps `last_seen`+`bytes_out`+`datagrams_out`. The listener-side `recv_from` calls `upstream.send_to(&buf[..n], upstream_addr)` and bumps `last_seen`+`bytes_in`+`datagrams_in`. Cancellation drops both tasks and the upstream socket. **No DNS path here** — that's US2 (depends on T027, T028, T029)
- [ ] T031 [US1] In `crates/forward-client/src/forwarder/mod.rs::activate`, dispatch on `rule.protocol`: `Protocol::Tcp` → existing `proxy.rs` path UNCHANGED; `Protocol::Udp` → spawn a `UdpListener` task per port (single-port for US1 — range handling lands in US3). Single-port UDP rule is one `UdpListener` task with the configured `udp_max_flows_per_rule` cap (provisional default 1024 — Welcome-derived value lands in US4). The TCP `proxy.rs` file MUST NOT be modified (FR-010)
- [ ] T032 [P] [US1] Extend `crates/forward-client/src/control.rs::send_stats_report`: include the new UDP fields in `RuleStats` (datagrams_in, datagrams_out, active_flows, flows_dropped_overflow) by reading the per-rule `RuleStats::snapshot_*` values added in T029. TCP rules emit zeros on these fields (their counters are never incremented)
- [ ] T033 [P] [US1] In `crates/forward-server/src/operator/cli.rs`, add `--protocol <PROTOCOL>` clap argument (enum: `tcp | udp`, default `tcp`) to the `push-rule` subcommand. Thread the value through `rule_cli::push`
- [ ] T034 [P] [US1] In `crates/forward-server/src/operator/rule_cli.rs`, add `protocol: Protocol` parameter to `push()`; emit it in the JSON request body as `"protocol": "tcp"|"udp"`. Update the response `StatsResponse` to include `protocol: String`. Update the text output for `push-rule` to include the protocol (`rule_id={} protocol={} ...`)
- [ ] T035 [P] [US1] In `crates/forward-server/src/operator/http.rs::PushRuleResponse`, add `protocol: String`. In the request handler, parse the optional `"protocol"` field defaulting to `"tcp"`, validate against `{"tcp", "udp"}` returning `invalid_protocol` HTTP 400 otherwise, and pass the value through to `RuleStore::push_*`
- [ ] T036 [US1] In `crates/forward-server/src/rules.rs`, change the port-conflict detection in `RuleStore::push_*` from "any rule on this `(client, listen_port)`" to "any rule on this `(client, listen_port, protocol)`". UDP `:6000` and TCP `:6000` on the same client become legal (data-model § Validation rules item 2). Existing port-conflict unit tests that don't specify protocol stay green by virtue of defaulting to TCP; add new tests that prove cross-protocol coexistence works and same-protocol conflict still fails
- [ ] T037 [P] [US1] In `crates/forward-server/src/metrics.rs`, register four new collectors (R-008): `forward_rule_active_flows: GaugeVec` (labels `["client","rule"]`), `forward_rule_udp_datagrams_in_total: IntCounterVec` (same labels), `forward_rule_udp_datagrams_out_total: IntCounterVec`, `forward_rule_flows_dropped_overflow_total: IntCounterVec`. Each gets a HELP string per `contracts/operator-api.md`
- [ ] T038 [US1] Extend `MetricsCache::observe` in `crates/forward-server/src/metrics.rs` to accept the four new UDP values (`datagrams_in`, `datagrams_out`, `active_flows`, `flows_dropped_overflow`), compute deltas vs the cached previous values, and feed them into the new collectors registered in T037. Extend `drop_rule` to also remove the four new label rows (extending the v0.3 `dns_failures` cleanup pattern); update `drop_rule` test to cover this. Update the existing collector-population test (`dns_failures_cardinality_is_one_row_per_rule` shape) to add a sibling test for `active_flows` cardinality
- [ ] T039 [US1] In `crates/forward-server/src/grpc/service.rs`, extend the `observe()` call to pass the four new fields read from the StatsReport message (depends on T038)
- [ ] T040 [US1] Update `crates/forward-server/src/operator/rule_cli.rs::stats` text output to be protocol-aware: TCP rules render `active_connections=N` (existing); UDP rules render `active_flows=N datagrams_in=N datagrams_out=N flows_dropped_overflow=N` instead. JSON shape carries both — the text formatter selects fields based on the rule's `protocol` value (data-model § Operator surface). Update the StatsResponse to include the four new fields and the `protocol` discriminator

**Checkpoint**: US1 fully functional — single-port UDP rules
end-to-end, capability gating works, metrics surface one row per
rule. TCP rules unchanged.

---

## Phase 4: User Story 2 - DNS-target UDP rule (Priority: P2)

**Goal**: A UDP rule whose `target_host` is a DNS name resolves the
name lazily on the first datagram of each new flow, caches per the
v0.3 resolver, and increments the existing `dns_failures` counter on
resolution failure. The rule stays Active throughout.

**Independent Test**: With a hosts entry `udp.test → 127.0.0.1`, push
`edge-01 6001 udp.test:9999 --protocol udp`, send a UDP datagram, see
the same `rule.dns_resolved` event the TCP path emits. Then break
DNS, observe `dns_failures` counter advance, restore DNS, observe
recovery without operator action. (Quickstart step 5.)

### Tests for User Story 2 ⚠️

- [ ] T041 [P] [US2] Resolver UDP-shim unit test in `crates/forward-client/src/forwarder/udp/flow.rs` `#[cfg(test)] mod tests`: with a `MockResolver` returning `[127.0.0.1]`, calling the new `build_flow_dns(target=Dns("test"), port=9999)` resolves once and dials the resolved address with `send_to`; multi-A `MockResolver` returning `[unreachable, 127.0.0.1]` falls back to the second address on the first `send_to` synchronous error
- [ ] T042 [P] [US2] dns_failures bump unit test in same file: with a `MockResolver` returning `Err(Lookup("nope"))`, calling `build_flow_dns` returns `Err(ConnectError::Resolution)` AND the per-rule `RuleStats.dns_failures` counter advances by exactly one. The flow is NOT inserted into the table (an unresolvable target shouldn't reserve a flow slot)
- [ ] T043 [P] [US2] e2e test `crates/forward-e2e/tests/udp_smoke.rs::test_udp_us2_dns_target` (NEW): write a temp resolv.conf or use the localhost-mini-resolver pattern from `dns_smoke.rs`; push a UDP rule with a DNS target; assert round-trip works; break the resolver target; assert the next datagram is dropped, `dns_failures` counter increments via `/metrics`, AND the rule's `list-rules` state is still `active`. Restore the resolver, assert recovery without operator intervention

### Implementation for User Story 2

- [ ] T044 [US2] Extend `UdpListener::build_flow` in `crates/forward-client/src/forwarder/udp/flow.rs` to accept a `&Target` (not just an IP): for `Target::Ip` keep the US1 fast path; for `Target::Dns` call `LiveResolver::resolve_target(rule_id, target, port, prefer_ipv6)` (added in T014), use the first returned `SocketAddr` as the flow's `upstream_addr`, and remember the rest in the flow as a `Vec<SocketAddr>` for fallback. Resolution errors → `ConnectError::Resolution` → caller bumps `dns_failures` and drops the datagram (depends on T014)
- [ ] T045 [US2] In the per-flow listener path inside `crates/forward-client/src/forwarder/udp/mod.rs::recv_loop`, when `upstream.send_to(&buf, addr)` returns a synchronous error (e.g. `EHOSTUNREACH`) AND the flow has remaining fallback addresses (multi-A from T044), retry on the next address before giving up. Each `send_to` failure on the LAST candidate → log at WARN with reason classifier, bump `dns_failures`, drop the datagram. Per FR-012, this MUST NOT fail the flow or the rule — just the one datagram
- [ ] T046 [US2] Wire the `Arc<LiveResolver>` constructed in `crates/forward-client/src/main.rs` into the UDP listener task spawn path (the TCP path already passes it through `proxy.rs`). The same resolver instance is shared across TCP and UDP rules so single-flight, cache, and `dns_failures` semantics carry over verbatim (R-006)
- [ ] T047 [P] [US2] In `crates/forward-server/src/rules.rs`, the existing v0.3 hostname validator (`Target::parse` on push) already covers UDP rule targets without code changes — add a unit test in `crates/forward-server/src/rules.rs` `#[cfg(test)] mod tests` confirming a UDP push with a malformed hostname (e.g. `foo_bar.example`) returns `invalid_target_host` exit-3 just like TCP

**Checkpoint**: US2 fully functional — UDP rules accept DNS-name
targets with the same TTL/single-flight/stale-while-error semantics
as TCP. `dns_failures` counter unified across protocols.

---

## Phase 5: User Story 3 - UDP port-range rule (Priority: P2)

**Goal**: A UDP range rule binds a contiguous listen-port window and
forwards each port to the same-offset upstream port. Per-port
datagram counters surface via `--per-port`. Per-rule Prometheus rows
stay one per collector (cardinality budget preserved).

**Independent Test**: Push `edge-01 6010-6019 127.0.0.1:9990-9999
--protocol udp`. Bring up echoes on each upstream port. Send a
datagram to edge `:6013`; assert it lands at upstream `:9993` and the
reply routes back. `rule-stats <id> --per-port` shows the per-port
detail. `/metrics` shows exactly ONE row of
`forward_rule_udp_datagrams_in_total` for the 10-port rule.

### Tests for User Story 3 ⚠️

- [ ] T048 [P] [US3] Range push unit test in `crates/forward-server/src/rules.rs` `#[cfg(test)] mod tests`: a UDP range push with same-length listen and target ranges succeeds; mismatched lengths return `mismatched_range` exit-3 (existing v0.2 code path; this just confirms it fires for UDP too)
- [ ] T049 [P] [US3] e2e test `crates/forward-e2e/tests/udp_smoke.rs::test_udp_us3_range_round_trip`: push a 10-port UDP range, send one datagram per port, assert each round-trips through its same-offset upstream
- [ ] T050 [P] [US3] e2e test `crates/forward-e2e/tests/udp_smoke.rs::test_udp_us3_per_port_stats`: same setup as T049; after the round-trips, call `rule-stats <id> --per-port`, assert the JSON `per_port` array contains one entry per port with non-zero `datagrams_in` / `datagrams_out` / `bytes_in` / `bytes_out` for the ports actually exercised
- [ ] T051 [P] [US3] e2e test `crates/forward-e2e/tests/udp_smoke.rs::test_udp_us3_metric_cardinality`: push the 10-port UDP range, drive datagrams through 3 ports, fetch `/metrics`, assert exactly **one** row of `forward_rule_udp_datagrams_in_total{rule="<id>"}` (NOT 10) — SC-004

### Implementation for User Story 3

- [ ] T052 [US3] In `crates/forward-client/src/forwarder/mod.rs::activate`, when `rule.protocol == Udp` AND `listen_port_end > listen_port`, spawn one `UdpListener` task per port in the range (mirroring the TCP range path in v0.2). Each port gets its own `UdpFlowTable` keyed independently, but all share the parent rule's `RuleStats` for aggregate counter roll-up. Per-port byte/datagram counts are kept on a per-listener struct for `--per-port` surface (depends on T031)
- [ ] T053 [US3] In `crates/forward-client/src/forwarder/udp/mod.rs`, add `PerPortUdpStats { listen_port: u16, bytes_in: AtomicU64, bytes_out: AtomicU64, datagrams_in: AtomicU64, datagrams_out: AtomicU64 }` and expose a per-rule `Vec<Arc<PerPortUdpStats>>`. The listener bumps its own per-port counters on every datagram in addition to the rule-level aggregates from T030
- [ ] T054 [P] [US3] Extend `crates/forward-client/src/control.rs::send_stats_report` to include the per-port UDP details in the existing `PerPortStats` repeated field on `RuleStats`. For each port in a UDP range, populate `bytes_in/out` (existing v0.2 fields) AND the new `datagrams_in/out` (added in T004). TCP per-port entries leave `datagrams_in/out` at zero
- [ ] T055 [P] [US3] In `crates/forward-server/src/operator/rule_cli.rs::stats --per-port`, render the new `datagrams_in` / `datagrams_out` columns for UDP rules. JSON `per_port` array entries include the new fields unconditionally (TCP entries show zeros)

**Checkpoint**: US3 fully functional — UDP range rules work with
per-port detail surfacing via `--per-port`, Prometheus cardinality
preserved at one row per rule.

---

## Phase 6: User Story 4 - Idle UDP flow eviction (Priority: P3)

**Goal**: Per-flow state is reaped after the configured idle window
without operator action. The per-rule cap activates the
`flows_dropped_overflow` counter when sustained churn exceeds the
configured ceiling. Server config flows through Welcome to the client.

**Independent Test**: Push a UDP rule. Send one datagram each from
N distinct source ports (N << cap). Wait the idle window. Observe
`active_flows` returns to 0. Re-send from one of the original
sources; observe a fresh upstream socket is opened (the prior was
reaped) and the reply still routes back. Separately, configure
`udp_max_flows_per_rule = 2` in `server.toml`, send from 3 distinct
sources in quick succession; observe `flows_dropped_overflow_total`
advance by 1.

### Tests for User Story 4 ⚠️

- [ ] T056 [P] [US4] Reaper unit test in `crates/forward-client/src/forwarder/udp/table.rs` `#[cfg(test)] mod tests`: construct a `UdpFlowTable` with `idle_window = 100ms` (test-only — the production min is 30s but the eviction algorithm is identical), insert 3 flows, sleep 250ms, manually trigger one reaper sweep, assert all 3 entries removed and `len() == 0`. Insert a fresh flow, sleep 50ms, sweep, assert it survives (not idle long enough yet)
- [ ] T057 [P] [US4] Welcome tunable propagation unit test in `crates/forward-client/src/control.rs` `#[cfg(test)] mod tests`: a Welcome with `udp_flow_idle_secs = 90` and `udp_max_flows_per_rule = 256` causes the client to construct subsequent `UdpFlowTable`s with those values. A Welcome with both fields = 0 falls back to compile-time defaults (60 / 1024) per data-model
- [ ] T058 [P] [US4] e2e test `crates/forward-e2e/tests/udp_smoke.rs::test_udp_us4_overflow_drop`: launch a server with `udp_max_flows_per_rule = 2` (via `server.toml` override or programmatic config), push a UDP rule, drive 3 datagrams from 3 distinct source ports in quick succession; assert `flows_dropped_overflow_total{rule="<id>"} == 1`. The two oldest sources still work (the third is dropped, not the existing two evicted)
- [ ] T059 [P] [US4] e2e test `crates/forward-e2e/tests/udp_smoke.rs::test_udp_us4_idle_eviction`: launch a server with `udp_flow_idle_secs = 30` (the minimum allowed; tests run faster if the constant is exposed via a test-only feature gate in `forward-core`); push a rule, drive 5 datagrams from 5 distinct source ports, sleep 35s, assert `active_flows{rule="<id>"} == 0`. Re-send from one of the original sources; assert the round-trip still works (a fresh upstream socket was opened)

### Implementation for User Story 4

- [ ] T060 [US4] Add a reaper task to `UdpFlowTable` in `crates/forward-client/src/forwarder/udp/table.rs`: spawn one task per table at construction time that sleeps `idle_window / 4` and then sweeps the entries map, evicting any whose `last_seen < now - idle_window`. Cancellation token shared with the parent rule cancels the reaper on rule remove / shutdown. Holds the mutex only across the eviction `retain` call, never across an `await` (R-002 invariant)
- [ ] T061 [US4] In `crates/forward-client/src/control.rs`, on Welcome receive, store `udp_flow_idle_secs` and `udp_max_flows_per_rule` (treating wire 0 as "use compile-time default") in a shared `Arc<UdpRuntimeConfig>` accessible to subsequent rule activations. The first UDP rule push after connect uses this config; reconnects refresh the config from the new Welcome
- [ ] T062 [US4] Thread `Arc<UdpRuntimeConfig>` into `UdpFlowTable::new` in `crates/forward-client/src/forwarder/udp/table.rs` and into the `UdpListener` spawn path in `crates/forward-client/src/forwarder/mod.rs::activate` so the runtime values land on the table at construction time. The provisional defaults from T028 are now replaced by the Welcome-derived values
- [ ] T063 [P] [US4] Document the new config keys in `deploy/server.toml.example`: add commented-out lines for `udp_flow_idle_secs` (default 60, range 30..=300) and `udp_max_flows_per_rule` (default 1024, range 1..=65535) per `contracts/persistence.md` § server.toml

**Checkpoint**: US4 fully functional — idle eviction reclaims flow
state, overflow drops are counted, server-side tuning propagates to
clients via Welcome. v0.4.0 feature surface is complete.

---

## Phase 7: Polish & Cross-Cutting

**Purpose**: Benchmarks, docs, CHANGELOG, regression gate. Nothing
new functional — just the release-readiness work.

- [ ] T064 [P] Add criterion bench `udp_data_plane::single_flow_throughput` in `crates/forward-client/benches/udp_data_plane.rs` (NEW): open one UDP flow over loopback, pump 60 s of 512-byte datagrams in one direction, measure dgrams/s. Asserts `>= 50_000` (SC-002). Same harness style as `data_plane.rs` / `range_install.rs` / `dns_resolver.rs` (inline reproduction of production shape because `forward-client` ships as a binary with no `lib` target). Add to `Cargo.toml` `[[bench]]` list. Document in the bench file's module doc-comment that the inline reproduction is a known divergence risk vs the production code in `forwarder/udp/`
- [ ] T065 [P] Add criterion bench `udp_data_plane::single_flow_rtt` in same file: single 512-byte datagram round-trip through the UDP listener, measure median latency. No hard threshold for v0.4.0 (SC-002 is throughput-driven), but used as a regression detector
- [ ] T066 [P] Re-run `cargo bench --bench data_plane -p forward-client` against the v0.4.0 binary; record median p50/p99 + throughput delta vs v0.3.0 baseline (`baselines/v0.1.0.json`). Assert no >5% regression per Constitution II / SC-006. Record the numbers in the PR description
- [ ] T067 [P] Update `README.md` with a v0.4.0 UDP-target push example block (matches `quickstart.md` § "Walkthrough" steps 3-4). Mention the new `--protocol udp` flag and the per-protocol port-conflict rule
- [ ] T068 [P] Update `docs/runbook.md`: drop "No UDP" from the Limitations section; add a "UDP forwarding (v0.4.0+)" subsection covering the 60s idle window, 1024 flow cap, per-protocol port spaces, and the new metric collectors. Document the `udp_flow_idle_secs` / `udp_max_flows_per_rule` knobs alongside the v0.2.0 `range_rule_max_ports` discussion
- [ ] T069 Run `quickstart.md` walkthrough end-to-end on a Linux host (or single-host loopback per the v0.2 SC-001 recipe). Capture wall-clock for SC-001 (push → first byte: target < 60 s), datagram throughput sanity check, and SC-004 cardinality assertion. Record measurements in `CHANGELOG.md` `### Verified` block under `[Unreleased]`
- [ ] T070 Final `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace --release` — all green before opening PR. The v0.3 workspace pedantic-allow set is expected to cover most lints; new violations (likely doc_markdown false positives on UDP-jargon strings, items_after_statements in test scaffolds) get either targeted `#[allow]` or inline fixes

---

## Dependencies

```text
Phase 1 Setup           (T001-T003)
        │
        ▼
Phase 2 Foundational    (T004-T020)
   ├── Wire schema      (T004 → T005)
   ├── Capability       (T006-T010, depends on T004)
   ├── Server config    (T011-T013, parallel with capability)
   ├── Resolver split   (T014-T016, parallel with capability)
   └── Push gating      (T017-T020, depends on capability)
        │
        ▼  ← all foundational must be green
Phase 3 US1 (P1 MVP)    (T021-T040)
        │
        ├──► Phase 4 US2 (P2)    (T041-T047, depends on resolver split + US1 listener)
        │
        ├──► Phase 5 US3 (P2)    (T048-T055, depends on US1 listener; can parallel with US2)
        │
        └──► Phase 6 US4 (P3)    (T056-T063, depends on US1 + Welcome plumbing)
                │
                ▼
        Phase 7 Polish  (T064-T070)
```

**Story-level parallelism**: US2, US3, US4 are independent of each
other once US1 lands. A team of 3 could work them in parallel.

**Within-phase parallelism (selected)**:
- Phase 2: T006/T007 (capability storage) + T011/T012 (config) + T014/T015/T016 (resolver split) all parallel
- Phase 3: T021-T026 (all tests) parallel; T029/T032/T033/T034/T035/T037 parallel implementation
- Phase 7: T064-T068 all parallel

## Implementation Strategy

**MVP scope**: Phase 1 + Phase 2 + Phase 3 (US1 only). At MVP
checkpoint, an operator can push UDP rules with IP-literal targets
end-to-end, observe per-rule UDP metrics, and TCP rules continue
unchanged. US2/US3/US4 are pure additions — none of them block US1
delivery.

**Incremental delivery**:
1. After US1 lands, ship a v0.4.0-rc1 if useful.
2. US2 adds DNS targets to UDP — the most-requested follow-up after
   US1 (mirrors the v0.3 trajectory).
3. US3 adds range rules — needed by gaming/RTP use cases.
4. US4 adds idle eviction + overflow accounting — necessary for
   production hardening but US1-3 work without it (flows accumulate
   to the cap and stop).

**Constitution gates throughout**:
- II (Performance): T066 is the hard gate. Re-run on every PR
  touching `forwarder/`.
- III (Test-First): every `[US*]` task is preceded by a sibling test
  task in the same phase. Order in tasks.md is execution order, so
  tests come first by construction.
- IV (Observability): T037-T040 (server-side metrics) and T032
  (client-side stats) land within US1 so the very first UDP rule is
  observable.
