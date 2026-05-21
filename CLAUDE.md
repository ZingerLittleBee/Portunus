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
Active feature: `014-udp-centralized-demux` on branch
`014-udp-centralized-demux`. v1.5.x corrects the UDP data-plane
flow-cap semantics and collapses per-flow receive buffers into a
single per-rule centralized demux. The v0.4 per-port-listener model
(each listen port held its own `recv` loop and `UdpFlowTable`) is
replaced by a single `UdpRuleRuntime` supervising one listener task
per port, a shared `UdpFlowRegistry` keyed by `(listen_port,
source_addr)`, and a single idle-window reaper.

Key invariants:
- **No operator surface change.** No new wire field, operator-API
  field, Web UI control, or `--help` flag. The runtime is an
  internal refactor; `udp_max_flows_per_rule` is the same setting,
  just enforced correctly.
- **Per-rule flow cap** (FR-002 / SC-002): `rule_cap` is a single
  registry-wide counter. A range rule with `cap=N` admits at most
  `N` concurrent flows across **all** listen ports, not `N × range_size`.
- **`portunus_rule_active_flows` reflects registry size** (FR-014):
  the gauge reads `registry.len()` directly; no more per-port
  `AtomicU32` last-writer-wins drift.
- **Upstream sockets are `connect()`-ed at flow creation** (SC-005):
  multi-A target selection happens once at the `connect()` seam.
  On Linux this enables ICMP error reflection (`ECONNREFUSED`,
  `EHOSTUNREACH`, `ENETUNREACH`) — affected flows are evicted
  immediately and the next datagram rebuilds the flow against a
  freshly-selected target.
- **No mid-flow multi-A fallback.** v0.4's `udp_send_to_fallback`
  / `udp_send_to_exhausted` tracing paths are removed; the ICMP-
  driven eviction provides equivalent coarse failover with
  unambiguous connected-socket semantics.
- **Receive-buffer memory** (SC-001a): per-rule recv buffer is
  `O(1) × 64 KiB`, not `O(flows) × 64 KiB`. The listener task owns
  the single 64 KiB heap buffer used by `recv_from`.
- **Ordered shutdown** (FR-015): on cancellation the supervisor
  stops accepting new flows, drains the registry, then joins
  listener/reaper tasks. `rule.udp_shutdown_unexpected_exit`
  fires only when a child task exits unexpectedly.
- **Three pre-existing failure paths get explicit tracing events**
  (FR-011/FR-013): `rule.udp_upstream_connect_failed`,
  `rule.udp_addflow_dropped`, `rule.udp_flow_evicted_icmp`,
  `rule.udp_reply_wouldblock`, `rule.udp_emsgsize`,
  `rule.udp_runtime_started`, `rule.udp_shutdown_unexpected_exit`.
  No new Prometheus metrics.
- **No new workspace dependencies.** The runtime reuses existing
  `tokio`, `tokio-util` (CancellationToken), `nix`, `tracing`.
- **Constitution Principle II perf gate**: `criterion` bench
  `udp_high_flow_count` validates SC-001a (RSS delta ≪ N × 64 KiB
  at N=1000 concurrent flows, Linux perf host only — not CI) and
  SC-004 (single-flow throughput / RTT scenarios stay within ±5 %).
  v1.4.3 baseline captured before this branch.
- **Constitution Principle III**: integration tests use real
  loopback sockets (`udp_range_rule_cap_is_per_rule`,
  `udp_smoke_icmp_evict`); `cargo test --workspace` is green
  (modulo the known macOS-only `udp_smoke` flake that pre-dates
  this branch).

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/014-udp-centralized-demux/plan.md`
- `specs/014-udp-centralized-demux/spec.md` (FR-001..FR-017,
  SC-001a..SC-006).

Inherited baselines (do not re-derive):
- v0.13.0 — `docs/superpowers/plans/2026-05-14-traffic-quotas-and-history.md`.
  Per-rule and per-owner traffic quotas. v1.5.x's flow-creation
  path consults the quota allow-list before installing a new
  registry entry; quota exhaustion is a flow rejection, not a
  mid-flow drop.
- v0.12.0 — `specs/012-tcp-zero-copy-splice/plan.md`. Linux
  `splice(2)` fast path; TCP-only, untouched by v1.5.x.
- v0.11.0 — `specs/011-rate-limiting-qos/plan.md`. Per-rule and
  per-owner rate limiting / QoS. v1.5.x runs the layered rate
  limiter at flow creation and (when configured) before each
  upstream `send_to` inside the listener's hot path.
- v0.10.0 — `specs/010-proxy-protocol-and-peek-histogram/plan.md`.
  PROXY protocol prelude (TCP-only); UDP is unaffected.
- v0.9.0 — `specs/009-tls-sni-routing/plan.md`. SNI dispatch
  (TCP-only).
- v0.8.0 — `specs/008-sqlite-storage/plan.md`. v1.5.x makes no
  schema changes; SQLite store untouched.
- v0.7.0 — `specs/007-multi-target-failover/plan.md`. Multi-A
  failover; v1.5.x selects at the `connect()` seam, then locks
  the flow. ICMP-evict + flow rebuild provides coarse failover.
- v0.6.0 — `specs/006-management-web-ui/plan.md`. React+Vite SPA;
  v1.5.x adds no UI surface.
- v0.5.0 — `specs/005-multi-user-rbac/plan.md`. RBAC owner is the
  tenant boundary the per-owner rate limit / quota keys on.
- v0.4.0 — `specs/004-udp-forward/plan.md`. UDP first-packet
  enforcement; v1.5.x supersedes the per-port-listener model with
  the rule-wide `UdpRuleRuntime` supervisor. v0.4's `udp_send_to_*`
  tracing events are removed.
- v0.3.0 — `specs/003-domain-name-forward/plan.md`. DNS resolver
  unchanged.
- v0.2.0 — `specs/002-port-range-forward/plan.md` (range rules);
  v1.5.x enforces caps at the rule level, not the port level.
- v0.1.0 — `specs/001-tcp-forward-mvp/plan.md` (TCP forwarding MVP).

Project-wide governance: `.specify/memory/constitution.md` (currently
v2.0.2 — TLS + bearer token, NOT mTLS; SQLite as bundled persistence;
`splice` permitted as soft optimization under `TODO(KERNEL_OFFLOAD)`).
<!-- SPECKIT END -->
