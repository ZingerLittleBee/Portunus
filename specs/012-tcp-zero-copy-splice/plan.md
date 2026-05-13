# Implementation Plan: TCP Zero-Copy Fast Path (Linux splice)

**Branch**: `wellington-v1` (work performed in an isolated worktree)
**Date**: 2026-05-13
**Spec**: [spec.md](./spec.md)
**Input**: Feature specification at `specs/012-tcp-zero-copy-splice/spec.md`

## Summary

Add a `splice(2)`-based zero-copy bidirectional copy primitive on Linux that
the `portunus-client` data plane uses automatically when a TCP rule has no
per-rule or per-owner bandwidth cap. The optimization is **operator-invisible**
(no wire field, no config knob, no Web UI control) and **silently absent on
non-Linux** (compiled out under `#[cfg(target_os = "linux")]`). The userspace
`tokio::io::copy_bidirectional_with_sizes` path remains the canonical, fallback,
and byte-stable reference implementation.

The design decisions locked in during brainstorming (see spec § Open Questions
closure):

- Borrowed-reference signatures throughout — `splice::copy_bidirectional` never
  takes ownership of `TcpStream`, so fallback never destroys caller state.
- `Unsupported`-only fallback **before the first moved byte**. Errors after
  any byte has entered the kernel pipe propagate as connection-level
  `io::Error`.
- `TcpStream::try_io` + `TcpStream::readable() / writable()` for Tokio reactor
  integration — no `AsyncFd` second-registration layer.
- Per-connection `pipe2(O_NONBLOCK | O_CLOEXEC)` pair, RAII-closed on drop.
  `fcntl(F_SETPIPE_SZ, 1 MiB)` is best-effort; failure is a `tracing::debug`,
  not a connection failure.

## Technical Context

**Language/Version**: Rust 1.88 (workspace MSRV; driven by `tonic`)
**Primary Dependencies**:
- `nix` 0.x (already a workspace dep) — safe wrappers for `splice(2)`,
  `pipe2(2)`, `fcntl(2)` with the `F_SETPIPE_SZ` operation.
- `tokio` (existing) — `TcpStream::try_io`, `readable()`, `writable()`,
  `Interest::READABLE / WRITABLE`, `try_join!`.
- `libc` (transitively via `nix`) — errno constants (`ENOSYS`, `EINVAL`,
  `EPERM`, `EOPNOTSUPP`).
- **No new workspace dependencies added.**

**Storage**: N/A — pure data-plane optimization. SQLite state, rule store,
operator API are untouched.

**Testing**:
- `cargo test --workspace` — existing unit / integration tests must pass
  unchanged with the optimization enabled.
- `cargo test --workspace` again with `PORTUNUS_DISABLE_SPLICE=1` set — same
  outcome (SC-003 byte stability).
- `cargo bench -p portunus-client --bench splice_throughput` — new criterion
  bench validating SC-001 / SC-002 on a dedicated perf host.
- `crates/portunus-e2e` whole-suite double run (splice on / off).

**Target Platform**:
- **Primary (optimized)**: Linux 5.10+ (Docker base image baseline).
- **Fallback**: Linux < 5.10 (older `splice` semantics → `Unsupported` first
  syscall → userspace path).
- **No-op**: macOS, Windows. `#[cfg(target_os = "linux")]` compiles the
  fast-path module out entirely.

**Project Type**: CLI / data-plane binary inside an existing Rust workspace
(`portunus-client`, the edge forwarder).

**Performance Goals** (from spec SC-001/SC-002):
- ≥ 1.4× single-connection 1 MiB-chunk throughput on dedicated Linux perf
  host, optimization on vs. `PORTUNUS_DISABLE_SPLICE=1`.
- p99 connection-setup latency within ±5 % of baseline.

**Constraints**:
- Constitution Principle II: hot-path PR requires criterion bench, regression
  >5 % blocked. Splice path is a new hot path → bench required.
- Byte-stability across SNI / PROXY-out / rate-limited rules — every existing
  integration test passes with optimization toggled.
- Zero new operator-visible surface. Zero new wire fields.
- `unsafe_code = "forbid"` workspace-wide. `nix` already provides the safe
  wrappers; no raw syscalls in our code.

**Scale/Scope**:
- ~1 new source file (`crates/portunus-client/src/forwarder/splice.rs`),
  ~250–400 LoC including tests.
- ~10 lines of changes to `crates/portunus-client/src/forwarder/proxy.rs`
  (the call-site branch).
- 1 new criterion bench file (~150 LoC) + 1 baseline JSON capture.
- No changes to `portunus-proto`, `portunus-server`, `portunus-auth`,
  `portunus-core`, `webui/`.

## Constitution Check

*Gate evaluation against `.specify/memory/constitution.md` v2.0.2.*

| Principle | Status | Notes |
|---|---|---|
| **I. Security by Default** | ✅ N/A | No control-plane, auth, TLS, or token-handling changes. The optimization sits below all auth boundaries. |
| **II. Performance Is a Feature** | ✅ PASS | This **is** the perf feature. Spec requires criterion bench (SC-001/SC-002). Plan ships `benches/splice_throughput.rs` and baseline-vs-optimization comparison. Regression gate already enforced by existing CI on `data_plane.rs` bench; new bench inherits the same pattern. |
| **III. Test-First Discipline** | ✅ PASS | Plan's Phase 1 schedules contract / integration test scaffolds **before** the splice implementation. Real-socket loopback tests required by integration tier (mocks rejected by constitution). |
| **IV. Observability & Operability** | ✅ PASS | New tracing events `proxy.splice_selected`, `proxy.splice_unsupported_fallback`, `proxy.splice_pipe_size_failed` use existing `proxy.*` namespace. No new Prometheus metrics — existing `bytes_in / bytes_out` counters retain identical semantics. Graceful reload unaffected — selection is per-connection at accept time. Shutdown drains as today (RAII pipe close on connection drop). |
| **V. Multi-Tenant Isolation** | ✅ PASS | Eligibility predicate consults per-owner bandwidth cap as well as per-rule cap. SC-005 cross-checks that all v0.11 rate-limit SCs continue to hold. No shared state between connections (each connection owns its own `PipePair`). |
| **Tech Constraints — Data path** | ✅ PASS | Constitution explicitly permits `splice` as a soft optimization under `TODO(KERNEL_OFFLOAD)`. Plan keeps userspace path as canonical reference; splice is opt-out via env var. **No constitutional amendment required.** |
| **Tech Constraints — Async runtime** | ✅ PASS | Stays on Tokio. No custom executor. `TcpStream::try_io` is Tokio's recommended pattern. |
| **Tech Constraints — Supported platforms** | ✅ PASS | Linux primary (optimized), macOS dev (no-op cfg-out), Windows out of scope (no-op cfg-out). |
| **Review gate** | ✅ NOTED | Forwarding hot path → requires a second reviewer with named performance context per `Development Workflow & Quality Gates`. Plan documents this requirement in the PR template guidance below. |

**Gate result**: PASS, no violations. Complexity Tracking table empty.

## Project Structure

### Documentation (this feature)

```text
specs/012-tcp-zero-copy-splice/
├── plan.md              # This file
├── spec.md              # User-facing requirements, committed 2026-05-13
├── research.md          # Phase 0 — design rationale (R-001..R-010)
├── data-model.md        # Phase 1 — internal types (CopyCtx, PipePair, etc.)
├── quickstart.md        # Phase 1 — bench-operator quickstart
├── contracts/
│   └── internal-api.md  # Internal seam contracts (signatures, events,
│                        # env var, metric continuity)
└── tasks.md             # Phase 2 — emitted by /speckit-tasks (NOT here)
```

### Source Code (repository root)

```text
crates/portunus-client/
├── src/
│   ├── forwarder/
│   │   ├── mod.rs                  # unchanged
│   │   ├── proxy.rs                # ~10 LoC delta: eligibility + branch
│   │   ├── splice.rs               # NEW — Linux fast path (cfg-gated module)
│   │   │                           # contains: CopyCtx (inputs), eligible(),
│   │   │                           # copy_bidirectional(), PipePair (RAII),
│   │   │                           # splice_dir() one-direction loop,
│   │   │                           # SpliceError, Transferred
│   │   ├── rate_limit/             # unchanged
│   │   ├── sni/                    # unchanged
│   │   ├── failover.rs             # unchanged
│   │   └── failover_path.rs        # unchanged
│   └── ...                         # all other modules unchanged
└── benches/
    ├── data_plane.rs               # unchanged — v1.2.0 baseline reference
    ├── splice_throughput.rs        # NEW — SC-001/SC-002 perf gate bench
    └── baselines/
        ├── v0.1.0.json             # unchanged
        └── v1.2.0.json             # NEW — captured before any splice code
                                    # lands; compared against post-splice
                                    # numbers
```

**Structure Decision**: Single project layout (Option 1 from the template).
The optimization is contained inside one existing crate
(`crates/portunus-client`). No new crates, no module restructure, no
inter-crate wire changes. `splice.rs` is a sibling module of `proxy.rs` and
is imported only from `proxy.rs`.

The deliberate isolation — `splice.rs` is the **only** file that touches
`libc::splice` / `pipe2` / `fcntl(F_SETPIPE_SZ)` — means a future swap to
`io_uring` (out of scope per spec) would replace this one file with no
ripple into `proxy.rs`, `failover_path.rs`, or any rate-limit code.

## Phase Outputs

- **Phase 0** → `research.md`: ten R-001..R-010 decisions (splice vs.
  sendfile, pipe lifecycle, pipe size, AsyncFd vs. try_io, batch length,
  fallback errno set, half-close, counter accounting point, bench shape,
  tracing names).
- **Phase 1** → `data-model.md`, `contracts/internal-api.md`,
  `quickstart.md`. The CLAUDE.md SPECKIT block is also updated to point
  at this plan.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

*Empty — Constitution Check passed with no violations.*

## PR Review Guidance

Per Constitution `Development Workflow & Quality Gates`:

- This PR touches the forwarding hot path → **second reviewer with named
  performance context required** in addition to the primary reviewer.
- Bench numbers (criterion output for `splice_throughput`, on the dedicated
  perf host the team uses for v0.11 SC-001) must appear in the PR
  description. Baseline-vs-optimization comparison; ≥ 1.4× throughput on
  1 MiB chunks, ≤ 5 % p99 setup-latency drift.
- A second `cargo test --workspace` invocation with
  `PORTUNUS_DISABLE_SPLICE=1` must be captured in the PR (CI does this in
  one matrix axis).
- CHANGELOG.md gets an `### Added — TCP zero-copy fast path on Linux`
  entry under the next version section, with operator-visible wording
  ("automatically uses Linux splice for TCP forwarding when no bandwidth
  caps apply; transparent fallback otherwise; can be force-disabled via
  `PORTUNUS_DISABLE_SPLICE=1` for triage").
