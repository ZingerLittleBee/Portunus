# AGENTS.md

Guidance for AI coding assistants (Codex, Cursor, Claude Code, etc.)
working in this repository. Kept in sync with `CLAUDE.md`.

## Repository status

Stable release line. Latest tag is **v1.9.1** (see `CHANGELOG.md`); the
default branch is `main`. Everything through `014-udp-centralized-demux`
has shipped (v1.5.x); v1.6.x–v1.9.1 since added the standalone stats TUI,
forwarder hardening (UDP HOL, time-boxed PROXY prelude, bounded DNS
cache, accept-loop backoff), the AGPL relicense + static `musl` Linux
binaries, the Railway image-based deploy template, and a Web UI pass
(in-dialog forms, split Live/History audit views).

The current workstream is the **active feature** in the SPECKIT block at
the bottom of this file — `015-client-stable-id` on branch
`015-client-stable-id`. This is design/spec context, not yet released;
verify any claim against `git log` / `CHANGELOG.md`.

## Architecture

Rust workspace, edition 2024, MSRV 1.88. Eight crates under `crates/`:

- `portunus-proto` — gRPC schema (tonic-prost generated).
- `portunus-core` — shared IDs, errors, config, log-redaction.
- `portunus-forwarder` — shared data-plane library (TCP/UDP forwarders,
  resolver, shutdown). Consumed by both `portunus-client` and
  `portunus-standalone`. No `tonic` / `prost` / `portunus-proto`
  dependencies — proto-free.
- `portunus-auth` — `Authenticator` trait + token store.
- `portunus-server` — control-plane binary: gRPC + operator HTTP +
  Prometheus + embedded Web UI (rust-embed).
- `portunus-client` — edge binary: bidi gRPC stream + TCP/UDP forwarding.
- `portunus-standalone` — TOML-driven TCP/UDP forwarder binary with no
  gRPC control plane. Reuses `portunus-forwarder` end-to-end. See
  `crates/portunus-standalone/contrib/` for deployment templates and
  `docs/content/docs/configuration/standalone.mdx` for the user guide.
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
make standalone        # build portunus-standalone binary
make standalone-check  # validate every tests/fixtures/valid_*.toml
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
- **Linux release artifacts are static `musl` binaries**, not glibc.
  `.github/workflows/release.yml` builds `x86_64`/`aarch64-unknown-linux-musl`
  natively (each runner builds its own arch — no cross-compilation) with
  `musl-tools`; aws-lc-sys's cmake C build uses `CC_<target>=musl-gcc`.
  One binary runs on every Linux distro — glibc,
  Alpine/musl, busybox — and `install.sh` downloads the musl artifact.
  Docker images base on `distroless/static`. macOS stays native `cargo
  build`. Caveat: `recvmmsg`/`sendmmsg` flags are `c_int` on glibc but
  `u32` on musl, so `forwarder/udp/batch.rs` casts `MSG_DONTWAIT as _`
  to compile on both — preserve that when touching the UDP batch path.

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
Active feature: `015-client-stable-id` on branch
`015-client-stable-id`. This is a v2.0-level, workspace-wide refactor
that separates a client's **identity** from its **label**. Today the
user-supplied `client_name` is overloaded as the unique identifier,
the URL path segment, the SQLite primary key, and the log/metric
correlation key, forcing a strict DNS-label rule on it. The refactor
introduces a system-generated, stable, opaque `ClientId` (ULID, reusing
the existing `ulid` crate already used by `RuleId`/`RequestId`) that
takes over the identifier / URL / primary-key / correlation roles, and
demotes `client_name` to a free-form display field.

Key decisions (resolved 2026-06-02, see `spec.md`):
- **`client_name` is display-only.** Validation is relaxed to reject
  only empty/whitespace-only names, control characters, and names over a
  max length (assume ≤255); uppercase, spaces, dots, underscores, and
  Unicode are all allowed. `portunus-core`'s `ClientName` keeps a
  newtype but drops the DNS-label rule; a new `ClientId` newtype is
  added alongside it.
- **Rename is supported and identity-safe.** Changing a client's display
  name leaves its `ClientId` — and therefore all rules, tokens, quotas,
  and traffic history — intact, and does not drop a live session.
- **Names are NOT unique.** Duplicate display names are freely allowed
  (no warning); listings disambiguate via a short form of the id.
- **Additive wire change.** `client_id` is added to the control-plane
  schema (`EnrollClientRequest`, `CredentialBundle`,
  `OwnerRateLimitUpdate`, `TrafficQuotaUpdate`) while `client_name` is
  retained for display. `Hello`/`Welcome` carry no name today — client
  identity is resolved from the bearer token — so the data-plane stream
  is unaffected.
- **Transparent upgrade, no re-enrollment.** A pre-upgrade credential
  bundle (token, no id) still connects: the authenticated token resolves
  to the client's newly-assigned `ClientId` server-side.
- **SQLite V011 migration.** Seven `client_name`-keyed tables
  (`client_tokens`, `rules`, `rate_limit_owner`, `traffic_quotas`,
  `traffic_usage_minute`, `traffic_usage_hour`, `client_enrollments`)
  are re-keyed to `client_id`. `client_tokens` is the source of truth:
  assign an id per client there, then backfill the rest by name-join.
  SQLite primary-key changes require the build-new-table + copy + rename
  dance; the migration must be idempotent and crash-safe.
- **Operator surface re-keys to id.** HTTP routes become
  `/v1/clients/{id}/...`, CLI args and Web UI routes
  (`/clients/:clientId`) address by id; `ConnectedClients` switches from
  `HashMap<ClientName, _>` to keying on `ClientId`.
- **Metric label stays name.** Prometheus `client="…"` labels keep the
  human-readable name for dashboard readability; internal correlation
  uses the id. A renamed client remains one logical entity.

For technical context, the migration strategy, and the Constitution
Check, read the current plan once generated:
- `specs/015-client-stable-id/spec.md` (FR-001..FR-014, SC-001..SC-007).
- `specs/015-client-stable-id/plan.md` (pending `/speckit-plan`).
- Background dossier: `docs/refactor-client-id-handoff.md`.

Inherited baselines (do not re-derive):
- v1.5.x — `specs/014-udp-centralized-demux/plan.md`. UDP centralized
  demux: one `UdpRuleRuntime` per rule supervising a listener task per
  port, a shared `UdpFlowRegistry` keyed by `(listen_port, source_addr)`,
  per-rule flow cap, ICMP-driven flow eviction, `O(1)×64 KiB` recv
  buffer. No operator-surface change. 015 re-keys the per-client store
  layer but does not touch this UDP runtime.
- v0.13.0 — `docs/superpowers/plans/2026-05-14-traffic-quotas-and-history.md`.
  Per-rule and per-owner traffic quotas. The `traffic_quotas` /
  `traffic_usage_*` tables are among the seven that 015's V011 migration
  re-keys from `client_name` to `client_id`.
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
