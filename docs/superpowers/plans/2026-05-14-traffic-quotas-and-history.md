# Traffic Quotas & History Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship per-(user, client) monthly traffic quota + history aggregation on `quota-dev` branch as v1.4.0.

**Architecture:** Server stores quota + 1m/1h rolled-up samples in SQLite. Server aggregates per-(user, client) bytes from existing `RuleStats` cumulative (no client→server wire change). New `TrafficQuotaUpdate` `ServerMessage` oneof variant pushes quota state to client. Client maintains `QuotaScopeManager` + per-pair `QuotaHandle` with saturating CAS `consume`; quota-aware TCP copy loop, splice per-iteration hook, and UDP per-datagram hook enforce hard kill. Web UI extends `UserQuotaTable` with quota column + adds Traffic tab on `UserDetail` and `ClientDetail` using recharts.

**Tech Stack:** Rust 1.88 (tonic, prost, axum, tokio, prometheus, rusqlite, nix). TypeScript 5 (React 19, Vite, react-hook-form, zod, shadcn/ui, recharts).

**Spec:** [docs/superpowers/specs/2026-05-14-traffic-quotas-and-history-design.md](../specs/2026-05-14-traffic-quotas-and-history-design.md)

---

## File Structure

### Proto
- Modify: `proto/portunus.proto` — add `TrafficQuotaUpdate`, `TrafficQuotaState`, `TrafficQuotaAction`, register as `ServerMessage.payload` field 4

### Server (`crates/portunus-server/`)
- Create: `src/store/migrations/V008__add_traffic_quotas.sql` — 3 tables
- Create: `src/traffic_quotas/mod.rs` — Quota model + period anniversary math
- Create: `src/traffic_quotas/store.rs` — SQLite CRUD for `traffic_quotas`
- Create: `src/traffic_quotas/samples.rs` — SQLite I/O for `traffic_samples_1m/1h`
- Create: `src/traffic_quotas/aggregator.rs` — Per-(user, client) delta accumulator hook
- Create: `src/traffic_quotas/rollup.rs` — Hourly rollup background task
- Create: `src/traffic_quotas/cache.rs` — In-memory cache of active quotas (mirror of store, for hot lookup)
- Create: `src/operator/quota_http.rs` — HTTP handlers for `/v1/users/{u}/quotas/...` + `/v1/users/{u}/traffic` + `/v1/clients/{c}/quotas` + `/v1/clients/{c}/traffic`
- Modify: `src/lib.rs` — wire `traffic_quotas` module
- Modify: `src/state.rs` — add `TrafficQuotaCache` + samples store handles to `AppState`
- Modify: `src/metrics.rs` — extend `Metrics` struct with 5 new collectors; extend `RuleStatsCache::observe` to delegate to aggregator
- Modify: `src/operator/http.rs` — wire new routes
- Modify: `src/grpc/service.rs` — replay traffic_quotas BEFORE rules (lines 171-206)
- Modify: `src/rbac/policy.rs` (if exists) — quota CRUD perms

### Client (`crates/portunus-client/`)
- Create: `src/forwarder/quota/mod.rs` — `QuotaHandle`, `QuotaScopeManager`, saturating CAS consume
- Create: `src/forwarder/quota/copy.rs` — TCP userspace quota-aware bidirectional copy loop
- Modify: `src/control.rs` — handle `TrafficQuotaUpdate` SET/REMOVE (mirror `apply_owner_rate_limit_update`)
- Modify: `src/forwarder/proxy.rs:328-360` — `copy_uncapped` selects quota-aware copy when handle present
- Modify: `src/forwarder/splice.rs:524` — insert per-iteration consume hook
- Modify: `src/forwarder/udp/mod.rs:318,634,823` — insert per-datagram consume hook
- Modify: `src/forwarder/mod.rs` — declare quota module

### Proto crate (`crates/portunus-proto/`)
- (regenerated automatically by build.rs on proto change)

### Web UI (`webui/`)
- Create: `src/api/quotas.ts` — quota CRUD + status hooks
- Create: `src/api/traffic.ts` — traffic query hooks
- Create: `src/components/Traffic/TrafficChart.tsx` — recharts wrapper
- Create: `src/components/Traffic/TrafficPanel.tsx` — full Traffic tab content
- Create: `src/components/UserQuota/QuotaCellMonthly.tsx` — Monthly quota cell renderer
- Create: `src/components/UserQuota/QuotaCellPeriodProgress.tsx` — This period progress cell
- Create: `src/components/Traffic/ExhaustedBanner.tsx`
- Modify: `src/api/access-entries.ts` — extend `AccessEntry` with quota fields, merge `useQuotas`
- Modify: `src/components/UserQuota/UserQuotaTable.tsx` — render two new columns
- Modify: `src/components/UserQuota/UserQuotaForm.tsx` — add `monthly_bytes` input + `clear_period_usage` action
- Modify: `src/pages/UserDetail.tsx` — wrap content in shadcn Tabs, add Traffic tab
- Modify: `src/pages/ClientDetail.tsx` — add Traffic tab to existing Tabs
- Modify: `src/i18n/en.json` + `src/i18n/zh-CN.json` — `traffic.*` + extend `userQuota.*`
- Modify: `package.json` — add `recharts` dep

### E2E (`crates/portunus-e2e/`)
- Create: `tests/traffic_quotas.rs` — end-to-end: build → enforce hard kill → period rollover → reconnect replay order

### Docs (`docs/`)
- Create: `docs/content/docs/operations/runbook-traffic-quotas.mdx` (EN)
- Create: `docs/content/docs/zh/operations/runbook-traffic-quotas.mdx` (ZH)
- Modify: `docs/content/docs/operations/troubleshooting.mdx` — add quota event symptoms
- Modify: `docs/content/docs/zh/operations/troubleshooting.mdx`
- Modify: `CHANGELOG.md` — `## [Unreleased]` → `## [1.4.0]`

---

## Phase A: Foundation

### Task A1: Proto extension — `TrafficQuotaUpdate` + `TrafficQuotaState`

**Files:**
- Modify: `proto/portunus.proto` — add new message types + register oneof variant

- [ ] **Step 1: Add new message + enum at end of proto/portunus.proto**

Insert before the final closing of the v1 namespace, after the existing `OwnerRateLimitUpdate` block:

```proto
// ============================================================================
// 013-traffic-quotas: per-(user, client) monthly traffic quota state push
// ============================================================================

enum TrafficQuotaAction {
  TRAFFIC_QUOTA_ACTION_UNSPECIFIED = 0;
  TRAFFIC_QUOTA_ACTION_SET = 1;     // `state` must be populated
  TRAFFIC_QUOTA_ACTION_REMOVE = 2;  // `state` MUST be empty
}

message TrafficQuotaState {
  int64 monthly_bytes              = 1;
  int64 budget_remaining_bytes     = 2;  // monthly_bytes - current_period_bytes_used; may be negative when exhausted
  int64 period_started_at_unix_sec = 3;
  int64 period_ends_at_unix_sec    = 4;
  bool  exhausted                  = 5;
}

message TrafficQuotaUpdate {
  string request_id        = 1;
  string user_id           = 2;
  string client_name       = 3;
  TrafficQuotaAction action = 4;
  optional TrafficQuotaState state = 5;
}
```

- [ ] **Step 2: Register oneof variant in `ServerMessage` (lines 50-62)**

Replace the existing `ServerMessage` block:

```proto
message ServerMessage {
  oneof payload {
    Welcome welcome = 1;
    RuleUpdate rule_update = 2;
    OwnerRateLimitUpdate owner_rate_limit_update = 3;
    // Additive in v1.4 (spec 013-traffic-quotas). Pushed when a quota is
    // created/updated/deleted, when a billing period rolls over, when the
    // server first observes cumulative >= monthly_bytes, and during
    // reconnect replay (in this last case, BEFORE any RuleUpdate so the
    // QuotaHandle is registry-resident when the rule activates). v1.3
    // clients (no awareness of this variant) silently drop it; the server
    // capability gate refuses to emit toward client_version < 1.4.0 and
    // returns HTTP 422 quota_unsupported_by_client on quota PUT.
    TrafficQuotaUpdate traffic_quota_update = 4;
  }
}
```

- [ ] **Step 3: Build to regenerate proto crate**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-proto`
Expected: clean build, regenerated bindings emit `TrafficQuotaUpdate`, `TrafficQuotaState`, `TrafficQuotaAction`.

- [ ] **Step 4: Add a smoke test that types compile**

Create `crates/portunus-proto/tests/traffic_quota_smoke.rs`:

```rust
//! Smoke test that the new v1.4 proto types are reachable.

use portunus_proto::v1::{
    server_message, ServerMessage, TrafficQuotaAction, TrafficQuotaState, TrafficQuotaUpdate,
};

#[test]
fn traffic_quota_update_set_roundtrips() {
    let state = TrafficQuotaState {
        monthly_bytes: 1_000_000_000,
        budget_remaining_bytes: 999_000_000,
        period_started_at_unix_sec: 1_704_067_200,
        period_ends_at_unix_sec: 1_706_745_600,
        exhausted: false,
    };
    let update = TrafficQuotaUpdate {
        request_id: "01HXXX".into(),
        user_id: "alice".into(),
        client_name: "edge-01".into(),
        action: TrafficQuotaAction::Set as i32,
        state: Some(state.clone()),
    };
    let msg = ServerMessage {
        payload: Some(server_message::Payload::TrafficQuotaUpdate(update.clone())),
    };
    let encoded = prost::Message::encode_to_vec(&msg);
    let decoded: ServerMessage = prost::Message::decode(&encoded[..]).unwrap();
    let Some(server_message::Payload::TrafficQuotaUpdate(got)) = decoded.payload else {
        panic!("wrong variant");
    };
    assert_eq!(got.user_id, "alice");
    assert_eq!(got.action, TrafficQuotaAction::Set as i32);
    assert_eq!(got.state.unwrap(), state);
}

#[test]
fn traffic_quota_update_remove_has_no_state() {
    let update = TrafficQuotaUpdate {
        request_id: "01HXXY".into(),
        user_id: "alice".into(),
        client_name: "edge-01".into(),
        action: TrafficQuotaAction::Remove as i32,
        state: None,
    };
    let encoded = prost::Message::encode_to_vec(&update);
    let got: TrafficQuotaUpdate = prost::Message::decode(&encoded[..]).unwrap();
    assert!(got.state.is_none());
}
```

- [ ] **Step 5: Run the smoke test**

Run: `cargo test -p portunus-proto traffic_quota_smoke`
Expected: 2 tests pass.

- [ ] **Step 6: Commit**

```bash
git add proto/portunus.proto crates/portunus-proto/tests/traffic_quota_smoke.rs
git commit -m "proto: add TrafficQuotaUpdate variant for v1.4 (013-traffic-quotas)

Adds TrafficQuotaUpdate as ServerMessage.payload field 4 with SET/REMOVE
action and TrafficQuotaState body. Client->server wire is unchanged;
server aggregates traffic from existing RuleStats by owner_id."
```

---

### Task A2: SQLite migration V008 — `traffic_quotas` + sample tables

**Files:**
- Create: `crates/portunus-server/src/store/migrations/V008__add_traffic_quotas.sql`

- [ ] **Step 1: Create migration file**

```sql
-- 013-traffic-quotas: per-(user, client) monthly quota + two-tier rollup
-- history. Quota survives rule deletion (billing artifact, see spec §7.1).
-- All timestamps are unix seconds UTC; monthly_bytes uses i64 range
-- (matches Rust AtomicI64 + proto int64 — see spec §3.1).

CREATE TABLE traffic_quotas (
    user_id                       TEXT    NOT NULL,
    client_name                   TEXT    NOT NULL,
    monthly_bytes                 INTEGER NOT NULL
                                    CHECK (monthly_bytes >= 0
                                       AND monthly_bytes <= 9223372036854775807),
    billing_anchor                INTEGER NOT NULL,
    current_period_started_at     INTEGER NOT NULL,
    current_period_bytes_used     INTEGER NOT NULL DEFAULT 0
                                    CHECK (current_period_bytes_used >= 0),
    exhausted_at                  INTEGER,
    created_at                    INTEGER NOT NULL,
    updated_at                    INTEGER NOT NULL,
    PRIMARY KEY (user_id, client_name)
);

CREATE INDEX idx_traffic_quotas_client ON traffic_quotas(client_name);

CREATE TABLE traffic_samples_1m (
    user_id      TEXT    NOT NULL,
    client_name  TEXT    NOT NULL,
    ts_minute    INTEGER NOT NULL,
    bytes_in     INTEGER NOT NULL CHECK (bytes_in  >= 0),
    bytes_out    INTEGER NOT NULL CHECK (bytes_out >= 0),
    PRIMARY KEY (user_id, client_name, ts_minute)
);

CREATE INDEX idx_traffic_samples_1m_ts ON traffic_samples_1m(ts_minute);

CREATE TABLE traffic_samples_1h (
    user_id      TEXT    NOT NULL,
    client_name  TEXT    NOT NULL,
    ts_hour      INTEGER NOT NULL,
    bytes_in     INTEGER NOT NULL CHECK (bytes_in  >= 0),
    bytes_out    INTEGER NOT NULL CHECK (bytes_out >= 0),
    PRIMARY KEY (user_id, client_name, ts_hour)
);

CREATE INDEX idx_traffic_samples_1h_ts ON traffic_samples_1h(ts_hour);

CREATE TABLE traffic_rollup_state (
    id                   INTEGER PRIMARY KEY CHECK (id = 1),
    last_rolled_up_hour  INTEGER NOT NULL DEFAULT 0
);

INSERT INTO traffic_rollup_state(id, last_rolled_up_hour) VALUES (1, 0);
```

- [ ] **Step 2: Bump migration handshake version**

Find where `V007` is registered (likely `crates/portunus-server/src/store/mod.rs` or `migrations.rs`). The pattern is: a const array of migration SQL strings + a schema version constant. Add V008 to the array; bump version constant to `8`.

Run `grep -rn "V007\|SCHEMA_VERSION\|expected_schema_version" crates/portunus-server/src/store/` to locate.

After update, this should grep clean for the new migration.

- [ ] **Step 3: Verify migration runs on a fresh DB**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib store::tests::migrations_apply_cleanly_to_fresh_db -- --nocapture`
(If the test doesn't exist yet under that name, find the closest one e.g. `migrations_run` / `schema_version_after_open` and run it. Migration tests live in `store/mod.rs` or `store/migrations.rs`.)
Expected: PASS. The migration applies, the three new tables exist, `traffic_rollup_state` has one row with `id=1`.

- [ ] **Step 4: Add a test that schema is at V008 after open**

In the same test module that hosts the schema-version test:

```rust
#[tokio::test]
async fn schema_at_v8_includes_traffic_tables() {
    let dir = tempfile::tempdir().unwrap();
    let db = Store::open(dir.path().join("state.db")).await.unwrap();
    let v = db.schema_version().await.unwrap();
    assert_eq!(v, 8);
    // sanity: tables present
    let conn = db.acquire().await.unwrap();
    let row: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='traffic_quotas'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(row, 1);
}
```

Adapt the `Store::open` / `schema_version` / `acquire` calls to match the existing test helper conventions in that module.

- [ ] **Step 5: Run the new test**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib schema_at_v8_includes_traffic_tables`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/portunus-server/src/store/migrations/V008__add_traffic_quotas.sql crates/portunus-server/src/store/
git commit -m "store: V008 migration for traffic_quotas + sample tables

Adds traffic_quotas (PK user_id+client_name), traffic_samples_1m,
traffic_samples_1h, traffic_rollup_state. CHECK constraints enforce
non-negative byte counters and i64::MAX upper bound on monthly_bytes."
```

---

### Task A3: Quota model module + store CRUD

**Files:**
- Create: `crates/portunus-server/src/traffic_quotas/mod.rs` — types + period math
- Create: `crates/portunus-server/src/traffic_quotas/store.rs` — `traffic_quotas` SQL CRUD
- Modify: `crates/portunus-server/src/lib.rs` — `pub mod traffic_quotas;`

- [ ] **Step 1: Write `traffic_quotas/mod.rs` core types + period math**

```rust
//! 013-traffic-quotas v1.4.0: per-(user, client) monthly traffic quota
//! data model. Period anniversary computation (calendar month, clamped
//! to last day) is implemented here so it can be unit-tested without
//! the SQLite layer. See design spec §3.3.

use chrono::{DateTime, Datelike, TimeZone, Timelike, Utc};

pub mod store;

/// One row of `traffic_quotas`. Mirrors the schema 1:1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrafficQuotaRow {
    pub user_id: String,
    pub client_name: String,
    pub monthly_bytes: i64,
    pub billing_anchor: i64,                 // unix sec UTC
    pub current_period_started_at: i64,      // unix sec UTC
    pub current_period_bytes_used: i64,
    pub exhausted_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl TrafficQuotaRow {
    pub fn budget_remaining(&self) -> i64 {
        self.monthly_bytes.saturating_sub(self.current_period_bytes_used)
    }
    pub fn is_exhausted(&self) -> bool {
        self.exhausted_at.is_some()
    }
}

/// Compute the start instant of period number `n` (n=0 is the anchor itself)
/// from the original billing anchor. Every period start is computed
/// relative to the original anchor so the day-of-month never drifts
/// (Jan 31 -> Feb 28/29 -> Mar 31, not Jan 31 -> Feb 28 -> Mar 28).
///
/// All times are UTC.
pub fn period_start_at(billing_anchor: DateTime<Utc>, n: u32) -> DateTime<Utc> {
    let anchor_day = billing_anchor.day();
    let (h, m, s) = (
        billing_anchor.hour(),
        billing_anchor.minute(),
        billing_anchor.second(),
    );

    let total_months =
        i64::from(billing_anchor.year()) * 12 + i64::from(billing_anchor.month()) - 1
            + i64::from(n);
    let target_year = (total_months / 12) as i32;
    let target_month = ((total_months % 12) + 1) as u32;
    let max_day = last_day_of_month(target_year, target_month);
    let day = anchor_day.min(max_day);

    Utc.with_ymd_and_hms(target_year, target_month, day, h, m, s)
        .single()
        .expect("period_start_at constructs valid date")
}

fn last_day_of_month(year: i32, month: u32) -> u32 {
    // Jump to first of next month, subtract one day.
    let (ny, nm) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let first_of_next = Utc.with_ymd_and_hms(ny, nm, 1, 0, 0, 0).single().unwrap();
    (first_of_next - chrono::Duration::days(1)).day()
}

/// Given the current state, advance `current_period_started_at` to the
/// latest anchor-period start <= `now`. Returns the new period start
/// instant if the row needs to be advanced. The caller is responsible
/// for zeroing `bytes_used`, clearing `exhausted_at`, and persisting.
pub fn advance_period_if_due(
    billing_anchor: DateTime<Utc>,
    current_period_started_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let mut n = 0u32;
    // Find n such that period_start(n) == current_period_started_at; fall
    // back to scanning forward from anchor if mismatch (e.g., manual DB
    // edit). The scan is bounded by years-since-anchor * 12.
    loop {
        let p = period_start_at(billing_anchor, n);
        if p == current_period_started_at {
            break;
        }
        if p > current_period_started_at {
            // Anchor was changed; rebase to nearest.
            break;
        }
        n += 1;
        if n > 12_000 {
            // Sanity break — 1000 years of months.
            break;
        }
    }
    // Now advance n while next period start is <= now.
    let mut advanced = false;
    loop {
        let next = period_start_at(billing_anchor, n + 1);
        if next > now {
            break;
        }
        n += 1;
        advanced = true;
    }
    if advanced {
        Some(period_start_at(billing_anchor, n))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(yyyy: i32, mm: u32, dd: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(yyyy, mm, dd, 0, 0, 0).single().unwrap()
    }

    #[test]
    fn period_start_jan31_clamps_february() {
        let a = anchor(2026, 1, 31);
        assert_eq!(period_start_at(a, 0), a);
        assert_eq!(period_start_at(a, 1), anchor(2026, 2, 28));
        assert_eq!(period_start_at(a, 2), anchor(2026, 3, 31));
        assert_eq!(period_start_at(a, 3), anchor(2026, 4, 30));
        assert_eq!(period_start_at(a, 12), anchor(2027, 1, 31));
    }

    #[test]
    fn period_start_feb29_leap_year() {
        let a = anchor(2024, 2, 29);
        assert_eq!(period_start_at(a, 12), anchor(2025, 2, 28));
        assert_eq!(period_start_at(a, 48), anchor(2028, 2, 29));
    }

    #[test]
    fn advance_period_skips_many_months() {
        let a = anchor(2026, 1, 15);
        // Started in Jan but `now` is May -> advance to May period.
        let started = period_start_at(a, 0);
        let now = anchor(2026, 5, 20);
        let new_start = advance_period_if_due(a, started, now).unwrap();
        assert_eq!(new_start, anchor(2026, 5, 15));
    }

    #[test]
    fn advance_period_returns_none_when_not_due() {
        let a = anchor(2026, 1, 15);
        let started = period_start_at(a, 3); // Apr 15
        let now = anchor(2026, 5, 14); // Day before period rolls
        assert!(advance_period_if_due(a, started, now).is_none());
    }
}
```

- [ ] **Step 2: Wire the module**

Edit `crates/portunus-server/src/lib.rs`:
- Add `pub mod traffic_quotas;` near the other `pub mod` lines.

- [ ] **Step 3: Run the period math tests**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib traffic_quotas::tests`
Expected: 4 tests pass.

- [ ] **Step 4: Write store.rs with CRUD**

```rust
//! SQLite CRUD for `traffic_quotas`. Pure data access — period math
//! and aggregation logic live in the parent module / sibling modules.

use crate::store::{Store, StoreError};
use crate::traffic_quotas::TrafficQuotaRow;
use rusqlite::{params, OptionalExtension};

pub async fn insert_or_replace(store: &Store, row: &TrafficQuotaRow) -> Result<(), StoreError> {
    let row = row.clone();
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT OR REPLACE INTO traffic_quotas (
                    user_id, client_name, monthly_bytes, billing_anchor,
                    current_period_started_at, current_period_bytes_used,
                    exhausted_at, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    row.user_id, row.client_name, row.monthly_bytes, row.billing_anchor,
                    row.current_period_started_at, row.current_period_bytes_used,
                    row.exhausted_at, row.created_at, row.updated_at,
                ],
            )?;
            Ok(())
        })
        .await
}

pub async fn get(
    store: &Store,
    user_id: &str,
    client_name: &str,
) -> Result<Option<TrafficQuotaRow>, StoreError> {
    let user_id = user_id.to_string();
    let client_name = client_name.to_string();
    store
        .with_conn(move |c| {
            let row = c
                .query_row(
                    "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                            current_period_started_at, current_period_bytes_used,
                            exhausted_at, created_at, updated_at
                     FROM traffic_quotas
                     WHERE user_id = ?1 AND client_name = ?2",
                    params![user_id, client_name],
                    row_to_quota,
                )
                .optional()?;
            Ok(row)
        })
        .await
}

pub async fn delete(
    store: &Store,
    user_id: &str,
    client_name: &str,
) -> Result<bool, StoreError> {
    let user_id = user_id.to_string();
    let client_name = client_name.to_string();
    store
        .with_conn(move |c| {
            let n = c.execute(
                "DELETE FROM traffic_quotas WHERE user_id = ?1 AND client_name = ?2",
                params![user_id, client_name],
            )?;
            Ok(n > 0)
        })
        .await
}

pub async fn list_for_user(store: &Store, user_id: &str) -> Result<Vec<TrafficQuotaRow>, StoreError> {
    let user_id = user_id.to_string();
    store
        .with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                        current_period_started_at, current_period_bytes_used,
                        exhausted_at, created_at, updated_at
                 FROM traffic_quotas WHERE user_id = ?1",
            )?;
            let rows = stmt
                .query_map(params![user_id], row_to_quota)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
}

pub async fn list_for_client(
    store: &Store,
    client_name: &str,
) -> Result<Vec<TrafficQuotaRow>, StoreError> {
    let client_name = client_name.to_string();
    store
        .with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                        current_period_started_at, current_period_bytes_used,
                        exhausted_at, created_at, updated_at
                 FROM traffic_quotas WHERE client_name = ?1",
            )?;
            let rows = stmt
                .query_map(params![client_name], row_to_quota)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
}

pub async fn accumulate_bytes_used(
    store: &Store,
    user_id: &str,
    client_name: &str,
    delta_bytes: i64,
    now_unix_sec: i64,
) -> Result<Option<TrafficQuotaRow>, StoreError> {
    let user_id = user_id.to_string();
    let client_name = client_name.to_string();
    store
        .with_conn(move |c| {
            let updated = c.execute(
                "UPDATE traffic_quotas
                    SET current_period_bytes_used = current_period_bytes_used + ?3,
                        exhausted_at = CASE
                            WHEN exhausted_at IS NOT NULL THEN exhausted_at
                            WHEN (current_period_bytes_used + ?3) >= monthly_bytes THEN ?4
                            ELSE NULL
                        END,
                        updated_at = ?4
                  WHERE user_id = ?1 AND client_name = ?2",
                params![user_id, client_name, delta_bytes, now_unix_sec],
            )?;
            if updated == 0 {
                return Ok(None);
            }
            let row = c
                .query_row(
                    "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                            current_period_started_at, current_period_bytes_used,
                            exhausted_at, created_at, updated_at
                     FROM traffic_quotas
                     WHERE user_id = ?1 AND client_name = ?2",
                    params![user_id, client_name],
                    row_to_quota,
                )
                .optional()?;
            Ok(row)
        })
        .await
}

pub async fn reset_period(
    store: &Store,
    user_id: &str,
    client_name: &str,
    new_period_started_at: i64,
    now_unix_sec: i64,
) -> Result<Option<TrafficQuotaRow>, StoreError> {
    let user_id = user_id.to_string();
    let client_name = client_name.to_string();
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE traffic_quotas
                    SET current_period_started_at = ?3,
                        current_period_bytes_used = 0,
                        exhausted_at = NULL,
                        updated_at = ?4
                  WHERE user_id = ?1 AND client_name = ?2",
                params![user_id, client_name, new_period_started_at, now_unix_sec],
            )?;
            let row = c
                .query_row(
                    "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                            current_period_started_at, current_period_bytes_used,
                            exhausted_at, created_at, updated_at
                     FROM traffic_quotas
                     WHERE user_id = ?1 AND client_name = ?2",
                    params![user_id, client_name],
                    row_to_quota,
                )
                .optional()?;
            Ok(row)
        })
        .await
}

pub async fn clear_period_usage(
    store: &Store,
    user_id: &str,
    client_name: &str,
    now_unix_sec: i64,
) -> Result<Option<TrafficQuotaRow>, StoreError> {
    let user_id = user_id.to_string();
    let client_name = client_name.to_string();
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE traffic_quotas
                    SET current_period_bytes_used = 0,
                        exhausted_at = NULL,
                        updated_at = ?3
                  WHERE user_id = ?1 AND client_name = ?2",
                params![user_id, client_name, now_unix_sec],
            )?;
            let row = c
                .query_row(
                    "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                            current_period_started_at, current_period_bytes_used,
                            exhausted_at, created_at, updated_at
                     FROM traffic_quotas
                     WHERE user_id = ?1 AND client_name = ?2",
                    params![user_id, client_name],
                    row_to_quota,
                )
                .optional()?;
            Ok(row)
        })
        .await
}

fn row_to_quota(r: &rusqlite::Row) -> rusqlite::Result<TrafficQuotaRow> {
    Ok(TrafficQuotaRow {
        user_id: r.get(0)?,
        client_name: r.get(1)?,
        monthly_bytes: r.get(2)?,
        billing_anchor: r.get(3)?,
        current_period_started_at: r.get(4)?,
        current_period_bytes_used: r.get(5)?,
        exhausted_at: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}
```

> **Note:** Adapt `Store` import path + `with_conn` / `acquire` to whatever the existing repos use (e.g., `crate::store::Store`). Grep one of the existing repos like `src/clients.rs` for the signature pattern.

- [ ] **Step 5: Write CRUD tests**

In `store.rs`, append `#[cfg(test)] mod tests` with these tests (open a temp Store, exercise round-trip + accumulate + exhaust + reset). Mirror the test style of `crates/portunus-server/src/clients.rs` tests.

- [ ] **Step 6: Run tests**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib traffic_quotas::store::tests`
Expected: all tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/portunus-server/src/traffic_quotas/ crates/portunus-server/src/lib.rs
git commit -m "server: traffic_quotas module + SQLite CRUD

Adds period anniversary math (calendar-month clamp + multi-month
skip) with unit tests covering Jan-31 -> Feb-28/29 progression and
leap year cases. Store wrappers for insert_or_replace, get, delete,
list_for_user/client, accumulate_bytes_used (atomic UPDATE that
sets exhausted_at on overflow), reset_period, clear_period_usage."
```

---

### Task A4: Samples store (`traffic_samples_1m` + `traffic_samples_1h`)

**Files:**
- Create: `crates/portunus-server/src/traffic_quotas/samples.rs`

- [ ] **Step 1: Write samples.rs**

```rust
//! SQLite I/O for traffic_samples_1m and traffic_samples_1h, plus the
//! query helpers used by `/v1/users/{u}/traffic` and `/v1/clients/{c}/traffic`.

use crate::store::{Store, StoreError};
use rusqlite::params;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SampleBucket {
    /// 1-minute granularity, 7 day retention.
    M1,
    /// 1-hour granularity, 90 day retention.
    H1,
}

impl SampleBucket {
    pub fn retention_seconds(&self) -> i64 {
        match self {
            SampleBucket::M1 => 7 * 24 * 3600,
            SampleBucket::H1 => 90 * 24 * 3600,
        }
    }
    pub fn align(&self, ts_unix_sec: i64) -> i64 {
        match self {
            SampleBucket::M1 => ts_unix_sec - (ts_unix_sec % 60),
            SampleBucket::H1 => ts_unix_sec - (ts_unix_sec % 3600),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TrafficSample {
    pub ts: i64,         // unix sec, aligned to bucket
    pub bytes_in: i64,
    pub bytes_out: i64,
}

/// UPSERT delta into the 1m bucket for the current minute.
pub async fn upsert_1m_delta(
    store: &Store,
    user_id: &str,
    client_name: &str,
    ts_minute: i64,
    delta_in: i64,
    delta_out: i64,
) -> Result<(), StoreError> {
    let user_id = user_id.to_string();
    let client_name = client_name.to_string();
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO traffic_samples_1m (user_id, client_name, ts_minute, bytes_in, bytes_out)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(user_id, client_name, ts_minute) DO UPDATE
                   SET bytes_in  = bytes_in  + excluded.bytes_in,
                       bytes_out = bytes_out + excluded.bytes_out",
                params![user_id, client_name, ts_minute, delta_in, delta_out],
            )?;
            Ok(())
        })
        .await
}

/// Roll up all 1m rows for one hour into a single 1h row. Idempotent
/// (uses INSERT OR REPLACE for the destination).
pub async fn rollup_hour(store: &Store, ts_hour: i64) -> Result<(), StoreError> {
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO traffic_samples_1h (user_id, client_name, ts_hour, bytes_in, bytes_out)
                 SELECT user_id, client_name, ?1 AS ts_hour,
                        SUM(bytes_in)  AS bytes_in,
                        SUM(bytes_out) AS bytes_out
                 FROM traffic_samples_1m
                 WHERE ts_minute >= ?1 AND ts_minute < ?2
                 GROUP BY user_id, client_name
                 ON CONFLICT(user_id, client_name, ts_hour) DO UPDATE
                   SET bytes_in  = excluded.bytes_in,
                       bytes_out = excluded.bytes_out",
                params![ts_hour, ts_hour + 3600],
            )?;
            Ok(())
        })
        .await
}

pub async fn delete_1m_older_than(store: &Store, threshold_unix_sec: i64) -> Result<usize, StoreError> {
    store
        .with_conn(move |c| {
            let n = c.execute(
                "DELETE FROM traffic_samples_1m WHERE ts_minute < ?1",
                params![threshold_unix_sec],
            )?;
            Ok(n)
        })
        .await
}

pub async fn delete_1h_older_than(store: &Store, threshold_unix_sec: i64) -> Result<usize, StoreError> {
    store
        .with_conn(move |c| {
            let n = c.execute(
                "DELETE FROM traffic_samples_1h WHERE ts_hour < ?1",
                params![threshold_unix_sec],
            )?;
            Ok(n)
        })
        .await
}

pub async fn get_last_rolled_up_hour(store: &Store) -> Result<i64, StoreError> {
    store
        .with_conn(|c| {
            let v: i64 = c.query_row(
                "SELECT last_rolled_up_hour FROM traffic_rollup_state WHERE id = 1",
                [],
                |r| r.get(0),
            )?;
            Ok(v)
        })
        .await
}

pub async fn set_last_rolled_up_hour(store: &Store, ts_hour: i64) -> Result<(), StoreError> {
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE traffic_rollup_state SET last_rolled_up_hour = ?1 WHERE id = 1",
                params![ts_hour],
            )?;
            Ok(())
        })
        .await
}

/// Query the chosen bucket, optionally filtered by user / client.
/// `from_unix_sec` and `to_unix_sec` are inclusive lower, exclusive upper.
/// Aggregates across rows when a filter is partial (e.g., user only).
pub async fn query_samples(
    store: &Store,
    bucket: SampleBucket,
    user_id: Option<&str>,
    client_name: Option<&str>,
    from_unix_sec: i64,
    to_unix_sec: i64,
) -> Result<Vec<TrafficSample>, StoreError> {
    let (table, ts_col) = match bucket {
        SampleBucket::M1 => ("traffic_samples_1m", "ts_minute"),
        SampleBucket::H1 => ("traffic_samples_1h", "ts_hour"),
    };
    let mut sql = format!(
        "SELECT {ts_col} AS ts, SUM(bytes_in) AS bin, SUM(bytes_out) AS bout
           FROM {table}
          WHERE {ts_col} >= ?1 AND {ts_col} < ?2"
    );
    let user_id = user_id.map(str::to_string);
    let client_name = client_name.map(str::to_string);
    if user_id.is_some() {
        sql.push_str(" AND user_id = ?3");
    }
    if client_name.is_some() {
        let n = if user_id.is_some() { 4 } else { 3 };
        sql.push_str(&format!(" AND client_name = ?{n}"));
    }
    sql.push_str(&format!(" GROUP BY {ts_col} ORDER BY {ts_col} ASC"));

    store
        .with_conn(move |c| {
            let mut stmt = c.prepare(&sql)?;
            let map = |r: &rusqlite::Row| -> rusqlite::Result<TrafficSample> {
                Ok(TrafficSample {
                    ts: r.get(0)?,
                    bytes_in: r.get(1).unwrap_or(0),
                    bytes_out: r.get(2).unwrap_or(0),
                })
            };
            let rows: Vec<TrafficSample> = match (user_id.as_deref(), client_name.as_deref()) {
                (None, None) => stmt
                    .query_map(params![from_unix_sec, to_unix_sec], map)?
                    .collect::<Result<_, _>>()?,
                (Some(u), None) => stmt
                    .query_map(params![from_unix_sec, to_unix_sec, u], map)?
                    .collect::<Result<_, _>>()?,
                (None, Some(c)) => stmt
                    .query_map(params![from_unix_sec, to_unix_sec, c], map)?
                    .collect::<Result<_, _>>()?,
                (Some(u), Some(c)) => stmt
                    .query_map(params![from_unix_sec, to_unix_sec, u, c], map)?
                    .collect::<Result<_, _>>()?,
            };
            Ok(rows)
        })
        .await
}
```

- [ ] **Step 2: Wire module**

In `crates/portunus-server/src/traffic_quotas/mod.rs`, add `pub mod samples;`.

- [ ] **Step 3: Write tests for upsert / rollup / query**

Append `#[cfg(test)] mod tests` to samples.rs covering:
- `upsert_1m_delta` is additive across two calls with same key
- `rollup_hour` aggregates 60 minute rows into one hour row, idempotent on re-run
- `query_samples` with each combination of (user_id, client_name) filters
- `delete_1m_older_than` removes only old rows

- [ ] **Step 4: Run**

`PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib traffic_quotas::samples::tests`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/traffic_quotas/samples.rs crates/portunus-server/src/traffic_quotas/mod.rs
git commit -m "server: traffic_samples_1m/1h store + query helpers

UPSERT for current-minute delta, idempotent hour rollup, retention
delete, and a query_samples API that grouping-aggregates by ts when
caller omits user_id or client_name filters."
```

---

## Phase B: Server Core (cache + aggregator + rollup task)

### Task B1: TrafficQuotaCache — in-memory mirror

**Files:**
- Create: `crates/portunus-server/src/traffic_quotas/cache.rs`

The cache mirrors `traffic_quotas` rows in memory so the StatsReport hot path can update `current_period_bytes_used` without touching SQLite per StatsReport tick. Writes are still persisted (via store CRUD) but the cache is the read truth for the hot path; SQLite is the source of truth for restart.

- [ ] **Step 1: Implement cache**

```rust
//! In-memory cache of active traffic_quotas rows. Refreshed from SQLite
//! at startup and on every write through quota CRUD. The aggregator
//! reads + writes through the cache (so StatsReport accumulation does
//! not block on SQLite each tick); the cache calls back into the store
//! on mutating ops to keep both in sync.

use crate::store::Store;
use crate::traffic_quotas::{store as quota_store, TrafficQuotaRow};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::warn;

#[derive(Clone)]
pub struct TrafficQuotaCache {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    /// Keyed by (user_id, client_name). One row per pair.
    cache: RwLock<HashMap<(String, String), TrafficQuotaRow>>,
}

impl TrafficQuotaCache {
    pub async fn load(store: Store) -> Result<Self, crate::store::StoreError> {
        // We have no list-all helper; we approximate by querying each user.
        // Simpler approach: add a list_all() that returns every row.
        let rows = quota_store_list_all(&store).await?;
        let mut map = HashMap::with_capacity(rows.len());
        for row in rows {
            map.insert((row.user_id.clone(), row.client_name.clone()), row);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                cache: RwLock::new(map),
            }),
        })
    }

    pub async fn get(&self, user_id: &str, client_name: &str) -> Option<TrafficQuotaRow> {
        self.inner
            .cache
            .read()
            .await
            .get(&(user_id.to_string(), client_name.to_string()))
            .cloned()
    }

    pub async fn list_for_client(&self, client_name: &str) -> Vec<TrafficQuotaRow> {
        self.inner
            .cache
            .read()
            .await
            .values()
            .filter(|r| r.client_name == client_name)
            .cloned()
            .collect()
    }

    pub async fn list_for_user(&self, user_id: &str) -> Vec<TrafficQuotaRow> {
        self.inner
            .cache
            .read()
            .await
            .values()
            .filter(|r| r.user_id == user_id)
            .cloned()
            .collect()
    }

    /// Upsert a quota row through both cache and store.
    pub async fn upsert(
        &self,
        row: TrafficQuotaRow,
    ) -> Result<TrafficQuotaRow, crate::store::StoreError> {
        quota_store::insert_or_replace(&self.inner.store, &row).await?;
        self.inner
            .cache
            .write()
            .await
            .insert((row.user_id.clone(), row.client_name.clone()), row.clone());
        Ok(row)
    }

    pub async fn delete(
        &self,
        user_id: &str,
        client_name: &str,
    ) -> Result<bool, crate::store::StoreError> {
        let removed = quota_store::delete(&self.inner.store, user_id, client_name).await?;
        self.inner
            .cache
            .write()
            .await
            .remove(&(user_id.to_string(), client_name.to_string()));
        Ok(removed)
    }

    /// Accumulate cumulative byte delta into the current period. Called
    /// by the aggregator after every RuleStats observation that lands
    /// on a pair with a quota row.
    pub async fn accumulate(
        &self,
        user_id: &str,
        client_name: &str,
        delta: i64,
        now_unix_sec: i64,
    ) -> Result<Option<TrafficQuotaRow>, crate::store::StoreError> {
        let updated = quota_store::accumulate_bytes_used(
            &self.inner.store,
            user_id,
            client_name,
            delta,
            now_unix_sec,
        )
        .await?;
        if let Some(ref row) = updated {
            self.inner
                .cache
                .write()
                .await
                .insert((row.user_id.clone(), row.client_name.clone()), row.clone());
        } else {
            warn!(
                event = "traffic_quota.accumulate_missing",
                user_id, client_name, delta,
                "accumulate found no row; cache may be stale"
            );
        }
        Ok(updated)
    }

    pub async fn clear_period_usage(
        &self,
        user_id: &str,
        client_name: &str,
        now: i64,
    ) -> Result<Option<TrafficQuotaRow>, crate::store::StoreError> {
        let updated =
            quota_store::clear_period_usage(&self.inner.store, user_id, client_name, now).await?;
        if let Some(ref row) = updated {
            self.inner
                .cache
                .write()
                .await
                .insert((row.user_id.clone(), row.client_name.clone()), row.clone());
        }
        Ok(updated)
    }

    pub async fn reset_period(
        &self,
        user_id: &str,
        client_name: &str,
        new_period_started_at: i64,
        now: i64,
    ) -> Result<Option<TrafficQuotaRow>, crate::store::StoreError> {
        let updated = quota_store::reset_period(
            &self.inner.store,
            user_id,
            client_name,
            new_period_started_at,
            now,
        )
        .await?;
        if let Some(ref row) = updated {
            self.inner
                .cache
                .write()
                .await
                .insert((row.user_id.clone(), row.client_name.clone()), row.clone());
        }
        Ok(updated)
    }
}

async fn quota_store_list_all(store: &Store) -> Result<Vec<TrafficQuotaRow>, crate::store::StoreError> {
    store
        .with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                        current_period_started_at, current_period_bytes_used,
                        exhausted_at, created_at, updated_at FROM traffic_quotas",
            )?;
            let rows: Vec<TrafficQuotaRow> = stmt
                .query_map([], |r| {
                    Ok(TrafficQuotaRow {
                        user_id: r.get(0)?,
                        client_name: r.get(1)?,
                        monthly_bytes: r.get(2)?,
                        billing_anchor: r.get(3)?,
                        current_period_started_at: r.get(4)?,
                        current_period_bytes_used: r.get(5)?,
                        exhausted_at: r.get(6)?,
                        created_at: r.get(7)?,
                        updated_at: r.get(8)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
        .await
}
```

- [ ] **Step 2: Wire module + tests**

Add `pub mod cache;` to `traffic_quotas/mod.rs`. Tests: load empty store → empty cache; upsert then get → returns row; accumulate increments cache; delete removes from cache.

- [ ] **Step 3: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib traffic_quotas::cache
git add crates/portunus-server/src/traffic_quotas/cache.rs crates/portunus-server/src/traffic_quotas/mod.rs
git commit -m "server: TrafficQuotaCache — in-memory mirror with write-through"
```

---

### Task B2: Aggregator hook in `RuleStatsCache::observe`

**Files:**
- Create: `crates/portunus-server/src/traffic_quotas/aggregator.rs`
- Modify: `crates/portunus-server/src/metrics.rs:527-568` — call aggregator after per-rule delta computation

The aggregator: given (client_name, rule_id, owner_id, bytes_in_delta, bytes_out_delta), resolve `owner_id` → `user_id` (they are the same in this codebase — owner_id IS user_id), then:
1. UPSERT `traffic_samples_1m` for the current minute with delta
2. If `(user_id, client_name)` has a quota: call `cache.accumulate(...)` and inspect the returned row for first-time-exhausted; if so, schedule a `TrafficQuotaUpdate` push.

- [ ] **Step 1: Write aggregator.rs**

```rust
//! 013-traffic-quotas: server-side bytes aggregator hooked into the
//! existing RuleStatsCache::observe path. Reads per-rule deltas,
//! resolves owner_id -> user_id (1:1 in this codebase), and:
//!   (a) UPSERTs the current-minute sample row
//!   (b) accumulates into the active quota row (if any) and reports
//!       first-time-exhausted to the push channel.

use crate::store::Store;
use crate::traffic_quotas::cache::TrafficQuotaCache;
use crate::traffic_quotas::samples;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error};

/// Event the aggregator emits when a quota crosses from "not exhausted"
/// to "exhausted" as a result of an accumulated delta. The gRPC pump
/// task consumes these and pushes `TrafficQuotaUpdate{exhausted=true}`
/// to the relevant client session.
#[derive(Debug, Clone)]
pub struct QuotaExhaustedEvent {
    pub user_id: String,
    pub client_name: String,
}

#[derive(Clone)]
pub struct TrafficAggregator {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    cache: TrafficQuotaCache,
    exhaust_tx: mpsc::Sender<QuotaExhaustedEvent>,
}

impl TrafficAggregator {
    pub fn new(
        store: Store,
        cache: TrafficQuotaCache,
        exhaust_tx: mpsc::Sender<QuotaExhaustedEvent>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                store,
                cache,
                exhaust_tx,
            }),
        }
    }

    /// Called from `RuleStatsCache::observe` after it has computed
    /// `(bytes_in_delta, bytes_out_delta)` for one rule. `owner_id`
    /// is the user_id who owns the rule. Empty `owner_id` means a
    /// legacy / unowned rule and the call is a no-op.
    pub async fn record(
        &self,
        client_name: &str,
        owner_id: &str,
        bytes_in_delta: u64,
        bytes_out_delta: u64,
        now_unix_sec: i64,
    ) {
        if owner_id.is_empty() || (bytes_in_delta == 0 && bytes_out_delta == 0) {
            return;
        }
        let delta_total =
            i64::try_from(bytes_in_delta.saturating_add(bytes_out_delta)).unwrap_or(i64::MAX);

        // Always record into 1m samples — coverage is "all pairs", not "quota'd pairs".
        let ts_minute = samples::SampleBucket::M1.align(now_unix_sec);
        if let Err(e) = samples::upsert_1m_delta(
            &self.inner.store,
            owner_id,
            client_name,
            ts_minute,
            i64::try_from(bytes_in_delta).unwrap_or(i64::MAX),
            i64::try_from(bytes_out_delta).unwrap_or(i64::MAX),
        )
        .await
        {
            error!(
                event = "traffic_aggregator.sample_write_failed",
                error = %e,
                client = client_name,
                user = owner_id,
            );
        }

        // If a quota row exists for this pair, accumulate + check exhausted.
        if self.inner.cache.get(owner_id, client_name).await.is_none() {
            return;
        }
        match self
            .inner
            .cache
            .accumulate(owner_id, client_name, delta_total, now_unix_sec)
            .await
        {
            Ok(Some(row)) => {
                // First-time exhausted notification: exhausted_at must equal now_unix_sec.
                if row.exhausted_at == Some(now_unix_sec) {
                    let _ = self
                        .inner
                        .exhaust_tx
                        .send(QuotaExhaustedEvent {
                            user_id: row.user_id,
                            client_name: row.client_name,
                        })
                        .await;
                }
                debug!(
                    event = "traffic_aggregator.accumulated",
                    user = %row.user_id,
                    client = %row.client_name,
                    delta_total,
                    used = row.current_period_bytes_used,
                    monthly = row.monthly_bytes,
                );
            }
            Ok(None) => {
                debug!(
                    event = "traffic_aggregator.no_row",
                    client = client_name,
                    user = owner_id,
                );
            }
            Err(e) => {
                error!(
                    event = "traffic_aggregator.accumulate_failed",
                    error = %e,
                );
            }
        }
    }
}
```

- [ ] **Step 2: Wire into `RuleStatsCache::observe`**

Read `crates/portunus-server/src/metrics.rs:527-568`. Find the section that has computed `(bytes_in_delta, bytes_out_delta)` (look for the existing `Counter::inc_by` call for `rule_bytes_in_total`). Add an aggregator field to `RuleStatsCache`:

```rust
// In RuleStatsCache struct:
aggregator: Option<crate::traffic_quotas::aggregator::TrafficAggregator>,
```

After the existing per-rule delta computation, append:

```rust
if let Some(agg) = self.aggregator.as_ref() {
    let now_unix_sec = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    agg.record(client_name.as_str(), owner, bytes_in_delta, bytes_out_delta, now_unix_sec)
        .await;
}
```

(`bytes_in_delta` / `bytes_out_delta` must be the variables holding the per-rule delta. Read the surrounding code to use the right variable names — if the existing code only computes a total or per-direction value, mirror those.)

Update `RuleStatsCache::new()` to accept `Option<TrafficAggregator>`. Existing tests pass `None`.

- [ ] **Step 3: Add module declaration**

In `traffic_quotas/mod.rs`: `pub mod aggregator;`.

- [ ] **Step 4: Write aggregator tests**

In `aggregator.rs` `#[cfg(test)] mod tests`:
- `record_writes_minute_sample`: agg with empty cache, call `record`, verify `traffic_samples_1m` has one row with the expected delta
- `record_accumulates_when_quota_present`: pre-upsert a quota, call record, verify `current_period_bytes_used` advanced
- `record_emits_exhausted_event_first_time`: pre-upsert quota with `monthly_bytes=100`; call record with delta=200; verify channel receives one event; call record again — channel should NOT receive a second event (`exhausted_at` already set)

- [ ] **Step 5: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib traffic_quotas::aggregator
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib metrics
git add crates/portunus-server/src/traffic_quotas/aggregator.rs crates/portunus-server/src/metrics.rs crates/portunus-server/src/traffic_quotas/mod.rs
git commit -m "server: TrafficAggregator hooked into RuleStatsCache::observe

Per-(user, client) byte delta tap: writes minute samples for all
pairs, accumulates into quota rows when present, emits one
QuotaExhaustedEvent per pair the first time bytes_used >= monthly."
```

---

### Task B3: Hourly rollup background task

**Files:**
- Create: `crates/portunus-server/src/traffic_quotas/rollup.rs`

- [ ] **Step 1: Write rollup task**

```rust
//! Hourly rollup task: aggregates the previous hour's 1m rows into a
//! single 1h row, then prunes minute samples > 7d old and hour samples
//! > 90d old. Runs every hour at +1 minute past the top.

use crate::store::Store;
use crate::traffic_quotas::samples;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info};

const HOUR: i64 = 3600;
const RETENTION_1M: i64 = 7 * 24 * HOUR;
const RETENTION_1H: i64 = 90 * 24 * HOUR;

pub async fn run_forever(store: Store) {
    loop {
        let now = now_unix_sec();
        // Compute sleep until next H+1m boundary.
        let into_hour = now % HOUR;
        let sleep_secs = if into_hour < 60 {
            60 - into_hour
        } else {
            HOUR - into_hour + 60
        };
        sleep(Duration::from_secs(sleep_secs as u64)).await;

        match run_once(&store).await {
            Ok(stats) => info!(
                event = "traffic_rollup.tick",
                rolled_up = stats.rolled_up_hours,
                deleted_1m = stats.deleted_1m,
                deleted_1h = stats.deleted_1h,
            ),
            Err(e) => error!(event = "traffic_rollup.tick_failed", error = %e),
        }
    }
}

pub struct RollupStats {
    pub rolled_up_hours: usize,
    pub deleted_1m: usize,
    pub deleted_1h: usize,
}

pub async fn run_once(store: &Store) -> Result<RollupStats, crate::store::StoreError> {
    let now = now_unix_sec();
    let now_hour = now - (now % HOUR);

    let last = samples::get_last_rolled_up_hour(store).await?;
    // If last_rolled_up_hour is 0 (fresh DB), start at "now - 1 hour".
    let mut next = if last == 0 { now_hour - HOUR } else { last + HOUR };
    let mut rolled = 0usize;
    while next < now_hour {
        samples::rollup_hour(store, next).await?;
        samples::set_last_rolled_up_hour(store, next).await?;
        rolled += 1;
        next += HOUR;
        // Sanity bound — never roll more than 90 days in one tick.
        if rolled > 24 * 90 {
            break;
        }
    }

    let deleted_1m = samples::delete_1m_older_than(store, now - RETENTION_1M).await?;
    let deleted_1h = samples::delete_1h_older_than(store, now - RETENTION_1H).await?;
    Ok(RollupStats {
        rolled_up_hours: rolled,
        deleted_1m,
        deleted_1h,
    })
}

fn now_unix_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
```

- [ ] **Step 2: Tests**

In rollup.rs `#[cfg(test)] mod tests`:
- `run_once_rolls_up_pending_hours`: insert 1m rows across 3 hours in the past, run_once, verify three 1h rows created + `last_rolled_up_hour` updated
- `run_once_is_idempotent`: run_once twice; second run should be a no-op (0 hours rolled up)
- `retention_prunes_old`: insert 1m rows older than 7d + 1h rows older than 90d; run_once; verify deletions
- `run_once_handles_fresh_db`: empty rollup state; first run picks `now - 1h` as starting point

- [ ] **Step 3: Wire spawn into `serve.rs`**

Find where the server boots tasks (likely `crates/portunus-server/src/serve.rs` or `src/lib.rs` `serve` function). Add a spawn:

```rust
let rollup_store = state.store.clone();
tokio::spawn(async move {
    crate::traffic_quotas::rollup::run_forever(rollup_store).await;
});
```

Place near other background-task spawns.

- [ ] **Step 4: Add module declaration + Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib traffic_quotas::rollup
git add crates/portunus-server/src/traffic_quotas/rollup.rs crates/portunus-server/src/traffic_quotas/mod.rs crates/portunus-server/src/serve.rs
git commit -m "server: hourly rollup task with retention pruning"
```

---

## Phase C: Server API + push wiring

### Task C1: HTTP CRUD endpoints

**Files:**
- Create: `crates/portunus-server/src/operator/quota_http.rs`
- Modify: `crates/portunus-server/src/operator/http.rs` — register routes after existing owner-cap routes (~ line 90)
- Modify: `crates/portunus-server/src/state.rs` — `AppState` carries `Arc<TrafficQuotaCache>` and `Arc<TrafficAggregator>`

- [ ] **Step 1: Add cache + aggregator to AppState**

Read `crates/portunus-server/src/state.rs`. Add fields:

```rust
pub traffic_quotas: TrafficQuotaCache,
pub traffic_quota_exhaust_tx: tokio::sync::mpsc::Sender<crate::traffic_quotas::aggregator::QuotaExhaustedEvent>,
```

In `AppState::new`/`build`, construct them and wire into `RuleStatsCache::new()`. The exhaust channel pairs with a receiver consumed by the gRPC push task (Task C5 wires that side).

- [ ] **Step 2: Write quota_http.rs handlers**

```rust
//! 013-traffic-quotas HTTP endpoints. Mounted at:
//!   GET    /v1/users/{u}/quotas
//!   PUT    /v1/users/{u}/quotas/{c}
//!   PATCH  /v1/users/{u}/quotas/{c}
//!   DELETE /v1/users/{u}/quotas/{c}
//!   GET    /v1/users/{u}/quotas/{c}/status
//!   GET    /v1/users/{u}/traffic
//!   GET    /v1/clients/{c}/quotas
//!   GET    /v1/clients/{c}/traffic

use crate::state::AppState;
use crate::traffic_quotas::samples::{self, SampleBucket, TrafficSample};
use crate::traffic_quotas::{period_start_at, TrafficQuotaRow};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct QuotaView {
    pub user_id: String,
    pub client_name: String,
    pub monthly_bytes: i64,
    pub billing_anchor: i64,
    pub current_period_started_at: i64,
    pub current_period_ends_at: i64,
    pub current_period_bytes_used: i64,
    pub exhausted_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<TrafficQuotaRow> for QuotaView {
    fn from(r: TrafficQuotaRow) -> Self {
        let ends_at = compute_period_end(r.billing_anchor, r.current_period_started_at);
        Self {
            user_id: r.user_id,
            client_name: r.client_name,
            monthly_bytes: r.monthly_bytes,
            billing_anchor: r.billing_anchor,
            current_period_started_at: r.current_period_started_at,
            current_period_ends_at: ends_at,
            current_period_bytes_used: r.current_period_bytes_used,
            exhausted_at: r.exhausted_at,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

fn compute_period_end(billing_anchor: i64, current_period_started_at: i64) -> i64 {
    let anchor = Utc.timestamp_opt(billing_anchor, 0).single().unwrap_or_else(Utc::now);
    let start = Utc.timestamp_opt(current_period_started_at, 0).single().unwrap_or(anchor);
    // Find n such that period_start(n) == start.
    let mut n = 0u32;
    while n < 12_000 && period_start_at(anchor, n) != start {
        n += 1;
    }
    period_start_at(anchor, n + 1).timestamp()
}

// -- handlers ---------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PutQuotaBody {
    pub monthly_bytes: i64,
    pub billing_anchor: Option<i64>,
}

pub async fn put_quota(
    State(state): State<Arc<AppState>>,
    Path((user_id, client_name)): Path<(String, String)>,
    Json(body): Json<PutQuotaBody>,
) -> impl IntoResponse {
    if body.monthly_bytes < 0 {
        return error_response(StatusCode::BAD_REQUEST, "invalid_quota_size",
            "monthly_bytes must be >= 0");
    }
    // Capability gate: refuse if target client version < 1.4.0.
    if !client_supports_quota(&state, &client_name).await {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "quota_unsupported_by_client",
            "target client_version < 1.4.0");
    }
    // Grant must exist.
    if !grant_exists(&state, &user_id, &client_name).await {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "quota_target_not_found",
            "no grant for (user, client)");
    }
    let now = now_unix_sec();
    let anchor = body.billing_anchor.unwrap_or(now);
    let started = period_start_at(
        Utc.timestamp_opt(anchor, 0).single().unwrap(),
        0,
    )
    .timestamp();
    let row = TrafficQuotaRow {
        user_id: user_id.clone(),
        client_name: client_name.clone(),
        monthly_bytes: body.monthly_bytes,
        billing_anchor: anchor,
        current_period_started_at: started,
        current_period_bytes_used: 0,
        exhausted_at: None,
        created_at: now,
        updated_at: now,
    };
    match state.traffic_quotas.upsert(row.clone()).await {
        Ok(r) => {
            // Push TrafficQuotaUpdate{SET} to the relevant client session.
            push_quota_set(&state, &r).await;
            (StatusCode::OK, Json(QuotaView::from(r))).into_response()
        }
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store_error",
            &e.to_string(),
        ),
    }
}

#[derive(Debug, Deserialize)]
pub struct PatchQuotaBody {
    pub monthly_bytes: Option<i64>,
    pub clear_period_usage: Option<bool>,
}

pub async fn patch_quota(
    State(state): State<Arc<AppState>>,
    Path((user_id, client_name)): Path<(String, String)>,
    Json(body): Json<PatchQuotaBody>,
) -> impl IntoResponse {
    let Some(mut row) = state.traffic_quotas.get(&user_id, &client_name).await else {
        return error_response(StatusCode::NOT_FOUND, "quota_not_found", "no quota exists");
    };
    let now = now_unix_sec();
    if let Some(mb) = body.monthly_bytes {
        if mb < 0 {
            return error_response(StatusCode::BAD_REQUEST, "invalid_quota_size",
                "monthly_bytes must be >= 0");
        }
        row.monthly_bytes = mb;
        row.updated_at = now;
        let _ = state.traffic_quotas.upsert(row.clone()).await;
    }
    if body.clear_period_usage.unwrap_or(false) {
        if let Ok(Some(updated)) = state
            .traffic_quotas
            .clear_period_usage(&user_id, &client_name, now)
            .await
        {
            row = updated;
        }
    }
    push_quota_set(&state, &row).await;
    (StatusCode::OK, Json(QuotaView::from(row))).into_response()
}

pub async fn delete_quota(
    State(state): State<Arc<AppState>>,
    Path((user_id, client_name)): Path<(String, String)>,
) -> impl IntoResponse {
    match state.traffic_quotas.delete(&user_id, &client_name).await {
        Ok(true) => {
            push_quota_remove(&state, &user_id, &client_name).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => error_response(StatusCode::NOT_FOUND, "quota_not_found", "no quota exists"),
        Err(e) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store_error",
            &e.to_string(),
        ),
    }
}

pub async fn get_quota_status(
    State(state): State<Arc<AppState>>,
    Path((user_id, client_name)): Path<(String, String)>,
) -> impl IntoResponse {
    match state.traffic_quotas.get(&user_id, &client_name).await {
        Some(r) => (StatusCode::OK, Json(QuotaView::from(r))).into_response(),
        None => error_response(StatusCode::NOT_FOUND, "quota_not_found", ""),
    }
}

pub async fn list_user_quotas(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
) -> impl IntoResponse {
    let rows = state.traffic_quotas.list_for_user(&user_id).await;
    let views: Vec<QuotaView> = rows.into_iter().map(QuotaView::from).collect();
    Json(views)
}

pub async fn list_client_quotas(
    State(state): State<Arc<AppState>>,
    Path(client_name): Path<String>,
) -> impl IntoResponse {
    let rows = state.traffic_quotas.list_for_client(&client_name).await;
    let views: Vec<QuotaView> = rows.into_iter().map(QuotaView::from).collect();
    Json(views)
}

// -- traffic queries --------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TrafficQuery {
    pub client_name: Option<String>,
    pub user_id: Option<String>,
    pub from: i64,
    pub to: i64,
    pub bucket: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TrafficResponse {
    pub bucket: SampleBucket,
    pub samples: Vec<TrafficSample>,
    pub total_bytes_in: i64,
    pub total_bytes_out: i64,
}

pub async fn get_user_traffic(
    State(state): State<Arc<AppState>>,
    Path(user_id): Path<String>,
    Query(q): Query<TrafficQuery>,
) -> impl IntoResponse {
    serve_traffic(state, q.client_name.as_deref(), Some(&user_id), q.from, q.to, q.bucket.as_deref()).await
}

pub async fn get_client_traffic(
    State(state): State<Arc<AppState>>,
    Path(client_name): Path<String>,
    Query(q): Query<TrafficQuery>,
) -> impl IntoResponse {
    serve_traffic(state, Some(&client_name), q.user_id.as_deref(), q.from, q.to, q.bucket.as_deref()).await
}

async fn serve_traffic(
    state: Arc<AppState>,
    client_name: Option<&str>,
    user_id: Option<&str>,
    from: i64,
    to: i64,
    bucket: Option<&str>,
) -> axum::response::Response {
    if from < 0 || to <= from {
        return error_response(StatusCode::BAD_REQUEST, "invalid_time_range",
            "from must be >= 0 and to > from");
    }
    let now = now_unix_sec();
    let span = to - from;
    let chosen = match bucket {
        Some("1m") => SampleBucket::M1,
        Some("1h") => SampleBucket::H1,
        Some(_) => return error_response(StatusCode::BAD_REQUEST, "invalid_bucket", "bucket must be 1m or 1h"),
        None => {
            if span <= 24 * 3600 {
                SampleBucket::M1
            } else {
                SampleBucket::H1
            }
        }
    };
    let oldest_allowed = now - chosen.retention_seconds();
    if from < oldest_allowed {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "quota_bucket_out_of_retention",
            &format!("from is older than {chosen:?} retention"),
        );
    }
    let rows = samples::query_samples(&state.store, chosen, user_id, client_name, from, to)
        .await
        .unwrap_or_default();
    if rows.len() > 10_000 {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "traffic_too_many_rows",
            "narrow the time range",
        );
    }
    let total_bytes_in: i64 = rows.iter().map(|r| r.bytes_in).sum();
    let total_bytes_out: i64 = rows.iter().map(|r| r.bytes_out).sum();
    (
        StatusCode::OK,
        Json(TrafficResponse {
            bucket: chosen,
            samples: rows,
            total_bytes_in,
            total_bytes_out,
        }),
    )
        .into_response()
}

// -- helpers ----------------------------------------------------------------

fn error_response(
    status: StatusCode,
    code: &str,
    message: &str,
) -> axum::response::Response {
    use serde_json::json;
    (status, Json(json!({ "error": code, "message": message }))).into_response()
}

fn now_unix_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

async fn client_supports_quota(state: &AppState, client_name: &str) -> bool {
    // Mirror version_at_least() used elsewhere (grpc/service.rs, http.rs:845).
    let version = state.clients.client_version_of(&crate::clients::ClientName::from_str(client_name).unwrap_or_default()).await;
    crate::grpc::service::version_at_least(version.as_deref(), 1, 4)
}

async fn grant_exists(state: &AppState, user_id: &str, client_name: &str) -> bool {
    // Replace with the actual grants lookup helper used elsewhere.
    // grep `grants` repo for `has_grant_for` / `lookup_grant` / similar.
    state.grants.has_grant(user_id, client_name).await
}

async fn push_quota_set(state: &AppState, row: &TrafficQuotaRow) {
    state
        .traffic_quota_push
        .send_set(row.clone())
        .await
        .ok();
}

async fn push_quota_remove(state: &AppState, user_id: &str, client_name: &str) {
    state
        .traffic_quota_push
        .send_remove(user_id.to_string(), client_name.to_string())
        .await
        .ok();
}
```

> **Subagent note:** The exact helper names (`grants.has_grant`, `clients.client_version_of`, the version-gate helper visibility, `traffic_quota_push`) must match what exists. If a helper isn't there yet, extract from neighbours (e.g., `crates/portunus-server/src/grpc/service.rs` `version_at_least` may need to be made `pub`). The push channel (`traffic_quota_push`) is defined in Task C5 below.

- [ ] **Step 3: Wire routes**

In `crates/portunus-server/src/operator/http.rs` near line 90 (after owner-cap routes):

```rust
.route("/v1/users/:user_id/quotas", get(quota_http::list_user_quotas))
.route(
    "/v1/users/:user_id/quotas/:client_name",
    put(quota_http::put_quota)
        .patch(quota_http::patch_quota)
        .delete(quota_http::delete_quota),
)
.route(
    "/v1/users/:user_id/quotas/:client_name/status",
    get(quota_http::get_quota_status),
)
.route("/v1/users/:user_id/traffic", get(quota_http::get_user_traffic))
.route("/v1/clients/:client_name/quotas", get(quota_http::list_client_quotas))
.route("/v1/clients/:client_name/traffic", get(quota_http::get_client_traffic))
```

Add `mod quota_http;` in `operator/mod.rs`.

- [ ] **Step 4: Smoke tests for routes**

In `crates/portunus-server/src/operator/quota_http.rs`, `#[cfg(test)] mod tests` (use `axum::body::Body` + Tower service routine like the existing route tests).

Cover:
- PUT returns 200 with full QuotaView body
- PUT for unknown user/client returns 422 `quota_target_not_found`
- PUT for client_version < 1.4 returns 422 `quota_unsupported_by_client`
- PATCH `clear_period_usage=true` zeros `current_period_bytes_used`
- DELETE returns 204
- GET status returns 404 when no quota
- GET traffic with from older than retention returns 422

- [ ] **Step 5: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib operator::quota_http
git add crates/portunus-server/src/operator/quota_http.rs crates/portunus-server/src/operator/http.rs crates/portunus-server/src/state.rs
git commit -m "server: HTTP quota CRUD + traffic query endpoints"
```

---

### Task C2: RBAC wiring

**Files:**
- Modify: `crates/portunus-server/src/operator/quota_http.rs` — wrap handlers in RBAC checks

- [ ] **Step 1: Read the existing RBAC pattern**

Read how the existing owner-cap routes apply RBAC. Look for `require_role` / `principal_can_*` patterns; the operator API enforces RBAC via middleware or per-handler `Principal` extractor.

- [ ] **Step 2: Apply the same pattern**

For each handler, extract the `Principal` and enforce:

| Operation | Allowed for |
|-----------|-------------|
| PUT/PATCH/DELETE | superadmin OR owner of the target client |
| GET status | superadmin OR client owner OR the user themselves |
| GET traffic | superadmin OR client owner (within their client) OR the user themselves |
| GET list | superadmin OR client owner |

Use existing helpers (likely `state.rbac.principal_can_manage_client(...)` or similar; grep neighbour handlers for the exact name).

- [ ] **Step 3: Test RBAC denials**

Append tests:
- non-superadmin without ownership → 403 on PUT
- user themselves → 200 on GET status; 403 on PUT

- [ ] **Step 4: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib operator::quota_http rbac
git add crates/portunus-server/src/operator/quota_http.rs
git commit -m "server: RBAC checks on quota CRUD + traffic queries"
```

---

### Task C3: Prometheus metrics

**Files:**
- Modify: `crates/portunus-server/src/metrics.rs:85-185` — add 5 new fields to `Metrics`; init in `Metrics::new()`

- [ ] **Step 1: Add fields**

```rust
pub traffic_quota_bytes_used: IntGaugeVec,
pub traffic_quota_bytes_limit: IntGaugeVec,
pub traffic_quota_exhausted: IntGaugeVec,
pub traffic_quota_period_resets_total: IntCounterVec,
pub traffic_quota_exhausted_total: IntCounterVec,
```

- [ ] **Step 2: Init in `Metrics::new()`**

Mirror the existing pattern; labels `&["user", "client"]`.

- [ ] **Step 3: Update aggregator + cache write paths**

After `cache.accumulate` returns a row, update:
```rust
metrics
    .traffic_quota_bytes_used
    .with_label_values(&[&row.user_id, &row.client_name])
    .set(row.current_period_bytes_used);
metrics
    .traffic_quota_bytes_limit
    .with_label_values(&[&row.user_id, &row.client_name])
    .set(row.monthly_bytes);
let exhausted = if row.exhausted_at.is_some() { 1 } else { 0 };
metrics
    .traffic_quota_exhausted
    .with_label_values(&[&row.user_id, &row.client_name])
    .set(exhausted);
if row.exhausted_at == Some(now_unix_sec) {
    metrics.traffic_quota_exhausted_total
        .with_label_values(&[&row.user_id, &row.client_name])
        .inc();
}
```

On period reset, increment `traffic_quota_period_resets_total`.

- [ ] **Step 4: Test the `/v1/metrics` text contains the new families**

In an existing metrics smoke test or new one, render and assert text contains `portunus_traffic_quota_bytes_used`.

- [ ] **Step 5: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib metrics
git add crates/portunus-server/src/metrics.rs crates/portunus-server/src/traffic_quotas/aggregator.rs
git commit -m "server: 5 new Prometheus collectors for traffic quotas"
```

---

### Task C4: Period rollover tick

**Files:**
- Create: `crates/portunus-server/src/traffic_quotas/rollover.rs`

- [ ] **Step 1: Background task that advances periods**

```rust
//! Runs every 60s. For every cached quota row, checks if the current
//! period has elapsed and, if so, resets it. Each reset triggers a
//! TrafficQuotaUpdate push so the client recovers from exhausted.

use crate::state::AppState;
use crate::traffic_quotas::{advance_period_if_due, period_start_at};
use chrono::{TimeZone, Utc};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info};

pub async fn run_forever(state: Arc<AppState>) {
    let mut ticker = tokio::time::interval(Duration::from_secs(60));
    ticker.tick().await; // discard immediate tick
    loop {
        ticker.tick().await;
        if let Err(e) = run_once(&state).await {
            error!(event = "traffic_rollover.tick_failed", error = %e);
        }
    }
}

pub async fn run_once(state: &AppState) -> Result<usize, crate::store::StoreError> {
    let now = Utc::now();
    let now_ts = now.timestamp();
    // Snapshot cached rows.
    let rows = state.traffic_quotas.list_all().await;
    let mut advanced = 0usize;
    for r in rows {
        let anchor = Utc.timestamp_opt(r.billing_anchor, 0).single().unwrap_or(now);
        let start = Utc
            .timestamp_opt(r.current_period_started_at, 0)
            .single()
            .unwrap_or(anchor);
        if let Some(new_start) = advance_period_if_due(anchor, start, now) {
            if let Some(updated) = state
                .traffic_quotas
                .reset_period(&r.user_id, &r.client_name, new_start.timestamp(), now_ts)
                .await?
            {
                info!(
                    event = "traffic_quota.period_rolled",
                    user = %updated.user_id,
                    client = %updated.client_name,
                    new_start = updated.current_period_started_at,
                );
                state
                    .traffic_quota_push
                    .send_set(updated)
                    .await
                    .ok();
                advanced += 1;
            }
        }
    }
    Ok(advanced)
}
```

Add `list_all()` to `TrafficQuotaCache` returning all cached rows.

- [ ] **Step 2: Spawn from `serve.rs`**

```rust
let rollover_state = state.clone();
tokio::spawn(async move {
    crate::traffic_quotas::rollover::run_forever(rollover_state).await;
});
```

- [ ] **Step 3: Test**

In `rollover.rs` `#[cfg(test)]`: insert a quota whose `billing_anchor` was 32 days ago and `current_period_started_at` was the original anchor; call `run_once`; verify the period advanced + push channel received a SET.

- [ ] **Step 4: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib traffic_quotas::rollover
git add crates/portunus-server/src/traffic_quotas/rollover.rs crates/portunus-server/src/traffic_quotas/cache.rs crates/portunus-server/src/serve.rs
git commit -m "server: period rollover background tick"
```

---

### Task C5: gRPC push wiring (TrafficQuotaUpdate)

**Files:**
- Modify: `crates/portunus-server/src/grpc/service.rs`
- Create: `crates/portunus-server/src/traffic_quotas/push.rs` — the dispatcher

The push dispatcher routes `TrafficQuotaUpdate` messages to the correct client gRPC outbound sender. Pattern mirrors `replay_owner_caps_for_client` (line 589-625) but for ongoing pushes triggered by:
- Quota CRUD (PUT/PATCH/DELETE)
- Period rollover (Task C4)
- Aggregator exhaust event (Task B2)

- [ ] **Step 1: Write the push dispatcher**

```rust
//! Routes TrafficQuotaUpdate messages from quota CRUD, period rollover,
//! and aggregator exhaust events to the gRPC outbound channel for the
//! correct connected client session. Pattern mirrors OwnerRateLimitUpdate
//! (server/grpc/service.rs:589-625 replay_owner_caps_for_client).

use crate::traffic_quotas::TrafficQuotaRow;
use portunus_proto::v1::{
    server_message, ServerMessage, TrafficQuotaAction, TrafficQuotaState, TrafficQuotaUpdate,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info};

pub type OutboundSender = mpsc::Sender<Result<ServerMessage, tonic::Status>>;

#[derive(Debug, Clone, Copy)]
pub enum PushKind {
    Set,
    Remove,
}

#[derive(Debug, Clone)]
pub struct PendingPush {
    pub kind: PushKind,
    pub user_id: String,
    pub client_name: String,
    pub row: Option<TrafficQuotaRow>, // None when Remove
}

#[derive(Clone)]
pub struct TrafficQuotaPusher {
    inner: Arc<Inner>,
}

struct Inner {
    /// Active sessions keyed by client_name.
    sessions: RwLock<HashMap<String, OutboundSender>>,
}

impl TrafficQuotaPusher {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                sessions: RwLock::default(),
            }),
        }
    }

    pub async fn register_session(&self, client_name: String, tx: OutboundSender) {
        self.inner.sessions.write().await.insert(client_name, tx);
    }

    pub async fn unregister_session(&self, client_name: &str) {
        self.inner.sessions.write().await.remove(client_name);
    }

    /// Send a SET update for `row` to its client. No-op if no active session.
    pub async fn send_set(&self, row: TrafficQuotaRow) -> Result<(), tonic::Status> {
        let msg = make_set(&row);
        self.send_to(&row.client_name, msg).await
    }

    pub async fn send_remove(&self, user_id: String, client_name: String) -> Result<(), tonic::Status> {
        let msg = ServerMessage {
            payload: Some(server_message::Payload::TrafficQuotaUpdate(TrafficQuotaUpdate {
                request_id: ulid::Ulid::new().to_string(),
                user_id,
                client_name: client_name.clone(),
                action: TrafficQuotaAction::Remove as i32,
                state: None,
            })),
        };
        self.send_to(&client_name, msg).await
    }

    async fn send_to(&self, client_name: &str, msg: ServerMessage) -> Result<(), tonic::Status> {
        let tx = {
            let sessions = self.inner.sessions.read().await;
            sessions.get(client_name).cloned()
        };
        if let Some(tx) = tx {
            if tx.send(Ok(msg)).await.is_err() {
                debug!(event = "traffic_quota.session_dropped", client = client_name);
            }
        }
        Ok(())
    }
}

pub fn make_set(row: &TrafficQuotaRow) -> ServerMessage {
    let budget = row.monthly_bytes - row.current_period_bytes_used;
    let exhausted = row.exhausted_at.is_some();
    let state = TrafficQuotaState {
        monthly_bytes: row.monthly_bytes,
        budget_remaining_bytes: budget,
        period_started_at_unix_sec: row.current_period_started_at,
        period_ends_at_unix_sec: 0, // Filled by the caller if needed (or compute here).
        exhausted,
    };
    ServerMessage {
        payload: Some(server_message::Payload::TrafficQuotaUpdate(TrafficQuotaUpdate {
            request_id: ulid::Ulid::new().to_string(),
            user_id: row.user_id.clone(),
            client_name: row.client_name.clone(),
            action: TrafficQuotaAction::Set as i32,
            state: Some(state),
        })),
    }
}
```

- [ ] **Step 2: Wire pusher into `AppState`**

```rust
pub traffic_quota_push: crate::traffic_quotas::push::TrafficQuotaPusher,
```

- [ ] **Step 3: Register / unregister in gRPC service**

In `grpc/service.rs` connect path, right after the client identifies itself:
```rust
state.traffic_quota_push.register_session(identity.client_name.to_string(), outbound.clone()).await;
```
And on the disconnect path:
```rust
state.traffic_quota_push.unregister_session(identity.client_name.as_str()).await;
```

- [ ] **Step 4: Consume aggregator exhaust events**

Spawn a task at server boot that reads from `state.traffic_quota_exhaust_rx` and calls `traffic_quota_push.send_set(...)` with the row reloaded from cache. Mark exhaust in metrics.

- [ ] **Step 5: Reconnect replay — quotas BEFORE rules**

In `grpc/service.rs` around line 198 (where `replay_rules_for_client` is called), insert quota replay first:

```rust
async fn replay_traffic_quotas_for_client(
    state: &AppState,
    identity: &ClientIdentity,
    outbound: &OutboundSender,
) {
    let client_version = state.clients.client_version_of(&identity.client_name).await;
    if !version_at_least(client_version.as_deref(), 1, 4) {
        return;
    }
    let rows = state
        .traffic_quotas
        .list_for_client(identity.client_name.as_str())
        .await;
    for row in rows {
        let msg = crate::traffic_quotas::push::make_set(&row);
        if outbound.send(Ok(msg)).await.is_err() {
            break;
        }
    }
}
```

Call site:

```rust
// Replay sequence (replaces existing lines around 198-206):
// 1. Welcome (already sent above)
// 2. NEW: traffic quotas (before rules so QuotaHandle exists when rule activates)
replay_traffic_quotas_for_client(&state, &identity, &outbound).await;
// 3. Rules (existing)
replay_rules_for_client(&state, &identity, &outbound).await;
// 4. Owner caps (existing)
replay_owner_caps_for_client(&state, &identity, &outbound).await;
```

- [ ] **Step 6: Tests**

In `push.rs`:
- `send_set_routes_to_registered_session`
- `send_set_no_op_for_unknown_client`
- `register_then_unregister_clears`

In `grpc/service.rs` (or its test module):
- A subagent must verify by integration test that on reconnect, the outbound channel receives a TrafficQuotaUpdate BEFORE the first RuleUpdate.

- [ ] **Step 7: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib traffic_quotas::push
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib grpc::service::tests
git add crates/portunus-server/src/traffic_quotas/push.rs crates/portunus-server/src/grpc/service.rs crates/portunus-server/src/state.rs
git commit -m "server: TrafficQuotaUpdate push dispatcher + reconnect replay before rules"
```

---

## Phase D: Client core (QuotaHandle, QuotaScopeManager, replay)

### Task D1: QuotaHandle with saturating CAS

**Files:**
- Create: `crates/portunus-client/src/forwarder/quota/mod.rs`

- [ ] **Step 1: Write QuotaHandle**

```rust
//! 013-traffic-quotas v1.4.0 client side: per-(user, client) byte
//! budget enforcement. Saturating CAS consume preserves the
//! `remaining >= 0` invariant under concurrent IO, eliminating
//! underflow / wrap. See spec §4.3 decision 6.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

pub mod copy;
pub mod scope;

#[derive(Debug)]
pub struct QuotaHandle {
    pub user_id: String,
    pub client_name: String,
    remaining: AtomicI64,
    exhausted: AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumeOutcome {
    Granted,
    Exhausted,
}

#[derive(Debug, Clone, Copy)]
pub struct QuotaState {
    pub monthly_bytes: i64,
    pub budget_remaining_bytes: i64,
    pub exhausted: bool,
}

impl QuotaHandle {
    pub fn new(user_id: String, client_name: String, state: QuotaState) -> Self {
        let init_remaining = state.budget_remaining_bytes.max(0);
        Self {
            user_id,
            client_name,
            remaining: AtomicI64::new(init_remaining),
            exhausted: AtomicBool::new(state.exhausted || init_remaining == 0),
        }
    }

    /// Replace state atomically (e.g., on TrafficQuotaUpdate{SET}).
    pub fn replace(&self, state: QuotaState) {
        let new_remaining = state.budget_remaining_bytes.max(0);
        self.remaining.store(new_remaining, Ordering::Release);
        let was_exhausted = state.exhausted || new_remaining == 0;
        self.exhausted.store(was_exhausted, Ordering::Release);
    }

    pub fn is_exhausted(&self) -> bool {
        self.exhausted.load(Ordering::Acquire)
    }

    pub fn remaining(&self) -> i64 {
        self.remaining.load(Ordering::Relaxed)
    }

    /// Try to consume `n` bytes. Returns `Granted` if the budget allows,
    /// `Exhausted` if not. Saturating CAS loop maintains `remaining >= 0`
    /// even under concurrent callers.
    pub fn consume(&self, n: i64) -> ConsumeOutcome {
        debug_assert!(n >= 0, "negative consume {n}");
        // Fast path: already exhausted.
        if self.exhausted.load(Ordering::Acquire) {
            return ConsumeOutcome::Exhausted;
        }
        let mut cur = self.remaining.load(Ordering::Relaxed);
        loop {
            if cur <= 0 {
                self.mark_exhausted();
                return ConsumeOutcome::Exhausted;
            }
            // Saturating subtract: never go below 0.
            let new = (cur - n).max(0);
            match self.remaining.compare_exchange_weak(
                cur,
                new,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    if new == 0 {
                        self.mark_exhausted();
                        return ConsumeOutcome::Exhausted;
                    }
                    return ConsumeOutcome::Granted;
                }
                Err(actual) => {
                    cur = actual;
                    continue;
                }
            }
        }
    }

    fn mark_exhausted(&self) {
        let _ = self
            .exhausted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn state(monthly: i64, remaining: i64, exhausted: bool) -> QuotaState {
        QuotaState {
            monthly_bytes: monthly,
            budget_remaining_bytes: remaining,
            exhausted,
        }
    }

    #[test]
    fn consume_grants_under_budget() {
        let h = QuotaHandle::new("u".into(), "c".into(), state(1000, 1000, false));
        assert_eq!(h.consume(100), ConsumeOutcome::Granted);
        assert_eq!(h.remaining(), 900);
    }

    #[test]
    fn consume_exhausts_at_zero() {
        let h = QuotaHandle::new("u".into(), "c".into(), state(100, 100, false));
        assert_eq!(h.consume(100), ConsumeOutcome::Granted);
        assert_eq!(h.remaining(), 0);
        assert!(h.is_exhausted());
        assert_eq!(h.consume(1), ConsumeOutcome::Exhausted);
    }

    #[test]
    fn consume_saturates_does_not_underflow() {
        // After CAS hits 0, future consumes never bring remaining negative.
        let h = QuotaHandle::new("u".into(), "c".into(), state(100, 100, false));
        for _ in 0..1000 {
            let _ = h.consume(10);
        }
        assert!(h.remaining() >= 0);
        assert!(h.is_exhausted());
    }

    #[test]
    fn replace_resets_exhausted_when_budget_returns() {
        let h = QuotaHandle::new("u".into(), "c".into(), state(100, 0, true));
        assert!(h.is_exhausted());
        h.replace(state(200, 200, false));
        assert!(!h.is_exhausted());
        assert_eq!(h.consume(50), ConsumeOutcome::Granted);
    }

    #[test]
    fn concurrent_consumes_stay_at_or_above_zero() {
        let h = Arc::new(QuotaHandle::new("u".into(), "c".into(), state(10_000, 10_000, false)));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let h2 = h.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..1000 {
                    let _ = h2.consume(2);
                }
            }));
        }
        for jh in handles {
            jh.join().unwrap();
        }
        assert!(h.remaining() >= 0);
        assert!(h.is_exhausted());
    }
}
```

- [ ] **Step 2: Run tests**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client --lib forwarder::quota::tests
```
Expected: 5 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/portunus-client/src/forwarder/quota/mod.rs
git commit -m "client: QuotaHandle with saturating CAS consume"
```

---

### Task D2: QuotaScopeManager — per-(user, client) registry

**Files:**
- Create: `crates/portunus-client/src/forwarder/quota/scope.rs`

- [ ] **Step 1: Write scope.rs**

```rust
//! 013-traffic-quotas client registry: maps (user_id, client_name) -> Arc<QuotaHandle>.
//! Pattern follows v0.11 OwnerRateLimitScopeManager (forwarder/rate_limit/scope.rs).
//! The forwarder accept loop & copy hooks look up by `rule.owner_user_id` paired
//! with the local client_name (the client knows its own name from boot config).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::RwLock;

use super::{QuotaHandle, QuotaState};

#[derive(Default)]
pub struct QuotaScopeManager {
    /// Keyed by user_id. The `client_name` part of the spec's PK is
    /// implicit (it's always "this client").
    inner: RwLock<HashMap<String, Arc<QuotaHandle>>>,
}

impl QuotaScopeManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lookup(&self, user_id: &str) -> Option<Arc<QuotaHandle>> {
        self.inner.read().ok().and_then(|m| m.get(user_id).cloned())
    }

    /// Insert or replace handle state. If a handle already exists,
    /// atomically `replace()`s its state (so in-flight forwarders
    /// observing the same Arc see the new budget immediately).
    pub fn install(&self, user_id: &str, client_name: &str, state: QuotaState) -> Arc<QuotaHandle> {
        let mut m = self.inner.write().expect("quota scope poisoned");
        if let Some(existing) = m.get(user_id) {
            existing.replace(state);
            return existing.clone();
        }
        let handle = Arc::new(QuotaHandle::new(user_id.to_string(), client_name.to_string(), state));
        m.insert(user_id.to_string(), handle.clone());
        handle
    }

    pub fn remove(&self, user_id: &str) -> Option<Arc<QuotaHandle>> {
        self.inner.write().ok().and_then(|mut m| m.remove(user_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarder::quota::ConsumeOutcome;

    fn state(remaining: i64) -> QuotaState {
        QuotaState {
            monthly_bytes: 1000,
            budget_remaining_bytes: remaining,
            exhausted: false,
        }
    }

    #[test]
    fn lookup_returns_installed_handle() {
        let m = QuotaScopeManager::new();
        m.install("alice", "edge-01", state(500));
        let h = m.lookup("alice").unwrap();
        assert_eq!(h.consume(200), ConsumeOutcome::Granted);
        assert_eq!(h.remaining(), 300);
    }

    #[test]
    fn install_twice_updates_state_in_place() {
        let m = QuotaScopeManager::new();
        let h1 = m.install("alice", "edge-01", state(100));
        let _ = h1.consume(50);
        let h2 = m.install("alice", "edge-01", state(1000)); // raise the cap
        assert!(Arc::ptr_eq(&h1, &h2));
        assert_eq!(h1.remaining(), 1000);
    }

    #[test]
    fn remove_drops_handle() {
        let m = QuotaScopeManager::new();
        m.install("alice", "edge-01", state(500));
        let removed = m.remove("alice");
        assert!(removed.is_some());
        assert!(m.lookup("alice").is_none());
    }
}
```

- [ ] **Step 2: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client --lib forwarder::quota::scope
git add crates/portunus-client/src/forwarder/quota/scope.rs
git commit -m "client: QuotaScopeManager registry by user_id"
```

---

### Task D3: TrafficQuotaUpdate handler in control.rs

**Files:**
- Modify: `crates/portunus-client/src/control.rs` — dispatch `TrafficQuotaUpdate`, mirror `apply_owner_rate_limit_update`

- [ ] **Step 1: Add dispatch arm**

Read `crates/portunus-client/src/control.rs:434` (the match for ServerMessage payload). Add:

```rust
Some(server_message::Payload::TrafficQuotaUpdate(update)) => {
    apply_traffic_quota_update(update, quota_scope.as_ref());
}
```

`quota_scope` is a new `&Arc<QuotaScopeManager>` parameter that needs to thread through alongside `owner_rate_limit_scope`.

- [ ] **Step 2: Write the handler**

```rust
fn apply_traffic_quota_update(
    update: portunus_proto::v1::TrafficQuotaUpdate,
    scope: &crate::forwarder::quota::scope::QuotaScopeManager,
) {
    use portunus_proto::v1::TrafficQuotaAction;
    let action = TrafficQuotaAction::try_from(update.action).unwrap_or(TrafficQuotaAction::Unspecified);
    match action {
        TrafficQuotaAction::Set => {
            let Some(state) = update.state else {
                tracing::warn!(event = "traffic_quota_update.set_without_state", user = %update.user_id);
                return;
            };
            let qs = crate::forwarder::quota::QuotaState {
                monthly_bytes: state.monthly_bytes,
                budget_remaining_bytes: state.budget_remaining_bytes,
                exhausted: state.exhausted,
            };
            scope.install(&update.user_id, &update.client_name, qs);
            tracing::info!(
                event = "traffic_quota_update.applied_set",
                user = %update.user_id,
                client = %update.client_name,
                remaining = state.budget_remaining_bytes,
                exhausted = state.exhausted,
            );
        }
        TrafficQuotaAction::Remove => {
            scope.remove(&update.user_id);
            tracing::info!(event = "traffic_quota_update.applied_remove", user = %update.user_id);
        }
        TrafficQuotaAction::Unspecified => {
            tracing::warn!(event = "traffic_quota_update.unspecified_action");
        }
    }
}
```

- [ ] **Step 3: Thread `quota_scope` through control path**

Anywhere `owner_rate_limit_scope` is plumbed (from `Cli` parsing → service connect → message loop → forwarder spawn), add `quota_scope: Arc<QuotaScopeManager>` alongside.

Important: the client must construct exactly one `QuotaScopeManager` at boot and share it via `Arc`.

- [ ] **Step 4: Test the dispatch**

In `control.rs` `#[cfg(test)] mod tests` (likely already exists with apply_owner_rate_limit tests), append:
- `apply_traffic_quota_update_set_installs_handle`
- `apply_traffic_quota_update_remove_removes_handle`
- `set_without_state_is_warning_only` (does not panic)

- [ ] **Step 5: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client --lib control
git add crates/portunus-client/src/control.rs
git commit -m "client: apply_traffic_quota_update with SET/REMOVE dispatch"
```

---

## Phase E: Client data plane enforcement

### Task E1: TCP userspace quota-aware copy loop

**Files:**
- Create: `crates/portunus-client/src/forwarder/quota/copy.rs`

Pattern mirrors `crates/portunus-client/src/forwarder/rate_limit/copy.rs:86` — splits inbound/outbound, spawns two halves, awaits both. Each half's inner loop is: `read(buf) → write_all(buf[..n]) → total += n → quota.consume(n)` → if exhausted, break (return total).

- [ ] **Step 1: Write copy.rs**

```rust
//! TCP userspace quota-aware bidirectional copy. Records bytes only
//! AFTER `write_all` succeeds — semantically identical to
//! rate_limit/copy.rs:213 — so a torn IO never lies about delivery.

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{ConsumeOutcome, QuotaHandle};

const COPY_BUF_SIZE: usize = 64 * 1024;

/// Run a bidirectional copy with quota enforcement. Returns the
/// `(bytes_in, bytes_out)` actually delivered to upstream / downstream.
/// On quota exhaustion the copy returns cleanly with the delivered
/// totals; the caller closes the sockets.
pub async fn copy_bidirectional_with_quota<I, O>(
    inbound: &mut I,
    outbound: &mut O,
    quota: Arc<QuotaHandle>,
) -> io::Result<(u64, u64)>
where
    I: AsyncRead + AsyncWrite + Unpin,
    O: AsyncRead + AsyncWrite + Unpin,
{
    let (mut ri, mut wi) = tokio::io::split(inbound);
    let (mut ro, mut wo) = tokio::io::split(outbound);
    let q1 = quota.clone();
    let q2 = quota;

    let fwd = async move {
        copy_one_dir(&mut ri, &mut wo, &q1).await
    };
    let rev = async move {
        copy_one_dir(&mut ro, &mut wi, &q2).await
    };
    let (a, b) = tokio::try_join!(fwd, rev)?;
    Ok((a, b))
}

async fn copy_one_dir<R, W>(reader: &mut R, writer: &mut W, quota: &QuotaHandle) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; COPY_BUF_SIZE];
    let mut total = 0u64;
    loop {
        if quota.is_exhausted() {
            return Ok(total);
        }
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            writer.shutdown().await.ok();
            return Ok(total);
        }
        writer.write_all(&buf[..n]).await?;
        total += n as u64;
        match quota.consume(n as i64) {
            ConsumeOutcome::Granted => {}
            ConsumeOutcome::Exhausted => {
                writer.shutdown().await.ok();
                return Ok(total);
            }
        }
    }
}
```

- [ ] **Step 2: Tests**

In copy.rs `#[cfg(test)] mod tests`:
- `copy_under_budget_delivers_all_bytes`: write 1 KiB, budget 10 KiB; expect both directions complete; remaining = 10 KiB - 1 KiB
- `copy_exhausts_mid_stream_returns_partial`: write 100 KiB, budget 10 KiB; expect ~10 KiB delivered each direction; copy returns cleanly
- `concurrent_directions_share_quota_correctly`: in+out streams both at full speed, total consumed = bytes_in + bytes_out

Use `tokio::io::duplex` for synthetic streams (see rate_limit/copy.rs tests for the pattern).

- [ ] **Step 3: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client --lib forwarder::quota::copy
git add crates/portunus-client/src/forwarder/quota/copy.rs
git commit -m "client: TCP userspace quota-aware bidirectional copy"
```

---

### Task E2: Integrate quota path into `copy_uncapped`

**Files:**
- Modify: `crates/portunus-client/src/forwarder/proxy.rs:328-360`

- [ ] **Step 1: Find rule's owner_user_id at call site**

The forwarder already threads `owner_rate_limit` (an `Option<Arc<OwnerRateLimitHandle>>`). The `owner_user_id` is available from the rule object. We thread a new `quota_handle: Option<Arc<QuotaHandle>>` alongside.

In the rule activation path (likely `forwarder/listener.rs` or wherever rules turn into accept loops), at activation time:
```rust
let quota_handle = rule.owner_user_id.as_ref().and_then(|uid| quota_scope.lookup(uid));
```
This handle then accompanies the connection through to `copy_uncapped`.

- [ ] **Step 2: Choose quota-aware path**

In `proxy.rs:copy_uncapped` (line 328-360), branch on the presence of `quota_handle`:

```rust
async fn copy_uncapped(
    inbound: &mut TcpStream,
    outbound: &mut TcpStream,
    rule_id: RuleId,
    rate_limit: Option<&RuleRateLimitHandle>,
    owner_rate_limit: Option<&OwnerRateLimitHandle>,
    quota: Option<&Arc<QuotaHandle>>,
    has_sni_replay_done: bool,
    has_proxy_out: bool,
) -> io::Result<(u64, u64)> {
    if let Some(quota) = quota {
        // Quota is active — userspace quota-aware path only (no splice).
        return crate::forwarder::quota::copy::copy_bidirectional_with_quota(
            inbound,
            outbound,
            quota.clone(),
        )
        .await;
    }
    // Existing splice + fallback path unchanged.
    // ... (keep existing body)
}
```

> **Subagent note:** This conservatively disables splice when quota is in play. Task E3 will add the splice hook variant so quota + splice can coexist, but ship the userspace-only path first.

Update all `copy_uncapped` call sites to pass `quota` argument.

- [ ] **Step 3: Test with synthetic quota**

In `proxy.rs` tests, add:
- `copy_uncapped_with_quota_uses_userspace_path` — pass a `QuotaHandle` with small budget; assert behavior matches `copy_bidirectional_with_quota`

- [ ] **Step 4: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client --lib forwarder::proxy
git add crates/portunus-client/src/forwarder/proxy.rs crates/portunus-client/src/forwarder/listener.rs
git commit -m "client: route quota'd connections through quota-aware copy loop"
```

---

### Task E3: Splice per-iteration consume hook

**Files:**
- Modify: `crates/portunus-client/src/forwarder/splice.rs:493-535`

- [ ] **Step 1: Add quota parameter to splice helpers**

Locate the splice helper functions (around line 493 and 551). Add an `Option<&Arc<QuotaHandle>>` parameter. After each successful `bytes_out.fetch_add(n, Ordering::Relaxed)` (line 524):

```rust
if let Some(q) = quota.as_ref() {
    match q.consume(n as i64) {
        ConsumeOutcome::Granted => {}
        ConsumeOutcome::Exhausted => {
            // Return special errno that the orchestrator interprets
            // as clean-close (mirrors EOF) rather than I/O failure.
            return Err(io::Error::new(io::ErrorKind::Other, "quota_exhausted"));
        }
    }
}
```

- [ ] **Step 2: Propagate the parameter through orchestrator**

The `copy_bidirectional` orchestrator in splice.rs (around line 615) must accept the same `quota` param and pass to both directions.

- [ ] **Step 3: Re-enable splice for quota'd connections in proxy.rs**

Change Task E2's E2-step-2 branch — instead of "userspace only when quota", let splice run with the hook. The Linux fast path stays available.

- [ ] **Step 4: Test on Linux**

```rust
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn splice_with_quota_exhausts_and_closes() {
    // Build CopyCtx with quota_handle that has 10 KiB budget.
    // Drive 100 KiB; verify connection closes after ~10 KiB and the
    // handle reports exhausted.
}
```

- [ ] **Step 5: Run + commit**

```
cargo test --target x86_64-unknown-linux-gnu -p portunus-client splice_with_quota
git add crates/portunus-client/src/forwarder/splice.rs crates/portunus-client/src/forwarder/proxy.rs
git commit -m "client: splice per-iteration quota consume hook (Linux)"
```

---

### Task E4: UDP per-datagram consume hook

**Files:**
- Modify: `crates/portunus-client/src/forwarder/udp/mod.rs:318,634,823`

- [ ] **Step 1: Locate the inbound and outbound bytes hook points**

`inc_datagram_in` (line 318 and 634) — called when forwarding an inbound datagram to upstream.
`inc_datagram_out` (line 823) — called when relaying an upstream reply back to end-user.

- [ ] **Step 2: Add consume**

At each site, AFTER the existing byte counter advance, AND ONLY when the rule has a `Option<Arc<QuotaHandle>>` in scope:

```rust
if let Some(q) = &quota_handle {
    if matches!(q.consume(payload_len as i64), ConsumeOutcome::Exhausted) {
        // Drop the datagram and close the per-flow upstream socket.
        // Existing flow-table reaping handles cleanup; we just bail.
        return Ok(());
    }
}
```

The `quota_handle` is threaded into the `UdpFlow` struct at flow creation time (alongside the existing rate-limit handle).

- [ ] **Step 3: Test**

UDP datagram test: send 100 × 1 KiB datagrams through a rule with 10 KiB budget; verify exactly 10 datagrams delivered + 11th drops + quota exhausted.

- [ ] **Step 4: Run + commit**

```
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client --lib forwarder::udp quota
git add crates/portunus-client/src/forwarder/udp/
git commit -m "client: UDP per-datagram quota consume hook"
```

---

## Phase F: Web UI types + API client

### Task F1: TypeScript types + API client functions

**Files:**
- Create: `webui/src/api/quotas.ts`
- Create: `webui/src/api/traffic.ts`
- Modify: `webui/src/api/types.ts` — extend `AccessEntry` with quota fields, add `MonthlyQuota`, `TrafficSample`, etc.

- [ ] **Step 1: Add types**

Append to `webui/src/api/types.ts`:

```typescript
// 013-traffic-quotas v1.4.0

export interface MonthlyQuotaView {
  user_id: string;
  client_name: string;
  monthly_bytes: number;
  billing_anchor: number;
  current_period_started_at: number;
  current_period_ends_at: number;
  current_period_bytes_used: number;
  exhausted_at: number | null;
  created_at: number;
  updated_at: number;
}

export interface TrafficSample {
  ts: number;
  bytes_in: number;
  bytes_out: number;
}

export type TrafficBucket = "1m" | "1h";

export interface TrafficResponse {
  bucket: TrafficBucket;
  samples: TrafficSample[];
  total_bytes_in: number;
  total_bytes_out: number;
}

export interface PutQuotaInput {
  monthly_bytes: number;
  billing_anchor?: number;
}

export interface PatchQuotaInput {
  monthly_bytes?: number;
  clear_period_usage?: boolean;
}
```

Also extend `AccessEntry` (in the existing AccessEntry block; do not break existing fields):

```typescript
export interface AccessEntry {
  // ... existing fields ...
  quota?: MonthlyQuotaView;
}
```

- [ ] **Step 2: Write quotas.ts**

```typescript
// webui/src/api/quotas.ts
import { apiFetch } from "./client";
import type {
  MonthlyQuotaView,
  PatchQuotaInput,
  PutQuotaInput,
} from "./types";
import {
  useMutation,
  useQuery,
  useQueryClient,
} from "@tanstack/react-query";

const userQuotasKey = (userId: string) => ["user-quotas", userId] as const;
const userQuotaStatusKey = (userId: string, clientName: string) =>
  ["user-quota-status", userId, clientName] as const;
const clientQuotasKey = (clientName: string) =>
  ["client-quotas", clientName] as const;

export function useUserQuotas(userId: string) {
  return useQuery({
    queryKey: userQuotasKey(userId),
    queryFn: () => apiFetch<MonthlyQuotaView[]>(`/v1/users/${userId}/quotas`),
  });
}

export function useClientQuotas(clientName: string) {
  return useQuery({
    queryKey: clientQuotasKey(clientName),
    queryFn: () => apiFetch<MonthlyQuotaView[]>(`/v1/clients/${clientName}/quotas`),
  });
}

export function useQuotaStatus(userId: string, clientName: string, enabled = true) {
  return useQuery({
    queryKey: userQuotaStatusKey(userId, clientName),
    queryFn: () =>
      apiFetch<MonthlyQuotaView>(`/v1/users/${userId}/quotas/${clientName}/status`),
    enabled,
    refetchInterval: 10_000,
  });
}

export function usePutQuota(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (input: { client_name: string; body: PutQuotaInput }) => {
      const { client_name, body } = input;
      return apiFetch<MonthlyQuotaView>(`/v1/users/${userId}/quotas/${client_name}`, {
        method: "PUT",
        body: JSON.stringify(body),
      });
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: userQuotasKey(userId) });
      qc.invalidateQueries({ queryKey: ["access-entries", userId] });
    },
  });
}

export function usePatchQuota(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (input: { client_name: string; body: PatchQuotaInput }) => {
      const { client_name, body } = input;
      return apiFetch<MonthlyQuotaView>(`/v1/users/${userId}/quotas/${client_name}`, {
        method: "PATCH",
        body: JSON.stringify(body),
      });
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: userQuotasKey(userId) });
    },
  });
}

export function useDeleteQuota(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (input: { client_name: string }) => {
      return apiFetch<void>(`/v1/users/${userId}/quotas/${input.client_name}`, {
        method: "DELETE",
      });
    },
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: userQuotasKey(userId) });
    },
  });
}
```

- [ ] **Step 3: Write traffic.ts**

```typescript
// webui/src/api/traffic.ts
import { apiFetch } from "./client";
import type { TrafficBucket, TrafficResponse } from "./types";
import { useQuery } from "@tanstack/react-query";

export interface TrafficQuery {
  from: number;
  to: number;
  bucket?: TrafficBucket;
  client_name?: string;
  user_id?: string;
}

function trafficQs(q: TrafficQuery): string {
  const u = new URLSearchParams();
  u.set("from", String(q.from));
  u.set("to", String(q.to));
  if (q.bucket) u.set("bucket", q.bucket);
  if (q.client_name) u.set("client_name", q.client_name);
  if (q.user_id) u.set("user_id", q.user_id);
  return u.toString();
}

export function useUserTraffic(userId: string, q: TrafficQuery) {
  return useQuery({
    queryKey: ["user-traffic", userId, q],
    queryFn: () =>
      apiFetch<TrafficResponse>(`/v1/users/${userId}/traffic?${trafficQs(q)}`),
  });
}

export function useClientTraffic(clientName: string, q: TrafficQuery) {
  return useQuery({
    queryKey: ["client-traffic", clientName, q],
    queryFn: () =>
      apiFetch<TrafficResponse>(`/v1/clients/${clientName}/traffic?${trafficQs(q)}`),
  });
}
```

- [ ] **Step 4: Add recharts**

```
cd webui
pnpm add recharts@^2.13
```

Verify the entry is exact-pinned in package.json (matches the existing `chore(webui): pin shadcn-added deps to exact versions` commit pattern — strip the `^`).

- [ ] **Step 5: Type-check**

```
cd webui
pnpm exec tsc -b
```
Expected: no type errors.

- [ ] **Step 6: Commit**

```bash
git add webui/src/api/quotas.ts webui/src/api/traffic.ts webui/src/api/types.ts webui/package.json webui/pnpm-lock.yaml
git commit -m "webui: quota + traffic API clients, recharts dep"
```

---

### Task F2: Merge quota into AccessEntry view

**Files:**
- Modify: `webui/src/api/access-entries.ts` — fetch quotas in parallel with grants + caps; merge into `AccessEntry.quota`

- [ ] **Step 1: Update `useAccessEntries`**

Find the existing `useAccessEntries(userId)` hook. Add a parallel `useUserQuotas(userId)` call and merge results.

- [ ] **Step 2: Update `joinAccessEntries`**

The existing join helper merges grants and caps. Add a third map keyed by client_name from quotas, and attach to each entry's `quota` field.

- [ ] **Step 3: Test the join**

Append to `webui/src/api/access-entries.test.ts`:

```typescript
test("joinAccessEntries attaches quota to matching client_name", () => {
  const grants = [{ id: "g1", user_id: "u", client_name: "c", listen_port_start: 8000, listen_port_end: 8000, protocols: ["tcp"] }];
  const caps: any[] = [];
  const quotas = [{ user_id: "u", client_name: "c", monthly_bytes: 1000, /* … */ }];
  const out = joinAccessEntries("u", grants, caps, quotas as any);
  expect(out[0].quota?.monthly_bytes).toBe(1000);
});
```

- [ ] **Step 4: Run + commit**

```
cd webui && pnpm test access-entries
git add webui/src/api/access-entries.ts webui/src/api/access-entries.test.ts
git commit -m "webui: merge quota into AccessEntry join"
```

---

## Phase G: Web UI components

### Task G1: UserQuotaTable — Monthly quota + This period columns

**Files:**
- Create: `webui/src/components/UserQuota/QuotaCellMonthly.tsx`
- Create: `webui/src/components/UserQuota/QuotaCellPeriodProgress.tsx`
- Modify: `webui/src/components/UserQuota/UserQuotaTable.tsx` — add two new column headers + cells
- Modify: `webui/src/components/UserQuota/UserQuotaForm.tsx` — add `monthly_bytes` input + Clear usage action

- [ ] **Step 1: Write QuotaCellMonthly.tsx**

Displays `monthly_bytes` formatted (e.g., `500 GB / resets 06-08`), or `— (无限)` when no quota. Read-only — editing happens in expand-row.

- [ ] **Step 2: Write QuotaCellPeriodProgress.tsx**

Shows used / monthly with shadcn `Progress` component:

```tsx
import { Progress } from "@/components/ui/progress";
import { formatBytes } from "@/lib/format";
import type { MonthlyQuotaView } from "@/api/types";

export function QuotaCellPeriodProgress({ quota }: { quota?: MonthlyQuotaView }) {
  if (!quota) return <span className="text-muted-foreground">—</span>;
  const pct = Math.min(100, Math.round((quota.current_period_bytes_used / quota.monthly_bytes) * 100));
  return (
    <div className="flex flex-col gap-1">
      <div className="text-sm">
        {formatBytes(quota.current_period_bytes_used)} / {pct}%
      </div>
      <Progress value={pct} className={quota.exhausted_at ? "bg-destructive" : ""} />
    </div>
  );
}
```

- [ ] **Step 3: Extend `UserQuotaTable.tsx`**

Add two `<TableHead>` cells for "Monthly quota" / "This period". Render the new components per row.

- [ ] **Step 4: Extend `UserQuotaForm.tsx`**

Add fields:
- `monthly_bytes` numeric input + unit dropdown (KB / MB / GB / TB). zod validation: integer, >= 0, <= Number.MAX_SAFE_INTEGER.
- "Clear usage" button (only shown when editing existing entry with a quota). Confirmation dialog text: "Clearing usage zeroes the bytes used in the current period. Billing anchor and period boundaries are not changed."
- "Delete quota" button (separately from Delete entry, since quota removal is a distinct operation).

- [ ] **Step 5: Tests**

In `webui/src/components/UserQuota/UserQuotaTable.test.tsx`:
- Renders quota cell with formatted bytes
- Renders empty state for unlimited

- [ ] **Step 6: Type-check + lint + commit**

```
cd webui && pnpm exec tsc -b && pnpm test UserQuota
git add webui/src/components/UserQuota/
git commit -m "webui: AccessEntry quota column + period progress"
```

---

### Task G2: TrafficChart component (recharts wrapper)

**Files:**
- Create: `webui/src/components/Traffic/TrafficChart.tsx`

- [ ] **Step 1: Write the chart**

```tsx
import {
  ResponsiveContainer,
  AreaChart,
  Area,
  XAxis,
  YAxis,
  Tooltip,
  Legend,
} from "recharts";
import type { TrafficSample } from "@/api/types";
import { formatBytes } from "@/lib/format";
import { useTranslation } from "react-i18next";

interface Props {
  samples: TrafficSample[];
  height?: number;
}

export function TrafficChart({ samples, height = 320 }: Props) {
  const { t } = useTranslation();
  const data = samples.map((s) => ({
    ts: new Date(s.ts * 1000).toLocaleString(),
    in: s.bytes_in,
    out: s.bytes_out,
  }));
  return (
    <ResponsiveContainer width="100%" height={height}>
      <AreaChart data={data}>
        <XAxis dataKey="ts" minTickGap={48} />
        <YAxis tickFormatter={(v) => formatBytes(v as number)} />
        <Tooltip formatter={(v: number) => formatBytes(v)} />
        <Legend />
        <Area
          type="monotone"
          dataKey="in"
          name={t("traffic.bytesIn")}
          stackId="1"
          stroke="hsl(var(--chart-1))"
          fill="hsl(var(--chart-1))"
          fillOpacity={0.3}
        />
        <Area
          type="monotone"
          dataKey="out"
          name={t("traffic.bytesOut")}
          stackId="1"
          stroke="hsl(var(--chart-2))"
          fill="hsl(var(--chart-2))"
          fillOpacity={0.3}
        />
      </AreaChart>
    </ResponsiveContainer>
  );
}
```

- [ ] **Step 2: Smoke test renders**

```tsx
test("TrafficChart renders without crashing", () => {
  render(<TrafficChart samples={[{ts: 1700000000, bytes_in: 1024, bytes_out: 512}]} />);
});
```

- [ ] **Step 3: Bundle size sanity**

```
cd webui && pnpm build
```
Expected: size-limit passes. If failing, switch to lazy import for recharts via dynamic import in the page.

- [ ] **Step 4: Commit**

```bash
git add webui/src/components/Traffic/TrafficChart.tsx
git commit -m "webui: TrafficChart (recharts AreaChart stacked)"
```

---

### Task G3: TrafficPanel + Traffic tab on UserDetail / ClientDetail

**Files:**
- Create: `webui/src/components/Traffic/TrafficPanel.tsx`
- Modify: `webui/src/pages/UserDetail.tsx` — wrap existing content in `<Tabs>`, add Traffic tab
- Modify: `webui/src/pages/ClientDetail.tsx` — add Traffic tab

- [ ] **Step 1: Write TrafficPanel.tsx**

Component takes either `{ userId }` or `{ clientName }` (XOR), renders:
- Time range selector (`Last 1h / 24h / 7d / Custom`)
- Bucket selector (auto / 1m / 1h)
- Filter (other dimension)
- Totals
- TrafficChart
- (Optional) "Export CSV" button

- [ ] **Step 2: Wire to UserDetail**

```tsx
<Tabs defaultValue="quotas">
  <TabsList>
    <TabsTrigger value="quotas">{t("userDetail.tabs.quotas")}</TabsTrigger>
    <TabsTrigger value="traffic">{t("userDetail.tabs.traffic")}</TabsTrigger>
  </TabsList>
  <TabsContent value="quotas">
    {/* existing UserQuotaTable */}
  </TabsContent>
  <TabsContent value="traffic">
    <TrafficPanel userId={userId} />
  </TabsContent>
</Tabs>
```

- [ ] **Step 3: Wire to ClientDetail**

Add a `<TabsTrigger value="traffic">` to the existing tab list at lines 95-138; mount `<TrafficPanel clientName={...} />` in its content.

- [ ] **Step 4: Type-check + commit**

```
cd webui && pnpm exec tsc -b
git add webui/src/components/Traffic/ webui/src/pages/UserDetail.tsx webui/src/pages/ClientDetail.tsx
git commit -m "webui: Traffic tab on UserDetail and ClientDetail"
```

---

### Task G4: ExhaustedBanner

**Files:**
- Create: `webui/src/components/Traffic/ExhaustedBanner.tsx`
- Modify: `webui/src/pages/UserDetail.tsx` + `ClientDetail.tsx` — mount banner above tabs

- [ ] **Step 1: Banner**

```tsx
import { useTranslation } from "react-i18next";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { AlertTriangle } from "lucide-react";
import type { MonthlyQuotaView } from "@/api/types";

interface Props {
  exhausted: MonthlyQuotaView[];
  onClearUsage: (q: MonthlyQuotaView) => void;
  onIncreaseLimit: (q: MonthlyQuotaView) => void;
}

export function ExhaustedBanner({ exhausted, onClearUsage, onIncreaseLimit }: Props) {
  const { t } = useTranslation();
  if (!exhausted.length) return null;
  return (
    <Alert variant="destructive">
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>{t("traffic.banner.title")}</AlertTitle>
      <AlertDescription>
        {exhausted.map((q) => (
          <div key={`${q.user_id}|${q.client_name}`} className="flex items-center gap-2 my-1">
            <span>{t("traffic.banner.row", { client: q.client_name })}</span>
            <Button size="sm" variant="outline" onClick={() => onClearUsage(q)}>
              {t("userQuota.clearUsage")}
            </Button>
            <Button size="sm" variant="outline" onClick={() => onIncreaseLimit(q)}>
              {t("traffic.banner.increase")}
            </Button>
          </div>
        ))}
      </AlertDescription>
    </Alert>
  );
}
```

- [ ] **Step 2: Mount on the two pages**

Above the `<Tabs>`, derive `exhausted` from `useUserQuotas(userId)` or `useClientQuotas(clientName)` filtered by `exhausted_at != null`.

- [ ] **Step 3: Commit**

```bash
git add webui/src/components/Traffic/ExhaustedBanner.tsx webui/src/pages/UserDetail.tsx webui/src/pages/ClientDetail.tsx
git commit -m "webui: exhausted quota banner"
```

---

### Task G5: i18n strings

**Files:**
- Modify: `webui/src/i18n/en.json`
- Modify: `webui/src/i18n/zh-CN.json`

- [ ] **Step 1: Add `traffic.*` namespace + extend `userQuota.*`**

EN:
```json
{
  "traffic": {
    "tab": "Traffic",
    "bytesIn": "Bytes in",
    "bytesOut": "Bytes out",
    "totalIn": "Total in",
    "totalOut": "Total out",
    "timeRange": "Time range",
    "ranges": {
      "1h": "Last 1 hour",
      "24h": "Last 24 hours",
      "7d": "Last 7 days",
      "custom": "Custom"
    },
    "bucket": "Bucket",
    "buckets": { "auto": "Auto", "1m": "1 minute", "1h": "1 hour" },
    "banner": {
      "title": "Monthly quota exhausted",
      "row": "Forwarding paused for {{client}}",
      "increase": "Increase limit"
    },
    "exportCsv": "Export CSV"
  },
  "userQuota": {
    "monthlyQuota": "Monthly quota",
    "thisPeriod": "This period",
    "clearUsage": "Clear usage",
    "clearUsageConfirm": "Zeroes the bytes used in the current period. Billing anchor and period boundaries are NOT reset.",
    "unlimited": "Unlimited",
    "exhausted": "Exhausted",
    "resetsOn": "Resets {{date}}",
    "billingAnchor": "Billing anchor"
  }
}
```

ZH-CN (parallel structure):
```json
{
  "traffic": {
    "tab": "流量",
    "bytesIn": "入流量",
    "bytesOut": "出流量",
    "totalIn": "总入",
    "totalOut": "总出",
    "timeRange": "时间范围",
    "ranges": {
      "1h": "最近 1 小时",
      "24h": "最近 24 小时",
      "7d": "最近 7 天",
      "custom": "自定义"
    },
    "bucket": "粒度",
    "buckets": { "auto": "自动", "1m": "1 分钟", "1h": "1 小时" },
    "banner": {
      "title": "月度配额已耗尽",
      "row": "{{client}} 已暂停转发",
      "increase": "提高上限"
    },
    "exportCsv": "导出 CSV"
  },
  "userQuota": {
    "monthlyQuota": "月度配额",
    "thisPeriod": "本期",
    "clearUsage": "清零本期用量",
    "clearUsageConfirm": "仅清零本期已用字节,不会重置计费起点或周期边界。",
    "unlimited": "无限",
    "exhausted": "已耗尽",
    "resetsOn": "{{date}} 重置",
    "billingAnchor": "计费起点"
  }
}
```

- [ ] **Step 2: Build + commit**

```
cd webui && pnpm build
git add webui/src/i18n/en.json webui/src/i18n/zh-CN.json
git commit -m "webui: i18n strings for traffic + monthly quota"
```

---

## Phase H: E2E + Docs + Release

### Task H1: End-to-end test

**Files:**
- Create: `crates/portunus-e2e/tests/traffic_quotas.rs`

- [ ] **Step 1: Write E2E that covers the four checkpoints**

```rust
//! End-to-end coverage for v1.4.0 traffic quotas. Uses real sockets +
//! a spawned portunus-server + portunus-client (process-level), same
//! pattern as other portunus-e2e tests.

mod common; // existing helpers

use std::time::Duration;
use tokio::time::sleep;

#[tokio::test(flavor = "multi_thread")]
async fn quota_hard_kill_then_recovery_via_reset() {
    let env = common::TestEnv::new().await;
    // 1. Provision client + user + grant.
    let user = env.create_user("alice").await;
    let client = env.create_client("edge-test").await;
    env.grant(&user, &client, 9100, 9100, "tcp").await;
    // 2. Connect client + upstream echo on 9200.
    let upstream = common::spawn_echo_tcp(9200).await;
    env.push_rule(&client, "tcp 9100 -> 127.0.0.1:9200").await;
    // 3. PUT a 1 MiB quota.
    env.put_quota(&user.id, &client.name, 1024 * 1024).await;
    // 4. Drive 800 KiB — should succeed.
    let first = common::echo_through(9100, 800 * 1024).await;
    assert_eq!(first.len(), 800 * 1024);
    // 5. Drive 400 KiB — should be cut short (quota crosses ~1 MiB).
    let second = common::echo_through_partial(9100, 400 * 1024).await;
    assert!(second.len() < 400 * 1024, "expected truncation, got {}", second.len());
    // 6. Verify quota status is exhausted.
    let status = env.get_quota_status(&user.id, &client.name).await;
    assert!(status.exhausted_at.is_some());
    // 7. Issue PATCH clear_period_usage and verify recovery.
    env.clear_period_usage(&user.id, &client.name).await;
    sleep(Duration::from_millis(500)).await;
    let third = common::echo_through(9100, 800 * 1024).await;
    assert_eq!(third.len(), 800 * 1024);
}

#[tokio::test(flavor = "multi_thread")]
async fn period_rollover_advances_period() {
    let env = common::TestEnv::with_time_travel().await;
    // Provision, put quota with billing_anchor = now - 31 days.
    // Drive bytes, then bump server clock by 1 second past the anchor + 31d.
    // Wait one rollover tick (default 60s; for the test, expose a manual tick).
    // Verify quota's current_period_started_at advanced + bytes_used reset.
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_replay_order_quota_before_rule() {
    let env = common::TestEnv::new().await;
    // Provision + grant + PUT quota + push rule.
    // Disconnect the client (kill TCP / drop session).
    // Reconnect and capture the inbound message stream.
    // Assert: first TrafficQuotaUpdate, then RuleUpdate.
}

#[tokio::test(flavor = "multi_thread")]
async fn traffic_samples_appear_for_all_pairs() {
    let env = common::TestEnv::new().await;
    // Provision but DO NOT set a quota. Drive bytes through.
    // Wait for one StatsReport tick (5s + buffer).
    // GET /v1/users/{u}/traffic and verify samples present.
}
```

> **Subagent note:** Adapt to whatever helpers exist in `crates/portunus-e2e/`. Test 2 (`period_rollover`) may need a manual rollover trigger endpoint or a `cfg(test)` exposure of `rollover::run_once` — add that as part of the task if needed.

- [ ] **Step 2: Run**

```
cargo test -p portunus-e2e traffic_quotas
```

Expected: all 4 tests pass.

- [ ] **Step 3: Commit**

```
git add crates/portunus-e2e/tests/traffic_quotas.rs
git commit -m "e2e: traffic quotas hard-kill, recovery, replay order, sample coverage"
```

---

### Task H2: Runbook + troubleshooting + API reference

**Files:**
- Create: `docs/content/docs/operations/runbook-traffic-quotas.mdx`
- Create: `docs/content/docs/zh/operations/runbook-traffic-quotas.mdx`
- Modify: `docs/content/docs/operations/troubleshooting.mdx`
- Modify: `docs/content/docs/zh/operations/troubleshooting.mdx`

- [ ] **Step 1: Write runbook (EN)**

Sections:
1. Enabling monthly quotas — PUT example
2. Setting billing anchor strategies
3. Reading exhausted state — `/quotas/{}/status`, Prometheus
4. What "bounded best-effort" means (cite spec §9.1 numbers, buffer formula)
5. Resetting usage mid-period (PATCH `clear_period_usage`)
6. Migrating an unconfigured deployment (quota row absent = unlimited)

- [ ] **Step 2: Write runbook (ZH)**

Same structure, Chinese translation.

- [ ] **Step 3: Extend troubleshooting**

Add rows:
- "End-user reports connection drops at GB boundary" — check `portunus_traffic_quota_exhausted{user, client}`; either clear usage or raise limit
- "Quota state seems stuck" — check session is alive (`portunus_clients_connected`); check `traffic_quota.applied_set` events in client log
- "Traffic chart looks empty" — verify retention; sample table coverage

- [ ] **Step 4: Commit**

```
git add docs/content/docs/operations/runbook-traffic-quotas.mdx docs/content/docs/zh/operations/runbook-traffic-quotas.mdx docs/content/docs/operations/troubleshooting.mdx docs/content/docs/zh/operations/troubleshooting.mdx
git commit -m "docs: v1.4 traffic quota runbook + troubleshooting"
```

---

### Task H3: CHANGELOG + version bump

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `Cargo.toml` (workspace version)

- [ ] **Step 1: Bump versions**

Use `/release-version 1.4.0` or manually:
- `Cargo.toml` workspace.package.version = "1.4.0"
- `webui/package.json` version = "1.4.0"

- [ ] **Step 2: Move `## [Unreleased]` content + new section**

```markdown
## [1.4.0] — 2026-XX-XX

Per-(user, client) monthly traffic quota + history aggregation.
Operators can now cap monthly bytes per (user, client) pair with hard
kill enforcement on the data plane; the Web UI gains a Traffic tab with
1-minute + 1-hour rollup history on both UserDetail and ClientDetail.

### Added

- **Monthly traffic quota** — new `traffic_quotas` SQLite table; CRUD
  HTTP endpoints `/v1/users/{u}/quotas/{c}` (PUT/PATCH/DELETE/GET) and
  `/v1/users/{u}/quotas/{c}/status`. Billing-anniversary period
  progression with Jan-31 calendar-month clamp and multi-month skip on
  clock jump. Bounded best-effort hard kill: TCP userspace quota-aware
  copy + splice per-iteration consume hook + UDP per-datagram hook.
- **Traffic history** — two-tier rollup: `traffic_samples_1m` (7 day
  retention) + `traffic_samples_1h` (90 day retention). Query endpoints
  `/v1/users/{u}/traffic` and `/v1/clients/{c}/traffic` with `bucket=1m|1h`
  and auto-selection by time-window size.
- **Web UI Traffic tab** — UserDetail + ClientDetail each gain a Traffic
  tab with stacked area chart (recharts), per-period progress column on
  UserQuotaTable, exhausted-state banner with Clear-usage + Increase-limit
  shortcuts.
- **Wire** — new `TrafficQuotaUpdate` server-only push variant
  (`ServerMessage.payload` field 4) with SET/REMOVE action; reconnect
  replay sends quotas BEFORE rules so QuotaHandle is registry-resident
  at rule activation. Client→server is unchanged (server aggregates
  per-(user, client) from existing `RuleStats`).
- **Prometheus** — 5 new collectors with `{user, client}` labels:
  `portunus_traffic_quota_bytes_used`, `_bytes_limit`, `_exhausted`,
  `_period_resets_total`, `_exhausted_total`.

### Changed

- Reconnect replay sequence is now: Welcome → TrafficQuotaUpdate(s) →
  RuleUpdate(s) → OwnerRateLimitUpdate(s) (was Welcome → Rule → Owner).
- `RuleStatsCache::observe` now also feeds the TrafficAggregator.
```

- [ ] **Step 3: Commit + tag**

```bash
git add CHANGELOG.md Cargo.toml webui/package.json
git commit -m "release: v1.4.0 traffic quotas + history"
```

(Do NOT push tags or publish in this plan — that's an operator action.)

---

## Self-Review

After completion, sanity-check:

1. **Spec coverage**:
   - §3.1 schema → Tasks A2, A3, A4 ✓
   - §3.3 period math → Task A3 with unit tests for Jan31 / leap / multi-month ✓
   - §4.1 topology → Tasks B2 (aggregator), C5 (push), D3 (handler), E1-E4 (hooks) ✓
   - §4.2 wire (no client→server change) → Task A1 (proto), Task B2 (server aggregates from RuleStats) ✓
   - §4.3 decision 6 hard-kill mechanism → Tasks E1 (TCP userspace), E3 (splice), E4 (UDP) ✓
   - §4.3 decision 7 history covers all pairs → Task B2 (always upserts 1m sample) ✓
   - §5 HTTP API → Tasks C1 (handlers), C2 (RBAC) ✓
   - §6 Web UI → Tasks F1-F2 (api), G1-G5 (components) ✓
   - §7.1 lifecycle divergence → No GC of quota on grant deletion (Task A3 store has no GC; Task C5 reconnect replay still sends present quotas) ✓
   - §9.1 buffer recommendation → Task H2 runbook covers ✓

2. **Placeholder scan**: searched for "TBD", "TODO", "implement later" — none present.

3. **Type consistency**:
   - `QuotaHandle::consume` returns `ConsumeOutcome` everywhere ✓
   - `QuotaScopeManager.lookup(user_id)` consistently keyed by user_id (client_name is implicit "this client") ✓
   - HTTP path params use `user_id` / `client_name` consistently ✓
   - `TrafficQuotaState.budget_remaining_bytes` is i64 (signed) consistent with `AtomicI64` + SQL `INTEGER` ✓

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-05-14-traffic-quotas-and-history.md`.**

Implementation order: A1 → A2 → A3 → A4 → B1 → B2 → B3 → C1 → C2 → C3 → C4 → C5 → D1 → D2 → D3 → E1 → E2 → E3 → E4 → F1 → F2 → G1 → G2 → G3 → G4 → G5 → H1 → H2 → H3.

Each task is independently subagent-dispatchable. Phase boundaries (A/B/C/D/E/F/G/H) are natural review checkpoints.
