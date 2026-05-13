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
Active feature: `011-rate-limiting-qos` on branch `011-rate-limiting-qos`.
v0.11 adds per-rule and per-owner connection rate limiting / QoS:
bandwidth (bytes/sec, both directions), new-connection rate (TCP
conn/sec or UDP flow/sec), and concurrent connection / flow count.
Each cap is independently optional; absent fields preserve v0.10
behaviour byte-for-byte. Bandwidth caps throttle in-flight flows via
a token bucket; connection-rate and concurrent caps reject new
connections (TCP RST after accept; UDP packet drop before NAT bind).
The rate limiter never closes existing connections — including under
hot-reload that lowers a concurrent cap below the live count
(graceful drain). Token-bucket implementation is hand-rolled; zero
new workspace deps.

Key invariants:
- "Per-client" cap is per-RBAC-owner within a portunus-client (Q1).
  Cap envelope keyed `(client, owner)`. Node-level aggregate caps
  are explicitly out of scope for v0.11.
- Wire fields are additive: `Rule.rate_limit = 12`,
  `RuleStats.rate_limit = 16`, `StatsReport.owner_rate_limit_stats = 4`,
  new server-push variant `OwnerRateLimitUpdate`. New messages
  `RateLimit`, `RateLimitStats`, `OwnerRateLimitStats`, enums
  `RateLimitRejectReason` (6 values) and `OwnerRateLimitAction`.
- Capability gate: `rate_limit` push (or any owner-cap mutation)
  to a pre-v0.11 client → `422 rate_limit_unsupported_by_client`
  before any rule activates anywhere.
- Per-owner ceiling binds **before** per-rule cap (FR-013); rejects
  carry distinct `owner_*` reasons (FR-014).
- Reject path: TCP accept-then-RST (Q3) — listener-pause was rejected
  because v0.7/v0.9 share listeners across rules.
- Burst defaults to `1 × rate`; optional per-cap `*_burst` field
  overrides (Q2). UI hides burst behind an "Advanced" disclosure.
- Hot-reload swaps `Arc<RateLimitConfig>`; concurrent cap lowered
  below live count drains gracefully (Q4) — no forced close.
- Per-owner cap REST path: `/v1/clients/{id}/owners/{owner_id}/rate-limit`
  (Q5). Web UI surfaces it as an "Owner quotas" tab on client detail.
- Data-plane reject/throttle events are tracing-only — they do NOT
  enter the SQLite operator audit ring (mirrors v0.9 D13 / v0.10
  invariant).
- SQLite migration V005 adds nullable cap columns to `rules` plus a
  new `rate_limit_owner` table; schema-version range
  `[1,3] → [1,4]`.

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/011-rate-limiting-qos/plan.md`
- Supporting artifacts in the same directory: `spec.md`,
  `research.md` (R-001..R-015 decisions), `data-model.md`,
  `contracts/wire.md`, `contracts/operator-api.md`, `quickstart.md`,
  `checklists/requirements.md`.

Project-wide governance: `.specify/memory/constitution.md` (currently v2.0.2 —
TLS + bearer token, NOT mTLS; SQLite as bundled persistence).
<!-- SPECKIT END -->
