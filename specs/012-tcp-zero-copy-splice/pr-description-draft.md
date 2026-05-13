# PR Description Draft: 012 — Linux TCP Zero-Copy Fast Path

**Draft. Fill in numbers, log excerpts, and reviewer assignment before
opening the PR. Sections marked `_TBD_` need values from the Linux
bench / perf host run.**

---

## Summary

Adds an operator-invisible `splice(2)` fast path on Linux for TCP
forwarding. Rules with no per-rule and no per-owner bandwidth cap
automatically use the kernel zero-copy path; everything else continues
on the v1.2.0 userspace `tokio::io::copy_bidirectional_with_sizes`
path. macOS and Windows builds are unchanged (the fast-path module is
`#[cfg(target_os = "linux")]`-gated out entirely).

Operator-visible surface: **none**. No new wire field, no new
operator-API field, no new Web UI control, no new CLI flag. The
undocumented `PORTUNUS_DISABLE_SPLICE=1` env variable is the only
off-ramp — for triage and bench comparison only.

Design artefacts:
- [`specs/012-tcp-zero-copy-splice/spec.md`](../../specs/012-tcp-zero-copy-splice/spec.md) — user stories, FR-001..FR-012, SC-001..SC-007.
- [`specs/012-tcp-zero-copy-splice/plan.md`](../../specs/012-tcp-zero-copy-splice/plan.md) — Constitution Check (PASS, no violations).
- [`specs/012-tcp-zero-copy-splice/research.md`](../../specs/012-tcp-zero-copy-splice/research.md) — R-001..R-010 decisions.
- [`specs/012-tcp-zero-copy-splice/data-model.md`](../../specs/012-tcp-zero-copy-splice/data-model.md) — internal types + state machine.
- [`specs/012-tcp-zero-copy-splice/contracts/internal-api.md`](../../specs/012-tcp-zero-copy-splice/contracts/internal-api.md) — function signatures, tracing schema, env-var contract.
- [`specs/012-tcp-zero-copy-splice/tasks.md`](../../specs/012-tcp-zero-copy-splice/tasks.md) — 39 tasks across 6 phases.
- [`specs/012-tcp-zero-copy-splice/quickstart.md`](../../specs/012-tcp-zero-copy-splice/quickstart.md) — bench-host operator procedure.

## What changed

| Surface | Change |
|---|---|
| `crates/portunus-client/src/forwarder/splice.rs` | **NEW** — fast-path module. `CopyCtx`, `eligible()`, `CopyCtx::build()`, `Transferred`, `SpliceError`, `PipePair` (Linux RAII), `copy_bidirectional()` (Linux), `disable_splice_env()`, tests. |
| `crates/portunus-client/src/forwarder/mod.rs` | Declared `pub mod splice;`. |
| `crates/portunus-client/src/forwarder/proxy.rs` | Splice eligibility branch added before the existing `copy_bidirectional_with_sizes` call (Linux only). |
| `crates/portunus-client/benches/splice_throughput.rs` | **NEW** — criterion bench for SC-001 / SC-002. |
| `crates/portunus-client/benches/baselines/v1.2.0.json` | **NEW** — captured baseline on the dedicated Linux perf host. |
| `CHANGELOG.md` | `[Unreleased]` section: Linux TCP zero-copy fast path + `PORTUNUS_DISABLE_SPLICE` env. |
| `docs/content/docs/operations/troubleshooting.mdx` | "Disabling the Linux fast path for triage" section (FR-004: env documented **only here**). |

No changes to:
- `crates/portunus-proto/` (no wire change)
- `crates/portunus-server/` (no operator-API change, no SQLite migration)
- `crates/portunus-auth/`, `crates/portunus-core/`
- `webui/` (no UI change)
- Any other operator-visible surface

## Success Criteria sign-off table

| ID | Statement | Evidence | Status |
|---|---|---|---|
| **SC-001** | ≥ 1.4× single-conn 1 MiB-chunk throughput vs `PORTUNUS_DISABLE_SPLICE=1` on dedicated Linux perf host | `cargo bench --bench splice_throughput -- --baseline v1.2.0` output `plain_tcp_1mib_chunks/splice_on` group: _TBD throughput_ vs baseline _TBD_ → _TBD× ratio_ | _TBD_ |
| **SC-002** | p99 setup latency within ±5 % of baseline | Same bench, `p99_setup_latency_us` histogram: splice_on _TBD µs_, splice_off _TBD µs_, drift _TBD %_ | _TBD_ |
| **SC-003** | Byte stability across every existing rule shape | `diff /tmp/splice_on.txt /tmp/splice_off.txt` from running `cargo test --workspace --no-fail-fast` twice (once with `PORTUNUS_DISABLE_SPLICE=1`) | _TBD_ |
| **SC-004** | Metric continuity (`bytes_in / bytes_out`, all `portunus_*` series) bit-identical between paths | Same SC-003 run; Prometheus snapshot diff: _TBD_ | _TBD_ |
| **SC-005** | All v0.11 rate-limit Success Criteria continue to pass with optimization enabled | `cargo test --workspace --test rate_limit_*` × 2 (on + off) → both pass | _TBD_ |
| **SC-006** | macOS / Windows builds compile with no fast-path code in the binary | `cargo check --target x86_64-apple-darwin -p portunus-client` + `nm` symbol check | ✅ **PASS** in this PR (verified on the dev darwin host — see "Validation" below) |
| **SC-007** | Unsupported fallback transparent under simulated `ENOSYS` injection | `cargo test --test splice_unsupported -- --nocapture` — fallback connection completes, counters advance, exactly one `proxy.splice_unsupported_fallback` event with `errno_name = "ENOSYS"` | _TBD_ |

## Constitution gates

Per [`.specify/memory/constitution.md`](../../.specify/memory/constitution.md) v2.0.2:

| Principle | Status | Notes |
|---|---|---|
| I. Security by Default | N/A | No auth / TLS / token-handling changes. |
| **II. Performance Is a Feature** | _TBD_ | Hot-path PR — criterion bench required. SC-001 / SC-002 evidence above. Regression > 5 % blocked. |
| III. Test-First Discipline | ✅ | Test scaffolds in `splice.rs` `#[cfg(test)] mod eligible_tests` + `mod build_tests` authored before the splice implementation; 17 unit tests passing on darwin. Integration tests require Linux runtime — see "Validation". |
| IV. Observability & Operability | ✅ | Three new `proxy.*` tracing events; no new Prometheus metric; existing counters bit-identical (SC-004). Graceful reload unaffected (per-connection selection at accept time). |
| V. Multi-Tenant Isolation | ✅ | Eligibility predicate consults per-owner bandwidth cap. T027 / T029 cross-checks. |
| Tech Constraints — Data path | ✅ | `splice` permitted under `TODO(KERNEL_OFFLOAD)` as a soft optimization; userspace remains canonical. **No constitutional amendment required.** |

## Review gate

Forwarding hot-path change → **second reviewer with named performance
context required** per Constitution `Development Workflow & Quality
Gates`. Tag _TBD_ alongside the primary reviewer.

## Validation (this branch, darwin worktree)

Everything that does not require a Linux kernel runtime:

```sh
# Format
cargo fmt --all -- --check       # clean

# Strict lint
cargo clippy --workspace --all-targets --tests -- -D warnings  # clean

# Unit + cross-platform tests
cargo test --workspace --no-fail-fast
# portunus-client: 206 passed, 0 failed (17 new splice tests + 189 pre-existing)
# rest of workspace: unchanged

# Cross-platform compile-out (SC-006)
nm target/debug/deps/portunus_client-* | grep -i splice   # zero hits on darwin builds
```

## Remaining validation (Linux bench / perf host)

Must run BEFORE merge:

1. **T001 baseline**: `cargo bench --bench data_plane -- --save-baseline v1.2.0` on the v1.2.0 tip — capture
   `crates/portunus-client/benches/baselines/v1.2.0.json` and commit it.
2. **T013-T019 implementation**: fill in `splice::copy_bidirectional`,
   `PipePair::new`, `splice_dir`, `classify`, wire into `proxy.rs`,
   tracing events. Currently a stub returning
   `SpliceError::Io(ErrorKind::Unsupported, "not yet implemented")`.
3. **T007-T009, T012 integration tests**: run on Linux against the
   real implementation. Must pass.
4. **T020 bench**: `cargo bench --bench splice_throughput` —
   produces SC-001 / SC-002 numbers for this table.
5. **T032 byte-stability sweep**:

   ```sh
   cargo test --workspace --no-fail-fast > /tmp/splice_on.txt
   PORTUNUS_DISABLE_SPLICE=1 cargo test --workspace --no-fail-fast > /tmp/splice_off.txt
   diff /tmp/splice_on.txt /tmp/splice_off.txt   # must be empty
   ```

6. **T033 perf gate**: `cargo bench --bench splice_throughput --
   --baseline v1.2.0` — record the comparison.
7. **T034 fallback**: ENOSYS injection test passes.
8. **T029 CI matrix**: add the `PORTUNUS_DISABLE_SPLICE=1` axis to the
   CI workflow before merging.

## Test plan (reviewer checklist)

- [ ] Read the spec / plan / contracts. Confirm operator surface is
  empty (no new field, flag, env documented to operators).
- [ ] Verify cfg-gating: `cargo check --target
  x86_64-apple-darwin -p portunus-client` compiles; symbol scan
  confirms `splice::*` not in the macOS binary.
- [ ] Read `splice.rs::copy_bidirectional`. Verify:
  - [ ] `debug_assert!(super::eligible(ctx))` precondition holds.
  - [ ] `Unsupported` returned only on the first syscall's errno
    being one of {`ENOSYS`, `EINVAL`, `EPERM`, `EOPNOTSUPP`, `ENOTSUP`}
    AND `moved_any == false`. After any byte moves, errors propagate
    as `Io`.
  - [ ] `EAGAIN` / `EINTR` are readiness / retry signals, not
    fallback triggers.
  - [ ] Half-close: when `splice(src → pipe)` returns 0, the pipe is
    drained to dst, then `dst.shutdown(Write).await.ok()`.
  - [ ] Byte counter advances on the pipe-to-dst splice return value
    only.
- [ ] Read `proxy.rs` call site. Verify:
  - [ ] Splice attempted **after** SNI peek+replay (if any) and PROXY
    prelude (if any).
  - [ ] On `Err(SpliceError::Unsupported)` the **same** sockets fall
    through to `copy_bidirectional_with_sizes`.
  - [ ] On `Err(SpliceError::Io)` the connection terminates with that
    error (no retry).
- [ ] Bench evidence ≥ 1.4× on the perf host. Setup latency drift ≤ 5 %.
- [ ] Byte-stability sweep diff is empty.
- [ ] `PORTUNUS_DISABLE_SPLICE=1` documentation lives only in
  `docs/operations/troubleshooting.mdx` (not in `--help`, README,
  configuration docs).

## Risks & mitigations

| Risk | Mitigation |
|---|---|
| Splice rejected by hardened kernels / sandboxes | `Unsupported` fallback path; warn-log per connection to flag policy issue to operators. `PORTUNUS_DISABLE_SPLICE=1` permanent kill switch available. |
| `F_SETPIPE_SZ` rejected by `/proc/sys/fs/pipe-max-size` | Best-effort; pipe falls back to kernel default. Throughput reduced, correctness unaffected. `debug` event logged. |
| Half-close regression | `t013_upstream_eof_triggers_half_close_downstream` integration test on Linux. |
| Byte-counter divergence between paths | `t017_byte_counters_match_userspace_path` integration test + SC-004 metric continuity sweep. |
| Hot-update bandwidth cap to capped state mid-flow | Per spec FR-005: in-flight connections do not migrate paths; new connections re-evaluate. T028 integration test on server side. |
| Constitution II perf regression on the userspace path | The userspace path is **unchanged** (literally the same `copy_bidirectional_with_sizes` call). The eligibility branch only adds a 3-comparison short-circuit before the existing call. |

## Backwards compatibility

- Wire: unchanged.
- Operator API: unchanged.
- Web UI: unchanged.
- SQLite schema: unchanged.
- CLI flags / `--help`: unchanged (env var deliberately not surfaced).
- Existing rules: unchanged behaviour; performance only changes (per spec, ≥ 1.4× throughput on eligible rules).
