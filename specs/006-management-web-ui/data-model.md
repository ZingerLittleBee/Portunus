# Data Model: 006-management-web-ui

This feature is largely a **read-side projection** of v0.5's existing
data model. The only new server-side entities are the audit ring buffer
and the per-rule SSE broadcast channel; the only new client-side
entities are the UI session and a few derived view-models.

The v0.5 data model (User, Credential, Grant, Rule, Client, RuleStats)
is referenced by name only — see `specs/005-multi-user-rbac/data-model.md`
for the canonical definitions.

---

## Server-side: new entities

### `AuditEntry`

One row in the in-memory ring buffer; one record per
`operator.allow` / `operator.deny` event emitted by `auth_layer`.

| Field | Type | Notes |
|---|---|---|
| `timestamp` | `chrono::DateTime<Utc>` | When the auth-layer decision happened (server clock). RFC 3339 over the wire. |
| `actor` | `String` | The post-verify `OperatorIdentity::user_id`. `_anonymous` for pre-auth denials (missing/bad bearer, bootstrap_required). |
| `role` | `Option<OperatorRole>` | `None` for pre-auth denials; otherwise the actor's role at the time of the request. |
| `method` | `String` | HTTP method (`GET`, `POST`, `DELETE`, …). |
| `path` | `String` | Request path (e.g., `/v1/users/alice/credentials`). Query string stripped (no token leakage). |
| `outcome` | `enum {Allow, Deny}` | Mirrors the existing log event names. |
| `reason` | `Option<String>` | `None` for Allow; for Deny, the static `RbacError::code()` string. Bounded enum. |

**Cardinality**: capacity 1000 (oldest dropped silently;
`portunus_audit_buffer_drops_total` counter increments).

**Ownership**: held inside `AppState::audit_log: Arc<AuditRing>`.
`AuditRing` is a thin wrapper over `Mutex<VecDeque<AuditEntry>>` with
`push` / `snapshot(limit)` methods. Reads are short critical sections
(clone the suffix into a `Vec`).

**Lifecycle**:
- Created at `AppState::new` time, empty.
- Pushed-to from inside `auth_middleware` (both allow and deny paths)
  *after* the existing `info!` / `warn!` log emission.
- Read from `GET /v1/audit?limit=N` handler.
- No persistence; cleared on server restart.

**Serialisation**:
```json
{
  "timestamp": "2026-05-07T13:42:11Z",
  "actor": "alice",
  "role": "user",
  "method": "POST",
  "path": "/v1/rules",
  "outcome": "deny",
  "reason": "port_outside_grant"
}
```

---

### `RuleStatsBroadcast` (per-rule fanout source)

One `tokio::sync::broadcast::Sender<RuleStatsSnapshot>` per active
rule, lazily created on first SSE subscriber.

| Field | Type | Notes |
|---|---|---|
| `sender` | `broadcast::Sender<RuleStatsSnapshot>` | One snapshot per StatsReport tick (5 s). |
| `rule_id` | `RuleId` | Used by `RuleStatsCache::observe` to look up the right sender. |

**Ownership**: lives inside `RuleStatsCache::inner`, alongside the
existing `HashMap<RuleId, CachedEntry>`. New field:
`broadcasts: HashMap<RuleId, broadcast::Sender<RuleStatsSnapshot>>`.

**Lifecycle**:
- Created lazily on first `GET /v1/rules/{id}/stats/stream` for a
  given rule.
- Each call to `RuleStatsCache::observe` (already in the StatsReport
  hot path) does a non-blocking `try_send` if the broadcast exists;
  no-op otherwise.
- Dropped when the rule is removed (`RuleStatsCache::drop_rule` is
  extended to also remove the broadcast entry; subscribers receive
  `RecvError::Closed` and the SSE stream ends naturally).

**Cardinality**: at most one per active rule. Auto-reclaimed when the
last receiver drops AND the rule is removed.

**No serialisation** — purely an in-process channel.

---

### `OperatorIdentitySelf` (response shape for `/v1/users/me`)

The minimal identity projection needed by the SPA on first load.
Subset of v0.5's `OperatorIdentity`.

| Field | Type | Notes |
|---|---|---|
| `user_id` | `String` | Caller's id. |
| `role` | `OperatorRole` | `superadmin` or `user`. |
| `display_name` | `String` | For UI greeting. |

Returned by a new 5-line handler that reads the `OperatorIdentity` from
request extensions (already populated by `auth_middleware`).

---

## Client-side: SPA state

### `UISession` (sessionStorage)

| Key | Value | Notes |
|---|---|---|
| `portunus.token` | string (URL-safe base64, 256 bit) | Bearer; sent on every request. Cleared on 401 / logout / tab close. |

Wrapped by `webui/src/auth/token-store.ts` — a minimal adapter
providing `get` / `set` / `clear` and a `subscribe` callback for
cross-tab logout (via `storage` events on `sessionStorage` siblings).

### `UIPreferences` (localStorage)

| Key | Value | Notes |
|---|---|---|
| `portunus.theme` | `"light" \| "dark" \| "system"` | `"system"` follows `prefers-color-scheme`. |
| `portunus.lang` | `"en" \| "zh-CN"` | `null` ⇒ derive from `navigator.languages[0]`. |

### `Identity` (in-memory, TanStack Query cache)

```ts
type Identity = {
  user_id: string;
  role: "superadmin" | "user";
  display_name: string;
};
```

Cached under query key `["me"]`. Refreshed by `<AuthGate>` on first
successful render. Used by `permissions.ts` to gate navigation and by
`<Header>` to greet the operator.

### `ListView<T>` (one per resource page)

Generic view-model around TanStack Query's result shape:

```ts
type ListView<T> = {
  data: T[];                       // filtered server response
  isLoading: boolean;
  isFetching: boolean;             // background refetch indicator
  error: Error | null;
  lastFetchedAt: Date;             // for "Last refreshed N s ago"
};
```

No new server data is invented; this is just the shape the page
components consume from the API hooks.

### `LiveStats` (per rule-detail page)

```ts
type LiveStats = {
  snapshot: RuleStatsSnapshot;     // latest received
  source: "sse" | "polling";       // which transport is currently live
  lastReceivedAt: Date;
  reconnectAttempts: number;       // 0 if connected
};
```

Updated on each SSE event or polling refresh. Memoised so React
re-renders only when the snapshot actually changes.

---

## Validation rules

### Server-side (new endpoints)

- `GET /v1/audit?limit=N`:
  - `N` defaults to 100, max 1000 (clamped server-side; exceeding
    returns 422 `invalid_limit`).
  - Caller must have role `superadmin`; otherwise 403 `role_required`.
- `GET /v1/rules/{id}/stats/stream`:
  - Same ownership check as the existing
    `GET /v1/rules/{id}/stats` (FR-013/R-007 of v0.5 carry over).
  - Sets `Content-Type: text/event-stream` and
    `Cache-Control: no-cache`. Disables HTTP/1.1 keep-alive
    coalescing in the response.
- `GET /v1/users/me`:
  - Authenticated only — 401 if no/invalid bearer.
  - Always returns 200 with the caller's projection; no extra
    auth gate.

### Client-side (UI forms)

The UI re-uses the v0.5 server-side validation. Forms display
validation errors inline next to the offending field (FR-012); they do
NOT pre-validate format-strict (e.g., `user_id` regex) because the
server is already authoritative and shape-drift between client and
server validation is a known-bug source.

---

## State transitions

### `AuditEntry` (none; immutable on creation)

### `RuleStatsBroadcast`

```
Absent
  └──first SSE subscriber──→ Active(receiver_count = 1)
                              ├──new subscriber──→ Active(receiver_count + 1)
                              ├──subscriber drops──→ Active(receiver_count - 1)  [if > 0]
                              └──rule removed──→ Closed (drop sender; receivers see Closed)
```

### `UISession`

```
Unauthenticated
  └──login form submit──→ Authenticated(token cached)
                            ├──401 from any /v1/* call──→ Unauthenticated (clear token)
                            ├──logout button──→ Unauthenticated (clear token)
                            └──tab close──→ (sessionStorage cleared by browser)
```

---

## Cardinality and resource budgets

| Object | Per-server cap | Resident memory at cap |
|---|---|---|
| `AuditEntry` | 1000 | ≈ 200 KB |
| `RuleStatsBroadcast` (sender) | one per rule (≤ 10k) | ≤ a few MB at 10k rules |
| `broadcast::Receiver` | one per SSE subscriber | tiny per-receiver state, capped by `broadcast` channel size = 16 (default) |
| Concurrent SSE connections | unbounded by design (R-008) | dominated by axum/hyper's per-connection cost (~few KB each) |

| Object (client) | Per-tab cap | Resident memory |
|---|---|---|
| TanStack Query cache (all resources) | bounded by user activity | typical 1-5 MB |
| Virtualised list rendered DOM | viewport-only, ≤ 50 rows visible | tiny |
| `LiveStats` history | 1 (latest snapshot only) | tiny |

All caps are well under reasonable limits.
