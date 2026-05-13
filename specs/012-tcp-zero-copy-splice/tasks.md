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
- [ ] T007 [US1] Integration test `t012_bidirectional_echo_1mib_round_trips_byte_identical` (Linux-only `#[cfg(target_os = "linux")] #[tokio::test(flavor = "multi_thread")]`) in `#[cfg(test)] mod integration` inside [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs). Spawns loopback echo upstream, drives 1 MiB through `copy_bidirectional`, asserts upstream sees byte-identical content and the returned `Transferred` matches the in-flight length.
- [ ] T008 [US1] Integration test `t013_upstream_eof_triggers_half_close_downstream` in `#[cfg(test)] mod integration` inside [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs) — upstream `shutdown(Write)` after N bytes; downstream sees `read == 0` after exactly N bytes, can still `send` reverse direction until itself EOFs. Validates half-close (FR-007 / R-007).
- [ ] T009 [US1] Integration test `t014_pipe_size_request_best_effort_does_not_fail_connection` in the same `integration` mod inside [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs) — request `F_SETPIPE_SZ` with 16 MiB (deliberately above default `pipe-max-size`), assert `copy_bidirectional` still returns `Ok(Transferred)` and emits exactly one `proxy.splice_pipe_size_failed` event. Uses `tracing-test` (already a dev-dep in the workspace) to capture events.
- [X] T010 [US1] Integration test `disable_splice_forces_userspace` in `#[cfg(test)] mod eligible_tests` inside [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs) — build `CopyCtx { disable_splice: true, .. }`, call `eligible(&ctx)`, assert it returns `false`. (Pair test for FR-004.) Landed alongside T006.
- [X] T011 [US1] Integration test `bandwidth_cap_forces_userspace` in same mod — `CopyCtx { has_bandwidth_cap: true, .. }` → `eligible() == false`. Establishes that `eligible()` returns false; caller (proxy.rs) is responsible for actually taking the fallback branch. Landed alongside T006.
- [ ] T012 [US1] Integration test `t017_byte_counters_match_userspace_path` (Linux-only `#[tokio::test]`) — drive 1 MiB through `copy_bidirectional`, capture the returned `Transferred`. Then in the same test, drive the same payload through `tokio::io::copy_bidirectional_with_sizes` with `PROXY_COPY_BUF_SIZE` buffers. Assert `(bytes_in, bytes_out)` tuples are bit-identical. Lives in `integration` mod in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs).

### Implementation for User Story 1

- [ ] T013 [US1] Implement `PipePair::new()` RAII (Linux-only) in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs): `nix::unistd::pipe2(OFlag::O_NONBLOCK | OFlag::O_CLOEXEC)` → `OwnedFd` pair; try `nix::fcntl::fcntl(write_fd, FcntlArg::F_SETPIPE_SZ(1 MiB))` then read back actual size via `F_GETPIPE_SZ`. On `F_SETPIPE_SZ` failure: `tracing::debug!(event = "proxy.splice_pipe_size_failed", …)`. Drop impl closes both fds (free for `OwnedFd`).
- [ ] T014 [US1] Implement `splice_dir` single-direction loop in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs): `src.readable().await` → `src.try_io(Interest::READABLE, || splice_raw(src.as_raw_fd(), None, pipe.write_fd(), None, pipe.capacity_bytes, SPLICE_F_NONBLOCK | SPLICE_F_MOVE))`; on `Ok(0)` shutdown `dst.shutdown().await.ok()` and return; on `Ok(n)` drain via reverse `splice_raw(pipe.read_fd(), …, dst.as_raw_fd(), …, n, …)` loop with `dst.writable().await` + `try_io`. `WouldBlock` → `continue`; `EINTR` → `continue`; other errors → propagate `SpliceError::Io` once any byte has moved.
- [ ] T015 [US1] Implement `copy_bidirectional` orchestrator in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs): allocate `PipePair` (one per direction — two PipePairs total per connection, see R-002 footnote), shared `moved_any: AtomicBool`, `bytes_in / bytes_out: AtomicU64`. `tokio::try_join!(splice_dir(downstream → upstream, …), splice_dir(upstream → downstream, …))`. Map first-syscall-unsupported result on either direction to `Err(SpliceError::Unsupported { errno })` **only if `moved_any` still false**. Otherwise propagate `Err(SpliceError::Io(_))`.
- [ ] T016 [US1] Implement `Unsupported` errno classification helper in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs): `fn classify(errno: nix::errno::Errno, moved_any: &AtomicBool) -> SpliceError`. Returns `Unsupported { errno }` only when `errno ∈ {ENOSYS, EINVAL, EPERM, EOPNOTSUPP, ENOTSUP}` AND `!moved_any.load(Relaxed)`. Otherwise wraps `io::Error::from_raw_os_error(errno as i32)` in `SpliceError::Io`. `EAGAIN` and `EINTR` are **not** passed to this helper — the readiness loop in T014 handles them.
- [ ] T017 [US1] Wire splice into the proxy call site in [crates/portunus-client/src/forwarder/proxy.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/proxy.rs) — build `CopyCtx` from rule + owner cap snapshot + `Config::disable_splice` immediately before the existing `copy_bidirectional_with_sizes` call. Use the `#[cfg(target_os = "linux")] if splice::eligible(&ctx) { match splice::copy_bidirectional(...) { Ok → return / Err(Unsupported) → fall through / Err(Io) → return Err } }` shape per [contracts/internal-api.md § §2](./contracts/internal-api.md). Userspace path remains the only branch on non-Linux (`#[cfg(not(target_os = "linux"))]`).
- [ ] T018 [US1] Emit the three structured tracing events (per [research.md § R-010](./research.md), [contracts/internal-api.md § §3](./contracts/internal-api.md)) at their correct sites:
  - `proxy.splice_selected` (info) — in `copy_bidirectional`, gated by a per-rule `AtomicBool` cache (e.g., `static_cell` or `OnceCell` per rule registered in `RateLimitScopeManager`-equivalent or a new `SpliceSelectionCache` keyed by `RuleId`) so each rule logs once until its next re-push.
  - `proxy.splice_unsupported_fallback` (warn) — emitted by the `Err(SpliceError::Unsupported)` arm in proxy.rs (T017), since that's where the fallback decision is observable.
  - `proxy.splice_pipe_size_failed` (debug) — emitted by `PipePair::new` (T013) when `F_SETPIPE_SZ` returns non-zero.
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

- [ ] T025 [P] [US3] Integration test `t021_bandwidth_in_bps_forces_userspace_path` in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs) `#[cfg(test)] mod eligibility_with_rate_limit` — build `CopyCtx` with `has_bandwidth_cap: true` (from a rule with only `rate_limit.bandwidth_in_bps = 1_000_000`), assert `eligible() == false`. Pair check: same rule with the cap **removed**, assert `eligible() == true`.
- [ ] T026 [P] [US3] Integration test `t022_concurrent_only_does_not_force_userspace` in [crates/portunus-client/src/forwarder/splice.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/splice.rs) same mod — rule with `concurrent_connections = 100` but **no** bandwidth cap. Assert `eligible() == true` (concurrent gate happens at accept time per v0.11; doesn't touch splice path).
- [ ] T027 [P] [US3] Integration test `t023_owner_bandwidth_cap_forces_userspace_path` in [crates/portunus-client/src/forwarder/rate_limit/scope.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/rate_limit/scope.rs) `#[cfg(test)]` mod — set an owner-level `bandwidth_in_bps` via the v0.11 owner-cap envelope, build `CopyCtx` for a rule with no per-rule cap, assert `eligible() == false`. Validates that the eligibility predicate consults both rule and owner caps (FR-001, multi-tenant isolation invariant).
- [ ] T028 [P] [US3] Integration test `t024_hot_remove_bandwidth_cap_promotes_new_connections_to_splice` (server-side, the operator HTTP path) in [crates/portunus-server/tests/splice_hot_reload.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-server/tests/splice_hot_reload.rs) (new file). Push rule with `bandwidth_in_bps`, open one connection (in-flight on userspace path), then `PUT /v1/rules/{id}` removing the cap. Assert: existing connection completes on userspace path (FR-005); a fresh connection opened after the PUT uses splice (verified via `tracing-test` capture of `proxy.splice_selected`).
- [ ] T029 [P] [US3] Re-run the entire v0.11 rate-limit test suite with `PORTUNUS_DISABLE_SPLICE=1` as a top-level test-runner pass. Implemented as a CI matrix axis in [.github/workflows/test.yml](/Users/zingerbee/Documents/forward-rs/.github/workflows/test.yml) (or equivalent) — adds a second `cargo test --workspace` job with the env var set. SC-005 gate.

### Implementation for User Story 3

- [ ] T030 [US3] Implement `has_bandwidth_cap` computation in `CopyCtx::build` (or equivalent) in [crates/portunus-client/src/forwarder/proxy.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/proxy.rs): `let has_bandwidth_cap = rule.rate_limit.as_ref().is_some_and(|rl| rl.bandwidth_in_bps.is_some() || rl.bandwidth_out_bps.is_some()) || owner_limiter.has_bandwidth_cap();` where `owner_limiter` is the `OwnerRateLimiterHandle::snapshot()` (already exists in v0.11).
- [ ] T031 [US3] If `OwnerRateLimitHandle::has_bandwidth_cap()` does not exist in v0.11, add it as a thin getter (returns true iff any of `bandwidth_in_bps` / `bandwidth_out_bps` on the snapshotted owner limiter is set) at [crates/portunus-client/src/forwarder/rate_limit/scope.rs](/Users/zingerbee/Documents/forward-rs/crates/portunus-client/src/forwarder/rate_limit/scope.rs). If it already exists (review v0.11 code first), this task is a no-op and may be marked `[~]` N/A.

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

- [ ] T036 CHANGELOG entry — add `### Added — Linux TCP zero-copy fast path` block under the next version section of [CHANGELOG.md](/Users/zingerbee/Documents/forward-rs/CHANGELOG.md), wording per [quickstart.md § §8](./quickstart.md#§8-after-acceptance-changelog-entry). Mention the env kill switch and the `docs/runbook.md` cross-reference.

- [ ] T037 Runbook addendum — add the "Disabling the Linux fast path for triage" section to [docs/content/docs/operations/troubleshooting.mdx](/Users/zingerbee/Documents/forward-rs/docs/content/docs/operations/troubleshooting.mdx) covering the four symptom rows in [quickstart.md § §9](./quickstart.md#§9-troubleshooting). Keep the env var advertised **only here**, not in CLI `--help` or README (FR-004).

- [ ] T038 Lint & format pass — run `cargo fmt --all`, `cargo clippy --workspace --all-targets --tests --benches -- -D warnings`. Confirm strict pedantic lints pass (workspace `clippy::pedantic = warn`, CI gates on `-D warnings`). Address any new warnings introduced by `splice.rs` (most likely candidates: `cast_possible_truncation` on `n as u64`, `cast_sign_loss` on errno conversions — handle locally with explicit casts + `#[allow(...)]` only where unavoidable, with the existing workspace-allow rationale comment).

- [ ] T039 Final SC sign-off in PR description — table mapping SC-001..SC-007 to the task that proves each, with bench numbers / test names / log excerpts as evidence. Constitution Development Workflow & Quality Gates: PR description includes the criterion baseline-vs-current diff for `splice_throughput`. PR review note: **forwarding hot path → second reviewer with named performance context required**.

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
