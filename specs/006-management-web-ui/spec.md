# Feature Specification: Management Web UI

**Feature Branch**: `006-management-web-ui`
**Created**: 2026-05-07
**Status**: Draft
**Input**: User description: "A management Web UI for Portunus (the '管理页面' originally promised by the constitution under TODO(WEB_UI)). Single-page application that operators load in a browser to manage users, credentials, grants, forwarding rules, and connected clients without dropping into the CLI."

## Clarifications

### Session 2026-05-07

- Q: 列表数据量超过阈值时 UI 用什么分页/虚拟化策略? → A: 客户端虚拟滚动 (全量拉取 + 视口内渲染),服务端 contract 不动
- Q: SSE 流式连接是否要在服务端设并发上限,达上限怎么处理? → A: 每条 rule 共享一个 broadcast 流,无硬上限,客户端订阅本地副本 (实际成本 O(rules))
- Q: 列表页是否提供"导出为 CSV/JSON"按钮?哪些列表? → A: v1 仅 audit log 提供 JSON 导出;Users/Grants/Rules/Clients 由 CLI `--format json` 覆盖,defer 到 v0.7+

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Superadmin manages tenants and rules visually (Priority: P1)

A superadmin opens the management page in a browser, pastes their bearer token to log in,
and from a dashboard sees connected clients, total active rules, and key health gauges.
They navigate to the Users page, create a new tenant, issue that tenant a credential,
and grant the tenant a port range on a named client. They then go to the Rules page,
push a forwarding rule on the tenant's behalf, watch the rule transition from Pending to
Active, and finally open the rule's live stats view to confirm bytes are flowing. None
of this requires shelling into the host or running CLI commands.

**Why this priority**: This is the core "管理页面" promise from the constitution.
A superadmin operating the platform from a browser is the foundational use case —
without it, there is no Web UI feature. P2 and P3 are refinements on top of this baseline.

**Independent Test**: Bootstrap a server with a `_legacy` superadmin token, open the
management page, paste the token, then walk through user-add → credential-issue →
grant-add → push-rule → live-stats end-to-end through the browser. The walkthrough
matches the equivalent quickstart sections from feature 005 but driven through the UI
instead of the CLI. Success: every step completes without leaving the browser, the rule
shows Active state, and live stats numbers advance in real time.

**Acceptance Scenarios**:

1. **Given** a freshly bootstrapped server and a superadmin token, **When** the operator
   opens the management page and pastes the token into the login form, **Then** they
   land on a dashboard showing the count of connected clients, total active rules, and
   the user's role (Superadmin).
2. **Given** the superadmin is on the Users page, **When** they fill out the
   "New User" form with a valid id and display name and submit, **Then** the new user
   appears in the list within 1 second without a full page reload.
3. **Given** the superadmin is viewing a user's detail page, **When** they click
   "Issue Credential" and confirm, **Then** the new bearer token is shown exactly once
   inside a modal dialog with a "Copy to clipboard" button, and after the modal is
   closed the token is no longer accessible anywhere in the UI.
4. **Given** a tenant has at least one grant covering a port and protocol, **When** the
   superadmin pushes a rule against that tenant from the Rules page, **Then** the rule
   appears in the list with its `owner` column set to the tenant's id and its state
   transitioning Pending → Active within the ack timeout.
5. **Given** the superadmin opens a rule's live stats view, **When** the client
   delivers a stats report, **Then** the bytes/connections/datagrams numbers visibly
   update within ≤ 6 seconds of the report arriving (one stats interval).

---

### User Story 2 - Tenant manages only their own rules (Priority: P2)

A tenant (role = `user`) logs into the same UI with their own bearer token. They see
ONLY their own rules, ONLY their own credentials, and ONLY the grants the superadmin
has issued to them. They can rotate their own credential, push rules within their
grant, and remove their own rules. Attempting to navigate to admin-only pages
(Users list, Audit log) shows a "Permission denied" placeholder rather than crashing or
leaking data.

**Why this priority**: Multi-tenant isolation is the whole point of v0.5; the UI
must inherit it. Without this, the UI either becomes superadmin-only (which is a
degradation from the CLI) or accidentally leaks cross-tenant data (a critical bug).
But it can ship after P1: a v1 UI that is superadmin-only is still useful internally.

**Independent Test**: Provision a tenant `alice` with one grant + one credential.
Log in as alice through the UI. Confirm:
(a) the Rules page shows zero rules until alice pushes one of her own;
(b) the Users page is either hidden from the navigation or shows "Permission denied";
(c) attempting to issue a credential for a different user via direct URL navigation
returns a permission error (not a 404, not a successful issuance).

**Acceptance Scenarios**:

1. **Given** alice has one grant and pushes one rule, **When** she opens the Rules
   page, **Then** she sees exactly one row and that row's owner is `alice`.
2. **Given** alice is logged in as a non-superadmin, **When** she navigates to the
   Users index, **Then** the page shows a clear "Permission denied" message and a
   link back to the dashboard, and no user list is fetched.
3. **Given** alice opens her own credential list, **When** she clicks "Rotate" on her
   active credential, **Then** a new token is shown once in a modal, the new token
   immediately works for subsequent UI actions, and the old token is rejected with
   the UI bouncing back to the login screen if it is still cached.

---

### User Story 3 - Operator inspects audit log and metrics (Priority: P2)

A superadmin investigating a denied request opens the Audit Log page and sees the
last 100 `operator.allow` / `operator.deny` events with actor, method, path, outcome,
and reason. They filter by `outcome=deny` to see only failures. They also open the
Metrics page to see a Prometheus-style raw text dump of `/metrics` for ad-hoc grepping.

**Why this priority**: Same priority as US2: the audit surface is what makes the
RBAC layer trustworthy from the ops side. But it's not blocking for an MVP if the
operator is willing to `tail -f` the JSON log file in a terminal.

**Independent Test**: Generate a known mix of allow/deny operator requests (e.g. by
hitting `/v1/users` with one valid superadmin token and one revoked token via curl).
Open the Audit Log page in the UI. Confirm that within 5 seconds: at least one allow
row and one deny row appear; the deny row's reason matches `RbacError::code()` from
the auth-layer; client-side filtering by outcome works locally without a server roundtrip.

**Acceptance Scenarios**:

1. **Given** the server has emitted 50 `operator.allow` events and 10
   `operator.deny` events recently, **When** a superadmin opens the Audit Log page,
   **Then** the most recent 100 events appear with a clear timestamp, actor, method,
   path, outcome, and reason column, sorted newest-first.
2. **Given** the audit log page is open, **When** the operator selects
   "Outcome: deny" in a filter dropdown, **Then** only deny rows remain visible and
   the count badge updates accordingly. The filter is client-side; no server query
   is fired.
3. **Given** a non-superadmin tenant tries to navigate to the Audit Log page,
   **When** the page loads, **Then** they see "Permission denied" without any audit
   data being fetched.

---

### User Story 4 - Operator localises and themes the UI (Priority: P3)

The operator (any role) opens the settings panel and switches the UI language between
English and 简体中文, and toggles between dark and light themes. Their choice persists
across browser reloads on the same device.

**Why this priority**: Quality-of-life for a Chinese-speaking operator team
(per the original project description). Not blocking for v1 functionality but
significantly improves day-to-day usability.

**Independent Test**: Open the UI in English, click the language toggle to 简体中文,
verify all visible navigation labels, button labels, and table headers switch language.
Reload the page, verify the language preference is remembered. Toggle theme to dark,
reload, verify theme is remembered.

**Acceptance Scenarios**:

1. **Given** the UI is loaded in English, **When** the operator selects 简体中文,
   **Then** every navigation item, page title, button label, and column header is
   replaced with its 中文 translation within 200 ms (no full page reload).
2. **Given** the operator has selected 简体中文, **When** they close the browser
   and reopen the management page, **Then** the UI loads directly in 简体中文.
3. **Given** the user's browser respects `prefers-color-scheme: dark`, **When**
   they open the page for the first time and have not yet chosen a theme, **Then**
   the dark theme is applied automatically.

---

### Edge Cases

- **Stale token after server restart**: After a server reboot the operator's
  cached token is still valid (tokens persist). After a `bootstrap-superadmin`
  rotation it is not. The UI MUST handle 401 by redirecting to login + clearing
  the cached token, not by retrying indefinitely.
- **Network outage mid-session**: Polling failures should surface as a single
  unobtrusive banner ("Connection lost — retrying"), not as a flood of error
  toasts. The banner clears automatically on the next successful poll.
- **Long-running stats view**: An operator leaves the live stats page open
  overnight. The streaming connection MUST reconnect transparently on temporary
  disconnect; memory usage MUST stay bounded (the UI shows the latest snapshot
  only, not a growing history buffer).
- **Token in URL**: The operator MUST NOT be able to share a "logged-in" link.
  Tokens never appear in URLs, query strings, or `Referer` headers.
- **Browser back button mid-form**: Hitting back from a half-filled
  user/grant/rule form prompts the operator to confirm discarding unsaved input.
- **Permission downgrade mid-session**: A superadmin who was demoted to `user`
  while the page is open MUST be unable to perform admin actions; the next
  superadmin-only request returns 403 and the UI updates the navigation to hide
  the admin-only sections within one polling interval.
- **One token, many tabs**: Multiple browser tabs open against the same UI
  share the bearer token via `sessionStorage`. Logging out in one tab logs out
  the others on their next request.
- **Empty state on first launch**: A fresh server has zero rules, zero users
  (besides the legacy superadmin), zero clients. Each list page MUST render a
  helpful empty state with a clear "Create your first..." call-to-action that
  links to the corresponding form.
- **JS bundle exceeds budget**: If the production bundle grows past the size
  budget, the build MUST fail loudly so the operator/developer notices before
  shipping a heavyweight binary.
- **Server-side filter mismatch**: The UI MUST trust the server's RBAC view
  unconditionally — i.e. it never re-filters the response client-side to "hide
  more". This avoids the UI accidentally masking an over-permissive server
  response.

## Requirements *(mandatory)*

### Functional Requirements

#### Authentication & session

- **FR-001**: The UI MUST present a single login screen on first visit that
  accepts a bearer token, with no field for username/password (matches v0.5 auth model).
- **FR-002**: The UI MUST send `Authorization: Bearer <token>` on every request
  to operator endpoints; tokens MUST NOT appear in URLs, query strings,
  `Referer` headers, or any persistent disk storage.
- **FR-003**: The UI MUST store the bearer in `sessionStorage` (cleared on
  browser close), NOT in `localStorage` or cookies.
- **FR-004**: On any 401 response from any operator endpoint, the UI MUST clear
  the cached token, redirect to the login screen, and show a non-blocking
  message "Session expired — please sign in again".
- **FR-005**: The UI MUST detect the logged-in user's role on first successful
  request and use it to gate navigation; admin-only pages MUST be hidden from
  the navigation for non-superadmins AND MUST refuse to load via direct URL.

#### Read views

- **FR-006**: The UI MUST provide read views (lists with sortable / filterable
  columns) for: Users, Credentials (per user), Grants, Rules, Clients, and
  Audit log entries. Each list MUST support empty states. Lists MUST use
  **client-side virtual scrolling** (viewport rendering only) when row counts
  exceed 200 — server endpoints stay unparameterised; the UI fetches the full
  list and virtualises rendering. Server-side pagination is explicitly
  rejected to keep the v0.5 HTTP contract additive-only.
- **FR-007**: List pages MUST refresh their data every 5 seconds without a
  full page reload, and MUST visibly indicate the last-refreshed time and
  any in-flight refresh.
- **FR-008**: Each rule row MUST display its `owner`. For superadmins, the
  Rules page MUST offer an `owner` filter dropdown populated with the list of
  known users.
- **FR-009**: The Rule Detail page MUST display live statistics (bytes in/out,
  active connections, DNS failures, UDP datagrams in/out, active flows, dropped
  flows) and update them at most ≤ 6 s after the underlying server-side stats
  report arrives.
- **FR-010**: The Audit Log page MUST display the last 100 operator.allow /
  operator.deny entries with timestamp, actor, method, path, outcome, and
  reason columns. It MUST support client-side filtering by outcome. The page
  MUST also provide a "Download as JSON" button that produces a
  newline-delimited JSON (NDJSON) file of the currently visible (post-filter)
  audit rows, named `audit-<timestamp>.ndjson`. This is the **only** built-in
  data-export affordance in v1; export from Users / Grants / Rules / Clients
  is deferred to a later release because operators already have the
  `portunus-server <subcmd> --format json` CLI equivalent.
- **FR-011**: The Metrics page MUST display the raw `/metrics` text dump in a
  monospaced, scrollable, read-only view, plus a small dashboard card on the
  main page showing the count of connected clients and total active rules.

#### Mutation views

- **FR-012**: The UI MUST provide forms to create users, issue/rotate/revoke
  credentials, add/revoke grants, push/remove rules, and provision/revoke
  clients. Each form MUST validate inputs client-side AND surface server-side
  validation errors inline next to the offending field.
- **FR-013**: Newly issued / rotated credential tokens MUST be displayed
  exactly once in a modal dialog with a "Copy to clipboard" affordance; closing
  the modal MUST permanently scrub the token from UI state.
- **FR-014**: Cascading delete operations (remove user, revoke grant) MUST
  show a preview of dependent objects (credentials, grants, rules) that will
  also be removed BEFORE the operator confirms.
- **FR-015**: After a successful mutation, the affected list view MUST
  reflect the change within 1 second without requiring a manual refresh.

#### Streaming & freshness

- **FR-016**: Live rule statistics MUST stream from a server endpoint that
  pushes one snapshot per stats interval; the UI MUST reconnect transparently
  on disconnection with exponential backoff (initial 1 s, max 30 s).
- **FR-017**: If the streaming endpoint is unavailable (e.g., proxy strips
  text/event-stream), the UI MUST fall back to polling the non-streaming
  endpoint every 5 s with no visible behaviour change to the user.

#### Cross-cutting & UX

- **FR-018**: The UI MUST support light and dark themes, defaulting to the
  user's `prefers-color-scheme`. The choice MUST persist across reloads on the
  same device.
- **FR-019**: The UI MUST support English and 简体中文; the language toggle
  MUST persist across reloads on the same device. Initial copy MUST be complete
  in both languages.
- **FR-020**: The UI MUST be keyboard-navigable: every interactive element
  reachable via Tab, focus rings visible, no keyboard traps. WCAG AA contrast
  ratios MUST hold in both themes.
- **FR-021**: The UI MUST be usable on mobile viewports for read-only views
  (≥ 375 px width); mutation forms MAY render as read-only with a "Use desktop"
  hint at narrow widths.
- **FR-022**: The UI MUST provide a single global error banner for transient
  network failures and a separate inline error mechanism for per-form
  validation; success notifications MUST be non-blocking and auto-dismiss
  within 5 s.
- **FR-023**: Browser back / forward buttons MUST work intuitively across all
  list pages (URL reflects active filters / pagination); navigating away from
  a partially-filled mutation form MUST prompt for confirmation.

#### Distribution

- **FR-024**: The compiled UI MUST be embedded into the `portunus-server`
  binary so that single-binary distribution is preserved. No separate
  frontend service, no Node runtime requirement at deployment time.
- **FR-025**: The compiled UI MUST be served on the same loopback HTTP
  listener that already serves `/v1/*` and `/metrics`, accessible at the path
  `/` (root).
- **FR-026**: The compiled UI MUST stay below an agreed gzipped JS bundle
  size budget so that the binary remains compact and the page loads quickly
  on a freshly opened browser.

#### Server-side additions (additive)

- **FR-027**: The server MUST expose a streaming variant of the rule-stats
  endpoint that emits one snapshot per stats interval, ownership-checked
  identically to the non-streaming endpoint (FR-013/R-007 of v0.5 carry over).
  Resource model: each rule has **one shared broadcast source** on the server
  side; multiple subscribers (e.g., several operators viewing the same rule)
  fan out from that single source rather than each spawning its own
  per-subscriber timer. There is no hard cap on concurrent subscribers — the
  cost is O(rules), not O(rules × subscribers). Subscriber disconnects
  (graceful or abrupt) MUST be cleaned up automatically without leaking server
  resources.
- **FR-028**: The server MUST expose a superadmin-only audit-log read endpoint
  that returns the most recent N (default 100, max 1000) `operator.allow` /
  `operator.deny` entries from an in-memory ring buffer. The endpoint MUST NOT
  surface raw bearer tokens (Constitution Principle IV — already true at the
  log-emit site, kept true here).
- **FR-029**: Both new endpoints MUST be backward-compatible additions: every
  existing CLI invocation, integration test, and v0.5 API consumer MUST
  continue to function byte-identical without modification.

### Key Entities

- **UI Session**: The browser-side ephemeral state holding the bearer token,
  current user identity (id + role), language preference, and theme preference.
  Token is in sessionStorage; preferences are in localStorage. No server-side
  session record.
- **Audit Entry**: One operator request's allow/deny outcome. Fields:
  timestamp, actor (user id), role, method, path, outcome, reason. Surfaced
  to the UI via the new audit endpoint, sourced from a server-side in-memory
  ring buffer of the last 1000 entries (older entries are dropped silently).
- **Rule Stats Snapshot Stream**: A unidirectional server-to-UI stream of
  `RuleStatsSnapshot` records (the same shape v0.5's `rule-stats` endpoint
  already returns), one per stats interval per subscribed rule. The stream
  closes when the rule is removed or the operator's session ends.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A first-time superadmin can complete the full bootstrap →
  user-add → credential-issue → grant-add → rule-push → live-stats walkthrough
  through the UI in under **5 minutes** of wall-clock on a developer-class
  laptop, without consulting the CLI or documentation.
- **SC-002**: 100% of v0.5 read endpoints reachable through the CLI are also
  reachable through the UI. After a 30-minute usability test, an operator
  unfamiliar with the project can locate every list view (Users, Credentials,
  Grants, Rules, Clients, Audit Log, Metrics) within 30 seconds each.
- **SC-003**: The UI inherits the server's RBAC isolation: across **100%** of
  attempted cross-tenant reads (alice trying to view bob's credentials, bob's
  rules, bob's grants — driven by automated UI tests), the UI surfaces a
  permission-denied state without leaking any cross-tenant data.
- **SC-004**: Live rule stats numbers reflect a server-side change within
  **6 seconds** of the change becoming visible on the equivalent CLI
  `rule-stats` invocation, and the streaming connection survives a transient
  network blip (≤ 30 s outage) without the operator needing to refresh.
- **SC-005**: The compiled UI ships entirely inside the `portunus-server`
  binary; the binary's size grows by **≤ 3 MB** compared to the v0.5.0 baseline
  and the gzipped JS bundle stays **≤ 500 KB**.
- **SC-006**: A token leak audit on the running UI (browser dev-tools session
  storage + network HAR + console output + DOM tree) shows the bearer token
  appears **only** in `sessionStorage` and the `Authorization` header — never
  in URLs, query strings, `Referer` headers, browser history, console logs,
  or DOM text.
- **SC-007**: Every visible UI string in the navigation, page titles, button
  labels, table headers, and form fields is translated into both English and
  简体中文; switching languages mid-session takes **≤ 200 ms** and does not
  trigger a full page reload.

## Assumptions

- **Existing v0.5 API is the canonical contract**: The UI consumes
  `/v1/users`, `/v1/users/{id}/credentials`, `/v1/grants`, `/v1/rules`,
  `/v1/clients`, `/v1/rules/{id}/stats`, and `/metrics` exactly as the
  v0.5.0 release defines them. No request/response shape changes are needed
  on those endpoints; only two additive endpoints (live stats stream, audit
  log read) are introduced.
- **Single-binary distribution is non-negotiable**: The constitution's
  operational constraint ("single static binary distribution per role") is
  treated as a hard requirement, ruling out separate frontend services.
- **Loopback-only by default**: The UI is served on the same operator HTTP
  listener that v0.5 already enforces is loopback-only. Operators who want
  remote access are expected to tunnel via SSH or a reverse proxy of their
  choice; remote-access concerns are out of scope for this feature.
- **Operator's browser is modern**: The UI targets the latest two releases of
  Chrome, Firefox, Safari, and Edge. Internet Explorer and pre-Chromium Edge
  are NOT supported.
- **One operator per tab**: Multi-account login (one tab per identity) is out
  of scope; each browser tab uses one bearer token at a time.
- **Audit log retention is best-effort in-memory**: The audit endpoint reads
  from a server-side ring buffer of the last 1000 entries. Long-term retention,
  cold-storage, and external SIEM forwarding are out of scope for this feature
  and remain available via the existing structured JSON log files.
- **No mTLS / no SSO**: Authentication remains the v0.5 bearer-token model.
  Cert-based client auth and SSO integration are explicitly out of scope and
  tracked separately under `TODO(MTLS_REVISIT)`.
- **Mobile is read-only**: The UI is desktop-primary; mobile viewports may
  view lists and detail pages but mutation forms render as read-only with a
  "Switch to desktop" hint.
- **i18n scope is bilingual**: Only English and 简体中文 are required. Other
  languages are out of scope for this feature.
- **Stream protocol is server-sent events** (a one-way text-based push channel
  over plain HTTP): chosen for compatibility with the existing single-listener
  architecture; WebSockets are not required.
- **Bulk data export is audit-only in v1**: Users / Grants / Rules / Clients
  have CLI `--format json` equivalents and do NOT need an in-UI export. Audit
  log has no CLI equivalent (it's a v0.6 server-side ring buffer), so it gets
  the only "Download as JSON" affordance in v1. Other list exports defer to
  a later release.

## Verified

Measurements taken 2026-05-08 against commit on branch `006-management-web-ui`,
release build (`cargo build --release -p portunus-server`), macOS x86_64.

- **SC-005 (gzipped JS bundle ≤ 500 KB)**: PASS.
  `pnpm build` reports `Size: 101.06 kB gzipped` for the main entry chunk;
  size-limit gate (configured at 500 KB) passes with ~80% headroom.
  Per-chunk breakdown: `index-*.js` 313 KB raw → 101 KB gzip; `UserDetail-*.js`
  39 KB raw → 13 KB gzip; `EmptyState-*.js` 16 KB raw → 6 KB gzip; CSS 20 KB
  raw → 5 KB gzip.

- **SC-004 (binary size delta acceptable)**: 161 KB delta vs UI-less build.
  - `PORTUNUS_SKIP_WEBUI=1 cargo build --release -p portunus-server`
    → 9,088,752 bytes (8.66 MB).
  - `cargo build --release -p portunus-server` (with embedded SPA, sourcemaps
    + stats.html excluded via rust-embed `include-exclude`)
    → 9,254,048 bytes (8.83 MB).
  - Single-binary distribution preserved; no separate Node runtime required.

- **SC-006 (token-leak audit)**: PASS via
  `webui/tests/e2e/token-leak-audit.spec.ts` (T066), executed against a
  release-built `portunus-server` binary in headless chromium 130.
  Asserts: sessionStorage holds the bearer; localStorage limited to
  `portunus.theme` + `portunus.lang`; bearer never appears in DOM text or
  URL; every captured `/v1/*` and `/metrics` request carries the bearer
  ONLY in the `Authorization` header (no cookies, no query string).

- **SC-001 (5-minute first-rule onboard)**: pending one-time stopwatch
  walkthrough of `quickstart.md` §3–6 by an unfamiliar operator. The
  e2e `quickstart-walkthrough.spec.ts` proves the SPA flow stays
  intact; wall-clock measurement of how fast a human operator can
  drive it from cold start is the missing piece.

- **Final test gate (T073)**: ALL GREEN.
  - `cargo test --workspace --tests` — 40 test files pass, 0 fail.
  - `cargo clippy --workspace --all-targets -- -D warnings` — clean.
  - `pnpm test` — 10/10 unit tests pass (permissions, audit-export,
    i18n-coverage).
  - `pnpm lint` — 0 errors, 5 informational `react-refresh` warnings.
  - `pnpm build` — green; size-limit gate passes (101 KB ≤ 500 KB).
  - `pnpm exec playwright test` — **12/12 e2e suites pass** in headless
    chromium 130 against a release-built `portunus-server` (T023, T041,
    T048, T057, T065, T066).

### Server-side follow-ups landed during e2e gate-up

Two real bugs surfaced by the Playwright suites and were fixed before
declaring the gate green:

1. **Cross-listener metrics access** — the SPA fetches Prometheus output
   to render the dashboard gauges + `/metrics` page, but `/metrics` is
   bound to a separate loopback listener (`metrics_listen`, default
   `127.0.0.1:7081`) for unauthenticated scrapers. When the SPA loads
   same-origin from `operator_http_listen` (default `127.0.0.1:7080`),
   it can't reach the metrics port. Added a superadmin-gated mirror at
   `GET /v1/metrics` on the operator HTTP listener; SPA now hits that
   path. The standalone `/metrics` listener is unchanged (Prometheus
   scrapers continue scraping it without bearer tokens).

2. **Self-rotation 401 race** — `useRotateCredential.onSuccess` invalidated
   the credentials cache before the new bearer was swapped into
   `sessionStorage`. The auto-refetch then sent the now-revoked old
   bearer → 401 → AuthGate's UNAUTHORIZED listener bounced the user to
   `/login` before the issuance modal could render. Fixed by removing
   the auto-invalidation from the rotate hook and having `UserDetail.tsx`
   swap the token first, then invalidate. The non-self path is
   unaffected.
