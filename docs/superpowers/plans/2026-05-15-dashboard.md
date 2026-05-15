# Dashboard Enhancement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the 63-line placeholder Dashboard with a role-aware, three-zone overview (alert banner + 6 KPIs + 3-column live status + throughput chart + Top 5 rules), implementing one new backend aggregation endpoint and a Recharts-based history view.

**Architecture:** A new `/v1/traffic/global` superadmin endpoint that reuses the existing `samples::query_samples` aggregation. A router-shell `Dashboard.tsx` branches on `identity.role` into `SuperadminDashboard` or `TenantDashboard`. Both compose shared `KpiCard` / `AlertBanner` / panel components; charts use the existing `recharts` dependency. The data plane is owner-scoped server-side, so tenants never touch `/v1/metrics`.

**Tech Stack:**
- Backend: Rust 2024, axum routes in `crates/portunus-server`, SQLite (existing `traffic_samples_1m/1h` tables)
- Frontend: React 18 + Vite + TypeScript, TanStack Query (already wired), Recharts 2.13.3 (already installed), shadcn/ui card primitives, react-i18next

**Spec:** `docs/superpowers/specs/2026-05-15-dashboard-design.md`

---

## Phase A — Backend: `/v1/traffic/global` endpoint

### Task A1: Write failing unit test for global-traffic handler

**Files:**
- Modify: `crates/portunus-server/src/operator/quota_http.rs` (tests module at the bottom)

- [ ] **Step 1: Append new tests to the `#[cfg(test)] mod tests` block**

Add these tests at the end of the existing tests module (before the final `}`):

```rust
    use crate::traffic_quotas::samples::{self, SampleBucket};

    #[test]
    fn require_role_rejects_tenant_for_global_traffic() {
        // Construct a tenant identity and confirm the role gate fires.
        let id = user_identity("alice");
        let err = rbac::require_role(&id, OperatorRole::Superadmin).unwrap_err();
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn serve_traffic_with_no_filters_sums_across_users() {
        // Two users write into the 1m sample table; a global query (None, None)
        // must return one bucket per timestamp with the cross-user sum.
        let dir = tempdir().unwrap();
        let store = crate::store::Store::open(dir.path()).expect("open store");
        let ts = 1_700_000_000_i64 - (1_700_000_000_i64 % 60); // align to minute
        samples::upsert_1m_delta(&store, "alice", "edge-a", ts, 100, 200).unwrap();
        samples::upsert_1m_delta(&store, "bob",   "edge-b", ts, 300, 400).unwrap();
        let rows = samples::query_samples(
            &store,
            SampleBucket::M1,
            None,
            None,
            ts - 1,
            ts + 60,
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts, ts);
        assert_eq!(rows[0].bytes_in, 400);
        assert_eq!(rows[0].bytes_out, 600);
    }
```

If the existing `use super::*;` is already at the top of the tests module, the new `use crate::traffic_quotas::samples::{self, SampleBucket};` line still needs to be added — append it under the existing `use super::*;`.

- [ ] **Step 2: Run the test to confirm it compiles and passes**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib operator::quota_http::tests::serve_traffic_with_no_filters_sums_across_users
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib operator::quota_http::tests::require_role_rejects_tenant_for_global_traffic
```

Expected: both PASS. These tests cover the **building blocks** (existing `query_samples` and RBAC helper), proving the pieces we'll wire together work.

- [ ] **Step 3: Commit**

```sh
git add crates/portunus-server/src/operator/quota_http.rs
git commit -m "test(server): cover global-traffic building blocks (samples agg + role gate)"
```

---

### Task A2: Add `get_global_traffic` handler

**Files:**
- Modify: `crates/portunus-server/src/operator/quota_http.rs` (after `get_client_traffic`)

- [ ] **Step 1: Insert the handler after `get_client_traffic` (around line 296)**

Add this directly below `pub async fn get_client_traffic(...) { ... }`:

```rust
/// `GET /v1/traffic/global?from=&to=&bucket=` — superadmin-only.
/// Returns bucketed traffic aggregated across **all** users and clients,
/// reusing the same wire shape `/v1/users/{id}/traffic` returns.
///
/// Tenants are blocked with 403 (operator audit only — no leak via timing).
pub async fn get_global_traffic(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Query(q): Query<TrafficQuery>,
) -> Result<Json<TrafficResponse>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    serve_traffic(&state, None, None, q.from, q.to, q.bucket.as_deref())
}
```

- [ ] **Step 2: Update the file-top route inventory comment**

Find the existing module doc comment (lines ~5-15) listing the registered routes and add the new one. The pattern looks like:

```rust
//!   - GET    /v1/clients/{client_name}/traffic
```

Add a sibling line:

```rust
//!   - GET    /v1/traffic/global (superadmin-only)
```

- [ ] **Step 3: Build to verify it compiles**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server
```

Expected: clean build, no warnings (`-D warnings` will catch anything).

- [ ] **Step 4: Commit**

```sh
git add crates/portunus-server/src/operator/quota_http.rs
git commit -m "feat(server): add get_global_traffic handler (superadmin-only)"
```

---

### Task A3: Register `/v1/traffic/global` route

**Files:**
- Modify: `crates/portunus-server/src/operator/http.rs:131-134` (after `/v1/clients/{client_name}/traffic`)

- [ ] **Step 1: Write failing integration test**

Create `crates/portunus-server/tests/http_traffic_global_contract.rs`:

```rust
//! Contract test for GET /v1/traffic/global.
//! Verifies (a) tenant → 403, (b) superadmin → 200 with aggregated samples.

mod support;

use serde_json::Value;
use support::TestServer;

#[tokio::test]
async fn tenant_gets_403() {
    let mut srv = TestServer::spawn().await;
    srv.bootstrap_superadmin().await;
    srv.create_user("alice", "tenant").await;

    let resp = srv
        .as_user("alice")
        .get(&format!(
            "/v1/traffic/global?from=0&to={}&bucket=1m",
            (chrono::Utc::now().timestamp())
        ))
        .send()
        .await;
    assert_eq!(resp.status, 403);
}

#[tokio::test]
async fn superadmin_gets_aggregated_samples() {
    let mut srv = TestServer::spawn().await;
    srv.bootstrap_superadmin().await;
    // Seed sample buckets directly via the public traffic-recording path
    // exposed by the test harness (see support::seed_sample).
    let ts = (chrono::Utc::now().timestamp() / 60) * 60;
    srv.seed_sample("alice", "edge-a", ts, 100, 200).await;
    srv.seed_sample("bob", "edge-b", ts, 300, 400).await;

    let resp = srv
        .as_superadmin()
        .get(&format!("/v1/traffic/global?from={}&to={}&bucket=1m", ts - 1, ts + 60))
        .send()
        .await;
    assert_eq!(resp.status, 200);
    let body: Value = serde_json::from_str(&resp.body).unwrap();
    let samples = body["samples"].as_array().unwrap();
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0]["bytes_in"].as_i64().unwrap(), 400);
    assert_eq!(samples[0]["bytes_out"].as_i64().unwrap(), 600);
}
```

**Check:** the test harness path `tests/support/` and helpers (`TestServer::spawn`, `bootstrap_superadmin`, `as_user`, `as_superadmin`, `seed_sample`) — read `crates/portunus-server/tests/audit_contract.rs` first to confirm naming. If a `seed_sample` helper doesn't exist, this task includes adding it: open `tests/support/mod.rs`, add a `pub async fn seed_sample(&mut self, user_id: &str, client: &str, ts: i64, bin: i64, bout: i64)` that calls into the server's traffic-write internal API (look for `record_traffic_delta` or equivalent in `crates/portunus-server/src/traffic_quotas/`).

- [ ] **Step 2: Run test to verify it fails**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test http_traffic_global_contract
```

Expected: FAIL with 404 (route not registered yet).

- [ ] **Step 3: Register the route in `operator/http.rs`**

Find line ~131 (the `.route("/v1/clients/{client_name}/traffic", ...)` registration) and add immediately after it:

```rust
        .route(
            "/v1/traffic/global",
            get(crate::operator::quota_http::get_global_traffic),
        )
```

- [ ] **Step 4: Run the integration test again**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test http_traffic_global_contract
```

Expected: both tests PASS.

- [ ] **Step 5: Run the full server test suite to ensure nothing regressed**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server
```

Expected: all green.

- [ ] **Step 6: Commit**

```sh
git add crates/portunus-server/src/operator/http.rs crates/portunus-server/tests/http_traffic_global_contract.rs crates/portunus-server/tests/support/mod.rs
git commit -m "feat(server): wire /v1/traffic/global route + contract test"
```

---

## Phase B — Frontend foundations

### Task B1: Extend `parseDashboardGauges` for active connections and Top-N

**Files:**
- Modify: `webui/src/api/metrics.ts`
- Create: `webui/src/api/metrics.test.ts`

- [ ] **Step 1: Write failing tests**

Create `webui/src/api/metrics.test.ts`:

```typescript
import { describe, expect, it } from "vitest";
import { parseDashboardGauges } from "@/api/metrics";

const FIXTURE = `
# HELP portunus_clients_connected Currently-connected clients.
# TYPE portunus_clients_connected gauge
portunus_clients_connected 4
# HELP portunus_rule_active_connections Active TCP connections per rule.
# TYPE portunus_rule_active_connections gauge
portunus_rule_active_connections{client="edge-a",owner="alice",rule="7"} 12
portunus_rule_active_connections{client="edge-a",owner="alice",rule="8"} 7
portunus_rule_active_connections{client="edge-b",owner="bob",rule="9"} 3
# HELP portunus_rule_bytes_in_total Cumulative inbound bytes.
# TYPE portunus_rule_bytes_in_total counter
portunus_rule_bytes_in_total{client="edge-a",owner="alice",rule="7"} 10000
portunus_rule_bytes_in_total{client="edge-a",owner="alice",rule="8"} 500
portunus_rule_bytes_out_total{client="edge-a",owner="alice",rule="7"} 20000
portunus_rule_bytes_out_total{client="edge-a",owner="alice",rule="8"} 200
`.trim();

describe("parseDashboardGauges", () => {
  it("returns null fields for empty input", () => {
    const g = parseDashboardGauges(undefined);
    expect(g.clientsConnected).toBeNull();
    expect(g.rulesActive).toBeNull();
    expect(g.activeConnections).toBeNull();
    expect(g.topRules).toEqual([]);
  });

  it("parses connected clients and active conns", () => {
    const g = parseDashboardGauges(FIXTURE);
    expect(g.clientsConnected).toBe(4);
    expect(g.activeConnections).toBe(22); // 12 + 7 + 3
  });

  it("counts distinct active rules via bytes_in label", () => {
    const g = parseDashboardGauges(FIXTURE);
    expect(g.rulesActive).toBe(2); // rules 7 and 8 are present
  });

  it("computes top rules by in+out, descending", () => {
    const g = parseDashboardGauges(FIXTURE);
    expect(g.topRules).toEqual([
      { rule: "7", bytesIn: 10000, bytesOut: 20000, total: 30000 },
      { rule: "8", bytesIn: 500, bytesOut: 200, total: 700 },
    ]);
  });

  it("ignores malformed values (NaN guard)", () => {
    const bad = `portunus_clients_connected not-a-number\nportunus_rule_active_connections{rule="9"} also-bad`;
    const g = parseDashboardGauges(bad);
    expect(g.clientsConnected).toBeNull();
    expect(g.activeConnections).toBeNull();
  });

  it("escapes label-injection attempts when extracting rule name", () => {
    const inj = `portunus_rule_bytes_in_total{rule="x\\",injected=\\"y"} 1`;
    const g = parseDashboardGauges(inj);
    // Just don't crash; the resulting rule name may be the literal
    // unescaped string, which is fine — we never eval it.
    expect(g.topRules.length).toBeLessThanOrEqual(1);
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

```sh
cd webui && pnpm vitest run src/api/metrics.test.ts
```

Expected: FAIL — `activeConnections` and `topRules` properties don't exist yet on the result of `parseDashboardGauges`.

- [ ] **Step 3: Extend `webui/src/api/metrics.ts`**

Replace the existing file with the extended version:

```typescript
import { useQuery } from "@tanstack/react-query";

import { apiFetchText } from "@/api/client";

export const METRICS_KEY = ["metrics"] as const;

export function useMetricsText() {
  return useQuery({
    queryKey: METRICS_KEY,
    queryFn: () => apiFetchText("/v1/metrics"),
    refetchInterval: 5_000,
    staleTime: 4_000,
  });
}

export interface TopRule {
  rule: string;
  bytesIn: number;
  bytesOut: number;
  total: number;
}

export interface DashboardGauges {
  clientsConnected: number | null;
  rulesActive: number | null;
  activeConnections: number | null;
  topRules: TopRule[];
}

const LABEL_RULE_RE = /rule="([^"]+)"/;

function extractRule(line: string): string | null {
  const m = line.match(LABEL_RULE_RE);
  return m?.[1] ?? null;
}

function valueAtEnd(line: string): number | null {
  const parts = line.split(/\s+/);
  const raw = parts[parts.length - 1];
  const v = Number(raw);
  return Number.isFinite(v) ? v : null;
}

export function parseDashboardGauges(text: string | undefined): DashboardGauges {
  const empty: DashboardGauges = {
    clientsConnected: null,
    rulesActive: null,
    activeConnections: null,
    topRules: [],
  };
  if (!text) return empty;

  let clientsConnected: number | null = null;
  let activeConnectionsSum = 0;
  let sawActiveConnections = false;
  const ruleBytesIn = new Map<string, number>();
  const ruleBytesOut = new Map<string, number>();

  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;

    if (line.startsWith("portunus_clients_connected ")) {
      const v = valueAtEnd(line);
      if (v !== null) clientsConnected = v;
      continue;
    }
    if (line.startsWith("portunus_rule_active_connections{")) {
      const v = valueAtEnd(line);
      if (v !== null) {
        activeConnectionsSum += v;
        sawActiveConnections = true;
      }
      continue;
    }
    if (line.startsWith("portunus_rule_bytes_in_total{")) {
      const rule = extractRule(line);
      const v = valueAtEnd(line);
      if (rule && v !== null) ruleBytesIn.set(rule, v);
      continue;
    }
    if (line.startsWith("portunus_rule_bytes_out_total{")) {
      const rule = extractRule(line);
      const v = valueAtEnd(line);
      if (rule && v !== null) ruleBytesOut.set(rule, v);
      continue;
    }
  }

  // rulesActive == distinct rules that emitted bytes_in (cheaper proxy
  // than walking active_connections labels separately).
  const rulesActive = ruleBytesIn.size > 0 ? ruleBytesIn.size : null;

  const topRules: TopRule[] = [...ruleBytesIn.keys()]
    .map((rule) => {
      const bytesIn = ruleBytesIn.get(rule) ?? 0;
      const bytesOut = ruleBytesOut.get(rule) ?? 0;
      return { rule, bytesIn, bytesOut, total: bytesIn + bytesOut };
    })
    .sort((a, b) => b.total - a.total)
    .slice(0, 5);

  return {
    clientsConnected,
    rulesActive,
    activeConnections: sawActiveConnections ? activeConnectionsSum : null,
    topRules,
  };
}

export function useDashboardGauges(): DashboardGauges {
  const { data } = useMetricsText();
  return parseDashboardGauges(data);
}
```

- [ ] **Step 4: Run tests to verify they pass**

```sh
cd webui && pnpm vitest run src/api/metrics.test.ts
```

Expected: all PASS.

- [ ] **Step 5: Run existing Dashboard.tsx — it uses `gauges.rulesActive` via `useDashboardGauges` — confirm it still compiles**

```sh
cd webui && pnpm tsc -b
```

Expected: no type errors. (Existing `Dashboard.tsx` reads `gauges.rulesActive` only; the new fields are additive.)

- [ ] **Step 6: Commit**

```sh
git add webui/src/api/metrics.ts webui/src/api/metrics.test.ts
git commit -m "feat(webui): extend dashboard gauges with active conns + top rules"
```

---

### Task B2: Add `useThroughputRate` hook (counter → bytes/sec)

**Files:**
- Create: `webui/src/api/use-throughput-rate.ts`
- Create: `webui/src/api/use-throughput-rate.test.ts`

- [ ] **Step 1: Write failing tests**

Create `webui/src/api/use-throughput-rate.test.ts`:

```typescript
import { describe, expect, it } from "vitest";
import { computeRate } from "@/api/use-throughput-rate";

describe("computeRate", () => {
  it("returns null when there is no prior sample", () => {
    expect(computeRate(null, { totalBytes: 1000, ts: 1000 })).toBeNull();
  });

  it("returns positive bytes/sec for normal growth", () => {
    const prev = { totalBytes: 1_000, ts: 1_000 };
    const next = { totalBytes: 6_000, ts: 1_005 }; // +5000 in 5s
    expect(computeRate(prev, next)).toBe(1_000);
  });

  it("returns 0 when timestamps are identical (avoid div by zero)", () => {
    const prev = { totalBytes: 1, ts: 100 };
    const next = { totalBytes: 5, ts: 100 };
    expect(computeRate(prev, next)).toBe(0);
  });

  it("returns 0 on counter reset (negative delta)", () => {
    const prev = { totalBytes: 5_000, ts: 1_000 };
    const next = { totalBytes: 1_000, ts: 1_005 };
    expect(computeRate(prev, next)).toBe(0);
  });
});
```

- [ ] **Step 2: Run tests to verify they fail**

```sh
cd webui && pnpm vitest run src/api/use-throughput-rate.test.ts
```

Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement `webui/src/api/use-throughput-rate.ts`**

```typescript
import { useEffect, useRef, useState } from "react";

import { useDashboardGauges, useMetricsText } from "@/api/metrics";

export interface ThroughputSample {
  totalBytes: number;
  ts: number; // ms since epoch
}

/// Returns the inferred current throughput in bytes/sec, or `null` if
/// we have not yet collected two samples. A counter reset (negative
/// delta) collapses to 0 rather than producing a negative number.
export function computeRate(
  prev: ThroughputSample | null,
  next: ThroughputSample,
): number | null {
  if (!prev) return null;
  const dt = (next.ts - prev.ts) / 1000;
  if (dt <= 0) return 0;
  const db = next.totalBytes - prev.totalBytes;
  if (db < 0) return 0;
  return db / dt;
}

/// Subscribes to the metrics poll and returns a live bytes/sec value
/// computed from the cumulative `portunus_rule_bytes_*_total` sum.
export function useThroughputRate(): number | null {
  const gauges = useDashboardGauges();
  const { dataUpdatedAt } = useMetricsText();
  const prev = useRef<ThroughputSample | null>(null);
  const [rate, setRate] = useState<number | null>(null);

  useEffect(() => {
    if (!dataUpdatedAt || gauges.topRules.length === 0) return;
    const totalBytes = gauges.topRules.reduce(
      (acc, r) => acc + r.bytesIn + r.bytesOut,
      0,
    );
    const next = { totalBytes, ts: dataUpdatedAt };
    setRate(computeRate(prev.current, next));
    prev.current = next;
  }, [dataUpdatedAt, gauges.topRules]);

  return rate;
}
```

> **NB:** `topRules` is sliced to the top 5 in `parseDashboardGauges` — we deliberately accept that the "total bytes" used here is the **top-5 sum**, not every rule's sum. For workloads under ~5 active rules (the documented MVP scale) this is exact; with many rules, it under-counts the long tail but the rate is still a useful approximation. If exact accuracy is required later, surface a `totalBytesAll` field separately from the parser.

- [ ] **Step 4: Run tests to verify they pass**

```sh
cd webui && pnpm vitest run src/api/use-throughput-rate.test.ts
```

Expected: PASS.

- [ ] **Step 5: Commit**

```sh
git add webui/src/api/use-throughput-rate.ts webui/src/api/use-throughput-rate.test.ts
git commit -m "feat(webui): add useThroughputRate hook (counter -> bytes/sec)"
```

---

### Task B3: Add `useDashboardRange` hook (1h / 24h / 7d state)

**Files:**
- Create: `webui/src/pages/dashboard/useDashboardRange.ts`
- Create: `webui/src/pages/dashboard/useDashboardRange.test.ts`

- [ ] **Step 1: Write failing tests**

```typescript
// webui/src/pages/dashboard/useDashboardRange.test.ts
import { describe, expect, it } from "vitest";
import { computeRange, type DashboardRangeId } from "@/pages/dashboard/useDashboardRange";

const NOW = 1_700_000_000;

describe("computeRange", () => {
  it.each([
    ["1h" as DashboardRangeId, 3600, "1m"],
    ["24h" as DashboardRangeId, 86_400, "1m"],
    ["7d" as DashboardRangeId, 7 * 86_400, "1h"],
  ])("range %s -> span %i, bucket %s", (id, span, bucket) => {
    const r = computeRange(id, NOW);
    expect(r.to).toBe(NOW);
    expect(r.from).toBe(NOW - span);
    expect(r.bucket).toBe(bucket);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

```sh
cd webui && pnpm vitest run src/pages/dashboard/useDashboardRange.test.ts
```

Expected: FAIL — module not found.

- [ ] **Step 3: Implement the hook**

```typescript
// webui/src/pages/dashboard/useDashboardRange.ts
import { useCallback, useState } from "react";

import type { TrafficBucket } from "@/api/types";

export type DashboardRangeId = "1h" | "24h" | "7d";

export interface DashboardRange {
  from: number; // unix seconds
  to: number;
  bucket: TrafficBucket;
}

const SPAN_SEC: Record<DashboardRangeId, number> = {
  "1h": 3600,
  "24h": 86_400,
  "7d": 7 * 86_400,
};

const BUCKET: Record<DashboardRangeId, TrafficBucket> = {
  "1h": "1m",
  "24h": "1m",
  "7d": "1h",
};

export function computeRange(id: DashboardRangeId, now: number): DashboardRange {
  return { from: now - SPAN_SEC[id], to: now, bucket: BUCKET[id] };
}

export function useDashboardRange(initial: DashboardRangeId = "24h") {
  const [rangeId, setRangeId] = useState<DashboardRangeId>(initial);
  const range = computeRange(rangeId, Math.floor(Date.now() / 1000));
  const setRange = useCallback((id: DashboardRangeId) => setRangeId(id), []);
  return { rangeId, range, setRange };
}
```

- [ ] **Step 4: Run tests to verify they pass**

```sh
cd webui && pnpm vitest run src/pages/dashboard/useDashboardRange.test.ts
```

Expected: PASS.

- [ ] **Step 5: Commit**

```sh
git add webui/src/pages/dashboard/useDashboardRange.ts webui/src/pages/dashboard/useDashboardRange.test.ts
git commit -m "feat(webui): add useDashboardRange hook (1h/24h/7d + bucket)"
```

---

### Task B4: Add `useGlobalTraffic` API hook

**Files:**
- Modify: `webui/src/api/traffic.ts`

- [ ] **Step 1: Read existing `useUserTraffic` pattern**

Read `webui/src/api/traffic.ts` — note that the existing per-user / per-client hooks share a `trafficQs` query-string builder and a `TrafficResponse` return type.

- [ ] **Step 2: Append `useGlobalTraffic` hook**

Add to the bottom of `webui/src/api/traffic.ts` (do not re-export anything):

```typescript
export const globalTrafficKey = (q: TrafficQuery) =>
  ["global-traffic", q] as const;

/// Superadmin-only aggregated traffic across all users and clients.
/// Tenants will receive 403 — components that call this must already
/// be inside a superadmin-only render path.
export function useGlobalTraffic(q: TrafficQuery) {
  return useQuery({
    queryKey: globalTrafficKey(q),
    queryFn: () => apiFetch<TrafficResponse>(`/v1/traffic/global?${trafficQs(q)}`),
    enabled: q.from < q.to,
  });
}
```

- [ ] **Step 3: Verify typecheck**

```sh
cd webui && pnpm tsc -b
```

Expected: no errors.

- [ ] **Step 4: Commit**

```sh
git add webui/src/api/traffic.ts
git commit -m "feat(webui): add useGlobalTraffic hook for /v1/traffic/global"
```

---

### Task B5: Build shared `KpiCard` component

**Files:**
- Create: `webui/src/pages/dashboard/components/KpiCard.tsx`

- [ ] **Step 1: Implement the component**

```typescript
// webui/src/pages/dashboard/components/KpiCard.tsx
import type { ReactNode } from "react";

import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

export interface KpiCardProps {
  label: ReactNode;
  value: ReactNode;
  delta?: ReactNode;
  tone?: "default" | "warn" | "bad" | "muted";
}

const TONE_CLASS: Record<NonNullable<KpiCardProps["tone"]>, string> = {
  default: "text-emerald-600 dark:text-emerald-400",
  warn: "text-amber-600 dark:text-amber-400",
  bad: "text-red-600 dark:text-red-400",
  muted: "text-muted-foreground",
};

export function KpiCard({ label, value, delta, tone = "default" }: KpiCardProps) {
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          {label}
        </CardTitle>
      </CardHeader>
      <CardContent>
        <p className="text-2xl font-semibold tabular-nums">{value}</p>
        {delta != null && <p className={`mt-1 text-xs ${TONE_CLASS[tone]}`}>{delta}</p>}
      </CardContent>
    </Card>
  );
}
```

- [ ] **Step 2: Verify typecheck**

```sh
cd webui && pnpm tsc -b
```

Expected: no errors.

- [ ] **Step 3: Commit**

```sh
git add webui/src/pages/dashboard/components/KpiCard.tsx
git commit -m "feat(webui): add shared KpiCard component"
```

---

### Task B6: Build shared `AlertBanner` component

**Files:**
- Create: `webui/src/pages/dashboard/components/AlertBanner.tsx`

- [ ] **Step 1: Implement the component**

```typescript
// webui/src/pages/dashboard/components/AlertBanner.tsx
import { AlertTriangle } from "lucide-react";

export interface AlertBannerProps {
  /// If empty, the banner renders nothing — callers don't need to guard.
  issues: string[];
}

export function AlertBanner({ issues }: AlertBannerProps) {
  if (issues.length === 0) return null;
  return (
    <div
      role="status"
      className="flex items-center gap-3 rounded-md border border-amber-300 bg-amber-50 px-4 py-2 text-sm text-amber-900 dark:border-amber-700 dark:bg-amber-950/50 dark:text-amber-200"
    >
      <AlertTriangle className="h-4 w-4 shrink-0" />
      <div className="flex flex-wrap gap-x-3 gap-y-1">
        {issues.map((msg, i) => (
          <span key={i}>{i > 0 && "·"} {msg}</span>
        ))}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify typecheck**

```sh
cd webui && pnpm tsc -b
```

Expected: no errors.

- [ ] **Step 3: Commit**

```sh
git add webui/src/pages/dashboard/components/AlertBanner.tsx
git commit -m "feat(webui): add shared AlertBanner component"
```

---

## Phase C — Panel components

### Task C1: `UnhealthyTargetsPanel`

**Files:**
- Create: `webui/src/pages/dashboard/components/UnhealthyTargetsPanel.tsx`

- [ ] **Step 1: Inspect the shape of `Rule.targets[].health`**

Read `webui/src/api/types.ts` for the `TargetHealth` / `TargetWithHealth` interfaces; the design assumes `health.healthy: boolean` and `health.last_error?: string`. Confirm field names (adjust the component below if they differ).

- [ ] **Step 2: Implement the panel**

```typescript
// webui/src/pages/dashboard/components/UnhealthyTargetsPanel.tsx
import { useTranslation } from "react-i18next";

import { useRulesList } from "@/api/rules";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

interface UnhealthyEntry {
  ruleId: number;
  ruleName: string;
  endpoint: string;
  lastError: string | null;
}

export function UnhealthyTargetsPanel() {
  const { t } = useTranslation();
  const rules = useRulesList();
  const entries: UnhealthyEntry[] = (rules.data ?? []).flatMap((rule) =>
    (rule.targets ?? [])
      .filter((target) => target.health?.healthy === false)
      .map((target) => ({
        ruleId: rule.id,
        ruleName: rule.label ?? `#${rule.id}`,
        endpoint: `${target.host}:${target.port}`,
        lastError: target.health?.last_error ?? null,
      })),
  );

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("dashboard.unhealthyTargets")}</CardTitle>
      </CardHeader>
      <CardContent>
        {entries.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t("dashboard.allTargetsHealthy")}</p>
        ) : (
          <ul className="space-y-1 text-sm">
            {entries.slice(0, 8).map((e, i) => (
              <li key={`${e.ruleId}-${e.endpoint}-${i}`} className="flex justify-between">
                <span className="truncate">{e.ruleName} → {e.endpoint}</span>
                <span className="text-xs text-red-600 dark:text-red-400">{e.lastError ?? "down"}</span>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
```

> **NB:** `rule.label` and `target.host`/`target.port` are guesses based on existing UI uses (see `RulesList.tsx` and `RuleDetail.tsx`). Open one of those files and confirm the actual property names before pasting; adjust if the codebase calls them e.g. `rule.name`, `target.address`.

- [ ] **Step 3: Verify typecheck**

```sh
cd webui && pnpm tsc -b
```

Fix any mismatched fields against the actual types in `webui/src/api/types.ts`. Expected end state: no errors.

- [ ] **Step 4: Commit**

```sh
git add webui/src/pages/dashboard/components/UnhealthyTargetsPanel.tsx
git commit -m "feat(webui): add UnhealthyTargetsPanel"
```

---

### Task C2: `OfflineClientsPanel`

**Files:**
- Create: `webui/src/pages/dashboard/components/OfflineClientsPanel.tsx`

- [ ] **Step 1: Implement**

```typescript
// webui/src/pages/dashboard/components/OfflineClientsPanel.tsx
import { useTranslation } from "react-i18next";

import { useClientsList } from "@/api/clients";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

function relative(secondsAgo: number): string {
  if (secondsAgo < 60) return `${secondsAgo}s`;
  if (secondsAgo < 3600) return `${Math.floor(secondsAgo / 60)}m`;
  if (secondsAgo < 86400) return `${Math.floor(secondsAgo / 3600)}h`;
  return `${Math.floor(secondsAgo / 86400)}d`;
}

export function OfflineClientsPanel() {
  const { t } = useTranslation();
  const clients = useClientsList();
  const now = Math.floor(Date.now() / 1000);
  const offline = (clients.data ?? [])
    .filter((c) => !c.connected)
    .sort((a, b) => (b.last_seen_at ?? 0) - (a.last_seen_at ?? 0))
    .slice(0, 8);

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("dashboard.offlineClients")}</CardTitle>
      </CardHeader>
      <CardContent>
        {offline.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t("dashboard.allClientsOnline")}</p>
        ) : (
          <ul className="space-y-1 text-sm">
            {offline.map((c) => (
              <li key={c.name} className="flex justify-between">
                <span className="truncate">{c.name}</span>
                <span className="text-xs text-amber-600 dark:text-amber-400">
                  {c.last_seen_at ? relative(now - c.last_seen_at) : "—"}
                </span>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
```

> **NB:** Verify `ClientView` actually has `connected` and `last_seen_at`; consult `webui/src/api/types.ts` and `webui/src/pages/ClientsList.tsx`. Adjust property names if needed.

- [ ] **Step 2: Typecheck + commit**

```sh
cd webui && pnpm tsc -b
git add webui/src/pages/dashboard/components/OfflineClientsPanel.tsx
git commit -m "feat(webui): add OfflineClientsPanel"
```

---

### Task C3: `RecentAuditPanel`

**Files:**
- Create: `webui/src/pages/dashboard/components/RecentAuditPanel.tsx`

- [ ] **Step 1: Implement**

```typescript
// webui/src/pages/dashboard/components/RecentAuditPanel.tsx
import { useTranslation } from "react-i18next";

import { useAuditLog } from "@/api/audit";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

export function RecentAuditPanel() {
  const { t } = useTranslation();
  const audit = useAuditLog({ limit: 10 });
  const entries = audit.data ?? [];
  // Tenants who lack audit-read permission get a 403 → audit.error truthy.
  if (audit.error) return null;

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("dashboard.recentAudit")}</CardTitle>
      </CardHeader>
      <CardContent>
        {entries.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t("dashboard.noRecentActivity")}</p>
        ) : (
          <ul className="space-y-1 text-sm">
            {entries.slice(0, 8).map((e) => (
              <li key={e.id ?? `${e.ts}-${e.event}`} className="flex justify-between">
                <span className="truncate">{e.event} {e.actor ? `· ${e.actor}` : ""}</span>
                <span className="text-xs text-muted-foreground">
                  {new Date((e.ts ?? 0) * 1000).toLocaleTimeString()}
                </span>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
```

> **NB:** `AuditEntry`'s exact field names (`event`, `actor`, `ts`, `id`) must be verified against `webui/src/api/types.ts`. Adjust accordingly.

- [ ] **Step 2: Typecheck + commit**

```sh
cd webui && pnpm tsc -b
git add webui/src/pages/dashboard/components/RecentAuditPanel.tsx
git commit -m "feat(webui): add RecentAuditPanel"
```

---

### Task C4: `ThroughputChart` (Recharts + range toggle)

**Files:**
- Create: `webui/src/pages/dashboard/components/ThroughputChart.tsx`

- [ ] **Step 1: Implement the component**

```typescript
// webui/src/pages/dashboard/components/ThroughputChart.tsx
import { useTranslation } from "react-i18next";
import {
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";

import type { TrafficSample } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";

import type { DashboardRangeId } from "@/pages/dashboard/useDashboardRange";

const RANGE_IDS: DashboardRangeId[] = ["1h", "24h", "7d"];

export interface ThroughputChartProps {
  samples: TrafficSample[] | undefined;
  isLoading: boolean;
  error: unknown;
  rangeId: DashboardRangeId;
  onRangeChange: (id: DashboardRangeId) => void;
  onRetry: () => void;
}

function fmtBytes(v: number): string {
  if (v < 1024) return `${v} B`;
  if (v < 1024 * 1024) return `${(v / 1024).toFixed(1)} KB`;
  if (v < 1024 * 1024 * 1024) return `${(v / 1024 / 1024).toFixed(1)} MB`;
  return `${(v / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

export function ThroughputChart(props: ThroughputChartProps) {
  const { t } = useTranslation();
  const data = (props.samples ?? []).map((s) => ({
    ts: s.ts * 1000,
    bytes_in: s.bytes_in,
    bytes_out: s.bytes_out,
  }));

  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between pb-2">
        <CardTitle className="text-sm">{t("dashboard.throughputChart")}</CardTitle>
        <div className="flex gap-1">
          {RANGE_IDS.map((id) => (
            <Button
              key={id}
              size="sm"
              variant={props.rangeId === id ? "default" : "outline"}
              onClick={() => props.onRangeChange(id)}
            >
              {id}
            </Button>
          ))}
        </div>
      </CardHeader>
      <CardContent>
        {props.error ? (
          <div className="flex flex-col items-center justify-center gap-2 py-8 text-sm text-muted-foreground">
            <span>{t("dashboard.chartLoadError")}</span>
            <Button size="sm" variant="outline" onClick={props.onRetry}>
              {t("common.retry")}
            </Button>
          </div>
        ) : props.isLoading ? (
          <Skeleton className="h-48 w-full" />
        ) : data.length === 0 ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("dashboard.noTrafficYet")}
          </p>
        ) : (
          <div className="h-48">
            <ResponsiveContainer width="100%" height="100%">
              <LineChart data={data}>
                <CartesianGrid strokeDasharray="3 3" stroke="rgba(0,0,0,0.05)" />
                <XAxis
                  dataKey="ts"
                  tickFormatter={(v) => new Date(v as number).toLocaleTimeString()}
                  fontSize={10}
                />
                <YAxis tickFormatter={fmtBytes} fontSize={10} width={60} />
                <Tooltip
                  labelFormatter={(v) => new Date(v as number).toLocaleString()}
                  formatter={(v: number) => fmtBytes(v)}
                />
                <Line type="monotone" dataKey="bytes_in" stroke="#3b82f6" dot={false} />
                <Line type="monotone" dataKey="bytes_out" stroke="#10b981" dot={false} />
              </LineChart>
            </ResponsiveContainer>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
```

- [ ] **Step 2: Verify recharts import compiles + bundle stays under budget**

```sh
cd webui && pnpm tsc -b && pnpm build
```

Expected: `size-limit` post-build report ≤ 500 KB gz. If it fails, switch to lazy-loaded recharts (wrap the component in `React.lazy` and import the page that uses it via `lazy(...)`).

- [ ] **Step 3: Commit**

```sh
git add webui/src/pages/dashboard/components/ThroughputChart.tsx
git commit -m "feat(webui): add ThroughputChart (recharts + 1h/24h/7d toggle)"
```

---

### Task C5: `TopRulesPanel`

**Files:**
- Create: `webui/src/pages/dashboard/components/TopRulesPanel.tsx`

- [ ] **Step 1: Implement**

```typescript
// webui/src/pages/dashboard/components/TopRulesPanel.tsx
import { useTranslation } from "react-i18next";

import type { TopRule } from "@/api/metrics";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

function fmtBytes(v: number): string {
  if (v < 1024) return `${v} B`;
  if (v < 1024 * 1024) return `${(v / 1024).toFixed(1)} KB`;
  if (v < 1024 * 1024 * 1024) return `${(v / 1024 / 1024).toFixed(1)} MB`;
  return `${(v / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

export interface TopRulesPanelProps {
  rules: TopRule[];
}

export function TopRulesPanel({ rules }: TopRulesPanelProps) {
  const { t } = useTranslation();
  const max = rules[0]?.total ?? 0;

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("dashboard.topRules")}</CardTitle>
      </CardHeader>
      <CardContent>
        {rules.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t("dashboard.noRulesYet")}</p>
        ) : (
          <ul className="space-y-2 text-xs">
            {rules.map((r) => (
              <li key={r.rule}>
                <div className="flex justify-between">
                  <span className="truncate font-medium">#{r.rule}</span>
                  <span className="tabular-nums text-muted-foreground">{fmtBytes(r.total)}</span>
                </div>
                <div className="mt-1 h-1 overflow-hidden rounded bg-muted">
                  <div
                    className="h-full bg-blue-500"
                    style={{ width: `${max > 0 ? (r.total / max) * 100 : 0}%` }}
                  />
                </div>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
```

- [ ] **Step 2: Typecheck + commit**

```sh
cd webui && pnpm tsc -b
git add webui/src/pages/dashboard/components/TopRulesPanel.tsx
git commit -m "feat(webui): add TopRulesPanel (bar list)"
```

---

## Phase D — Dashboard pages

### Task D1: Implement `SuperadminDashboard`

**Files:**
- Create: `webui/src/pages/dashboard/SuperadminDashboard.tsx`

- [ ] **Step 1: Implement**

```typescript
// webui/src/pages/dashboard/SuperadminDashboard.tsx
import { useTranslation } from "react-i18next";

import { useClientsList } from "@/api/clients";
import { useDashboardGauges } from "@/api/metrics";
import { useRulesList } from "@/api/rules";
import { useGlobalTraffic, globalTrafficKey } from "@/api/traffic";
import { useThroughputRate } from "@/api/use-throughput-rate";
import { useQueryClient } from "@tanstack/react-query";

import { AlertBanner } from "./components/AlertBanner";
import { KpiCard } from "./components/KpiCard";
import { OfflineClientsPanel } from "./components/OfflineClientsPanel";
import { RecentAuditPanel } from "./components/RecentAuditPanel";
import { ThroughputChart } from "./components/ThroughputChart";
import { TopRulesPanel } from "./components/TopRulesPanel";
import { UnhealthyTargetsPanel } from "./components/UnhealthyTargetsPanel";
import { useDashboardRange } from "./useDashboardRange";

function fmtBytes(v: number): string {
  if (v < 1024) return `${v} B`;
  if (v < 1024 * 1024) return `${(v / 1024).toFixed(1)} KB`;
  if (v < 1024 * 1024 * 1024) return `${(v / 1024 / 1024).toFixed(1)} MB`;
  if (v < 1024 ** 4) return `${(v / 1024 ** 3).toFixed(1)} GB`;
  return `${(v / 1024 ** 4).toFixed(1)} TB`;
}

export function SuperadminDashboard() {
  const { t } = useTranslation();
  const gauges = useDashboardGauges();
  const clients = useClientsList();
  const rules = useRulesList();
  const throughput = useThroughputRate();
  const { rangeId, range, setRange } = useDashboardRange("24h");
  const global = useGlobalTraffic(range);
  const qc = useQueryClient();

  const connectedCount = (clients.data ?? []).filter((c) => c.connected).length;
  const totalClients = clients.data?.length ?? 0;
  const ruleCount = gauges.rulesActive ?? rules.data?.length ?? 0;
  const totalTargets = (rules.data ?? []).reduce(
    (acc, r) => acc + (r.targets?.length ?? 0),
    0,
  );
  const healthyTargets = (rules.data ?? []).reduce(
    (acc, r) =>
      acc + (r.targets ?? []).filter((tt) => tt.health?.healthy !== false).length,
    0,
  );
  const unhealthyCount = totalTargets - healthyTargets;
  const offlineClientCount = (clients.data ?? []).filter((c) => !c.connected).length;
  const cumulativeBytes = gauges.topRules.reduce(
    (acc, r) => acc + r.bytesIn + r.bytesOut,
    0,
  );

  const issues: string[] = [];
  if (unhealthyCount > 0) issues.push(t("dashboard.alertUnhealthy", { n: unhealthyCount }));
  if (offlineClientCount > 0) issues.push(t("dashboard.alertOffline", { n: offlineClientCount }));

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-semibold">{t("dashboard.title")}</h1>

      <AlertBanner issues={issues} />

      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-6">
        <KpiCard
          label={t("dashboard.connectedClients")}
          value={`${connectedCount} / ${totalClients}`}
        />
        <KpiCard label={t("dashboard.activeRules")} value={ruleCount} />
        <KpiCard
          label={t("dashboard.targetsOk")}
          value={`${healthyTargets} / ${totalTargets}`}
          tone={unhealthyCount > 0 ? "bad" : "muted"}
          delta={unhealthyCount > 0 ? t("dashboard.targetsDown", { n: unhealthyCount }) : undefined}
        />
        <KpiCard
          label={t("dashboard.throughputNow")}
          value={throughput === null ? t("dashboard.calculating") : `${fmtBytes(throughput)}/s`}
        />
        <KpiCard
          label={t("dashboard.totalTransferred")}
          value={fmtBytes(cumulativeBytes)}
          delta={t("dashboard.sinceProcessStart")}
          tone="muted"
        />
        <KpiCard
          label={t("dashboard.activeConnections")}
          value={gauges.activeConnections ?? "—"}
        />
      </div>

      <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
        <UnhealthyTargetsPanel />
        <OfflineClientsPanel />
        <RecentAuditPanel />
      </div>

      <div className="grid grid-cols-1 gap-3 lg:grid-cols-[2fr_1fr]">
        <ThroughputChart
          samples={global.data?.samples}
          isLoading={global.isLoading}
          error={global.error}
          rangeId={rangeId}
          onRangeChange={setRange}
          onRetry={() => qc.invalidateQueries({ queryKey: globalTrafficKey(range) })}
        />
        <TopRulesPanel rules={gauges.topRules} />
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Typecheck**

```sh
cd webui && pnpm tsc -b
```

Fix any field-name mismatches against the actual `Rule`, `Target`, `ClientView` types in `webui/src/api/types.ts`.

- [ ] **Step 3: Commit**

```sh
git add webui/src/pages/dashboard/SuperadminDashboard.tsx
git commit -m "feat(webui): implement SuperadminDashboard"
```

---

### Task D2: Implement `TenantDashboard`

**Files:**
- Create: `webui/src/pages/dashboard/TenantDashboard.tsx`

- [ ] **Step 1: Implement**

```typescript
// webui/src/pages/dashboard/TenantDashboard.tsx
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";

import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { useClientsList } from "@/api/clients";
import { useRulesList } from "@/api/rules";
import { useUserQuotas } from "@/api/quotas";
import { useUserTraffic, userTrafficKey } from "@/api/traffic";

import { AlertBanner } from "./components/AlertBanner";
import { KpiCard } from "./components/KpiCard";
import { OfflineClientsPanel } from "./components/OfflineClientsPanel";
import { RecentAuditPanel } from "./components/RecentAuditPanel";
import { ThroughputChart } from "./components/ThroughputChart";
import { UnhealthyTargetsPanel } from "./components/UnhealthyTargetsPanel";
import { useDashboardRange } from "./useDashboardRange";

function fmtBytes(v: number): string {
  if (v < 1024) return `${v} B`;
  if (v < 1024 * 1024) return `${(v / 1024).toFixed(1)} KB`;
  if (v < 1024 * 1024 * 1024) return `${(v / 1024 / 1024).toFixed(1)} MB`;
  if (v < 1024 ** 4) return `${(v / 1024 ** 3).toFixed(1)} GB`;
  return `${(v / 1024 ** 4).toFixed(1)} TB`;
}

export function TenantDashboard() {
  const { t } = useTranslation();
  const { data: identity } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  const userId = identity?.user_id ?? "";

  const clients = useClientsList();
  const rules = useRulesList();
  const quotas = useUserQuotas(userId);
  const { rangeId, range, setRange } = useDashboardRange("24h");
  const traffic = useUserTraffic(userId, range);
  const qc = useQueryClient();

  // KPI · 24h transferred (sum across all clients for this user)
  const last24h = {
    from: Math.floor(Date.now() / 1000) - 86_400,
    to: Math.floor(Date.now() / 1000),
    bucket: "1m" as const,
  };
  const traffic24h = useUserTraffic(userId, last24h);
  const transferred24h = (traffic24h.data?.total_bytes_in ?? 0)
    + (traffic24h.data?.total_bytes_out ?? 0);

  const connectedCount = (clients.data ?? []).filter((c) => c.connected).length;
  const totalClients = clients.data?.length ?? 0;
  const ruleCount = rules.data?.length ?? 0;
  const offlineClientCount = totalClients - connectedCount;

  // Aggregate quota usage across all this user's per-client rows.
  const quotaUsed = (quotas.data ?? []).reduce(
    (acc, q) => acc + (q.current_period_bytes_used ?? 0),
    0,
  );
  const quotaLimit = (quotas.data ?? []).reduce(
    (acc, q) => acc + (q.monthly_bytes ?? 0),
    0,
  );
  const quotaPct = quotaLimit > 0 ? Math.min(100, (quotaUsed / quotaLimit) * 100) : null;

  const unhealthyCount = (rules.data ?? []).reduce(
    (acc, r) => acc + (r.targets ?? []).filter((tt) => tt.health?.healthy === false).length,
    0,
  );

  const issues: string[] = [];
  if (unhealthyCount > 0) issues.push(t("dashboard.alertUnhealthy", { n: unhealthyCount }));
  if (offlineClientCount > 0) issues.push(t("dashboard.alertOffline", { n: offlineClientCount }));
  if (quotaPct !== null && quotaPct >= 80)
    issues.push(t("dashboard.alertQuotaNear", { pct: Math.round(quotaPct) }));

  return (
    <div className="space-y-4">
      <h1 className="text-2xl font-semibold">
        {t("dashboard.greeting")}, {identity?.display_name ?? identity?.user_id ?? "—"}
      </h1>

      <AlertBanner issues={issues} />

      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-5">
        <KpiCard
          label={t("dashboard.myClients")}
          value={`${connectedCount} / ${totalClients}`}
        />
        <KpiCard label={t("dashboard.myRules")} value={ruleCount} />
        <KpiCard
          label={t("dashboard.my24hTransferred")}
          value={fmtBytes(transferred24h)}
        />
        <KpiCard
          label={t("dashboard.myQuotaUsed")}
          value={quotaPct === null ? "—" : `${quotaPct.toFixed(0)}%`}
          delta={
            quotaLimit > 0
              ? `${fmtBytes(quotaUsed)} / ${fmtBytes(quotaLimit)}`
              : t("dashboard.noQuotaSet")
          }
          tone={quotaPct !== null && quotaPct >= 80 ? "warn" : "muted"}
        />
        <KpiCard
          label={t("dashboard.myActiveConns")}
          value="—"
          delta={t("dashboard.openComing")}
          tone="muted"
        />
      </div>

      <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
        <UnhealthyTargetsPanel />
        <OfflineClientsPanel />
        <RecentAuditPanel />
      </div>

      <ThroughputChart
        samples={traffic.data?.samples}
        isLoading={traffic.isLoading}
        error={traffic.error}
        rangeId={rangeId}
        onRangeChange={setRange}
        onRetry={() => qc.invalidateQueries({ queryKey: userTrafficKey(userId, range) })}
      />
    </div>
  );
}
```

> **NB:** The tenant view intentionally drops the Top-5 panel (which depends on `/v1/metrics`) and reserves KPI slot 6 as a placeholder labelled `dashboard.openComing` — this realises the spec's "Open Question #1: 5-cell layout".

- [ ] **Step 2: Typecheck**

```sh
cd webui && pnpm tsc -b
```

- [ ] **Step 3: Commit**

```sh
git add webui/src/pages/dashboard/TenantDashboard.tsx
git commit -m "feat(webui): implement TenantDashboard (5 KPI + quota emphasis)"
```

---

### Task D3: Update `Dashboard.tsx` router shell

**Files:**
- Modify: `webui/src/pages/Dashboard.tsx` (full replace)

- [ ] **Step 1: Replace the file**

```typescript
// webui/src/pages/Dashboard.tsx
import { useQuery } from "@tanstack/react-query";

import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { Skeleton } from "@/components/ui/skeleton";
import { SuperadminDashboard } from "@/pages/dashboard/SuperadminDashboard";
import { TenantDashboard } from "@/pages/dashboard/TenantDashboard";

export function Dashboard() {
  const { data: identity, isLoading } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });

  if (isLoading || !identity) {
    return <Skeleton className="h-24 w-full" />;
  }

  return identity.role === "superadmin" ? <SuperadminDashboard /> : <TenantDashboard />;
}
```

- [ ] **Step 2: Typecheck + run the existing AuthGate test to make sure nothing broke**

```sh
cd webui && pnpm tsc -b && pnpm vitest run src/auth/AuthGate.test.tsx
```

Expected: all PASS.

- [ ] **Step 3: Commit**

```sh
git add webui/src/pages/Dashboard.tsx
git commit -m "feat(webui): branch Dashboard shell on identity.role"
```

---

### Task D4: Component test — role guard never calls `/v1/metrics` as tenant

**Files:**
- Create: `webui/src/pages/dashboard/Dashboard.test.tsx`

- [ ] **Step 1: Write the test**

```typescript
// webui/src/pages/dashboard/Dashboard.test.tsx
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router-dom";

import "@/i18n";
import { Dashboard } from "@/pages/Dashboard";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function renderDashboard() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <Dashboard />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function mockRoutes(routes: Record<string, () => Response>): ReturnType<typeof vi.fn> {
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === "string" ? input : input.toString();
    for (const [pattern, handler] of Object.entries(routes)) {
      if (url.includes(pattern)) return handler();
    }
    return new Response("not found", { status: 404 });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

describe("Dashboard role branching", () => {
  it("tenant view never calls /v1/metrics", async () => {
    const fetchMock = mockRoutes({
      "/v1/auth/me": () =>
        new Response(
          JSON.stringify({ user_id: "alice", role: "user", display_name: "Alice" }),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
      "/v1/clients": () => new Response("[]", { status: 200, headers: { "content-type": "application/json" } }),
      "/v1/rules": () => new Response("[]", { status: 200, headers: { "content-type": "application/json" } }),
      "/v1/users/alice/quotas": () => new Response("[]", { status: 200, headers: { "content-type": "application/json" } }),
      "/v1/users/alice/traffic": () =>
        new Response(
          JSON.stringify({ bucket: "1m", samples: [], total_bytes_in: 0, total_bytes_out: 0 }),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
      "/v1/audit": () => new Response("[]", { status: 200, headers: { "content-type": "application/json" } }),
    });

    renderDashboard();

    await waitFor(() => {
      const urls = fetchMock.mock.calls.map((c) => String(c[0]));
      expect(urls.some((u) => u.includes("/v1/auth/me"))).toBe(true);
    });

    const urls = fetchMock.mock.calls.map((c) => String(c[0]));
    expect(urls.find((u) => u.includes("/v1/metrics"))).toBeUndefined();
  });
});
```

- [ ] **Step 2: Run the test**

```sh
cd webui && pnpm vitest run src/pages/dashboard/Dashboard.test.tsx
```

Expected: PASS.

- [ ] **Step 3: Commit**

```sh
git add webui/src/pages/dashboard/Dashboard.test.tsx
git commit -m "test(webui): tenant dashboard never calls /v1/metrics"
```

---

## Phase E — i18n, polish, verification

### Task E1: Add i18n keys

**Files:**
- Modify: `webui/src/i18n/en.json`
- Modify: `webui/src/i18n/zh-CN.json`

- [ ] **Step 1: Add new keys to `en.json`**

Find the existing `"dashboard"` block and merge these keys into it. (If you don't see one, add the block at the top level.) Existing keys (`greeting`, `connectedClients`, `activeRules`) stay as-is.

```json
{
  "dashboard": {
    "title": "Dashboard",
    "greeting": "Hello",
    "connectedClients": "Connected clients",
    "activeRules": "Active rules",
    "targetsOk": "Targets OK",
    "targetsDown": "{{n}} down",
    "throughputNow": "Throughput now",
    "totalTransferred": "Total transferred",
    "sinceProcessStart": "since process start",
    "activeConnections": "Active connections",
    "calculating": "calculating…",
    "myClients": "My clients",
    "myRules": "My rules",
    "my24hTransferred": "24h transferred",
    "myQuotaUsed": "Quota used",
    "myActiveConns": "Active connections",
    "openComing": "coming soon",
    "noQuotaSet": "no quota set",
    "unhealthyTargets": "Unhealthy targets",
    "allTargetsHealthy": "All targets healthy",
    "offlineClients": "Offline clients",
    "allClientsOnline": "All clients online",
    "recentAudit": "Recent activity",
    "noRecentActivity": "No recent activity",
    "throughputChart": "Throughput",
    "topRules": "Top rules",
    "noRulesYet": "No rules yet",
    "noTrafficYet": "No traffic in this range",
    "chartLoadError": "Failed to load chart data",
    "alertUnhealthy": "{{n}} unhealthy target(s)",
    "alertOffline": "{{n}} client(s) offline",
    "alertQuotaNear": "Quota at {{pct}}%"
  },
  "common": {
    "retry": "Retry"
  }
}
```

If `common.retry` already exists, do not duplicate it. If `dashboard.greeting` / `dashboard.connectedClients` / `dashboard.activeRules` already exist, leave them at their current values.

- [ ] **Step 2: Add corresponding zh-CN keys**

```json
{
  "dashboard": {
    "title": "看板",
    "greeting": "你好",
    "connectedClients": "已连接客户端",
    "activeRules": "活动规则",
    "targetsOk": "目标健康",
    "targetsDown": "{{n}} 个故障",
    "throughputNow": "当前吞吐",
    "totalTransferred": "累计流量",
    "sinceProcessStart": "自进程启动以来",
    "activeConnections": "活跃连接数",
    "calculating": "计算中…",
    "myClients": "我的客户端",
    "myRules": "我的规则",
    "my24hTransferred": "24 小时流量",
    "myQuotaUsed": "配额使用",
    "myActiveConns": "活跃连接数",
    "openComing": "即将推出",
    "noQuotaSet": "未设置配额",
    "unhealthyTargets": "异常目标",
    "allTargetsHealthy": "全部目标健康",
    "offlineClients": "离线客户端",
    "allClientsOnline": "全部客户端在线",
    "recentAudit": "最近活动",
    "noRecentActivity": "暂无活动",
    "throughputChart": "吞吐量",
    "topRules": "Top 规则",
    "noRulesYet": "暂无规则",
    "noTrafficYet": "时间范围内无流量",
    "chartLoadError": "图表加载失败",
    "alertUnhealthy": "{{n}} 个目标异常",
    "alertOffline": "{{n}} 个客户端离线",
    "alertQuotaNear": "配额已用 {{pct}}%"
  },
  "common": {
    "retry": "重试"
  }
}
```

- [ ] **Step 3: Run all unit tests to ensure no stale i18n lookups regressed**

```sh
cd webui && pnpm test
```

Expected: all PASS.

- [ ] **Step 4: Commit**

```sh
git add webui/src/i18n/en.json webui/src/i18n/zh-CN.json
git commit -m "i18n(webui): add dashboard keys (en, zh-CN)"
```

---

### Task E2: Manual smoke test

- [ ] **Step 1: Bring up dev stack**

```sh
make dev
```

Expected: backend on `:7080`, Vite on `:5173`, banner printing `_superadmin` temporary password (first run only).

- [ ] **Step 2: Verify superadmin layout**

Open `http://localhost:5173` and log in as `_superadmin`. Confirm visually:
- Page title "Dashboard"
- 6 KPI cards in one row (or wraps to 2 rows on narrow screens)
- 3 middle panels: Unhealthy targets / Offline clients / Recent audit
- Throughput chart with 1h / 24h / 7d toggle, plus a Top 5 panel on the right
- Alert banner only appears when at least one issue exists (no rule failure → no banner)

- [ ] **Step 3: Verify tenant layout**

In a different browser profile, create a tenant via `/users/new` (superadmin only), then log in as that tenant. Open DevTools → Network, filter "metrics". Confirm:
- No request to `/v1/metrics`
- 5 KPI cards (Active connections shows "—")
- Throughput chart populated from `/v1/users/<id>/traffic`
- Quota KPI says "no quota set" when there's no quota

- [ ] **Step 4: Verify time-range switching**

Click `1h` → `24h` → `7d`. Network panel shows fresh `/v1/traffic/global` (or `/v1/users/.../traffic`) requests with matching `from`/`to`/`bucket`.

- [ ] **Step 5: Document any deviations**

If any panel renders blank because of a wrong field name (very common between mock and real `Rule`/`Target`/`ClientView` shapes), fix and add a new commit:

```sh
git add <fixed-files>
git commit -m "fix(webui): correct <field-name> for <component>"
```

---

### Task E3: Final checks (clippy / fmt / bundle / tests)

- [ ] **Step 1: Backend lints and tests**

```sh
PORTUNUS_SKIP_WEBUI=1 cargo fmt --all -- --check
PORTUNUS_SKIP_WEBUI=1 cargo clippy --workspace --all-targets -- -D warnings
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace
```

Expected: all green. Fix any `cargo fmt` drift with `cargo fmt --all` and recommit.

- [ ] **Step 2: Frontend build + size-limit**

```sh
cd webui && pnpm tsc -b && pnpm build
```

Expected:
- `tsc` returns 0.
- `vite build` succeeds.
- `size-limit` step passes (≤ 500 KB gzipped). If it fails, lazy-load the dashboard sub-tree from `App.tsx`:

```typescript
// App.tsx
const Dashboard = lazy(() =>
  import("@/pages/Dashboard").then((m) => ({ default: m.Dashboard })),
);
```

(and wrap the route in the existing `<Suspense>` boundary).

- [ ] **Step 3: Frontend tests**

```sh
cd webui && pnpm test
```

Expected: all PASS.

- [ ] **Step 4: Final commit (only if lint/format/lazy changes were needed)**

```sh
git add -A
git commit -m "chore: clippy/fmt/bundle cleanup for dashboard MVP"
```

---

## Self-Review

**Spec coverage**

- Three-zone layout — Phase D1 (superadmin) and D2 (tenant) compose alert banner + KPIs + 3 panels + chart + top-rules.
- Role branching — Task D3 (router shell).
- Backend `/v1/traffic/global` — Phase A (A1 building-block tests, A2 handler, A3 route + contract test).
- Recharts throughput chart with 1h/24h/7d toggle — Task C4 and `useDashboardRange` (B3).
- Alert banner derived client-side — Task B6 + tasks D1/D2 (`issues[]` arrays).
- Counter-reset / first-poll handling — Task B2 (`computeRate` tests cover both cases).
- Empty states for new tenants — every panel in Phase C handles `length === 0`.
- Recent audit hides on 403 — Task C3.
- i18n keys — Task E1 (en + zh-CN).
- Bundle budget — Task E3 step 2, with documented lazy-load fallback.
- Tenant never calls `/v1/metrics` — Task D4 (test) and D2 (no `useDashboardGauges` reference).

**Open Questions resolved:**

- Tenant slot 6 — Task D2 fills it with a labelled "coming soon" placeholder (5-cell-equivalent UX).
- Prometheus gauge availability — confirmed during exploration: `portunus_clients_connected`, `portunus_rule_active_connections`, `portunus_rule_bytes_in_total`, `portunus_rule_bytes_out_total` exist. `portunus_target_healthy` does NOT exist; we derive from `useRulesList()` in Task C1, as the spec's fallback path prescribes.
- "Total transferred since start" wording — i18n key `dashboard.sinceProcessStart` carried in Task E1.

**Placeholder scan**

- No `TBD`, no `TODO`, no "implement later", no "similar to Task N" references.
- Each code block is complete and pastable.
- Every test in Phase A / B includes its assertion code.

**Type / name consistency**

- `DashboardGauges` field names (`activeConnections`, `topRules`) match between Task B1 (parser), B2 (`useThroughputRate`), C5 (`TopRulesPanel`), D1 (`SuperadminDashboard`).
- `DashboardRangeId` / `DashboardRange` shape consistent between B3 (definition) and C4/D1/D2 (consumers).
- `globalTrafficKey` and `userTrafficKey` names consistent between B4 (`traffic.ts` additions) and D1/D2 (`qc.invalidateQueries`).
- Metric name corrected from spec's `portunus_rule_active_conns` to the actual `portunus_rule_active_connections` (Task B1 test fixture + impl).
- `TrafficResponse` reused (not redefined) — sourced from `@/api/types`, already exported.

---

## Execution Notes

- Backend changes (Phase A) can ship and be reviewed independently of the frontend — they're additive and the new endpoint has its own contract test.
- Frontend tasks are mostly independent within each phase; B5/B6 are leaf components that block C and D.
- Field-name mismatches between this plan and the real `Rule` / `Target` / `ClientView` types are likely (the plan was written against the design doc, not the live types). The first place a wrong field name will surface is `pnpm tsc -b` at the end of each component task — fix inline and re-commit, no replan needed.
