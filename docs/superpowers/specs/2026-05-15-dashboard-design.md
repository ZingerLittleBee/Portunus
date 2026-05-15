# Dashboard Enhancement — Design

**Date:** 2026-05-15
**Branch:** `semarang`
**Status:** Approved, awaiting implementation plan
**Replaces:** the 63-line placeholder `webui/src/pages/Dashboard.tsx`

## Goal

Replace the current two-card Dashboard with a role-aware, three-zone
overview that exposes the data Portunus already collects (clients,
rules, target health, Prometheus gauges, per-user traffic history,
audit log, quotas) and gives both superadmins and tenant users a
single page that answers:

1. **What is happening right now?** (KPIs, alerts)
2. **What needs my attention?** (unhealthy targets, offline clients,
   recent operator actions)
3. **What happened over the last hour / day / week?** (throughput
   chart, Top-N rules)

The MVP delivers all three zones for both roles in a single PR.

## Audience

Both **superadmin** and **tenant user**. The same route
`/dashboard` switches content based on `identity.role`:

- **SuperadminDashboard** — global aggregates across all tenants,
  reads `/v1/metrics` (Prometheus text).
- **TenantDashboard** — owner-scoped data only, never calls
  `/v1/metrics` (lacks permission); uses
  `/v1/users/{me}/traffic` and per-rule stats.

## Layout — Information-Dense (option A)

Selected in brainstorm `2026-05-15`:

```
┌──────────────────────────────────────────────────────────────────┐
│  ⚠ Alert banner (only if issues > 0)                             │
├──────────────────────────────────────────────────────────────────┤
│  KPI · 6 cards in single row                                     │
├──────────────────────────────────────────────────────────────────┤
│  Unhealthy targets   │  Offline clients   │  Recent audit (10)   │
├──────────────────────┴────────────────────┴──────────────────────┤
│  Throughput chart · 1h/24h/7d toggle      │  Top 5 rules         │
└───────────────────────────────────────────┴──────────────────────┘
```

KPI content (superadmin):
1. Connected clients (X / Y)
2. Active rules
3. Targets OK (X / Y)
4. Throughput now (B/s, two-point derivative)
5. Total transferred since start (cumulative)
6. Connections now

KPI content (tenant):
1. My connected clients
2. My active rules
3. My throughput now
4. My 24h transferred
5. My quota used % (with hard-cap context)
6. (slot reserved — see Open Questions)

The alert banner is derived client-side from the same queries — no
new endpoint for it.

## Architecture

```
webui/src/pages/Dashboard.tsx              router shell, ~30 LOC
webui/src/pages/dashboard/
  SuperadminDashboard.tsx                  ~120 LOC
  TenantDashboard.tsx                      ~120 LOC
  components/
    KpiCard.tsx
    AlertBanner.tsx
    UnhealthyTargetsPanel.tsx
    OfflineClientsPanel.tsx
    RecentAuditPanel.tsx
    ThroughputChart.tsx        (Recharts LineChart + range toggle)
    TopRulesPanel.tsx
    useDashboardRange.ts       (1h/24h/7d state + bucket selection)
    useThroughputRate.ts       (cumulative counter → B/s, useRef
                               keeps prior sample)
```

The current `Dashboard.tsx` is a single 63-line file; the enhanced
version totals ~350 LOC, which exceeds the project's
single-responsibility threshold. The split keeps each file under
~120 LOC with one clear purpose, isolating the role-divergent data
sources (which differ enough that conditional rendering in one file
would entangle two distinct query graphs).

## Data Sources

### Superadmin

| Region | Source | Refresh |
|---|---|---|
| KPI · Clients | `useClientsList()`, derive connected/total | react-query default |
| KPI · Rules | `useRulesList()` | default |
| KPI · Targets OK | extended `parseDashboardGauges()` reads `portunus_target_healthy{...}` | 5s |
| KPI · Throughput now | `useThroughputRate()` — diff of `portunus_rule_bytes_*_total` between two polls (timestamp-aware) | 5s |
| KPI · Total transferred | sum of `portunus_rule_bytes_*_total` (labelled clearly as cumulative, not 24h) | 5s |
| KPI · Conns now | `portunus_active_connections` (or sum of `portunus_rule_active_conns` if no top-level gauge exists — verify during implementation) | 5s |
| Alert banner | derived: unhealthy-target count + offline-client count | piggybacks above queries |
| Unhealthy targets panel | `useRulesList()` flat-map targets, filter `health.healthy === false` | default |
| Offline clients panel | `useClientsList()` filter `connected=false`, sort by `last_seen_at` | default |
| Recent audit panel | `useAuditLog({ limit: 10 })` | default |
| Throughput chart | **new endpoint** `/v1/traffic/global` (see Backend) | on range change |
| Top 5 rules | parse `portunus_rule_bytes_*_total{rule="..."}` from metrics text, sort by in+out, take 5 | 5s |

### Tenant

| Region | Source | Notes |
|---|---|---|
| KPI · My clients | `useClientsList()` (already owner-filtered server-side) | |
| KPI · My rules | `useRulesList()` | |
| KPI · My throughput now | aggregate `useRuleStats(id)` deltas across my rules | per-rule fan-out |
| KPI · 24h transferred | `useUserTraffic(myUserId, last 24h)` summed | existing API |
| KPI · Quota used % | `useUserQuotas(myUserId)` | existing |
| Alert banner | derived: my offline clients + my unhealthy targets + quota ≥ 80% | no new endpoint |
| Unhealthy / Offline / Audit panels | same hooks as superadmin (server already scopes) | |
| Throughput chart | `useUserTraffic(myUserId, range)` | existing bucketed API |
| Top 5 rules | sort my rules by cumulative bytes from `useRuleStats` | |

## Backend Changes

**Exactly one new endpoint:**

`GET /v1/traffic/global?from=&to=&bucket=` — superadmin-only

- Location: `crates/portunus-server/src/operator/quota_http.rs`,
  alongside the existing per-user / per-client traffic handlers.
- Auth: rejects non-superadmin with 403.
- Behaviour: in SQLite, `SELECT bucket_ts, SUM(bytes_in) AS bytes_in,
  SUM(bytes_out) AS bytes_out FROM user_traffic_buckets WHERE
  bucket_ts BETWEEN ? AND ? GROUP BY bucket_ts ORDER BY bucket_ts`,
  reusing the bucket-resolution logic (1m if range ≤ 7d, else 1h).
- Wire shape: identical `TrafficResponse { samples: TrafficSample[] }`
  the per-user endpoint returns — frontend type already exists.
- Contract tests:
  - tenant `GET` → 403
  - superadmin `GET` with mixed-user fixture → samples equal the sum
    of all `user_traffic` rows in the window.

**No other backend changes.** Prometheus exposition is not modified
(no new metric, no relabelling) — the additional gauges
(`portunus_target_healthy`, `portunus_active_connections`,
`portunus_rule_active_conns`) referenced above must already exist;
during implementation, verify which subset is present in v1.4.0 and
either parse what's there or fall back to deriving from
list endpoints. If a referenced gauge is genuinely missing, that
KPI is dropped from the layout rather than added to the backend in
this scope.

## Error Handling

- Each panel queries independently; one failure does not affect
  others. No `ErrorBoundary` is introduced.
- `/v1/metrics` not available (tenant) → `useDashboardGauges()` is
  never called; the gauge-derived KPIs are simply absent on the
  tenant layout.
- `/v1/traffic/global` failure → chart shows an inline alert + Retry
  button; KPI row and middle panels stay live.
- Alert banner with 0 issues → not rendered.
- Empty-tenant case (no clients, no rules) → each panel renders an
  empty-state with i18n strings and a deep link
  (`/clients/provision`, `/rules/push`).
- First poll of `useThroughputRate` (no prior sample) → KPI shows
  `calculating…`; second poll yields the rate.
- Counter reset (negative diff) → treat as 0, do not display a
  negative throughput.
- Time-range toggle → keep chart skeleton in place, only swap data
  to avoid layout shift.
- Quota fetch fails on tenant view → quota KPI shows `—`, no toast.

## Testing

Per Constitution Principle III, integration tests use real sockets
where applicable; component tests stay in jsdom.

**Frontend** (`webui/src/pages/dashboard/__tests__/`):
- `parseDashboardGauges.test.ts` — extended cases: targets, active
  conns, top-rules sort, label injection, NaN, missing lines.
- `useThroughputRate.test.ts` — two-sample diff, timestamp gap,
  counter reset, first-call `null` return.
- `SuperadminDashboard.test.tsx` — MSW mocks for `/v1/metrics` and
  `/v1/traffic/global`; assert all 6 KPI cells render, banner shows
  iff issues > 0, role guard redirects non-superadmin.
- `TenantDashboard.test.tsx` — MSW mocks for tenant-scoped
  endpoints; assert `/v1/metrics` is never called.

**Backend** (`crates/portunus-server/tests/`):
- `traffic_global_contract.rs` — superadmin 200 + tenant 403; sum
  invariant against per-user fixture.
- doc-test or unit test in `quota_http.rs` for the aggregation SQL.

**Manual checklist:**
- `make dev` → superadmin login → all 6 KPIs + global chart render.
- Create a tenant via `/users/create`, log in as tenant → tenant
  layout; DevTools confirms no `/v1/metrics` request.
- Kill an upstream target → unhealthy banner + panel appear within
  one refetch.
- Toggle 1h / 24h / 7d → chart re-fetches with the correct bucket.

## Bundle Size & Performance

- Recharts adds ~100 KB gzipped. Current `webui/dist` is ~280 KB,
  ending state ~380 KB, within the 500 KB `size-limit` budget that
  `pnpm build` enforces.
- The 5s `/v1/metrics` poll already exists; the additional parser
  work is microseconds on a ~10 KB document.
- Top-N sort on ≤ 1000 rules is O(n log n), no virtualization
  needed.

## Open Questions (resolve during implementation, do not block plan)

1. Tenant KPI slot 6 — quota uses 5 slots already. Confirm during
   implementation whether to fill with "Active conns" (if a
   tenant-readable equivalent of `portunus_rule_active_conns`
   exists via `useRuleStats`) or leave a 5-cell layout.
2. Which Prometheus gauges are actually exposed in v1.4.0
   (`portunus_target_healthy`, `portunus_active_connections`).
   Verify by reading
   `crates/portunus-server/src/metrics/` before the parser
   extension lands.
3. "Total transferred since start" label wording — needs i18n; the
   tooltip must clarify this is a cumulative value since process
   start, not a sliding window.

## Out of Scope

- Configurable dashboards / drag-and-drop layout
- WebSocket push for KPI freshness (5s polling is sufficient)
- Per-rule drill-down on the dashboard (already exists at
  `/rules/:id`)
- Alert acknowledgement / muting
- Email / webhook notifications for unhealthy state
- Historical retention beyond what `user_traffic_buckets` already
  stores

## Implementation Order (advisory)

The plan written next will sequence the work. Suggested order:

1. Backend: `/v1/traffic/global` endpoint + contract test.
2. Frontend: extend `parseDashboardGauges()` + `useThroughputRate`
   hook with unit tests.
3. Frontend: extract `KpiCard`, `AlertBanner` shared components.
4. Frontend: `SuperadminDashboard` with all six regions.
5. Frontend: `TenantDashboard` with the tenant-scoped variants.
6. Frontend: router shell in `Dashboard.tsx` that branches on role.
7. i18n keys, empty states, polishing.
8. Bundle-size check (`pnpm build`).
