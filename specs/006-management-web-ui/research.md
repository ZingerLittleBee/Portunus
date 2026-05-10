# Research: 006-management-web-ui

Decisions captured during Phase 0 of the implementation plan. Each entry
follows the **Decision / Rationale / Alternatives considered** structure.

---

## R-001 — UI framework: React 18 + Vite + TypeScript

**Decision**: React 18 (latest stable) on Vite 5 with TypeScript 5.x in
strict mode.

**Rationale**:
- Largest documentation + component library ecosystem of any SPA
  framework, cutting first-time-setup friction for an internal team.
- shadcn/ui (the component library we want, see R-002) ships
  exclusively for React.
- Vite's dev-server HMR and zero-config production bundle keep
  build-time complexity low; the bundle is small enough to honour the
  500 KB gzipped budget without aggressive code splitting.
- TypeScript strict mode catches RBAC mismatches between UI types and
  v0.5 server response shapes at compile time.

**Alternatives considered**:
- **Solid.js + Vite** — smaller runtime (~7 KB gzipped) and finer-grained
  reactivity, but the ecosystem is shallow and shadcn equivalents
  ("solid-ui") are alpha. Lock-in risk to a small library author.
- **Vue 3 + Vite + Naive UI** — strong 中文 community fit, smaller core
  bundle than React. Rejected because TanStack Query's React story is
  the most mature, and we want one less thing to learn.
- **Next.js / Remix** — server-rendered React. Rejected: requires Node
  runtime at deploy time, breaking the constitution's single-binary
  rule.
- **HTMX + server-rendered HTML from axum** — would require server-side
  templating + state, drag the operator path into the rendering loop,
  and conflict with the offline-friendly SPA model.

---

## R-002 — Component library: shadcn/ui (Radix + Tailwind)

**Decision**: shadcn/ui as the component vocabulary; Tailwind 3 for
utility classes.

**Rationale**:
- shadcn ships components as code (you copy the source into your repo
  via the `npx shadcn` CLI), not a runtime dependency. Bundle includes
  only the components actually imported.
- Built on Radix primitives → WCAG AA accessibility (focus rings,
  keyboard nav, ARIA) is baked in, satisfying FR-020.
- HSL-token-based dark/light theming via CSS variables → FR-018
  ("dark + light mode follow `prefers-color-scheme`") is a copy-paste.
- Visual style is plain and operator-tool-friendly (no marketing
  flourish), matching the audience.

**Alternatives considered**:
- **Mantine** — fuller-featured, but ships a 50–100 KB runtime and is
  harder to bundle-trim under 500 KB.
- **Chakra UI v3** — similar size pressure as Mantine; v2 → v3
  migration story is unstable.
- **Custom from-scratch components** — owns the size budget but
  multiplies surface for accessibility bugs. Not worth the time.
- **Headless UI / Radix without shadcn** — fine technically, but the
  shadcn additions (sensible Tailwind presets, theming tokens, the CLI
  copy-flow) materially speed up authoring.

---

## R-003 — Server-state library: TanStack Query 5

**Decision**: TanStack Query 5 for all `/v1/*` reads and mutations.

**Rationale**:
- Built-in cache invalidation makes "after-mutation list refresh"
  (FR-015) a one-liner.
- `refetchInterval` on each query satisfies the 5 s polling target
  (FR-007) without manual `setInterval` plumbing.
- Background-refetch + retry-with-backoff handles transient network
  failures (edge case: "Network outage mid-session") with the right
  defaults; the UI just needs a single `<ErrorBanner />` listening to
  the global query error count.
- TypeScript types flow end-to-end from `fetcher` → `useQuery` → page
  component, removing a class of bugs.

**Alternatives considered**:
- **SWR** — slimmer API, comparable features. Rejected because
  TanStack's mutation API is more explicit and its DevTools panel is
  better for debugging RBAC mismatches.
- **Redux Toolkit Query** — integrates with Redux store, useful if
  we needed cross-page derived state. We don't; pages are mostly
  independent. Adds Redux ceremony without payoff.
- **Plain fetch + `useEffect`** — reinvents what TanStack Query gives
  for free; bug-prone around stale closures and race conditions.

---

## R-004 — Routing: React Router 6

**Decision**: React Router 6 with declarative `<Route>` definitions and
`useSearchParams` for filter state in the URL (per FR-023).

**Rationale**:
- Largest install base of any React router; copious recipes for
  auth-guarded routes (the `<AuthGate>` pattern in `src/auth/`).
- Native support for nested routes (e.g., `/users/:id/credentials/:cid`)
  matches our resource hierarchy.
- `useSearchParams` puts active filters in the URL so the browser
  back/forward buttons "just work" (FR-023).

**Alternatives considered**:
- **TanStack Router** — typed routes, good story. Still under heavy
  iteration; type ergonomics are great but the API churn risk on a
  long-lived internal tool is real.
- **No router (single-page modal switcher)** — rejected; we have ≥ 8
  distinct pages and bookmarkable URLs are an FR-023 requirement.

---

## R-005 — Bundle-size enforcement

**Decision**: `vite-plugin-bundle-visualizer` for diagnostics +
`size-limit` v11 as a hard CI gate at 500 KB gzipped of the main JS
chunk; the build script (`pnpm build`) wraps `vite build && size-limit`.

**Rationale**:
- size-limit fails the build with a non-zero exit if the budget is
  exceeded (edge case "JS bundle exceeds budget" — FR-026).
- Visualizer is dev-only; no runtime cost.
- 500 KB gzipped accommodates React (45 KB) + TanStack Query (15 KB)
  + Radix (per-component ~3 KB) + Tailwind classes (~10 KB after
  purge) + i18next (10 KB) + per-page logic, with comfortable margin.

**Alternatives considered**:
- **No gate, manual review** — invites silent regressions; rejected.
- **Webpack Bundle Analyzer** — Vite-native is better aligned.

---

## R-006 — Embedding static assets in the Rust binary

**Decision**: `rust-embed = "8"` with the `compression` feature on,
sourcing from `webui/dist/`. A `build.rs` in `portunus-server`
panics with a clear error if the directory is missing AND env var
`PORTUNUS_SKIP_WEBUI=1` is unset (release builds never set it).

**Rationale**:
- `rust-embed` is the de-facto crate for "compile-time include of a
  directory tree" in Rust; mature, maintained, single dep.
- The `compression` feature gzips assets at compile time so served
  bytes are pre-compressed (~3× smaller HTTP responses).
- The build.rs gate keeps releases honest: a CI release build that
  forgot to run `pnpm build` will fail loudly, preventing a UI-less
  binary from being tagged.
- The `PORTUNUS_SKIP_WEBUI=1` escape hatch keeps the dev loop painless
  for backend-only work — `cargo test` doesn't need Node.

**Alternatives considered**:
- **`include_dir`** — comparable functionality, slightly less
  ergonomic API (no built-in HTTP handler for axum; would need glue).
- **`include_bytes!`** macro per file** — DIY; doesn't scale past a
  handful of files.
- **Serve from a config-dir at runtime** — breaks single-binary
  distribution; operators would need to manage two artefacts.
- **Separate static-file server alongside `portunus-server`** —
  same problem; rejected by Constitution.

---

## R-007 — SSE vs WebSocket vs long-polling for live stats

**Decision**: Server-Sent Events via `axum::response::sse::Sse` on
`GET /v1/rules/{id}/stats/stream`. Browser uses `EventSource`. The
client implements exponential backoff (1 s → 30 s) on disconnect
(FR-016). If `EventSource` is missing or stream throws within 1 s of
connect, fall back to 5 s `refetchInterval` polling on the existing
`GET /v1/rules/{id}/stats` (FR-017) — invisible to the user.

**Rationale**:
- One-way, server-to-client, time-series push — exactly the use case
  SSE was built for.
- Multiplexes onto plain HTTP/1.1 — no protocol upgrade, no separate
  port, runs through any reverse proxy without WS-specific config.
- Reconnect semantics are defined by the browser (`EventSource`
  retries automatically with `retry:` field) — minimal client code.
- Plays nicely with `tokio::sync::broadcast`: one broadcast source
  per rule, each subscriber is a `Stream<Item = Event>` adapter.

**Alternatives considered**:
- **WebSocket** — bidirectional, but we don't need client → server
  data on this channel. WS adds protocol complexity and a separate
  framing layer. Rejected.
- **Long-polling** — simpler than WS but burns one HTTP request per
  interval. Acceptable as fallback only (FR-017).
- **Plain 5 s polling everywhere** — uniform, no streaming code at
  all, but the live-stats UX is materially worse (visible flicker,
  ≥ 5 s lag). Rejected for the rule-detail page; kept as fallback.

---

## R-008 — SSE fanout strategy (resolved by Q2 in clarify)

**Decision**: One `tokio::sync::broadcast::Sender<RuleStatsSnapshot>`
per rule, lazily created on first subscriber, dropped when the rule
is removed (the existing `RuleStatsCache::drop_rule` path). Every
subscriber holds a `BroadcastStream` adapter that surfaces incoming
snapshots as SSE events.

**Rationale**:
- Cost is **O(rules)**, not O(rules × subscribers): the broadcast
  source is a single allocation per rule plus a small `Receiver`
  per subscriber.
- `tokio::sync::broadcast` already drops the slowest subscriber's
  oldest messages if a queue overflows — back-pressure is local,
  the hot path is unaffected.
- Subscriber disconnect (browser tab closed, network drop) is
  detected when the SSE stream's `Sender` errors on next write;
  we drop the `Receiver` and the broadcast slot is reclaimed.
- No hard cap is needed: at 100 operators × 10 rules each = 1000
  receivers fanning out from ≤ 10k broadcast sources, total memory
  is ≤ a few MB and CPU cost is dominated by the existing
  StatsReport tick anyway.

**Alternatives considered**:
- **`tokio::sync::watch`** — only the latest value is kept;
  subscribers might miss intermediate snapshots if they're slow.
  For a 5 s stream that's not catastrophic, but `broadcast` is the
  clearer fit.
- **Per-subscriber timer** — each SSE handler runs its own 5 s
  `tokio::time::interval` and reads `RuleStatsCache::get`. Linear
  resource cost in subscribers; rejected.
- **Hard global cap** — defensive but introduces a 503 surface
  that needs documentation, monitoring, and operator messaging.
  Rejected as premature.

---

## R-009 — Audit log persistence model

**Decision**: In-memory `Mutex<VecDeque<AuditEntry>>` ring buffer
inside `AppState`, capacity 1000. The existing `auth_layer` allow
and deny emit sites (already calling `info!` / `warn!`) gain a
sibling call that pushes into the ring buffer. On overflow, the
oldest entry is dropped silently (a Prometheus counter
`portunus_audit_buffer_drops_total` is incremented for ops visibility).
Server restart wipes the buffer — by design; the structured JSON
log file is the cold-storage of record.

**Rationale**:
- Bounded memory: ≈ 200 KB at full capacity (1000 × ~200 bytes /
  entry). No risk of unbounded growth.
- No new disk IO on the audit hot path → no per-request latency
  cost (Constitution Principle II).
- Operators who want long-term retention pipe the existing
  structured log file into a SIEM (out-of-scope for this feature
  per Assumptions).
- Counter for drops gives a signal if the buffer is too small (1k
  entries / hour ≈ 16 / minute, plenty for a small team).

**Alternatives considered**:
- **SQLite** — durable, queryable, but ties us to a new on-disk
  artefact + schema migration story. `TODO(STORAGE_CHOICE)` is
  still open at the constitution level; we don't want to pre-empt
  it for a feature that doesn't need it.
- **Append-only file separate from the JSON log** — duplicates
  the log's content; pointless.
- **Ring buffer of 100 instead of 1000** — too small for a
  realistic ops investigation; doesn't meet FR-010 ("last 100
  entries" with extra headroom for filter drilling).

---

## R-010 — Auth bootstrap: how does the UI know the user's role?

**Decision**: After login, the UI immediately calls
`GET /v1/users/me` and caches `{ user_id, role }` in TanStack Query
+ React context. **A new server endpoint `GET /v1/users/me`** is
added (additive, not in the original FR-027/FR-028 list — discovered
during plan; no spec change because the spec already requires the
UI to gate navigation by role and the existing `/v1/users` endpoint
is superadmin-only). All role gates and navigation visibility key
off this cached identity.

**Rationale**:
- The token alone doesn't tell the UI who it belongs to; the v0.5
  server already knows (every authed request has the post-verify
  `OperatorIdentity` in extensions).
- A dedicated `/me` endpoint is conventional, cheap, and prevents
  the UI from having to probe other endpoints to infer role.
- On 401 the call fails first → the UI bounces to login before
  showing any sensitive page.

**Implementation sketch**:
```
GET /v1/users/me
Authorization: Bearer <token>
→ 200 {"user_id":"alice","role":"user","display_name":"Alice"}
→ 401 {"error":{"code":"unauthenticated", ...}}
```

The handler is a 5-line wrapper around the existing `OperatorIdentity`
extension; no new authentication or RBAC logic.

**Alternatives considered**:
- **Decode the bearer token client-side** — tokens are opaque
  blake3 hashes server-side; the UI can't decode them.
- **Probe `/v1/users` and infer** — superadmin-only; if the UI is
  a non-superadmin tenant it gets 403, which is fine, but coupling
  navigation state to a permission check is fragile.
- **Embed identity into the URL fragment** — leaks identity into
  history; rejected.

---

## R-011 — i18n approach

**Decision**: `i18next` + `react-i18next` with two JSON resource
bundles: `webui/src/i18n/en.json` and `webui/src/i18n/zh-CN.json`.
First-load language defaults to `navigator.languages[0]` if it
matches `zh-*`, else `en`. User toggle persists to `localStorage`
under key `portunus.lang`.

**Rationale**:
- `i18next` is the standard React i18n stack; lazy-load support
  keeps the initial bundle small.
- Two-bundle scope matches the spec (FR-019); we can add more
  later by dropping a JSON file.
- Browser-language detection covers the common case; the toggle
  handles operators on a non-localised browser.

**Alternatives considered**:
- **Lingui** — message-extraction-based; better for very
  large surface areas. Overkill at our copy size.
- **Custom `<T>` component + a switch statement** — fine for ~50
  strings; we have ≥ 200, would become a maintenance burden.

---

## R-012 — Token storage: sessionStorage vs IndexedDB vs cookies

**Decision**: `sessionStorage` (per Q1 of session, already in spec
FR-003). Keyed under `portunus.token`. **No** cookies, **no**
localStorage, **no** IndexedDB.

**Rationale**:
- sessionStorage is per-tab and cleared on tab close — narrows the
  exfiltration window vs localStorage.
- Cookies would require CSRF defence; not free, and we have no need
  for them since requests are same-origin to the embedded server.
- IndexedDB is overkill for a single 256-bit token.
- Constitution Principle IV (no raw tokens in logs) — sessionStorage
  is a leaf in the threat model: only XSS within the same origin
  can exfiltrate it. Our SPA renders no untrusted HTML, so XSS
  surface is small.

**Alternatives considered**:
- **HttpOnly cookies** — server-side cookie auth would require
  rewriting `auth_layer` to read cookies; rejected (additive-only
  constraint + Principle I single seam).
- **Token in URL fragment** — leaks into history, bookmarks,
  Referer headers in mis-configured envs.

---

## R-013 — Vite dev server proxying for backend during development

**Decision**: `vite.config.ts` configures a dev-server proxy that
forwards `/v1/*` and `/metrics` to `http://127.0.0.1:7080` (the
default operator HTTP port). Devs run `portunus-server serve` in one
terminal and `pnpm dev --host 127.0.0.1` in another.

**Rationale**:
- Same-origin policy honoured — the SPA in dev believes it's
  talking to the same host as in production.
- Hot-reload on UI changes is instant (Vite HMR); backend changes
  require `cargo run` cycle as usual.
- No CORS configuration on the server side; production never sees
  the proxy.

**Alternatives considered**:
- **CORS allow-list on `portunus-server`** — punctures a hole in
  the operator-loopback assumption; rejected.
- **Embed during dev too** — incompatible with HMR; you'd need
  to rebuild the UI on every save.

---

## R-014 — Playwright vs Cypress for e2e

**Decision**: Playwright. Tests live under `webui/tests/e2e/`. A
fixture spawns `portunus-server` with a known `operator_token`,
discovers the operator HTTP port from stderr (matches v0.4 / v0.5
e2e pattern), and runs the SPA against the live binary.

**Rationale**:
- First-class TypeScript + cross-browser engine support (Chromium /
  Firefox / WebKit) — covers the "latest two" browser commitment
  in spec assumptions.
- Trace viewer (`playwright show-report`) makes flaky test debugging
  much faster than Cypress's runner.
- Built-in network mocking if we need it; we don't, but it's there.

**Alternatives considered**:
- **Cypress** — popular, but Chromium-only (with experimental
  Firefox); doesn't satisfy our cross-browser commitment.
- **Plain Vitest + jsdom** — fine for unit tests; can't drive a
  real browser, can't validate the SSE flow.
- **No e2e tests** — Constitution Principle III rules this out.

---

## Open items (non-blocking, defer to /speckit-tasks or later)

- **D-001**: Bundle splitting — chunk per page vs single bundle.
  Decide post-implementation when actual sizes are visible. The
  500 KB budget should accommodate single-bundle initially.
- **D-002**: Service Worker for offline / install — explicitly out
  of scope for v1 (Assumptions); revisit if the operator team asks
  for "PWA install" later.
- **D-003**: A11y CI integration (axe-playwright) — desirable, but
  the manual keyboard test in the e2e suite is a sufficient first
  pass. Add to a future hardening pass if we ship.
- **D-004**: Audit log search — currently only filter-by-outcome.
  Free-text search across the 1000-entry buffer is cheap to add but
  requires a UI affordance (search input + matching highlighting).
  Defer to /speckit-tasks; trivial extension if requested.

---

All other technical context fields in plan.md are decided.
**No `NEEDS CLARIFICATION` markers remain.**
