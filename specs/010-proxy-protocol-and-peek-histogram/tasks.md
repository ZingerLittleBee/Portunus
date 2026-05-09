---
description: "Tasks for 010 — PROXY protocol injection and SNI peek histogram"
---

# Tasks: PROXY-Protocol Injection & SNI Peek-Duration Histogram

**Input**: Design documents from `/specs/010-proxy-protocol-and-peek-histogram/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: REQUIRED. Constitution Principle III applies.

## Format: `[ID] [P?] [Story] Description`

---

## Phase 1: Foundational

- [X] T001 [P] Add additive proto field reservations and documentation for target-level PROXY protocol and listener-level peek histogram payload in [proto/forward.proto](/Users/zingerbee/Documents/forward-rs/proto/forward.proto)
- [X] T002 [P] Extend the shared target model with per-target PROXY protocol mode and validation in [crates/forward-core/src/rule_target.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-core/src/rule_target.rs) and [crates/forward-core/tests/rule_target_validation.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-core/tests/rule_target_validation.rs)
- [X] T003 [P] Add wire-compat tests for the new target field and listener histogram payload in [crates/forward-proto/tests/targets_wire_compat.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-proto/tests/targets_wire_compat.rs)
- [X] T004 [P] Add additive storage support for per-target PROXY protocol in the server store/migrations under [crates/forward-server/src/store/migrations](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/store/migrations)

## Phase 2: User Story 1 — Backends see the real client IP (P1)

**Goal**: Opted-in TCP targets receive a correct PROXY v1/v2 prelude before forwarded bytes.

- [X] T005 [P] [US1] Add operator API validation / capability-gate contract tests for per-target `proxy_protocol` in [crates/forward-server/tests/rules_multi_target_contract.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/rules_multi_target_contract.rs) or new focused server tests
- [X] T006 [P] [US1] Add forward-client integration tests for PROXY v1/v2 emission and mixed-target behaviour in [crates/forward-client/tests](/Users/zingerbee/Documents/forward-rs/crates/forward-client/tests)
- [X] T007 [US1] Implement proto/server/client plumbing for per-target `proxy_protocol` in [proto/forward.proto](/Users/zingerbee/Documents/forward-rs/proto/forward.proto), [crates/forward-server/src/operator/http.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/operator/http.rs), [crates/forward-server/src/operator/rule_cli.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/operator/rule_cli.rs), and related rule/store mapping files
- [X] T008 [US1] Implement PROXY v1/v2 header encoding and upstream prelude injection in the forward-client TCP dial path under [crates/forward-client/src/forwarder/failover.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/failover.rs), [crates/forward-client/src/forwarder/failover_path.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/failover_path.rs), and/or [crates/forward-client/src/forwarder/proxy.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/proxy.rs)
- [X] T009 [US1] Emit structured diagnostics and count PROXY prelude write failures as connect failures in the existing failover path

## Phase 3: User Story 2 — Operators can see ClientHello peek latency tails (P2)

**Goal**: SNI listeners export a Prometheus histogram for peek duration, including timeouts and parse failures.

- [X] T010 [P] [US2] Add listener-stats / server-metrics contract tests for the histogram payload in [crates/forward-server/tests/sni_metrics_surface.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/tests/sni_metrics_surface.rs) and relevant client tests
- [X] T011 [P] [US2] Add forward-client SNI listener tests asserting histogram observations for success, timeout, and parse failure in [crates/forward-client/src/forwarder/sni/listener.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/sni/listener.rs) and related test files
- [X] T012 [US2] Implement fixed-bucket peek-duration histogram accumulation on SNI listeners in [crates/forward-client/src/forwarder/sni/listener.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/sni/listener.rs) and [crates/forward-client/src/forwarder/stats.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-client/src/forwarder/stats.rs)
- [X] T013 [US2] Extend `StatsReport` / server metrics fold to publish `forward_tls_client_hello_peek_duration_seconds_*` in [crates/forward-server/src/grpc/service.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/grpc/service.rs) and [crates/forward-server/src/metrics.rs](/Users/zingerbee/Documents/forward-rs/crates/forward-server/src/metrics.rs)

## Phase 4: Polish

- [X] T014 Update feature docs and AGENTS context for `010` in [AGENTS.md](/Users/zingerbee/Documents/forward-rs/AGENTS.md) and `specs/010-proxy-protocol-and-peek-histogram/*`
- [X] T015 Run `cargo fmt`, targeted tests, `cargo clippy --all --benches --tests --examples --all-features`, and mark completed tasks
