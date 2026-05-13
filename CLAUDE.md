# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository status

Stable release line. Latest tag is **v1.2.0** (see `CHANGELOG.md`). The
active branch is `wellington-v1`. The SPECKIT block below is auto-managed
by speckit and may still reference an older feature
(`011-rate-limiting-qos`) until the next `/speckit-specify` run refreshes
it — treat that block as historical context, not as the currently active
workstream.

## Architecture

Rust workspace, edition 2024, MSRV 1.88. Six crates under `crates/`:

- `portunus-proto` — gRPC schema (tonic-prost generated).
- `portunus-core` — shared IDs, errors, config, log-redaction.
- `portunus-auth` — `Authenticator` trait + token store.
- `portunus-server` — control-plane binary: gRPC + operator HTTP +
  Prometheus + embedded Web UI (rust-embed).
- `portunus-client` — edge binary: bidi gRPC stream + TCP/UDP forwarding.
- `portunus-e2e` — process-level integration tests.

`webui/` is a React + Vite + TypeScript SPA compiled to `webui/dist/`
and embedded into `portunus-server` at compile time. There is no runtime
Node dependency on the deployment host.

Persistent server state lives in `<data-dir>/state.db` (SQLite,
bundled). Auth model is **TLS + bearer token**, not mTLS (Constitution
v2.0+ — see `.specify/memory/constitution.md`).

## Common commands

Prefer the `Makefile` over raw cargo for everyday work (run `make help`
for the full list):

```sh
make dev          # hot-reload backend + Vite UI together (http://localhost:5173)
make backend      # backend only, PORTUNUS_SKIP_WEBUI=1 (pair with `make ui`)
make ui           # Vite dev server only, proxies /v1 → 127.0.0.1:7080
make serve        # release server with embedded UI on http://127.0.0.1:7080
make test         # server lib tests + auth/password contract tests
make test-csrf    # focused CSRF unit tests (fast)
make clean        # nuke /tmp/portunus-dev (forces re-bootstrap)
```

Raw cargo when working across the workspace:

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo bench -p portunus-client --bench data_plane    # compares to v0.1.0 baseline
```

Run a single test:

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib operator::csrf::tests::same_origin_allowed
```

Web UI commands (from `webui/`):

```sh
pnpm install --frozen-lockfile
pnpm dev          # Vite dev server on :5173, expects backend on :7080
pnpm build        # tsc -b && vite build && size-limit (≤ 500 KB gz)
```

## Non-obvious patterns

- **`PORTUNUS_SKIP_WEBUI=1`** is required to build/run/test
  `portunus-server` without first running `pnpm build` in `webui/`. The
  `build.rs` of `portunus-server` errors if `webui/dist/index.html` is
  missing unless this env var is set. `make dev`/`make backend`/`make
  test` all set it.
- **Vite proxy is hard-coded** to `127.0.0.1:7080` in
  `webui/vite.config.ts`. Changing `LISTEN=` for `make dev` requires
  syncing that file too.
- **First-run bootstrap**: `make dev` auto-creates `_superadmin` and
  prints a `temporary_password=…` banner. Login at http://localhost:5173.
  The marker `$(DATA_DIR)/.dev-credentials-set` prevents re-bootstrap;
  `make clean` removes it.
- **Strict lints**: workspace sets `clippy::pedantic = warn`. CI gates
  on `-D warnings`. See `[workspace.lints.clippy]` in `Cargo.toml` for
  the intentional `allow` list and the reason each is allowed — do not
  remove an `allow` without re-reading that comment.
- **Data-plane perf gate**: `.github/workflows/bench.yml` fails PRs
  that regress median benchmark by >25% vs
  `crates/portunus-client/benches/baselines/v0.1.0.json`.
- **Operator HTTP listener is loopback-pinned** at startup. Remote
  access is an operator concern (SSH tunnel or reverse proxy with auth).
- **Data-plane reject/throttle events are tracing-only** — they do NOT
  enter the SQLite operator audit ring (mirrors v0.9 D13 / v0.10 / v0.11
  invariant).

## Spec-driven workflow

This repo uses speckit for feature work: each
`specs/NNN-feature-name/` directory contains `spec.md`, `plan.md`,
`tasks.md`, `contracts/`. The SPECKIT block in this file is regenerated
when a new feature is created via `/speckit-specify`. Historical feature
plans (v0.1.0 – v0.11.0) remain in `specs/` for reference.

## Documentation pointers

- `README.md` — install, basic flow, operator API entry points.
- `Makefile` — every target has a `##` help comment (try `make help`).
- `docs/runbook.md` — day-1/day-2 ops, troubleshooting.
- `webui/README.md` — frontend toolchain & bundle-size budget.
- `.specify/memory/constitution.md` — project principles (auth model,
  perf gates).

<!-- SPECKIT START -->
Active feature: `012-tcp-zero-copy-splice` on branch `wellington-v1`
(work in an isolated worktree). v1.3.0 adds an internal, operator-
invisible TCP zero-copy fast path on Linux via `splice(2)` + a
per-connection `pipe2` pair. The `tokio::io::copy_bidirectional_with_sizes`
userspace path remains the canonical reference and the fallback for
non-Linux platforms and ineligible rules.

Key invariants:
- Operator surface is empty: no wire field, no operator-API field,
  no Web UI control, no rule flag, no `--help` advertisement.
  `PORTUNUS_DISABLE_SPLICE=1` is an internal env kill switch for
  triage / bench A/B only.
- Eligibility predicate: `cfg(target_os = "linux") &&
  protocol == Tcp && !disable_splice && !has_bandwidth_cap`, where
  `has_bandwidth_cap` is the OR of {rule.bandwidth_in_bps,
  rule.bandwidth_out_bps, owner.bandwidth_in_bps,
  owner.bandwidth_out_bps}.
- `concurrent_connections` and `new_connections_per_sec` (v0.11)
  gate at accept time and remain compatible — they do not force the
  userspace path.
- SNI peek+replay (v0.9) and PROXY-out prelude (v0.10) are
  prefix-only; splice runs for the post-prelude byte stream and
  benefits these rules.
- Fallback contract: a connection may transition to the userspace
  path **only** when the first splice syscall returns one of
  {`ENOSYS`, `EINVAL`, `EPERM`, `EOPNOTSUPP` / `ENOTSUP`} AND zero
  bytes have moved into the pipe. After any byte has moved, errors
  are terminal `io::Error` (no path switch).
- Tokio reactor integration uses `TcpStream::try_io` +
  `readable() / writable()`; no `AsyncFd` (avoids double registration
  and ownership churn).
- Per-connection `pipe2(O_NONBLOCK | O_CLOEXEC)` pair with
  best-effort `F_SETPIPE_SZ` = 1 MiB. `F_SETPIPE_SZ` failure is
  `tracing::debug`, never a connection failure.
- Half-close semantics mirror `tokio::io::copy_bidirectional`
  exactly. Byte counters advance only on the pipe-to-destination
  splice return value (delivered bytes, not received bytes —
  mirrors `copy_bidirectional_with_sizes` semantics).
- Three new tracing events under `proxy.*`:
  `proxy.splice_selected` (info, once per rule),
  `proxy.splice_unsupported_fallback` (warn, per fallback connection),
  `proxy.splice_pipe_size_failed` (debug, per affected connection).
  No new Prometheus metrics; existing counters are bit-identical
  across paths (SC-004).
- No new workspace dependencies. `nix` (already in workspace)
  provides the safe wrappers for `splice`, `pipe2`, `fcntl`.
- Constitution Principle II perf gate: `criterion` bench
  `splice_throughput.rs` validates SC-001 (≥ 1.4× throughput on
  1 MiB chunks, Linux perf host only — not CI) and SC-002 (p99
  setup latency within ±5 %). v1.2.0 baseline captured **before**
  any splice code lands.
- Constitution Principle III: integration tests use real sockets
  (loopback acceptable); existing `cargo test --workspace` passes
  identically with and without `PORTUNUS_DISABLE_SPLICE=1` (SC-003
  byte-stability gate).

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/012-tcp-zero-copy-splice/plan.md`
- Supporting artifacts in the same directory: `spec.md`,
  `research.md` (R-001..R-010 decisions), `data-model.md`,
  `contracts/internal-api.md`, `quickstart.md`.

Inherited baselines (do not re-derive):
- v0.11.0 — `specs/011-rate-limiting-qos/plan.md`. Per-rule and
  per-owner rate limiting / QoS. v1.3.0 consults v0.11's bandwidth-cap
  presence (rule + owner) as the eligibility gate; concurrent /
  new-conn caps remain accept-time gates and stay on the fast path.
- v0.10.0 — `specs/010-proxy-protocol-and-peek-histogram/plan.md`.
  Per-target PROXY v1/v2 prelude + SNI ClientHello peek-duration
  histogram. v1.3.0 runs **after** the PROXY prelude write completes;
  prelude itself is byte-identical to v1.2.0.
- v0.9.0 — `specs/009-tls-sni-routing/plan.md`. Per-listener SNI
  dispatch. v1.3.0 runs **after** SNI peek+replay; the peeked bytes
  reach the upstream byte-identical and the remaining stream uses
  the fast path.
- v0.8.0 — `specs/008-sqlite-storage/plan.md`. v1.3.0 makes no
  schema changes; SQLite store untouched.
- v0.7.0 — `specs/007-multi-target-failover/plan.md`. Multi-target
  rules; v1.3.0 selection is per-connection at accept time and does
  not interact with failover.
- v0.6.0 — `specs/006-management-web-ui/plan.md`. React+Vite SPA;
  v1.3.0 adds no UI surface.
- v0.5.0 — `specs/005-multi-user-rbac/plan.md`. RBAC owner is the
  tenant boundary v1.3.0's per-owner-bandwidth check keys on.
- v0.4.0 — `specs/004-udp-forward/plan.md`. UDP first-packet
  enforcement; v1.3.0 is TCP only — UDP rules always use the
  existing recv/send path.
- v0.3.0 — `specs/003-domain-name-forward/plan.md`. DNS resolver
  unchanged.
- v0.2.0 — `specs/002-port-range-forward/plan.md` (range rules);
  v1.3.0 applies per-connection independent of range parent.
- v0.1.0 — `specs/001-tcp-forward-mvp/plan.md` (TCP forwarding MVP).

Project-wide governance: `.specify/memory/constitution.md` (currently
v2.0.2 — TLS + bearer token, NOT mTLS; SQLite as bundled persistence;
`splice` permitted as soft optimization under `TODO(KERNEL_OFFLOAD)`).
<!-- SPECKIT END -->
