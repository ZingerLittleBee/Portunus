# forward-webui

Operator Web UI for `forward-server`. React + Vite + TypeScript SPA,
embedded into the server binary at compile time via `rust-embed` and
served on the existing operator HTTP listener at `/`.

This package is part of the `forward-rs` monorepo (spec
`006-management-web-ui`). It is a build-time tool — the deployed
binary has no runtime Node dependency.

## Toolchain

- Node >= 20 LTS
- pnpm >= 9
- A modern browser for `pnpm dev` (Chrome / Firefox / Safari / Edge,
  latest two releases).

## Build

```sh
pnpm install --frozen-lockfile
pnpm build           # tsc -b && vite build && size-limit (≤ 500 KB gz)
```

The output lands in `webui/dist/`. The next `cargo build -p forward-server`
will pick it up via `rust-embed` and bake it into the binary.

`pnpm build` fails the build if the gzipped main bundle exceeds 500 KB
(spec FR-026 / SC-005). To inspect bundle composition open
`dist/stats.html` after a build.

## Develop

In one shell, start the server (or any v0.5+ build with the operator
HTTP listener exposed):

```sh
cargo run -p forward-server -- --config-dir /tmp/forward-dev serve
```

In another shell:

```sh
pnpm dev             # Vite, port 5173, proxies /v1 + /metrics → 127.0.0.1:7080
```

Open <http://127.0.0.1:5173/> and paste the operator bearer token.
Hot-reload works for any source under `src/`.

## Skip the UI for backend-only work

```sh
FORWARD_SKIP_WEBUI=1 cargo build -p forward-server
```

The build emits a stub `webui/dist/index.html` so `rust-embed` still has
content to embed. Release pipelines NEVER set this env var.

## Test

```sh
pnpm test            # Vitest (unit + component, jsdom-free via happy-dom)
pnpm test:e2e        # Playwright (spawns a real forward-server)
pnpm lint            # eslint --ext ts,tsx
```

## Bundle-size budget

Gate: `pnpm build` runs `size-limit` against `dist/assets/index-*.js`.
Limit: **500 KB gzipped**. Exceeding the limit fails CI.

## Ownership / Layout

```
src/
  api/        TanStack Query hooks (one file per resource)
  auth/       AuthGate, LoginPage, token-store
  components/ shadcn UI primitives + DataTable + cross-cutting widgets
  pages/      Top-level routed pages
  i18n/       i18next init + en + zh-CN bundles
  theme/      Theme provider + CSS tokens
  lib/        Pure helpers (cn, format, permissions, ndjson)
tests/
  unit/       Vitest specs
  e2e/        Playwright specs (one per user story)
```

See `specs/006-management-web-ui/` for the full design contracts.
