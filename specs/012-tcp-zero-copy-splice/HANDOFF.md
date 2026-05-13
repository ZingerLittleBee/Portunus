# 012 — Linux TCP Zero-Copy Splice — Linux Handoff

**Purpose**: complete, self-contained instructions for picking up 012
on a Linux dev / bench host. Everything that can be done from
darwin has been done; the remaining 23 tasks need a Linux runtime.

**Last update**: 2026-05-13, end of darwin worktree session.

---

## What's already on the branch (`wellington-v1`)

```
specs/012-tcp-zero-copy-splice/
├── spec.md                       FR-001..FR-012 + SC-001..SC-007
├── plan.md                       Constitution Check (PASS) + structure
├── research.md                   R-001..R-010 design decisions
├── data-model.md                 internal types + state machine
├── contracts/internal-api.md     function signatures + tracing + env
├── quickstart.md                 bench-host operator procedure
├── tasks.md                      39 tasks, 16 done, 23 blocked-on-Linux
├── pr-description-draft.md       SC sign-off table (_TBD_ cells)
└── HANDOFF.md                    (this file)

crates/portunus-client/src/forwarder/
├── splice.rs                     NEW. CopyCtx + eligible() + build()
│                                 cross-platform. PipePair / copy_bidirectional
│                                 declared but body is stub (Err Io
│                                 "not yet implemented").
└── mod.rs                        declares `pub mod splice;`

CHANGELOG.md                      [Unreleased] section with two bullets
docs/content/docs/operations/troubleshooting.mdx   triage runbook
```

Recent commits (most recent first):

```
4cfd6af chore(012): apply rustfmt + draft PR description (T038, T039)
e432e78 feat(client/012): CopyCtx::build + US3 rate-limit tests + docs (T025-T027, T030-T031, T036-T037)
1cc509e feat(client/012): scaffold splice fast-path module (T002-T006, T010-T011)
8966378 docs(012): /speckit-tasks output - 39 ordered tasks across 6 phases
bf9b50e docs(012): /speckit-plan output - plan, research, data-model, contracts, quickstart
5533aed docs(012): tighten spec after review
8209eb6 docs(012): draft spec for Linux TCP zero-copy splice fast path
787a75d docs(011): close deferred tasks after manual verification
```

## What's done (16 / 39 tasks)

| Task | What |
|---|---|
| T002 | `splice.rs` module scaffold + cfg gate, `mod.rs` declaration |
| T003 | `PORTUNUS_DISABLE_SPLICE` read via `OnceLock`-cached helper |
| T004 | `CopyCtx` struct + cross-platform `eligible()` predicate |
| T005 | `Transferred`, `SpliceError`, `PipePair` type signatures (body stub) |
| T006 | 8 eligibility truth-table unit tests |
| T010 | `disable_splice` → ineligible test |
| T011 | `bandwidth_cap` → ineligible test |
| T025 | rule `bandwidth_in` forces userspace (+ pair test) |
| T026 | rule `concurrent_connections` / `new_connections_per_sec` only stay eligible |
| T027 | owner bandwidth forces userspace (+ concurrent-only counterpart) |
| T030 | `CopyCtx::build()` constructor with rule/owner OR semantics |
| T031 | N/A (`has_bandwidth_cap()` already exists on both handles) |
| T036 | CHANGELOG `[Unreleased]` entry |
| T037 | troubleshooting.mdx triage section |
| T038 | `cargo fmt --all` + `cargo clippy --workspace -- -D warnings` clean |
| T039 | `pr-description-draft.md` with SC sign-off table |

**Verification on darwin**: 17 splice tests + 189 pre-existing = 206/206 pass.

## What's left (23 / 39 tasks, all need Linux)

### Phase 1: Setup (1 task)

- **T001** Capture v1.2.0 perf baseline. **Do this FIRST on the bench host,
  ON the commit immediately preceding any splice implementation change**
  (i.e., before you start landing T013-T019). Otherwise SC-001 has no
  honest comparison point.

  ```sh
  git checkout 4cfd6af   # or v1.2.0 tag — same data-plane
  cd crates/portunus-client
  cargo bench --bench data_plane -- --save-baseline v1.2.0
  git checkout wellington-v1
  git add benches/baselines/v1.2.0.json
  git commit -m "perf(012): capture v1.2.0 data-plane bench baseline (T001)"
  ```

### Phase 3: US1 implementation (10 tasks: T013-T020)

The actual splice loop. Implement in this order — each task is sized to
be one commit.

**T013** `PipePair::new()` in `splice::linux` mod:

```rust
fn new() -> nix::Result<Self> {
    use nix::fcntl::{fcntl, FcntlArg, OFlag};
    use nix::unistd::pipe2;

    let (read_fd, write_fd) = pipe2(OFlag::O_NONBLOCK | OFlag::O_CLOEXEC)?;
    const TARGET_PIPE_SIZE: i32 = 1024 * 1024;
    // best-effort: failure is debug-logged, not propagated
    let _ = fcntl(write_fd.as_raw_fd(), FcntlArg::F_SETPIPE_SZ(TARGET_PIPE_SIZE));
    let capacity_bytes = fcntl(write_fd.as_raw_fd(), FcntlArg::F_GETPIPE_SZ)
        .map(|sz| sz as usize)
        .unwrap_or(64 * 1024);
    if capacity_bytes < TARGET_PIPE_SIZE as usize {
        tracing::debug!(
            event = "proxy.splice_pipe_size_failed",
            requested_bytes = TARGET_PIPE_SIZE,
            actual_default_bytes = capacity_bytes,
        );
    }
    Ok(Self { read_fd, write_fd, capacity_bytes })
}
```

**T014** `splice_dir()` single-direction loop. Pseudo-code in
`contracts/internal-api.md § §1` is the contract. Key details:

- `src.readable().await?` → `src.try_io(Interest::READABLE, ||
  splice_raw(src.as_raw_fd(), pipe.write_fd(), pipe.capacity_bytes))`
- `Ok(0)` from source-side → drain pipe to dst, then
  `dst.shutdown().await.ok()`, return `Ok(())` (this direction done)
- `Ok(n)` → set `moved_any = true`; drain n bytes pipe→dst via
  `dst.writable().await + try_io(Interest::WRITABLE, ...)` loop
- `Err(WouldBlock)` from `try_io` → `continue`
- `Err(EINTR)` → `continue` (retryable)
- `Err(unsupported)` while `!moved_any` → return
  `SpliceError::Unsupported { errno }` (T016 classifier)
- Any other error → `SpliceError::Io(io::Error::from_raw_os_error(...))`

Use `libc::splice(...)` directly (nix's `splice` wrapper requires
fd ownership which conflicts with try_io's closure). Flags:
`libc::SPLICE_F_NONBLOCK | libc::SPLICE_F_MOVE`.

**T015** `copy_bidirectional()` orchestrator:

```rust
async fn copy_bidirectional(downstream, upstream, ctx) -> Result<Transferred, SpliceError> {
    debug_assert!(super::eligible(ctx));
    let pipe_dn_to_up = PipePair::new().map_err(|e| SpliceError::Io(e.into()))?;
    let pipe_up_to_dn = PipePair::new().map_err(|e| SpliceError::Io(e.into()))?;
    let moved_any = AtomicBool::new(false);
    let bytes_in = AtomicU64::new(0);
    let bytes_out = AtomicU64::new(0);

    let (r1, r2) = tokio::try_join!(
        splice_dir(downstream, upstream, &pipe_dn_to_up, &moved_any, &bytes_in),
        splice_dir(upstream, downstream, &pipe_up_to_dn, &moved_any, &bytes_out),
    )?;

    Ok(Transferred {
        bytes_in: bytes_in.load(Ordering::Relaxed),
        bytes_out: bytes_out.load(Ordering::Relaxed),
    })
}
```

**T016** `classify_errno()`:

```rust
fn classify(errno: nix::errno::Errno, moved_any: &AtomicBool) -> SpliceError {
    if moved_any.load(Ordering::Relaxed) {
        return SpliceError::Io(io::Error::from_raw_os_error(errno as i32));
    }
    match errno {
        Errno::ENOSYS | Errno::EINVAL | Errno::EPERM | Errno::EOPNOTSUPP | Errno::ENOTSUP
            => SpliceError::Unsupported { errno },
        other => SpliceError::Io(io::Error::from_raw_os_error(other as i32)),
    }
}
```

**T017** Wire into `proxy.rs`. The shape per `contracts/internal-api.md`:

```rust
// Existing prelude phases (SNI peek+replay, PROXY-out write, accept gate) ...

let ctx = splice::CopyCtx::build(rule_id, protocol, rate_limit.as_deref(),
                                   owner_rate_limit.as_deref(),
                                   /* has_sni_replay_done */ true_if_sni,
                                   /* has_proxy_out */ rule.target.proxy_protocol_set);

#[cfg(target_os = "linux")]
let result = if splice::eligible(&ctx) {
    match splice::copy_bidirectional(&mut downstream, &mut upstream, &ctx).await {
        Ok(t) => Some(Ok((t.bytes_in, t.bytes_out))),
        Err(splice::SpliceError::Unsupported { errno }) => {
            tracing::warn!(
                event = "proxy.splice_unsupported_fallback",
                rule_id = rule_id.0,
                errno_name = %errno,
                errno_value = errno as i32,
            );
            None  // fall through to userspace
        }
        Err(splice::SpliceError::Io(e)) => Some(Err(e)),
    }
} else { None };

let (bytes_in, bytes_out) = match result {
    Some(Ok(t)) => t,
    Some(Err(e)) => return Err(e.into()),
    None => tokio::io::copy_bidirectional_with_sizes(
        &mut downstream, &mut upstream,
        PROXY_COPY_BUF_SIZE, PROXY_COPY_BUF_SIZE,
    ).await?,
};
```

**T018** Emit `proxy.splice_selected` (once per rule via
`OnceLock<()>` keyed on `RuleId` — see `splice.rs` for the
per-rule cache). `proxy.splice_unsupported_fallback` is emitted in T017
above. `proxy.splice_pipe_size_failed` is emitted in `PipePair::new()`
(T013).

**T019** Counter wiring is already correct if you follow T017's shape:
the existing `bytes_in / bytes_out` counters in `proxy.rs` are fed by
the `(u64, u64)` tuple regardless of which path produced it.

**T020** New criterion bench `benches/splice_throughput.rs` —
reproduces the proxy shape inline (no `[lib]` target; see
`benches/data_plane.rs` for the pattern).

Register in `crates/portunus-client/Cargo.toml`:

```toml
[[bench]]
name = "splice_throughput"
harness = false
```

### Phase 3: US1 tests (4 tasks: T007, T008, T009, T012)

Each is a Linux-only `#[cfg(target_os = "linux")] #[tokio::test]` in
`mod integration` inside `splice.rs`. Spec in `tasks.md`. Real
loopback `TcpListener` + echo upstream.

### Phase 4: US2 (4 tasks: T021-T024)

SNI peek+replay then splice (T021), PROXY-out prelude then splice
(T022), SNI peek timeout never invokes splice (T023). T024 is a
**code-reading** validation — confirm proxy.rs call-site is after
both prelude phases. After T017 lands, T024 should be a 5-minute
sanity check.

### Phase 5: US3 server-side test (1 task: T028)

`crates/portunus-server/tests/splice_hot_reload.rs` — push a rule
with `bandwidth_in_bps`, open a connection (on userspace path),
`PUT /v1/rules/{id}` removing the cap, verify the existing
connection completes on userspace path and a fresh connection
takes the splice path. Needs the gRPC + operator HTTP test harness
already used by `rate_limit_rule_contract.rs`.

### Phase 5: CI (1 task: T029)

Add a matrix axis to `.github/workflows/test.yml` (or whatever CI
runs `cargo test --workspace`) with `PORTUNUS_DISABLE_SPLICE=1` set.
Both axes must pass identically.

### Phase 6: Polish (4 tasks: T032-T035)

- **T032** Run full integration suite twice, diff results — SC-003
  byte stability.
- **T033** Run `cargo bench --bench splice_throughput -- --baseline
  v1.2.0` — SC-001 (≥ 1.4× throughput) + SC-002 (≤ 5 % setup-latency
  drift). Numbers fill the `_TBD_` cells in
  `pr-description-draft.md`.
- **T034** Fallback resilience test under `mod unsupported` — inject
  `ENOSYS` at the syscall boundary; verify transparent fallback.
- **T035** `cargo check --target x86_64-apple-darwin -p portunus-client`
  + `nm` symbol scan — confirm splice symbols absent from macOS binary
  (SC-006). Can in principle be done on the Linux host via
  `rustup target add x86_64-apple-darwin` plus the Apple linker
  cross-toolchain, but easier to run from the next darwin session.

## Suggested order of attack

1. **First commit**: T001 baseline (5 minutes, run a bench, capture
   JSON, commit it).
2. **Second commit**: T013-T016 — the four pieces of the splice loop.
   ~150 LoC. Author together with the four Linux integration tests
   (T007-T009, T012) and verify all four pass on this commit before
   moving on.
3. **Third commit**: T017-T019 — proxy.rs call site + tracing events
   + counter wiring. Verify with `cargo test --workspace` that no
   pre-existing test regresses.
4. **Fourth commit**: T021-T023 US2 tests — SNI / PROXY-then-splice.
5. **Fifth commit**: T020 — criterion bench.
6. **Sixth commit**: T028 — server-side hot-reload test.
7. **Seventh commit**: T029 — CI matrix axis.
8. **Eighth commit**: T032-T034 polish runs + capture numbers.
9. **Ninth commit**: Fill in `pr-description-draft.md` _TBD_ cells
   with actual bench numbers. Rename to a PR description proper, or
   just paste into the GitHub PR body.
10. **Open the PR**: second reviewer with named perf context required
    per Constitution. Bench numbers + byte-stability diff in the PR
    body.

## Things to double-check on Linux first

- `nix` 0.29 features `["fs", "net", "socket"]` expose `pipe2`,
  `fcntl(F_SETPIPE_SZ / F_GETPIPE_SZ)`, `Errno::*`. If something is
  missing, you'll need to add the feature in the workspace
  `Cargo.toml` (`nix = { ..., features = [..., "extra"] }`).
- `libc::splice` is available on Linux glibc and musl. The function
  signature is `unsafe extern "C" fn splice(fd_in, off_in,
  fd_out, off_out, len: usize, flags: c_uint) -> isize`. Wrap in
  a tiny `splice_raw` helper to avoid `unsafe_code` lint violations
  in the public API — the workspace already permits the raw syscall
  through `nix` etc., but `unsafe { libc::splice(...) }` will
  trigger `unsafe_code = forbid`. Best: gate one helper with
  `#[allow(unsafe_code)]` + inline rationale comment.

  Alternative: `nix::fcntl::splice` (under the `fs` feature) takes
  owned `BorrowedFd`s and avoids the raw-syscall question entirely.
  Verify it works inside `tokio::TcpStream::try_io` closures (the
  closure receives no fd argument, so we'd reborrow the
  `BorrowedFd` from the captured `as_raw_fd()`).

- `tracing-test` (used by T009's pipe-size-failed assertion) is a
  dev-dependency on `tracing` 0.1. If not already in
  `crates/portunus-client/Cargo.toml [dev-dependencies]`, add it:
  `tracing-test = "0.2"`.

## Cron status

The `/loop` cron job `c820310c` (every 10 min, "use speckit to
drive 012") was **cancelled** at handoff time. To resume periodic
driving on the Linux host:

```
/loop 10m 使用 speckit 来驱动 specs/012-tcp-zero-copy-splice/spec.md 任务的完成
```

(Session-only, expires after 7 days, cancel with `CronDelete <id>`.)

## Open questions for the operator

None. Spec, plan, research, data-model, contracts, quickstart,
tasks, and PR draft are all locked.

If during implementation you find something that contradicts a
locked decision, **stop**: it's a spec bug. Either update the spec
under a versioned "Clarifications" entry, or escalate; do not
silently diverge.
