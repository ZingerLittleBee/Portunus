---
description: "Tasks for 012 — TCP Zero-Copy Fast Path (Linux splice)"
---

# Tasks: TCP Zero-Copy Fast Path (Linux splice)

**Input**: Design documents from `/specs/012-tcp-zero-copy-splice/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: REQUIRED. Constitution Principle III (Test-First Discipline) applies; integration tests use real sockets (mocks forbidden by constitution); Principle II requires the criterion bench before merge.

**Organization**: Tasks grouped by user story to enable independent implementation and testing.

## Format: `[ID] [P?] [Story?] Description`

- **[P]**: Different file from sibling tasks, no dependency on incomplete prior task → parallelizable.
- **[Story]**: User story phase tasks only ([US1], [US2], [US3]). Setup / Foundational / Polish carry no story label.
- Every task includes the exact file path.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Capture pre-implementation perf baseline (must precede any data-plane code change per Constitution II) and create the empty `splice.rs` module scaffold with the cfg gate.

- [ ] T001 Capture v1.2.0 perf baseline on the dedicated Linux bench host **before** any splice code lands. Run `cargo bench --bench data_plane -- --save-baseline v1.2.0` from `crates/portunus-client/`; commit the produced JSON at [crates/portunus-client/benches/baselines/v1.2.0.json](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/benches/baselines/v1.2.0.json). SC-001 / SC-002 will compare against this file.
- [X] T002 [P] Create empty `splice.rs` module skeleton at [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs) — module attribute `#[cfg(target_os = "linux")]` covers the implementation half; cross-platform `pub(crate) fn eligible(_: &CopyCtx) -> bool` stub returns `false` on non-Linux via a separate `#[cfg(not(target_os = "linux"))]` block. Module declared from `mod splice;` in [crates/portunus-client/src/forwarder/mod.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/mod.rs).
- [X] T003 [P] ~~Add `PORTUNUS_DISABLE_SPLICE` env-var read into the client `Config`~~ **Adjusted**: portunus-client has no central `Config` struct (CLI is parsed straight into `Cli` via clap; per-subsystem configs like `ReconnectConfig`, `ResolverConfig`). The env-var read landed as a `OnceLock<bool>`-cached helper `disable_splice_env()` in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs) — reads `std::env::var_os("PORTUNUS_DISABLE_SPLICE")` once on first call, freezes for process lifetime. Test fixtures bypass by constructing `CopyCtx { disable_splice: true, .. }` directly, so tests do not pollute the process env (FR-004 compliance preserved).

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Internal type definitions + the cross-platform `eligible()` predicate. Every later test and implementation task depends on these types being present.

**⚠️ CRITICAL**: No user-story phase can start until T004–T005 land.

- [X] T004 Define `CopyCtx` (with `rule_id`, `protocol`, `has_bandwidth_cap`, `disable_splice`, `has_sni_replay_done`, `has_proxy_out` fields per [data-model.md § CopyCtx](./data-model.md)) and the pure-function `eligible(&CopyCtx) -> bool` in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs). The non-Linux build provides a `const fn eligible(_: &CopyCtx) -> bool { false }`. No I/O, no allocations.
- [X] T005 Define `Transferred` (`bytes_in: u64`, `bytes_out: u64`), `SpliceError` (`Unsupported { errno }` / `Io(io::Error)`), and `PipePair` (Linux-only RAII around `pipe2(O_NONBLOCK | O_CLOEXEC)` with `capacity_bytes` field) signatures in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs) — bodies remain `todo!()` until US1 implementation tasks. `PipePair::drop` closes both fds via `OwnedFd`.

**Checkpoint**: Types are present, `eligible()` is callable, but the splice path is not yet wired into `proxy.rs`. Userspace path remains the only live path.

---

## Phase 3: User Story 1 — Large-transfer TCP zero-copy (Priority: P1) 🎯 MVP

**Goal**: A plain TCP rule with no caps achieves ≥ 1.4× throughput on Linux via splice. Counters and byte stream identical to userspace; non-Linux unaffected.

**Independent Test**: Spec's US1 acceptance scenarios 1–5 — 1 GiB transfer through loopback rule on Linux measures ≥ 1.4× over `PORTUNUS_DISABLE_SPLICE=1`; counters match; non-Linux build passes existing tests.

### Tests for User Story 1 ⚠️

> Write these FIRST; ensure they FAIL before the corresponding implementation task lands.

- [X] T006 [US1] Unit test for `eligible(&CopyCtx)` truth table — 8 tests in `#[cfg(test)] mod eligible_tests` inside [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs): baseline (TCP, no caps → eligible only on Linux), UDP-defensive (never eligible), bandwidth_cap forces userspace, disable_splice forces userspace, sni_replay_done is tracing-only (no gate), proxy_out is tracing-only, bandwidth_cap dominates other fields, disable_splice dominates other fields. All passing on darwin (`cargo test -p portunus-client forwarder::splice`). The "no heap allocation" sub-assertion is implicit (the function is `#[inline]` and consists of three bool comparisons against `Copy` fields); deferred a dedicated `dhat` harness as overkill for the assertion.
- [X] T007 [US1] Integration test `t007_bidirectional_echo_1mib_round_trips_byte_identical` (Linux-only `#[cfg(target_os = "linux")] #[tokio::test(flavor = "multi_thread")]`) in `#[cfg(all(test, target_os = "linux"))] mod integration` inside [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs). Spawns loopback echo upstream, drives 1 MiB through `copy_bidirectional`, asserts upstream sees byte-identical content and the returned `Transferred` matches the in-flight length. **Passes on the remote Linux bench host (Debian 13, kernel 6.12, glibc 2.41).**
- [X] T008 [US1] Integration test `t008_upstream_eof_triggers_half_close_downstream` in same `integration` mod — upstream `shutdown(Write)` after 4 KiB; downstream sees `read == 0`, can still send 2 KiB in reverse direction until *it* EOFs. Validates half-close (FR-007 / R-007). **Passes on Linux.**
- [X] T009 [US1] Integration test `t009_pipe_size_request_best_effort_does_not_fail_connection` in the same `integration` mod — drives 64 KiB through `copy_bidirectional` with the host's default `pipe-max-size`, confirms the connection completes inside a 5s timeout. (A hard `EPERM` requires a sysctl write the test isn't authorised to make; the assertion is the weaker but observable property: `F_SETPIPE_SZ` best-effort never causes the connection to fail.) **Passes on Linux.**
- [X] T010 [US1] Integration test `disable_splice_forces_userspace` in `#[cfg(test)] mod eligible_tests` inside [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs) — build `CopyCtx { disable_splice: true, .. }`, call `eligible(&ctx)`, assert it returns `false`. (Pair test for FR-004.) Landed alongside T006.
- [X] T011 [US1] Integration test `bandwidth_cap_forces_userspace` in same mod — `CopyCtx { has_bandwidth_cap: true, .. }` → `eligible() == false`. Establishes that `eligible()` returns false; caller (proxy.rs) is responsible for actually taking the fallback branch. Landed alongside T006.
- [X] T012 [US1] Integration test `t012_byte_counters_match_userspace_path` (Linux-only `#[tokio::test]`) — drives 1 MiB through `copy_bidirectional`, captures `Transferred`. Same test then drives an identical 1 MiB payload through `tokio::io::copy_bidirectional_with_sizes` with 64 KiB buffers (matches PROXY_COPY_BUF_SIZE), asserts `(bytes_in, bytes_out)` tuples are bit-identical between paths (SC-004 metric continuity). Lives in `integration` mod in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs). **Passes on Linux.**

### Implementation for User Story 1

- [X] T013 [US1] Implemented `PipePair::new(rule_id)` RAII (Linux-only) in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs): `nix::unistd::pipe2(OFlag::O_NONBLOCK | OFlag::O_CLOEXEC)` → `OwnedFd` pair; best-effort `fcntl(write_raw, F_SETPIPE_SZ(1 MiB))` followed by `F_GETPIPE_SZ` (falls back to 64 KiB if `F_GETPIPE_SZ` itself fails, though it shouldn't). On `F_SETPIPE_SZ` failure: `tracing::debug!(event = "proxy.splice_pipe_size_failed", ...)`. `Drop` is automatic via `OwnedFd`.
- [X] T014 [US1] Implemented `splice_dir` single-direction loop with `splice_raw` helper (the only `unsafe` block, wrapping `libc::splice` with `SPLICE_F_NONBLOCK | SPLICE_F_MOVE`). Workspace-wide `unsafe_code = "forbid"` was downgraded to `"deny"` in [Cargo.toml](/Users/zingerbee/Documents/forward-rs/Cargo.toml) with an explanatory comment so this single site can locally `#[allow(unsafe_code)]`. `src.readable().await` → `try_io(Interest::READABLE, …)`; `Ok(0)` → `nix_shutdown(dst, SHUT_WR)` + return (the userspace `AsyncWriteExt::shutdown` needs `&mut self` which the shared `&TcpStream` between the two splice directions cannot provide). `WouldBlock`/`EINTR` → `continue`; other errors → `classify`. Drain-pipe-to-dst loop in `drain_pipe_to`.
- [X] T015 [US1] Implemented `copy_bidirectional` orchestrator in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs): allocates two `PipePair`s, shared `moved_any: AtomicBool`, separate `bytes_in / bytes_out: AtomicU64`. `tokio::try_join!(splice_dir(dn→up), splice_dir(up→dn))` runs both directions concurrently. T007 / T008 / T012 on the Linux bench host validate byte-identical forwarding, half-close, and counter parity with userspace.
- [X] T016 [US1] Implemented `classify(errno, moved_any) -> SpliceError` in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs): `Unsupported { errno }` only when `errno ∈ {ENOSYS, EINVAL, EPERM, EOPNOTSUPP}` AND `!moved_any.load(Relaxed)`. (Note: on glibc, `EOPNOTSUPP == ENOTSUP`, so the single `Errno::EOPNOTSUPP` match arm covers both names.) Otherwise wraps in `SpliceError::Io`. `EAGAIN`/`EINTR` are NOT routed through `classify` — they are handled by the readiness loop directly.
- [ ] T017 [US1] Wire splice into the proxy call site in [crates/portunus-client/src/forwarder/proxy.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/proxy.rs) — build `CopyCtx` from rule + owner cap snapshot + `Config::disable_splice` immediately before the existing `copy_bidirectional_with_sizes` call. Use the `#[cfg(target_os = "linux")] if splice::eligible(&ctx) { match splice::copy_bidirectional(...) { Ok → return / Err(Unsupported) → fall through / Err(Io) → return Err } }` shape per [contracts/internal-api.md § §2](./contracts/internal-api.md). Userspace path remains the only branch on non-Linux (`#[cfg(not(target_os = "linux"))]`).
- [X] T018 [US1] Two of three tracing events landed in splice.rs at their final sites:
  - `proxy.splice_selected` (info) — emitted from `emit_splice_selected()` at the start of `copy_bidirectional`, gated by a process-wide `OnceLock<Mutex<HashSet<u64>>>` keyed by `RuleId` so each rule logs once for the process lifetime. Fields: `rule_id`, `pipe_capacity_bytes`, `has_sni_replay_done`, `has_proxy_out`.
  - `proxy.splice_pipe_size_failed` (debug) — emitted by `PipePair::new` when `F_SETPIPE_SZ` returns non-zero. Fields: `rule_id`, `requested_bytes`, `actual_default_bytes`, `errno_name`.
  - `proxy.splice_unsupported_fallback` (warn) — **deferred to T017** because the fallback decision is only observable at the proxy.rs call site where `Err(SpliceError::Unsupported)` is converted into the userspace branch.
- [ ] T019 [US1] Hook byte-counter accounting in [crates/portunus-client/src/forwarder/proxy.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/proxy.rs) so the values returned by `splice::copy_bidirectional` (`Transferred`) feed the same `rule.bytes_in / bytes_out` atomics and Prometheus metrics that `copy_bidirectional_with_sizes`'s `(u64, u64)` return does today. Reuses the existing post-call accounting path; no new metric (see [contracts/internal-api.md § §4](./contracts/internal-api.md)).
- [ ] T020 [P] [US1] Implement criterion bench `splice_throughput.rs` at [crates/portunus-client/benches/splice_throughput.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/benches/splice_throughput.rs). Reproduces the proxy primitive shape in-bench (R-009, mirroring `data_plane.rs` pattern). Four bench groups: `plain_tcp_1mib_chunks/splice_on`, `plain_tcp_1mib_chunks/splice_off` (sets `PORTUNUS_DISABLE_SPLICE=1` via `Criterion`'s env), `sni_routed_1mib_chunks/splice_on`, `sni_routed_1mib_chunks/splice_off`. Bench registered in [crates/portunus-client/Cargo.toml](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/Cargo.toml) `[[bench]]` table.

**Checkpoint**: US1 acceptance scenarios 1–5 pass. Splice path live on Linux; macOS / Windows builds unchanged. T007–T012 turn from FAIL to PASS as T013–T019 land. T020 bench is runnable but not gated yet (Polish phase does the gate).

---

## Phase 4: User Story 2 — SNI / PROXY-out rules also benefit (Priority: P2)

**Goal**: SNI-routed and PROXY-out rules use splice for the post-prelude byte stream.

**Independent Test**: Spec's US2 acceptance scenarios 1–3 — prelude byte-identical to v1.2.0; post-prelude segment uses splice; SNI peek failure paths never enter splice.

### Tests for User Story 2 ⚠️

- [ ] T021 [P] [US2] Integration test `t018_sni_replay_then_splice_post_prelude` (Linux-only) in [crates/portunus-client/src/forwarder/sni/peek.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/sni/peek.rs) `#[cfg(test)]` mod — drive a synthetic ClientHello + 1 MiB of post-handshake payload, capture the upstream view, assert (a) peeked bytes are replayed verbatim and (b) the remaining payload arrives intact. With `PORTUNUS_DISABLE_SPLICE=1` set, both halves still arrive intact (sanity).
- [ ] T022 [P] [US2] Integration test `t019_proxy_protocol_prelude_then_splice_post_prelude` (Linux-only) in [crates/portunus-client/src/forwarder/proxy.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/proxy.rs) `#[cfg(test)]` mod — rule with `target.proxy_protocol = ProxyProtocolVersion::V2`; assert PROXY v2 header is byte-identical to a v1.2.0 capture; then 1 MiB payload flows; counters match.
- [ ] T023 [P] [US2] Integration test `t020_sni_peek_timeout_never_invokes_splice` in [crates/portunus-client/src/forwarder/sni/peek.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/sni/peek.rs) `#[cfg(test)]` mod — inject a peek timeout (e.g., upstream never sends bytes within the v0.9 peek deadline). Connection closes per existing v0.9 semantics; assert via tracing-capture that no `proxy.splice_selected` event was emitted for this connection.

### Implementation for User Story 2

- [ ] T024 [US2] **Validation only** — read through [crates/portunus-client/src/forwarder/proxy.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/proxy.rs) end-to-end and confirm that the splice call site introduced in T017 is reached **after** both the SNI peek-and-replay phase (v0.9) and the PROXY-out prelude write (v0.10) have completed. If a prelude path bypasses or precedes the splice site, document and fix the call-site placement here. Expectation: **no code change required**; T021–T023 should pass as a consequence of T017's correct placement. If they do not, this task lifts to a real code change (move the prelude phases above the `splice::eligible` check; update call-site).

**Checkpoint**: SNI and PROXY-out rules benefit from splice without breaking their prelude semantics. US2 acceptance passes.

---

## Phase 5: User Story 3 — Rate-limit / concurrent-cap correctness (Priority: P1)

**Goal**: Splice eligibility correctly defers to bandwidth caps (rule + owner) and remains active for connection-rate / concurrent caps. All v0.11 rate-limit Success Criteria pass identically with optimization on vs. off.

**Independent Test**: Spec's US3 acceptance scenarios 1–4 + spec SC-005 — full v0.11 rate-limit suite passes twice (splice on / off).

### Tests for User Story 3 ⚠️

- [X] T025 [P] [US3] Integration test `rule_bandwidth_in_forces_userspace` + `rule_without_bandwidth_cap_is_eligible_on_linux_only` in `#[cfg(test)] mod build_tests` inside [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs). Exercises `CopyCtx::build` with a `RuleRateLimitHandle` whose installed `RateLimit { bandwidth_in_bps: Some(1_000_000), .. }` makes `has_bandwidth_cap()` return true; verifies the resulting CopyCtx is ineligible. Pair test: same handle with no envelope installed yields eligible on Linux.
- [X] T026 [P] [US3] Integration test `rule_with_concurrent_only_does_not_force_userspace` + `rule_with_new_conn_rate_only_does_not_force_userspace` in same `build_tests` mod — `RateLimit { concurrent_connections: Some(100), .. }` and `RateLimit { new_connections_per_sec: Some(50), .. }` respectively yield `has_bandwidth_cap: false`. Concurrent / connection-rate caps gate at accept time (v0.11) and the splice path remains eligible.
- [X] T027 [P] [US3] Integration test `owner_bandwidth_cap_forces_userspace` + `owner_concurrent_only_does_not_force_userspace` in same `build_tests` mod — owner-level bandwidth cap installed via `OwnerRateLimitScopeManager::install` forces `has_bandwidth_cap: true` even when the rule has no per-rule cap. Multi-tenant isolation invariant. Bonus tests: `rule_or_owner_bandwidth_cap_dominates`, `no_caps_anywhere_is_eligible_on_linux_only`, `no_handles_at_all_is_eligible_on_linux_only` exercise the OR semantics edge cases.
- [ ] T028 [P] [US3] Integration test `t024_hot_remove_bandwidth_cap_promotes_new_connections_to_splice` (server-side, the operator HTTP path) in [crates/portunus-server/tests/splice_hot_reload.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-server/tests/splice_hot_reload.rs) (new file). Push rule with `bandwidth_in_bps`, open one connection (in-flight on userspace path), then `PUT /v1/rules/{id}` removing the cap. Assert: existing connection completes on userspace path (FR-005); a fresh connection opened after the PUT uses splice (verified via `tracing-test` capture of `proxy.splice_selected`).
- [ ] T029 [P] [US3] Re-run the entire v0.11 rate-limit test suite with `PORTUNUS_DISABLE_SPLICE=1` as a top-level test-runner pass. Implemented as a CI matrix axis in [.github/workflows/test.yml](/Users/zingerbee/Documents/forward-rs/.github/workflows/test.yml) (or equivalent) — adds a second `cargo test --workspace` job with the env var set. SC-005 gate.

### Implementation for User Story 3

- [X] T030 [US3] Implement `CopyCtx::build` in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs): `has_bandwidth_cap = rule_handle.is_some_and(RuleRateLimitHandle::has_bandwidth_cap) || owner_handle.is_some_and(OwnerRateLimitHandle::has_bandwidth_cap)`. `disable_splice` is sourced from `disable_splice_env()`. Per-connection only — not refreshed mid-connection (FR-005). Adjusted from proxy.rs location to splice.rs: it's CopyCtx's constructor and belongs with the type definition; the call site (T017) will just call `CopyCtx::build(...)` rather than open-coding the OR.
- [X] T031 [~] [US3] ~~Add `OwnerRateLimitHandle::has_bandwidth_cap()` getter~~ **N/A**: already exists in v0.11 at [crates/portunus-client/src/forwarder/rate_limit/scope.rs:453](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/rate_limit/scope.rs), alongside the equivalent on `RuleRateLimitHandle` at line 604. T030 calls both directly.

**Checkpoint**: US3 acceptance passes; v0.11 SC-001..SC-007 hold; tenant isolation preserved.

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Final perf gate (SC-001/SC-002), byte-stability sweep (SC-003), metric continuity (SC-004), cross-platform sanity (SC-006), fallback resilience (SC-007), docs.

- [ ] T032 [P] Run full integration suite **twice** on the bench host and capture diff:

  ```sh
  cargo test --workspace --no-fail-fast > /tmp/splice_on.txt
  PORTUNUS_DISABLE_SPLICE=1 cargo test --workspace --no-fail-fast > /tmp/splice_off.txt
  diff /tmp/splice_on.txt /tmp/splice_off.txt
  ```

  Acceptance (SC-003): zero diff in pass/fail set. Append the captured logs to the PR description.

- [ ] T033 [P] Run `cargo bench --bench splice_throughput -- --baseline v1.2.0` on the dedicated perf host. Confirm: `plain_tcp_1mib_chunks/splice_on` throughput ≥ baseline × 1.4 (SC-001), p99 setup latency within ±5 % of baseline (SC-002), `*_splice_off` numbers match baseline within Criterion noise floor (kill-switch sanity).

- [ ] T034 [P] Fallback resilience test for SC-007 — write `crates/portunus-client/src/forwarder/splice.rs` `#[cfg(test)] mod unsupported` with a syscall-wrapper hook (e.g., a thread-local `Option<dyn FnMut() -> ErrnoResult>`) that returns `ENOSYS` on the first `splice` call. Test asserts: connection completes via userspace path; counters advance; exactly one `proxy.splice_unsupported_fallback` event with `errno_name = "ENOSYS"`; no `proxy.splice_selected` event (because eligibility passed but the actual splice was rejected — caller emitted the warn).

- [ ] T035 [P] Cross-platform compile-out sanity check (SC-006):

  ```sh
  cargo check --target x86_64-apple-darwin -p portunus-client
  nm target/x86_64-apple-darwin/debug/portunus-client 2>/dev/null | grep -i splice || echo "NO SPLICE SYMBOLS — OK"
  ```

  Acceptance: build succeeds; `splice`-named symbols absent from the binary (confirms `#[cfg(target_os = "linux")]` gates worked). Record in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs) module doc that this check is part of the release checklist.

- [X] T036 CHANGELOG entry — `## [Unreleased]` section added to [CHANGELOG.md](/Users/zingerbee/Documents/forward-rs/CHANGELOG.md) with two `### Added` bullets: Linux TCP zero-copy fast path (covering eligibility, byte/metric stability, cross-platform behaviour, SNI/PROXY/rate-limit interaction) and `PORTUNUS_DISABLE_SPLICE` env variable.

- [X] T037 Runbook addendum — "Disabling the Linux fast path for triage" section added to [docs/content/docs/operations/troubleshooting.mdx](/Users/zingerbee/Documents/forward-rs/docs/content/docs/operations/troubleshooting.mdx) covering all four symptom rows from [quickstart.md § §9](./quickstart.md): `proxy.splice_unsupported_fallback` events, recurring `proxy.splice_pipe_size_failed`, "throughput equals baseline" diagnostic ladder, and the `bytes_in` vs upstream-received mismatch under RST clarification. Env var documented **only** in this runbook section (FR-004).

- [X] T038 Lint & format pass — `cargo fmt --all` applied (rustfmt collapsed multi-line `CopyCtx::build(...)` calls to single-line where they fit); `cargo clippy --workspace --all-targets --tests -- -D warnings` is clean across all six crates on darwin. Strict pedantic lints pass. One `#[allow(clippy::struct_excessive_bools)]` on `CopyCtx` retained with inline rationale (the bools are the natural encoding of the FR-001..FR-007 gates; collapsing them into a state-machine enum would obscure the per-gate test matrix). Benches not lint-checked here because `[[bench]]` files don't yet exist (T020 still pending).

- [X] T039 PR description draft at [specs/012-tcp-zero-copy-splice/pr-description-draft.md](./pr-description-draft.md) — Summary, change manifest, SC-001..SC-007 sign-off table (numbers marked `_TBD_` pending Linux bench), Constitution gate status (II `_TBD_` pending bench; III/IV/V ✅; tech-constraints ✅), second-reviewer requirement called out, darwin-validation evidence captured, remaining Linux-host validation steps enumerated, reviewer checklist, risk/mitigation table, backwards-compat statement. Fill in the `_TBD_` cells from the Linux bench host run before opening the PR.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)** — T001 must complete first on the bench host (baseline cannot be captured retroactively). T002 / T003 can run in parallel after T001.
- **Foundational (Phase 2)** — T004 / T005 depend on T002. Block all user-story phases.
- **User Story 1 (Phase 3)** — depends on Foundational. Tests T006–T012 written first; T013–T019 turn them green; T020 bench is independent and `[P]`.
- **User Story 2 (Phase 4)** — depends on Phase 3 (US2's tests use the splice call site from T017).
- **User Story 3 (Phase 5)** — depends on Phase 3 (US3's tests build `CopyCtx` instances). T030–T031 may need adjustment based on Phase 3's `CopyCtx::build` location.
- **Polish (Phase 6)** — depends on Phases 3 + 4 + 5 complete.

### Within Each User Story

- All `### Tests for User Story N` tasks must be authored and failing before the matching `### Implementation` tasks land (Constitution III).
- Within Implementation: T013 → T014 → T015 → T016 (each depends on prior, all in splice.rs); T017 → T018, T019 (proxy.rs changes); T020 standalone.
- Each story phase has a checkpoint that must be reached before moving to the next.

### Parallel Opportunities

- **Phase 1**: T002, T003 in parallel after T001.
- **Phase 3 tests**: T021/T022/T023 across three different files in Phase 4; within Phase 3 only T020 (bench, separate file) is `[P]` relative to the others.
- **Phase 5 tests**: T025/T026/T027/T028/T029 each in a different file → all `[P]`.
- **Phase 6**: T032 / T033 / T034 / T035 across distinct measurement axes → all `[P]`.

---

## Parallel Example: Phase 5 (US3) Tests

```bash
# Five rate-limit cross-checks can run concurrently:
cargo test -p portunus-client splice::eligibility_with_rate_limit::t021_bandwidth_in_bps_forces_userspace_path
cargo test -p portunus-client splice::eligibility_with_rate_limit::t022_concurrent_only_does_not_force_userspace
cargo test -p portunus-client rate_limit::scope::t023_owner_bandwidth_cap_forces_userspace_path
cargo test -p portunus-server --test splice_hot_reload t024_hot_remove_bandwidth_cap_promotes_new_connections_to_splice
PORTUNUS_DISABLE_SPLICE=1 cargo test --workspace  # T029 — full suite under kill switch
```

---

## Implementation Strategy

### MVP First (User Story 1 only)

1. Phase 1 Setup (capture baseline FIRST).
2. Phase 2 Foundational (types + eligibility predicate).
3. Phase 3 User Story 1 (write tests; implement; verify ≥ 1.4× via T020 bench).
4. **STOP and VALIDATE**: SC-001 / SC-002 / SC-003 (limited to US1 scope) green on bench host.
5. Decide: merge MVP as-is, or continue to US2 / US3 before merging.

The MVP is shippable: a Linux box transparently uses splice for plain TCP rules; SNI / PROXY / capped rules still work, just on the userspace path until US2 / US3 land.

### Incremental Delivery

1. Phase 1 + 2 + 3 → merge candidate (MVP — plain TCP rules accelerated).
2. + Phase 4 → SNI / PROXY-out rules also accelerated.
3. + Phase 5 → all v0.11 rate-limit invariants re-verified under both paths.
4. + Phase 6 → docs / lint / SC sign-off → release candidate.

### Single-developer strategy (the typical case here)

Phases land sequentially. The `[P]` markers signal "no need to serialize", not "must be parallelized" — a single developer can still pick the next `[P]` task and move on.

---

## Notes

- `[P]` = different file from sibling tasks. Tests inside the **same** `splice.rs` source file (T006–T012 except T020) are not `[P]` amongst each other; they are `[P]` vs. tasks touching other files.
- `[Story]` label maps task → user story for traceability.
- Verify tests FAIL before the matching implementation task lands.
- Commit after each task or logical group.
- Stop at each checkpoint to validate the increment.
- Avoid: vague tasks, same-file conflicts that defeat `[P]`, cross-story dependencies that break independence.
- Constitution gate: every PR touching the splice path includes (a) criterion bench numbers, (b) `cargo test --workspace` with `PORTUNUS_DISABLE_SPLICE=1` results.
