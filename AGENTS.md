# AGENTS.md

Guidance for AI coding assistants (Codex, Cursor, Claude Code, etc.)
working in this repository. Kept in sync with `CLAUDE.md`.

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
  missing unless this env var is set.
- **Vite proxy is hard-coded** to `127.0.0.1:7080` in
  `webui/vite.config.ts`. Changing `LISTEN=` for `make dev` requires
  syncing that file too.
- **First-run bootstrap**: `make dev` auto-creates `_superadmin` and
  prints a `temporary_password=…` banner. Login at http://localhost:5173.
  The marker `$(DATA_DIR)/.dev-credentials-set` prevents re-bootstrap;
  `make clean` removes it.
- **Strict lints**: workspace sets `clippy::pedantic = warn`. CI gates
  on `-D warnings`. See `[workspace.lints.clippy]` in `Cargo.toml` for
  the intentional `allow` list and the reason each is allowed.
- **Data-plane perf gate**: `.github/workflows/bench.yml` fails PRs
  that regress median benchmark by >25% vs
  `crates/portunus-client/benches/baselines/v0.1.0.json`.
- **Operator HTTP listener is loopback-pinned** at startup. Remote
  access is an operator concern (SSH tunnel or reverse proxy with auth).
- **Data-plane reject/throttle events are tracing-only** — they do NOT
  enter the SQLite operator audit ring.

## Spec-driven workflow

This repo uses speckit for feature work: each
`specs/NNN-feature-name/` directory contains `spec.md`, `plan.md`,
`tasks.md`, `contracts/`. The SPECKIT block below is regenerated when a
new feature is created via `/speckit-specify`. Historical feature plans
(v0.1.0 – v0.11.0) remain in `specs/` for reference.

## Documentation pointers

- `README.md` — install, basic flow, operator API entry points.
- `Makefile` — every target has a `##` help comment (try `make help`).
- `docs/runbook.md` — day-1/day-2 ops, troubleshooting.
- `webui/README.md` — frontend toolchain & bundle-size budget.
- `.specify/memory/constitution.md` — project principles (auth model,
  perf gates).

<!-- SPECKIT START -->
Active feature: `012-tcp-zero-copy-splice` on branch `wellington-v1`
(work in an isolated worktree). v1.3.0 adds an internal,
operator-invisible TCP zero-copy fast path on Linux via `splice(2)` +
a per-connection `pipe2` pair. The
`tokio::io::copy_bidirectional_with_sizes` userspace path remains the
canonical reference and the fallback for non-Linux platforms and
ineligible rules. No wire / config / Web UI surface; no new workspace
dependencies.

Key invariants:
- Eligibility: `cfg(target_os = "linux") && protocol == Tcp &&
  !disable_splice && !has_bandwidth_cap`. `has_bandwidth_cap` is the OR
  of {rule.bandwidth_in_bps, rule.bandwidth_out_bps,
  owner.bandwidth_in_bps, owner.bandwidth_out_bps}.
- `concurrent_connections` and `new_connections_per_sec` (v0.11) gate at
  accept time and remain compatible with the fast path.
- SNI peek+replay (v0.9) and PROXY-out prelude (v0.10) are prefix-only;
  splice runs for the post-prelude byte stream.
- Fallback contract: only when the **first** `splice` syscall returns
  one of {`ENOSYS`, `EINVAL`, `EPERM`, `EOPNOTSUPP`/`ENOTSUP`} AND zero
  bytes have moved. After any byte moved → terminal `io::Error`, no
  path switch.
- Tokio integration: `TcpStream::try_io` + `readable()`/`writable()`;
  no `AsyncFd`.
- Per-connection `pipe2(O_NONBLOCK | O_CLOEXEC)` pair with best-effort
  `F_SETPIPE_SZ = 1 MiB`; failure is `tracing::debug`.
- Half-close matches `tokio::io::copy_bidirectional`; counters advance
  on the pipe-to-destination splice return value (delivered bytes).
- Tracing events under `proxy.*`: `proxy.splice_selected` (info, once
  per rule), `proxy.splice_unsupported_fallback` (warn, per fallback
  connection), `proxy.splice_pipe_size_failed` (debug). No new
  Prometheus metrics.
- `PORTUNUS_DISABLE_SPLICE=1` env is the only kill switch
  (internal/triage; not advertised in `--help`).
- Perf gate (Constitution II): criterion `splice_throughput` bench on
  dedicated Linux host — ≥ 1.4× throughput on 1 MiB chunks, p99 setup
  latency within ±5 %; v1.2.0 baseline captured **before** any splice
  code lands. Byte-stability gate: full integration suite passes
  identically with and without `PORTUNUS_DISABLE_SPLICE=1`.

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/012-tcp-zero-copy-splice/plan.md`
- Supporting artifacts in the same directory: `spec.md`,
  `research.md` (R-001..R-010 decisions), `data-model.md`,
  `contracts/internal-api.md`, `quickstart.md`.

Project-wide governance: `.specify/memory/constitution.md` (currently
v2.0.2 — TLS + bearer token, NOT mTLS; SQLite as bundled persistence;
`splice` permitted as soft optimization under `TODO(KERNEL_OFFLOAD)`).
<!-- SPECKIT END -->
