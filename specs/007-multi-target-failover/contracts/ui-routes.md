# Contract: Web UI surface deltas

**Phase**: 1 (design) | **Feature**: 007-multi-target-failover | **Date**: 2026-05-08

This contract defines the rendering and form deltas inside the 006 React+Vite SPA (`webui/`). No new routes are added; the existing `/rules`, `/rules/new`, and `/rules/:id` pages each gain a small extension. Everything single-target continues to work and look identical to v0.6.0.

---

## 1. Route map (unchanged)

| Route | Page | Delta in v0.7 |
|---|---|---|
| `/rules` | `pages/RulesList.tsx` | none — list rendering unchanged. Optional: badge "multi-target" on rows where `targets.len() > 1`. |
| `/rules/new` | `pages/RulePush.tsx` | targets list builder + active-probe field |
| `/rules/:id` | `pages/RuleDetail.tsx` | targets section with per-target health badges + per-target byte counters |

No new top-level navigation entry. No new permission scope.

## 2. `RulePush.tsx` — form delta

### Single-target default (unchanged baseline)

The form opens with one target row pre-filled (host + port). Submitting that as-is produces a v0.7 POST whose `targets[]` array has length 1 — server-side back-compat folds that into the legacy on-the-wire shape (FR-020 byte-identity at the data plane).

### Multi-target controls

- **"Add another target" button** — appends a new target row. The new row's priority defaults to its row index (0, 1, 2, …).
- **Per-row controls**: host (text input), port (number input), priority (number input, optional — placeholder shows the row index), remove (X button — disabled on the only row).
- **"Active health check (optional)" collapsible section**: a single `health_check_interval_secs` number input. Empty / 0 means "no active probe — passive only". 1..=3600 enables the probe.
- **Validation, client-side, before submit**:
  - At least one target row.
  - At most 8 target rows.
  - Each row has a non-empty host and a port in `1..=65535`.
  - No two rows share `(host, port)`.
  - `health_check_interval_secs` empty OR in `1..=3600`.

Server-side validation (see `operator-api.md` §1) is the source of truth — the client mirror is for fast feedback only. Server errors surface as the existing toast + inline error UI from spec 006.

### Submitted payload

The form ALWAYS submits the new shape (`targets[]`), even for the single-target default. The server folds a length-1 `targets[]` to the legacy on-wire encoding for the rule push (data-plane byte-identity). This keeps the form code single-pathed.

## 3. `RuleDetail.tsx` — page delta

### Existing sections (unchanged)

- Rule header (id, listen port range, protocol, owner)
- Live stats panel — bytes in/out, active connections, DNS failures (5 s SSE refresh from `GET /v1/rules/{id}/stats/stream`)
- Audit timeline (from spec 005 / 006)

### New: Targets section

Renders below the live stats panel, before the audit timeline. Shape:

```text
Targets (2)                                      [target_failovers_total: 3]
┌──────────────────────────────────────────────────────────────────────────────┐
│ #  Host:Port                  Priority  Health     Last Failure   Last OK   │
├──────────────────────────────────────────────────────────────────────────────┤
│ 0  primary.example.com:80         0     ● Healthy   10:30:55      10:42:11   │
│    bytes_in 8.0M    bytes_out 6.0M    conns 12                              │
├──────────────────────────────────────────────────────────────────────────────┤
│ 1  secondary.example.com:80       1     ● Healthy   —             10:30:42   │
│    bytes_in 4.3M    bytes_out 3.9M    conns 5                               │
└──────────────────────────────────────────────────────────────────────────────┘
```

#### Health badge mapping

| State | Badge color | Text |
|---|---|---|
| `healthy` (consecutive_failures == 0) | green | "● Healthy" |
| `healthy` (consecutive_failures > 0)  | yellow | "● Degraded" — visual cue that failures are accumulating but the threshold hasn't been hit |
| `failed` | red | "● Failed" |

The "Degraded" state is purely a UI cue; on the wire the target is still `healthy = 0` until the threshold (3 failures within 30 s).

#### Live updates

The page subscribes to `GET /v1/rules/{id}/stats/stream?per_target=true` (existing 5 s SSE channel from spec 006, gains `per_target` query support per `operator-api.md` §5). Per-tick the targets section re-renders with the latest `per_target[]` snapshot — no separate fetch.

#### Single-target rules

For a rule with `targets.len() == 1`, the targets section renders as:

```text
Target                                            [target_failovers_total: 0]
  example.com:80   (single-target rule — no failover state)
```

No health badge, no per-target byte counters (FR-002 — no per-target state allocated). The `target_failovers_total` chip stays for visual consistency but always reads 0.

## 4. `RulesList.tsx` — optional row decoration

A small "MT" pill on rows where `targets.len() > 1`. Optional — does not block the release if dropped. No interactive surface; no filter on `MT` rows in v0.7.

## 5. API client (`webui/src/api/`)

### Updated types (`api/types.ts`)

```ts
export type TargetHealthState = "healthy" | "failed";

export interface Target {
  host: string;
  port: number;
  priority: number;
}

export interface TargetHealth {
  state: TargetHealthState;
  consecutive_failures: number;
  last_failure_at: string | null;   // ISO 8601
  last_success_at: string | null;
}

export interface TargetWithHealth extends Target {
  health: TargetHealth | null;       // null for single-target rules
}

export interface PerTargetStats extends Target {
  index: number;
  health: TargetHealthState;
  consecutive_failures: number;
  last_failure_at: string | null;
  last_success_at: string | null;
  bytes_in: number;
  bytes_out: number;
  connections_accepted: number;
}

export interface RuleWithTargets {
  rule_id: number;
  client: string;
  listen_port: number;
  listen_port_end: number;
  protocol: "tcp" | "udp";
  prefer_ipv6?: boolean;
  targets: TargetWithHealth[];
  health_check_interval_secs?: number;
}

export interface RuleStats {
  rule_id: number;
  bytes_in: number;
  bytes_out: number;
  active_connections: number;
  dns_failures: number;
  datagrams_in: number;
  datagrams_out: number;
  active_flows: number;
  flows_dropped_overflow: number;
  target_failovers_total: number;     // NEW
  per_target?: PerTargetStats[];      // NEW (optional, only when ?per_target=true)
}
```

### New / updated hooks

- `usePushRule` (existing) — body type changes to accept the `targets[]`+`health_check_interval_secs` shape (drop legacy `target_host`/`target_port` from the form payload entirely; the server folds back to legacy on the wire if length 1).
- `useRule(id)` (existing) — return type widens from `Rule` to `RuleWithTargets`.
- `useRuleStatsStream(id, { perTarget })` (new opt) — subscribes to the SSE channel with optional `?per_target=true`. Returns `RuleStats` snapshots.

## 6. i18n

New string keys (added to `webui/src/i18n/en.json` and `zh-CN.json` per spec 006's translation contract):

```text
rules.targets.heading                  "Targets"
rules.targets.add_button               "Add another target"
rules.targets.remove_button            "Remove target"
rules.targets.priority_placeholder     "auto ({index})"
rules.targets.health.healthy           "Healthy"
rules.targets.health.degraded          "Degraded"
rules.targets.health.failed            "Failed"
rules.targets.failovers_total          "Failovers: {count}"
rules.targets.single_target_note       "Single-target rule — no failover state"
rules.targets.active_probe.heading     "Active health check (optional)"
rules.targets.active_probe.interval    "Probe interval (seconds, 1–3600)"
rules.targets.active_probe.disabled    "Disabled — passive failure detection only"
rules.targets.validation.duplicate     "This target is already in the list"
rules.targets.validation.too_many      "Maximum 8 targets per rule"
```

## 7. e2e coverage (Playwright)

Mirroring the spec-006 e2e pattern, the v0.7 test pass adds:

- `webui/tests/e2e/us1-multi-target-push.spec.ts` — operator pushes a 2-target rule via the form, verifies the response carries `targets[]` of length 2.
- `webui/tests/e2e/us3-target-detail-render.spec.ts` — operator opens a multi-target rule's detail page, verifies the Targets section renders both rows with health badges and live byte counters.
- `webui/tests/e2e/us4-single-target-back-compat.spec.ts` — operator pushes a single-target rule via the form (one target row), verifies the rendered detail page shows the "single-target rule" note rather than per-target counters.

These ride the existing playwright fixture (spawns `forward-server` on port 47080, embedded SPA).

## 8. What the contract does NOT change

- No new theme tokens. Health badge colors reuse the spec-006 success/warning/danger tokens.
- No new font / icon dependencies — health badges are the existing dot+text combo.
- No new responsive breakpoints — the targets table reuses the standard data-table layout from spec 006.
- No deep-link to "this target" — targets are not addressable URLs, only inline rows on the rule.
- No bulk-edit affordance — operators replace a multi-target rule by re-pushing it (R-011 in research.md).
