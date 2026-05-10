---
description: "Tasks for 006-management-web-ui (constitution-aligned, test-first per Principle III)"
---

# Tasks: Management Web UI

**Input**: Design documents from `/specs/006-management-web-ui/`
**Prerequisites**: plan.md, spec.md, research.md (R-001..R-014), data-model.md,
contracts/{audit-endpoint,stats-stream-endpoint,ui-routes}.md, quickstart.md

**Tests**: REQUIRED. Constitution Principle III ("Test-First Discipline,
NON-NEGOTIABLE"): contract tests for the two new server endpoints land before
their implementation; the SPA gets unit (Vitest) + e2e (Playwright) coverage
for each user story.

**Organization**: Tasks are grouped by user story (US1..US4). The first user
story alone is a shippable MVP — superadmin-only UI driving the existing v0.5
operator API.

## Format: `[ID] [P?] [Story?] Description`

- **[P]**: Different files, no dependencies on incomplete tasks → parallelisable
- **[Story]**: Maps to `spec.md` user stories (US1, US2, US3, US4); omitted in
  Setup / Foundational / Polish phases
- File paths are absolute relative to the repository root

## Path Conventions

- Server-side Rust code lives under `crates/portunus-server/src/`
- Server-side tests live under `crates/portunus-server/tests/` (integration) or
  inline `#[cfg(test)] mod tests` (unit)
- SPA lives under `webui/` (a new top-level project; sibling of `crates/`)
- Spec / plan artefacts live under `specs/006-management-web-ui/`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Bootstrap the `webui/` project, wire `rust-embed` into the server
build, and gate bundle size at compile time.

- [X] T001 Create `webui/` directory at the repository root (sibling of `crates/`) with `package.json` declaring React 18, Vite 5, TypeScript 5, Tailwind 3, shadcn/ui's transitive deps (Radix), TanStack Query 5, React Router 6, `@tanstack/react-virtual`, `i18next`, `react-i18next`. Add `pnpm-lock.yaml` from `pnpm install`. Pin a single major per dep — no `^` ranges on `react`, `vite`, `typescript`, `@tanstack/react-query`.
- [X] T002 [P] In `webui/tsconfig.json`, enable `strict`, `noUncheckedIndexedAccess`, `exactOptionalPropertyTypes`, `noImplicitOverride`, target ES2022, lib `["ES2022","DOM","DOM.Iterable"]`. In `webui/vite.config.ts`, configure dev-server proxy `/v1` and `/metrics` → `http://127.0.0.1:7080`, set `build.target = "es2020"`, register the bundle-size visualizer plugin, and the `size-limit` runner that fails the build at 500 KB gzipped on the main chunk. Add `webui/.eslintrc.cjs` (typescript-eslint recommended + react-hooks).
- [X] T003 [P] Initialize Tailwind + shadcn under `webui/`: run `npx tailwindcss init -p`, write `webui/tailwind.config.ts` (content globs over `src/**/*.{ts,tsx}`), `webui/components.json` (shadcn config: style "new-york", tailwind config path, css path), `webui/src/theme/tokens.css` (HSL token vars from shadcn defaults), and `webui/postcss.config.js`. Install the initial shadcn components: `button`, `dialog`, `input`, `label`, `select`, `table`, `dropdown-menu`, `toast`, `tooltip`, `card`, `tabs`, `badge`, `separator`, `skeleton` via `npx shadcn add`.
- [X] T004 [P] Create the SPA bootstrap files under `webui/src/`: `main.tsx` (React.StrictMode + QueryClientProvider + ThemeProvider + I18nextProvider + RouterProvider), `App.tsx` (Router shell with `<AuthGate>` wrapper), `lib/format.ts` (bytes / duration helpers), `lib/permissions.ts` (role gate predicates `isSuperadmin`, `canSeeUsersList`, `canSeeAuditLog`).
- [X] T005 In `crates/portunus-server/Cargo.toml` add `rust-embed = { version = "8", features = ["compression"] }`. Create `crates/portunus-server/build.rs` that errors with a clear message if `webui/dist/index.html` is missing AND env var `PORTUNUS_SKIP_WEBUI` is unset; emit `cargo:rerun-if-changed=../../webui/dist`. Verify `cargo check -p portunus-server` still passes when `PORTUNUS_SKIP_WEBUI=1`.
- [X] T006 [P] In `webui/README.md`, document: (a) build command (`pnpm install --frozen-lockfile && pnpm build`), (b) dev workflow (run `portunus-server` in one shell, `pnpm dev` in another), (c) the `PORTUNUS_SKIP_WEBUI=1` escape hatch for backend-only work, (d) the bundle-size budget and how to measure locally.
- [X] T007 Create `webui/dist/.gitkeep` and add `webui/dist/` (the directory contents, NOT the directory itself) to `.gitignore` so a fresh clone has the directory present (lets `rust-embed` find it during `PORTUNUS_SKIP_WEBUI=1` checks) but doesn't commit build artefacts.

**Checkpoint**: `cargo check -p portunus-server` passes (with `PORTUNUS_SKIP_WEBUI=1`); `cd webui && pnpm install && pnpm build` produces a tiny stub `dist/index.html`.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: The server-side audit ring buffer + stats broadcast plumbing + the
shared SPA primitives (auth gate, fetch wrapper, virtualised table, theme,
i18n) that every user story depends on.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

### Server-side foundation

- [X] T008 In `crates/portunus-server/src/operator/audit.rs` (NEW), define `AuditEntry { timestamp, actor, role: Option<OperatorRole>, method, path, outcome: AuditOutcome, reason: Option<String> }` (serde-serializable per `data-model.md` § AuditEntry) and `AuditRing { inner: Mutex<VecDeque<AuditEntry>>, drops: Arc<AtomicU64> }` with capacity 1000. Methods: `push(entry) -> ()` (drops oldest + bumps `drops` on overflow), `snapshot(limit, outcome_filter: Option<AuditOutcome>) -> Vec<AuditEntry>` (newest-first). Inline unit tests cover ring overflow + filter correctness.
- [X] T009 [P] In `crates/portunus-server/src/state.rs`, add field `pub audit: Arc<AuditRing>` and initialize it inside `AppState::new`. In `crates/portunus-server/src/metrics.rs` add `portunus_audit_buffer_drops_total` `IntCounter` and wire it into the `AuditRing::push` overflow path (via an Arc clone in AppState construction).
- [X] T010 In `crates/portunus-server/src/operator/auth_layer.rs`, after each existing `info!(event="operator.allow", ...)` and `warn!(event="operator.deny", ...)` emit point, push a corresponding `AuditEntry` into `state.audit`. The push is non-blocking (lock contention is bounded by the 1000-entry queue and request rate). Tokens MUST NOT appear anywhere in the entry — only the post-verify `actor`, `role`, `method`, `path`, `outcome`, `reason`.
- [X] T011 [P] In `crates/portunus-server/src/metrics.rs`, extend `RuleStatsCache` with field `broadcasts: HashMap<RuleId, broadcast::Sender<RuleStatsSnapshot>>`. Add methods `subscribe(rule_id) -> broadcast::Receiver<RuleStatsSnapshot>` (lazily creates the sender) and `drop_rule_broadcasts(rule_id)`. In `RuleStatsCache::observe`, after updating the cached snapshot, do a non-blocking `try_send` if a broadcast for this rule exists. In `RuleStatsCache::drop_rule`, also drop the broadcast entry.
- [X] T012 In `crates/portunus-server/src/operator/users_me.rs` (NEW), implement `get_users_me` axum handler that reads `OperatorIdentity` from request extensions and returns `OperatorIdentitySelf { user_id, role, display_name }`. In `crates/portunus-server/src/operator/http.rs`, mount the route `GET /v1/users/me` behind the existing `auth_middleware`.

### SPA foundation

- [X] T013 [P] In `webui/src/api/client.ts`, implement a typed `apiFetch<T>(path, init)` wrapper: injects `Authorization: Bearer <token>` from the token store, parses JSON, throws a tagged `ApiError` (`{ status, code, message }`) on non-2xx, and emits a global event on 401 that triggers logout. Add `streamSse(path, onEvent, onError)` helper using native `EventSource` with exponential backoff (1 s → 30 s) and `onclose` returned for explicit teardown.
- [X] T014 [P] In `webui/src/auth/token-store.ts`, expose `getToken() / setToken(t) / clearToken()` operating on `sessionStorage` under key `portunus.token`; provide a `subscribe(cb)` returning an unsubscribe fn that listens to `window.storage` events for cross-tab sync.
- [X] T015 In `webui/src/auth/AuthGate.tsx` and `webui/src/auth/LoginPage.tsx`, implement: `<AuthGate role?="superadmin">` (renders children only if a token is present AND, on first mount, `GET /v1/users/me` succeeds; on 401 clears token and redirects to `/login`; on `role="superadmin"` and identity.role !== "superadmin" renders `<PermissionDenied />` immediately). `<LoginPage>` (single bearer-input field, "Sign in" submits → calls `/v1/users/me` with the candidate token, on success caches token + identity in TanStack Query and navigates to `?next` or `/`).
- [X] T016 [P] In `webui/src/components/DataTable/`, implement a generic virtualised list using `@tanstack/react-virtual`: viewport-only rendering, sticky header, sortable columns via column-config props, keyboard navigation (arrows, PageUp/PageDown, Home/End), `aria-rowindex` on each row. Include a `<Toolbar>` slot for filter inputs + last-refreshed timestamp. Vitest covers: 10k-row render time < 50 ms, virtualised window correctness, keyboard nav.
- [X] T017 [P] In `webui/src/components/{ConfirmDialog,TokenRevealModal,ErrorBanner,EmptyState,ThemeToggle,LanguageToggle}.tsx`, implement: `<ConfirmDialog>` (cascade-preview confirm — accepts `dependents` prop and renders a list before the destructive button); `<TokenRevealModal>` (one-shot token display with Copy button; on close, scrubs internal state); `<ErrorBanner>` (single global banner subscribing to TanStack Query's `errorCount`); `<EmptyState>` (icon + heading + CTA); `<ThemeToggle>` and `<LanguageToggle>` (settings panel triggers).
- [X] T018 [P] In `webui/src/theme/ThemeProvider.tsx`, implement light/dark/system theme using CSS variables on `<html>`, persisted to `localStorage.portunus.theme`. On `system`, listen to `prefers-color-scheme` change events. In `webui/src/i18n/{index.ts,en.json,zh-CN.json}`, set up `i18next` with the two resource bundles, default-language detection (`navigator.languages[0]` → match `zh-*` else fallback to `en`), persist to `localStorage.portunus.lang`.
- [X] T019 In `webui/src/App.tsx`, wire React Router to the routes specified in `contracts/ui-routes.md`. Each route is wrapped in `<AuthGate>` with the appropriate `role` prop. `<NotFound>` catch-all and the `?next=` redirect contract for unauthenticated users are implemented here.

**Checkpoint**: `cargo check -p portunus-server` passes; `portunus-server serve` starts cleanly with the new audit ring buffer and `/v1/users/me` endpoint live; `pnpm dev` in `webui/` boots an empty SPA shell that gates routes correctly behind `<AuthGate>`.

---

## Phase 3: User Story 1 - Superadmin manages tenants and rules visually (Priority: P1) 🎯 MVP

**Goal**: A superadmin can log in, manage users/credentials/grants/rules, and watch live stats — all through the browser, end-to-end.

**Independent Test**: Bootstrap a server with the `_legacy` operator token, open the UI, paste the token, walk through user-add → credential-issue → grant-add → push-rule → live-stats. Every step completes without leaving the browser; the rule shows Active state; live stats update within ≤ 6 s of a new StatsReport.

### Tests for User Story 1 (test-first per Principle III) ⚠️

> Write these tests FIRST; ensure they FAIL before implementation lands.

- [X] T020 [P] [US1] In `crates/portunus-server/tests/audit_contract.rs` (NEW), implement the 8-test plan from `contracts/audit-endpoint.md`: empty buffer → []; N entries returned newest-first; `?limit=2000` → 422 `invalid_limit`; `?outcome=deny` filters; `?outcome=banana` → 422 `invalid_outcome`; role=user caller → 403 `role_required`; missing bearer → 401 `unauthenticated`; reads-after-actions reflect those actions in order.
- [X] T021 [P] [US1] In `crates/portunus-server/tests/rule_stats_stream_contract.rs` (NEW), implement the 8-test plan from `contracts/stats-stream-endpoint.md`: subscribe as owner → snapshot ≤ 6 s; subscribe as superadmin to alice's rule → snapshot; subscribe as bob → 403 `not_owner`; non-existent rule → 404; missing bearer → 401; two subscribers receive identical snapshot; rule removed mid-stream → end-of-stream within 1 s; slow consumer doesn't block fast one.
- [X] T022 [P] [US1] In `crates/portunus-server/tests/users_me_contract.rs` (NEW), test: superadmin token → `{ user_id, role: "superadmin", display_name }`; user token → `{ user_id, role: "user", display_name }`; missing bearer → 401; revoked token → 401.
- [X] T023 [P] [US1] In `webui/tests/e2e/us1-superadmin.spec.ts` (NEW, Playwright), implement the US1 walkthrough mirroring `quickstart.md` § 3-5: login with operator_token → dashboard renders → create alice user → issue alice credential (token shown once + copy works + scrubbed on close) → add grant → push rule → watch live-stats panel update on `nc | nc` traffic. Use a Playwright fixture that spawns `portunus-server` with a known token.

### Implementation for User Story 1

#### Server (additive endpoints)

- [X] T024 [US1] In `crates/portunus-server/src/operator/audit_http.rs` (NEW), implement `get_audit` axum handler: `?limit` (default 100, max 1000, validation → 422 `invalid_limit`), `?outcome` (allow/deny/None, validation → 422 `invalid_outcome`), superadmin-only RBAC check (`RbacError::RoleRequired` on miss → 403). Return `Vec<AuditEntry>` newest-first via `state.audit.snapshot(...)`. Sets `Cache-Control: no-store`. In `crates/portunus-server/src/operator/http.rs`, mount `GET /v1/audit`.
- [X] T025 [US1] In `crates/portunus-server/src/operator/stats_stream.rs` (NEW), implement `get_rule_stats_stream` axum SSE handler: ownership check (matches `GET /v1/rules/{id}/stats`); call `RuleStatsCache::subscribe(rule_id)`; convert `BroadcastStream` into `Sse<Event>` mapping each snapshot to `Event::default().event("stats").json_data(snap)`; apply `KeepAlive::new().interval(30s).text("keepalive")`; emit `retry: 1000\n` on first frame; emit one initial snapshot from the cache if any. In `crates/portunus-server/src/operator/http.rs`, mount `GET /v1/rules/{id}/stats/stream`.

#### SPA: API hooks (TanStack Query)

- [X] T026 [P] [US1] In `webui/src/api/users.ts`, define typed React hooks: `useUsersList()` (5 s `refetchInterval`), `useUser(id)`, `useCreateUser()` (mutation; on success invalidate `["users"]`), `useDeleteUser()` (mutation; cascade preview returned in response, on success invalidate `["users"]` + `["rules"]` + `["grants"]`).
- [X] T027 [P] [US1] In `webui/src/api/credentials.ts`, define hooks: `useCredentialsList(userId)`, `useIssueCredential(userId)` (mutation returning `{credential_id, token}`; on success invalidate user's credentials list — token NEVER persisted in the cache, only returned to the caller for one-shot display), `useRotateCredential(userId, credId)`, `useRevokeCredential(userId, credId)`.
- [X] T028 [P] [US1] In `webui/src/api/grants.ts`, define hooks: `useGrantsList({ userId? })`, `useCreateGrant()`, `useRevokeGrant()` (returns `{ grant_id, removed_rule_ids }`).
- [X] T029 [P] [US1] In `webui/src/api/rules.ts`, define hooks: `useRulesList({ client?, owner? })` (5 s polling), `useRule(id)`, `usePushRule()`, `useRemoveRule()`, `useRuleStats(id)` (one-shot; consumed by detail page when SSE is unavailable).
- [X] T030 [P] [US1] In `webui/src/api/clients.ts`, define hooks: `useClientsList()`, `useProvisionClient()` (returns the full bundle for download), `useRevokeClient()`.
- [X] T031 [P] [US1] In `webui/src/api/stats-stream.ts`, define `useRuleStatsStream(ruleId)`: subscribes via the `streamSse` helper; on event, updates a local `LiveStats` state; on first failure or `EventSource` absent, falls back to `useRuleStats(id)` polling at 5 s (transparent to the page component); exposes `{ snapshot, source: "sse" | "polling", lastReceivedAt, reconnectAttempts }`.

#### SPA: pages

- [X] T032 [US1] In `webui/src/pages/Dashboard.tsx`, render: greeting card with role badge, two metric cards (`Connected clients` from `useClientsList`, `Active rules` from `useRulesList`), and a "Recent activity" panel reading the latest 10 audit entries via `useAuditLog({limit:10})`. The audit panel renders only for superadmins; tenants see a "Your rules" panel instead.
- [X] T033 [P] [US1] In `webui/src/pages/UsersList.tsx`, render the virtualised users table with columns id, display_name, role, credential count, grant count. Toolbar with text filter (client-side) + `<EmptyState>` when zero users. Wrapped in `<AuthGate role="superadmin">`.
- [X] T034 [P] [US1] In `webui/src/pages/UserDetail.tsx`, render: user header (id, display_name, role), credentials sub-list (issue / rotate / revoke buttons; each token shown via `<TokenRevealModal>`), grants sub-list (revoke button → cascade preview via `<ConfirmDialog>` with `removed_rule_ids` shown).
- [X] T035 [P] [US1] In `webui/src/pages/UserCreate.tsx`, render the new-user form (id + display_name); validate id format client-side as a UX nicety; submit calls `useCreateUser`; on success navigate to `/users/<id>`; on 4xx surface inline form errors keyed to the server's response shape.
- [X] T036 [P] [US1] In `webui/src/pages/GrantsList.tsx` and `webui/src/pages/GrantCreate.tsx`, render the grants table (filter by user_id from URL `?user_id=`) and the new-grant form (user picker, client text input with `*` wildcard support, port range start/end, protocol checkboxes). Form submits via `useCreateGrant`.
- [X] T037 [P] [US1] In `webui/src/pages/RulesList.tsx` and `webui/src/pages/RulePush.tsx`, render the rules table (always shows `owner` column; superadmin sees an `?owner=` filter dropdown populated from `useUsersList`) and the push-rule form (client / listen_port / target / protocol / optional range). Form submits via `usePushRule`; on success, the row appears in the list within 1 s (TanStack invalidation).
- [X] T038 [US1] In `webui/src/pages/RuleDetail.tsx`, render rule metadata + a `<LiveStatsPanel>` showing `bytes_in/out`, `active_connections`, `dns_failures`, UDP counters; backed by `useRuleStatsStream`. Show a "Streaming" / "Polling" badge based on the hook's `source`. Reconnect counter visible in a tooltip.
- [X] T039 [P] [US1] In `webui/src/pages/ClientsList.tsx` and `webui/src/pages/ClientProvision.tsx`, render the clients table (connected indicator from the `connected` field) and a provision form. Provision response includes the bundle JSON; offer "Download bundle" + "Copy to clipboard" buttons (no token displayed unless the operator clicks "Reveal").
- [X] T040 [US1] In `webui/src/i18n/{en.json,zh-CN.json}`, add full translations for every visible string introduced in T032–T039 (page titles, table headers, button labels, form fields, error messages). Wire all components to use `useTranslation` instead of literal strings.

**Checkpoint**: US1 is fully shippable. Run T023's Playwright spec end-to-end against a real `portunus-server` to confirm.

---

## Phase 4: User Story 2 - Tenant manages only their own rules (Priority: P2)

**Goal**: A non-superadmin tenant logs in, sees only their own rules / credentials / grants, can rotate their own credential, and is denied admin pages cleanly.

**Independent Test**: Provision a tenant `alice` with one grant + one credential. Log into the UI as alice. The Rules page shows zero rows initially; after she pushes a rule, exactly one row with `owner=alice`. Direct-navigating to `/users` shows `<PermissionDenied />` without any API request firing. Cross-tenant credential access (URL-typing `/users/bob/credentials`) returns `<PermissionDenied />`.

### Tests for User Story 2 ⚠️

- [X] T041 [P] [US2] In `webui/tests/e2e/us2-tenant-isolation.spec.ts` (NEW), implement: provision alice + bob, log in as alice → Users page hidden in nav; direct /users URL → `<PermissionDenied />`, no `/v1/users` request fires (Playwright network log assertion); /users/bob/credentials → `<PermissionDenied />`; alice's Rules page filtered to her rules; rotating her credential → modal token works on the next request, old token rejected with redirect to login.
- [X] T042 [P] [US2] In `webui/tests/unit/permissions.test.ts` (NEW, Vitest), exhaustively test `permissions.ts` predicates against the role table from `contracts/ui-routes.md`: every role × every gate combination.

### Implementation for User Story 2

- [X] T043 [US2] In `webui/src/auth/AuthGate.tsx` (already exists from Phase 2), tighten the `role="superadmin"` branch: render `<PermissionDenied />` BEFORE any TanStack Query is registered for the gated subtree. This is the difference between "denied after a 403 round-trip" and "denied with zero requests" — the spec requires the latter (FR-005).
- [X] T044 [P] [US2] In `webui/src/components/PermissionDenied.tsx` (NEW), render the placeholder component used by `<AuthGate>`: title "Permission denied", short explanation, "Back to dashboard" button. Translations in `en.json` + `zh-CN.json`. NEVER mention the resource id or content.
- [X] T045 [US2] In `webui/src/components/Nav.tsx` (NEW or extending the App shell), conditionally render the admin-only nav items (`Users`, `Audit log`, `Clients > Provision`) using `permissions.ts` predicates against the cached identity. Tenants do not see them at all.
- [X] T046 [US2] In `webui/src/pages/UserDetail.tsx`, when the URL `:user_id` does not match the cached identity AND the caller is not superadmin, render `<PermissionDenied />` BEFORE firing the `GET /v1/users/{id}` request. (Server still enforces the same; UI is a safety net.)
- [X] T047 [P] [US2] In `webui/src/pages/CredentialsRotate.tsx` (or extend `UserDetail.tsx`), wire the "Rotate" button: call `useRotateCredential`, show new token in `<TokenRevealModal>`, on close write the new token to `sessionStorage` (replacing the old one) so subsequent UI requests use the rotated bearer immediately. The OLD token's behaviour (rejected by server, UI bounces to login) is already handled by the global 401 listener from T013.

**Checkpoint**: US2 e2e (T041) passes. Tenant can self-rotate without admin involvement.

---

## Phase 5: User Story 3 - Operator inspects audit log and metrics (Priority: P2)

**Goal**: Superadmin opens the Audit Log page, sees the last 100 entries, filters by outcome, downloads as NDJSON. Metrics page renders raw `/metrics` text.

**Independent Test**: Generate a known mix of allow/deny operator requests (3 violations from Phase 4 e2e + a few admin reads). Open the Audit Log page → at least one allow + one deny visible within 5 s, filter by `outcome=deny` shows only deny rows (no new request), "Download as JSON" produces an NDJSON file with one valid `AuditEntry` per line. Tenant attempting `/audit` → `<PermissionDenied />`.

### Tests for User Story 3 ⚠️

- [X] T048 [P] [US3] In `webui/tests/e2e/us3-audit-and-metrics.spec.ts` (NEW), implement the walkthrough: superadmin generates traffic (mix of allow + deny via UI actions) → opens /audit → table renders ≥ 1 allow + ≥ 1 deny row newest-first → outcome filter applied → no new request fires → "Download as JSON" → NDJSON file downloaded → first line parses as a valid `AuditEntry`; tenant /audit → `<PermissionDenied />`; superadmin /metrics → text dump renders + dashboard gauges parse `portunus_clients_connected` and `portunus_rules_active`.
- [X] T049 [P] [US3] In `webui/tests/unit/audit-export.test.ts` (NEW, Vitest), test the NDJSON export helper: input array of `AuditEntry`s → output Blob with one entry per line, each line round-trips through `JSON.parse`.

### Implementation for User Story 3

- [X] T050 [P] [US3] In `webui/src/api/audit.ts`, define hook: `useAuditLog({ limit?: number, outcome?: "allow" | "deny" })` (5 s polling). Server contract per `contracts/audit-endpoint.md`.
- [X] T051 [P] [US3] In `webui/src/lib/ndjson.ts` (NEW), implement `toNdjsonBlob(rows: AuditEntry[]): Blob` and `downloadBlob(blob, filename)` helpers. NDJSON = one JSON object per line, no array brackets.
- [X] T052 [US3] In `webui/src/pages/AuditLog.tsx` (NEW), render the audit log table (timestamp, actor, role, method, path, outcome, reason). Toolbar: outcome filter dropdown (client-side filter — no server roundtrip; server already returned the full window), and a "Download as JSON" button calling the helper from T051 with the currently visible rows. Wrapped in `<AuthGate role="superadmin">`.
- [X] T053 [P] [US3] In `webui/src/api/metrics.ts`, define hook: `useMetricsText()` — `fetch('/metrics')` returning the raw text, no JSON parse. Cache 5 s. `useDashboardGauges()` parses out `portunus_clients_connected` and a count of `portunus_rule_*` rows for the dashboard cards.
- [X] T054 [US3] In `webui/src/pages/Metrics.tsx` (NEW), render: the raw `/metrics` text in a `<pre>` block (monospaced, scrollable, read-only) with a "Copy all" button. Above the pre, render a small "Key gauges" card extracted by `useDashboardGauges`.
- [X] T055 [US3] In `webui/src/pages/Dashboard.tsx`, integrate the `useDashboardGauges` hook into the existing two metric cards introduced in T032. Tenant view (no audit, no metrics access) shows only the rules-owned card.
- [X] T056 [US3] In `webui/src/i18n/{en.json,zh-CN.json}`, translate the new audit + metrics strings.

**Checkpoint**: US3 e2e (T048) passes. Superadmin can investigate denied requests visually.

---

## Phase 6: User Story 4 - Operator localises and themes the UI (Priority: P3)

**Goal**: Settings panel: theme toggle (light / dark / system) + language toggle (English / 简体中文); both persist across reloads.

**Independent Test**: Open Settings, toggle theme to Dark — page transitions without reload. Toggle language to 简体中文 — every nav / button / table header switches within 200 ms. Reload the browser — both preferences persist. Open in a fresh tab with `prefers-color-scheme: dark` set in dev-tools — UI loads in dark theme.

### Tests for User Story 4 ⚠️

- [X] T057 [P] [US4] In `webui/tests/e2e/us4-themes-and-i18n.spec.ts` (NEW), implement: open settings, toggle theme to dark → background colour changes within 200 ms (CSS variable assertion); toggle language to zh-CN → `[data-i18n-test="dashboard-greeting"]` text matches Chinese expectation; reload → both settings persist; open with `prefers-color-scheme: dark` → page loads in dark mode without manual toggle.
- [X] T058 [P] [US4] In `webui/tests/unit/i18n-coverage.test.ts` (NEW, Vitest), assert: every key present in `en.json` is also present in `zh-CN.json` (and vice versa). This catches regressions where a developer adds an English string but forgets the translation.

### Implementation for User Story 4

- [X] T059 [US4] In `webui/src/pages/Settings.tsx` (NEW), render the settings panel: `<ThemeToggle>` (light / dark / system radio group) + `<LanguageToggle>` (en / zh-CN radio group). Layout via shadcn `<Card>` + `<RadioGroup>`. Wrapped in `<AuthGate>` (any authed role).
- [X] T060 [US4] In `webui/src/theme/ThemeProvider.tsx`, ensure the toggle value writes to `localStorage.portunus.theme` and the React state propagates to the `data-theme` attribute on `<html>` within 200 ms (synchronous on click + transition CSS, no full reload).
- [X] T061 [US4] In `webui/src/i18n/index.ts`, ensure the language toggle calls `i18n.changeLanguage(...)` and writes to `localStorage.portunus.lang`. Validate that `useTranslation` consumers re-render synchronously on language change.
- [X] T062 [P] [US4] In `webui/src/i18n/{en.json,zh-CN.json}`, finalise full translation coverage for every page introduced in US1–US3 + the Settings page itself. Use the i18n-coverage test from T058 to gate.

**Checkpoint**: US4 e2e (T057) passes. UI is operator-team-friendly for both English and Chinese audiences.

---

## Phase N: Polish & Cross-Cutting Concerns

**Purpose**: Hardening, docs, release prep — touches multiple user stories.

- [X] T063 [P] In `crates/portunus-server/src/operator/webui.rs` (NEW), use `rust-embed` to expose `webui/dist/` at the operator HTTP root path `/`. Implement `serve_webui` axum handler: looks up the requested path (default `index.html`); for `index.html` and any path not corresponding to a static asset, serve `index.html` (SPA history-API fallback); set `Content-Type` from MIME and `Cache-Control: public, max-age=0, must-revalidate` for `index.html`, immutable cache for hashed asset names. Mount in `crates/portunus-server/src/operator/http.rs` AFTER all `/v1/*` and `/metrics` routes (router fallback).
- [X] T064 In `crates/portunus-server/tests/embed_smoke.rs` (NEW), test that `portunus-server` running with the embedded UI returns a non-empty `index.html` at `GET /` with the right Content-Type, and an arbitrary `GET /some/route/that/spa/handles` also returns `index.html` (SPA fallback). Skipped via `#[cfg]` if `PORTUNUS_SKIP_WEBUI=1` was set during compile.
- [X] T065 [P] In `webui/tests/e2e/quickstart-walkthrough.spec.ts` (NEW), implement the full `quickstart.md` § 1–11 walkthrough end-to-end as one Playwright test (server spawn → login → user/grant/rule mutations → traffic → live stats → restart roundtrip → cleanup). This is the executable counterpart of `quickstart.md`.
- [X] T066 [P] In `webui/tests/e2e/token-leak-audit.spec.ts` (NEW), automate § 9 of quickstart.md: drive a session, then assert via `page.evaluate` that (a) `sessionStorage.portunus.token` exists, (b) `localStorage` has only theme + lang, (c) no DOM text contains the bearer prefix, (d) every captured network request includes the bearer ONLY in `Authorization` header (Playwright's `page.on('request')` hook). SC-006 verification.
- [X] T067 In `webui/.github/workflows/ci.yml` (or extend existing CI yaml), add a job that: installs pnpm, runs `pnpm install --frozen-lockfile`, runs `pnpm lint`, runs `pnpm test` (Vitest), runs `pnpm build` (which fails if size-limit is exceeded), runs `pnpm exec playwright test` against a binary built with `cargo build --release -p portunus-server`.
- [X] T068 [P] In `CHANGELOG.md`, add an entry under `[Unreleased]` (NOT a new release block — that's done at /speckit-implement polish time): "Multi-user operator Web UI (spec 006-management-web-ui). React + Vite SPA embedded into portunus-server via rust-embed. Two new server endpoints: GET /v1/audit (superadmin), GET /v1/rules/{id}/stats/stream (SSE, ownership-checked). New GET /v1/users/me. Bundle size gated at ≤ 500 KB gzipped. ..."
- [X] T069 [P] In `README.md`, add a "Web UI" section under the existing v0.5 RBAC walkthrough: how to access the UI (point browser at the operator HTTP listener), required browser versions, the loopback-only reminder, and a screenshot or two if convenient.
- [X] T070 [P] In `deploy/server.toml.example`, add a comment block documenting that the operator HTTP listener now also serves the Web UI at `/`. No new config knob needed.
- [X] T071 [P] In `webui/README.md`, finalise the build documentation: `pnpm install --frozen-lockfile`, `pnpm dev` (HMR against a running `portunus-server`), `pnpm build` (production with size-limit gate), `pnpm test` (Vitest + Playwright). Document the `PORTUNUS_SKIP_WEBUI=1` workflow for backend devs.
- [X] T072 Run the full quickstart.md walkthrough manually one time, verify each step matches the spec, and capture wall-clock numbers for SC-001, SC-004, SC-005 (binary size delta, gzipped JS). Append the measurements to a `Verified` section in `specs/006-management-web-ui/spec.md` (NOT in CHANGELOG yet — that lands at release-tag time).
- [X] T073 Run `cargo test --workspace --tests` AND `pnpm test` in `webui/` AND `pnpm exec playwright test`. All must pass green. Run `cargo clippy --workspace --all-targets -- -D warnings` AND `pnpm lint`. Both must pass clean.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Phase 1 (Setup)**: No dependencies — start immediately.
- **Phase 2 (Foundational)**: Depends on Phase 1 — BLOCKS all user stories.
- **Phase 3+ (US1..US4)**: All depend on Phase 2 completion.
  - In a one-developer flow: P1 → P2 → P3 → P4 sequentially.
  - In a multi-developer flow: P1 alone is shippable as MVP; P2/P3/P4 can run in parallel after Phase 2.
- **Polish (Phase N)**: Depends on all desired user stories being complete.

### User Story Dependencies

- **US1 (P1, MVP)**: After Phase 2. No dependency on other stories — superadmin-only UI is shippable on its own.
- **US2 (P2)**: After Phase 2. Builds on US1's `<AuthGate>` and the API hooks; requires US1 to have rendered tables to filter / hide. In practice US1 + US2 ship together (no point shipping a superadmin-only UI given v0.5 is multi-tenant).
- **US3 (P2)**: After Phase 2. Independent of US1/US2 — adds two new pages (Audit + Metrics) and one API hook.
- **US4 (P3)**: After Phase 2. Most independent — touches `Settings` page + theme/i18n providers (the providers themselves were scaffolded in Phase 2).

### Within Each User Story

- Tests (T020-T023, T041-T042, T048-T049, T057-T058) MUST be written and failing BEFORE the implementation tasks land.
- Server-side (T024-T025) before SPA-side (T026..) for US1 — the SPA hooks need real endpoints to call.
- API hooks (`webui/src/api/*.ts`) before the page components that consume them.

### Parallel Opportunities

- All `[P]` tasks within Phase 1 (T002, T003, T004, T006).
- All `[P]` tasks within Phase 2 (T009, T011, T013, T014, T016, T017, T018).
- All test tasks within a user story phase (T020/T021/T022/T023; T041/T042; T048/T049; T057/T058).
- All API-hook tasks within US1 (T026-T031).
- Most page tasks within US1 (T033-T037, T039) — different files, no cross-deps.
- All translation-bundle work in `webui/src/i18n/*.json` is a single file pair, so US1's T040, US3's T056, US4's T062 are **sequential** (same files); the rest of the task is parallel.

---

## Parallel Example: User Story 1

```bash
# Test-first batch (run these together, expect them to fail):
Task: "Contract test for /v1/audit in crates/portunus-server/tests/audit_contract.rs"           # T020
Task: "Contract test for /v1/rules/{id}/stats/stream in crates/portunus-server/tests/rule_stats_stream_contract.rs"  # T021
Task: "Contract test for /v1/users/me in crates/portunus-server/tests/users_me_contract.rs"     # T022
Task: "Playwright e2e for US1 superadmin walkthrough in webui/tests/e2e/us1-superadmin.spec.ts" # T023

# API-hook batch (after T024+T025 land):
Task: "Users hooks in webui/src/api/users.ts"          # T026
Task: "Credentials hooks in webui/src/api/credentials.ts"  # T027
Task: "Grants hooks in webui/src/api/grants.ts"        # T028
Task: "Rules hooks in webui/src/api/rules.ts"          # T029
Task: "Clients hooks in webui/src/api/clients.ts"      # T030
Task: "Stats stream hook in webui/src/api/stats-stream.ts"  # T031

# Page batch (after API hooks land; T032 is sequential because it pulls from many hooks):
Task: "UsersList page in webui/src/pages/UsersList.tsx"           # T033
Task: "UserDetail page in webui/src/pages/UserDetail.tsx"         # T034
Task: "UserCreate page in webui/src/pages/UserCreate.tsx"         # T035
Task: "GrantsList + GrantCreate pages"                            # T036
Task: "RulesList + RulePush pages"                                # T037
Task: "ClientsList + ClientProvision pages"                       # T039
```

---

## Implementation Strategy

### MVP First (User Story 1 only)

1. Phase 1 Setup — bootstrap `webui/`, wire `rust-embed`, gate bundle size.
2. Phase 2 Foundational — server audit ring + broadcast plumbing + SPA shell.
3. Phase 3 US1 — superadmin walkthrough end-to-end. **Ship this** as a v0.5.x preview release if you want to demo before US2/3/4.
4. **STOP and validate** — run T023 + the manual quickstart walkthrough.

### Incremental Delivery

- After MVP: add US2 (T041..T047) → tenants can self-serve → Demo.
- Add US3 (T048..T056) → ops investigation surface → Demo.
- Add US4 (T057..T062) → polish for Chinese-speaking team → Demo.
- Add Polish (T063..T073) → ship as v0.6.0.

### Test discipline

- Every server-side endpoint task (T024, T025) is preceded by its contract test (T020, T021).
- Every UI-side user story has a Playwright e2e suite (T023, T041, T048, T057) that fails until the implementation lands.
- Constitution Principle III non-negotiable: do NOT skip the failing-test step.

---

## Format validation

All 73 tasks above use the strict format:

```
- [ ] [TaskID] [P?] [Story?] Description with file path
```

Spot-check: tasks in Setup/Foundational/Polish phases have **no** `[Story]` label; tasks in US1..US4 phases all carry the matching `[US1]`..`[US4]` label. Every task names at least one absolute repo-relative file path.
