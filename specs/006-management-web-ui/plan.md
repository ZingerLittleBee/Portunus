# Implementation Plan: Management Web UI

**Branch**: `006-management-web-ui` | **Date**: 2026-05-07 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `specs/006-management-web-ui/spec.md`

## Summary

Ship the long-deferred `TODO(WEB_UI)` from the constitution: a single-page
browser app that lets operators run the v0.5 RBAC + forwarding stack
without dropping into the CLI. The SPA is built with **React + TypeScript +
Vite + shadcn/ui + TanStack Query + React Router**, embedded into the
`portunus-server` binary at compile time via the `rust-embed` crate and served
on the existing operator HTTP listener at `/`. Two new server endpoints are
added (additive, ownership-checked):

- `GET /v1/rules/{id}/stats/stream` — text/event-stream backed by a
  per-rule `tokio::sync::broadcast` so N concurrent subscribers cost
  O(rules) not O(rules × subscribers).
- `GET /v1/audit?limit=N` — superadmin-only read of an in-memory ring
  buffer (size 1000) populated by the existing `auth_layer` allow/deny
  emit sites.

Lists use **client-side virtual scrolling** (`@tanstack/react-virtual`),
no server-side pagination — preserves the v0.5 contract additive-only.
Auth = bearer token in `sessionStorage`; on 401 the UI clears + bounces to
login. i18n covers English + 简体中文; theme follows
`prefers-color-scheme`. Bundle size budget: ≤ 500 KB gzipped.

## Technical Context

**Language/Version**:
- Server (existing): Rust 1.88 (workspace MSRV) — no language change.
- UI: TypeScript 5.x targeting ES2022, transpiled to ES2020 for bundle.

**Primary Dependencies**:
- Server: + `rust-embed = "8"` (compile-time embed of `webui/dist/`),
  + `axum::response::sse` (already in axum 0.8 transitive).
- UI: React 18 + Vite 5 + TypeScript 5 + Tailwind 3 + shadcn/ui (Radix
  primitives) + TanStack Query 5 + React Router 6 + `@tanstack/react-virtual`
  + `i18next` + `react-i18next`. Lockfile authoritative, no auto-bumps.

**Storage**:
- Server: in-memory `tokens.json` + `identity.json` (unchanged from v0.5).
  New: in-memory `Mutex<VecDeque<AuditEntry>>` ring buffer (1000-entry cap).
  No new disk persistence.
- UI: `sessionStorage` (token, role) + `localStorage` (theme, language).

**Testing**:
- Server: existing `cargo test --workspace --tests` suite + 2 new contract
  tests (`tests/audit_contract.rs`, `tests/rule_stats_stream_contract.rs`).
- UI: Vitest (unit), Playwright (e2e against a real `portunus-server`
  spawned by a pre-test fixture). Initial Playwright suite covers the
  three P1/P2 user stories from spec.md.

**Target Platform**:
- Server: Linux x86_64 / aarch64 + macOS dev (matches v0.5).
- UI: latest two releases of Chrome / Firefox / Safari / Edge.
  No IE / pre-Chromium Edge support.

**Project Type**:
- Existing: Rust workspace with binary crates (server / client) + library
  crates (core / proto / auth / e2e).
- New: a separate `webui/` Vite project at the repo root. Its build
  artefact (`webui/dist/`) is consumed at compile time by
  `portunus-server` via `rust-embed`. Decision below in **Project Structure**.

**Performance Goals**:
- Initial page load (LCP) ≤ **2 s** on a developer-class laptop with
  warm cache, ≤ **3 s** cold.
- List page interaction (scroll / sort / filter) ≤ **16 ms / frame**
  (60 fps target) up to 10k rows by virtualisation.
- Mutation round-trip (e.g., create user) reflected in list ≤ **1 s**.
- Live stats lag ≤ **6 s** from server-side stats interval (SC-004).

**Constraints**:
- **Bundle size**: gzipped JS ≤ 500 KB, hard fail at build time if
  exceeded (vite plugin + size-limit checker in CI).
- **Single binary**: zero new runtime dependencies on the deployment
  host. Node is a build-time tool only.
- **Loopback only**: UI served on the existing
  `cfg.operator_http_listen` (loopback-pinned via runtime assertion in
  `serve.rs`); remote access remains an operator concern (SSH tunnel /
  reverse proxy).
- **Token hygiene (Principle IV)**: bearer never in URL, query string,
  history, or DOM text; `sessionStorage` only.
- **Additive only**: every existing v0.5 HTTP endpoint, CLI subcommand,
  and integration test passes byte-identical post-merge.

**Scale/Scope**:
- Up to 10k rules / 100 users / 100 grants per server (v0.5 in-memory
  data model bounds; UI virtualisation handles this comfortably).
- ≤ 100 concurrent browser sessions (one operator team), each watching
  ≤ 10 live-stats panels = ≤ 1000 SSE subscribers fanning out from
  ≤ 10k broadcast sources — well under axum / hyper limits.
- Audit ring buffer: 1000 entries, ≈ 200 KB resident memory.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Compliance | Notes |
|---|---|---|
| **I. Security by Default** | ✅ | Bearer token sent in `Authorization` header only; UI never bypasses `auth_middleware`; cert pinning unaffected; audit emit unchanged. SSO and mTLS remain deferred (`TODO(MTLS_REVISIT)`). |
| **II. Performance Is a Feature** | ✅ | Forwarding hot path is **not touched**. The SSE broadcast endpoint reads from the existing `RuleStatsCache`; no new allocations on the per-packet path. UI rendering is virtualised; bundle gated at 500 KB. |
| **III. Test-First Discipline** | ✅ | Two new server endpoints get contract tests (`tests/audit_contract.rs`, `tests/rule_stats_stream_contract.rs`) before implementation. UI uses Vitest + Playwright; e2e walkthrough mirrors quickstart sections. |
| **IV. Observability & Operability** | ✅ | Audit endpoint is a READ surface over the same structured log records the auth_layer already emits. New endpoints emit standard `operator.allow` / `operator.deny` log lines. No new tokens or secrets logged. |
| **V. Multi-Tenant Isolation** | ✅ | UI never re-implements RBAC; it consumes the server's already-filtered responses. New SSE endpoint runs the same ownership check the existing `GET /v1/rules/{id}/stats` does. Audit endpoint is superadmin-only at the server level. |

**Decision**: PASS. No Complexity Tracking entries needed.

## Project Structure

### Documentation (this feature)

```text
specs/006-management-web-ui/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md        # Phase 1 output (/speckit-plan command)
├── quickstart.md        # Phase 1 output (/speckit-plan command)
├── contracts/           # Phase 1 output (/speckit-plan command)
│   ├── audit-endpoint.md       # GET /v1/audit JSON shape + auth + pagination
│   ├── stats-stream-endpoint.md # GET /v1/rules/{id}/stats/stream SSE shape
│   └── ui-routes.md            # SPA route table + role gates + URL params
└── tasks.md             # Phase 2 output (/speckit-tasks command)
```

### Source Code (repository root)

```text
crates/                          # existing workspace, mostly unchanged
├── portunus-core/                # unchanged
├── portunus-proto/               # unchanged
├── portunus-auth/                # unchanged
├── portunus-server/
│   ├── build.rs                 # NEW — fails build if webui/dist/ stale
│   ├── Cargo.toml               # NEW dep: rust-embed = "8"
│   └── src/
│       ├── lib.rs               # unchanged surface
│       ├── operator/
│       │   ├── audit.rs         # NEW — RingBuffer + emit hooks
│       │   ├── audit_http.rs    # NEW — GET /v1/audit handler
│       │   ├── stats_stream.rs  # NEW — GET /v1/rules/{id}/stats/stream
│       │   ├── auth_layer.rs    # MOD — push entries into the ring buffer
│       │   ├── webui.rs         # NEW — `rust-embed` static asset router
│       │   └── http.rs          # MOD — mount /v1/audit + /v1/rules/{id}/stats/stream + webui fallback
│       └── metrics.rs           # MOD — add portunus_audit_buffer_drops_total counter
├── portunus-client/              # unchanged
└── portunus-e2e/                 # MOD — webui-aware fixtures (loopback HTTP smoke test, not playwright)

webui/                           # NEW top-level frontend project
├── package.json
├── pnpm-lock.yaml               # pnpm chosen; size-of-disk + reproducibility
├── vite.config.ts               # bundle-size plugin gate
├── tsconfig.json
├── tailwind.config.ts
├── postcss.config.js
├── components.json              # shadcn config
├── index.html
├── public/
├── src/
│   ├── main.tsx                 # bootstrap, providers
│   ├── App.tsx                  # Router shell + AuthGate
│   ├── api/                     # TanStack Query hooks per resource
│   │   ├── client.ts            # fetch wrapper, bearer injector, 401 handler
│   │   ├── users.ts
│   │   ├── credentials.ts
│   │   ├── grants.ts
│   │   ├── rules.ts
│   │   ├── clients.ts
│   │   ├── audit.ts
│   │   └── stats-stream.ts      # EventSource w/ exponential backoff + polling fallback
│   ├── auth/
│   │   ├── token-store.ts       # sessionStorage adapter
│   │   ├── AuthGate.tsx         # router guard
│   │   └── LoginPage.tsx
│   ├── components/
│   │   ├── ui/                  # shadcn-installed components (button, table, dialog, …)
│   │   ├── DataTable/           # virtualised list (TanStack Virtual + Table)
│   │   ├── ConfirmDialog.tsx    # cascade-preview confirm
│   │   ├── TokenRevealModal.tsx # one-shot token display + clipboard
│   │   ├── ErrorBanner.tsx
│   │   ├── EmptyState.tsx
│   │   └── ThemeToggle.tsx
│   ├── pages/
│   │   ├── Dashboard.tsx
│   │   ├── UsersList.tsx        # superadmin-only
│   │   ├── UserDetail.tsx
│   │   ├── GrantsList.tsx
│   │   ├── RulesList.tsx        # owner-aware filter
│   │   ├── RuleDetail.tsx       # SSE stats panel
│   │   ├── ClientsList.tsx
│   │   ├── AuditLog.tsx         # superadmin-only, NDJSON export
│   │   └── Metrics.tsx          # raw /metrics text view
│   ├── i18n/
│   │   ├── index.ts             # i18next init
│   │   ├── en.json
│   │   └── zh-CN.json
│   ├── theme/
│   │   ├── ThemeProvider.tsx    # dark/light + prefers-color-scheme
│   │   └── tokens.css           # shadcn HSL tokens
│   └── lib/
│       ├── format.ts            # bytes / duration helpers
│       └── permissions.ts       # role gates (used by AuthGate + nav)
├── tests/
│   ├── unit/                    # Vitest
│   └── e2e/                     # Playwright; spawns real portunus-server
└── README.md                    # build instructions, embed notes

# Build glue (server-side):
# `cargo build -p portunus-server` runs `portunus-server/build.rs`,
# which `panic!`s if `webui/dist/index.html` is missing AND env var
# `PORTUNUS_SKIP_WEBUI=1` is unset. Local devs without Node can set
# `PORTUNUS_SKIP_WEBUI=1` to compile a UI-less binary; release builds
# never set it (CI assertion in .github/workflows/release.yml).
```

**Structure Decision**:
- A separate top-level `webui/` Vite project (parallel to `crates/`),
  not a Rust crate. Reason: Cargo and Vite are independent build
  systems; merging them produces awkward `cargo build`-triggers-`pnpm`
  failure modes. Instead, `cargo build -p portunus-server` checks for
  `webui/dist/` and embeds it via `rust-embed`. `webui/` has its own
  package.json + lockfile + tsconfig.
- The build.rs gate keeps single-binary distribution honest: a
  release build cannot succeed without a fresh `webui/dist/`.
  Local dev workflows that don't touch the UI use
  `PORTUNUS_SKIP_WEBUI=1` to skip.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No violations. The plan introduces a new top-level directory (`webui/`)
but stays within the constitution's "single-binary distribution" rule
because the directory's compiled output is statically embedded into
`portunus-server`. No new runtime dependency on the host beyond what v0.5
already requires.
