# Phase 1 Data Model: Client Stable Identifier

## Core types (`portunus-core/src/id.rs`)

### `ClientId` (NEW)

- **Representation**: `ClientId(Ulid)` newtype. `Copy`, `Eq`, `Hash`, `Ord` (ULID is
  sortable), `Serialize`/`Deserialize` (transparent, canonical 26-char Crockford base32),
  `Display`, `FromStr` (parse + validate ULID).
- **Lifecycle**: assigned once at client creation/enrollment; immutable thereafter.
- **Role**: canonical key for every persisted client-scoped row, the in-memory connected-
  client map, operator HTTP/CLI/Web-UI addressing, and internal log/metric correlation.

### `ClientName` (MODIFIED — validation relaxed)

- **Representation**: unchanged newtype `ClientName(String)`.
- **New validation** (`ClientName::new`):
  - Reject empty or whitespace-only.
  - Reject any `char::is_control()` character.
  - Reject length > 255 **bytes**.
  - Accept everything else: uppercase, spaces, `.`, `_`, `-` (any position/repetition),
    non-Latin Unicode.
  - Stored verbatim (no case-fold, no Unicode normalization).
- **New error enum** `ClientNameError`: `Empty`, `TooLong(usize)`, `ControlChar`.
  (Old `BadLength`/`BadEdge`/`BadChar`/`DoubleHyphen` removed.)
- **Role**: free-form display label only. Not an identity, not unique, not an authz input.

## `ClientIdentity` (`portunus-auth/src/lib.rs`)

- Gains `pub client_id: ClientId` (the authenticated client's stable id).
- Retains `pub client_name: ClientName` (display).
- This struct is the single auth seam: `Authenticator::verify(token) -> ClientIdentity`.

## Persisted entities (SQLite — re-keyed in `V011`)

Source of truth for the client roster is `client_tokens`. All others join by `client_name`
to backfill `client_id`.

| Table | Before (key) | After (key) | Notes |
|-------|--------------|-------------|-------|
| `client_tokens` | `client_name TEXT PRIMARY KEY` | `client_id TEXT PRIMARY KEY` | add `client_id` (fresh ULID per row), keep `client_name` column (now mutable display, non-unique allowed) |
| `rules` | `client_name` col + `rules_client_idx(client_name, listen_port)` | `client_id` col + `rules_client_idx(client_id, listen_port)` | FK → `client_tokens(client_id)`; drop name from index |
| `rate_limit_owner` | `PRIMARY KEY (client_name, owner_id)` | `PRIMARY KEY (client_id, owner_id)` | tenant isolation preserved on (owner, client) |
| `traffic_quotas` | `PRIMARY KEY (user_id, client_name)` | `PRIMARY KEY (user_id, client_id)` | |
| `traffic_usage_minute` | `PRIMARY KEY (user_id, client_name, ts_minute)` | `PRIMARY KEY (user_id, client_id, ts_minute)` | |
| `traffic_usage_hour` | `PRIMARY KEY (user_id, client_name, ts_hour)` | `PRIMARY KEY (user_id, client_id, ts_hour)` | |
| `client_enrollments` | `client_name TEXT NOT NULL` | `client_id TEXT NOT NULL` | re-keyed; name kept for display if surfaced |

**Display-name column**: `client_tokens.client_name` becomes the mutable display field. A
rename is an `UPDATE client_tokens SET client_name = ? WHERE client_id = ?`; no other table
needs touching on rename (they key on `client_id`).

**Duplicate names**: no `UNIQUE` constraint on `client_name` anywhere after `V011`.

## Relationships

```
client_tokens (client_id PK, client_name display)
   ├─1:N─ rules                (client_id FK)
   ├─1:N─ rate_limit_owner     (client_id, owner_id)
   ├─1:N─ traffic_quotas       (user_id, client_id)
   ├─1:N─ traffic_usage_minute (user_id, client_id, ts_minute)
   ├─1:N─ traffic_usage_hour   (user_id, client_id, ts_hour)
   └─1:N─ client_enrollments   (client_id)
```

## In-memory state (`portunus-server`)

| Structure | Before | After |
|-----------|--------|-------|
| `ConnectedClients.inner` | `HashMap<ClientName, ConnectedClient>` | `HashMap<ClientId, ConnectedClient>` |
| metric handles | `client_name: ClientName` | `client_id: ClientId` (internal) + `client_name` for label value |

## State transitions

- **Create / enroll**: server generates `ClientId`, stores `(client_id, client_name, token
  hash, …)`. Returns a bundle carrying both `client_id` and `client_name`.
- **Rename**: `client_name` updated for a fixed `client_id`; all dependent rows untouched;
  live session (keyed by `client_id`) uninterrupted; audit record emitted.
- **Connect**: token → `ClientIdentity{client_id, client_name}`; registry keyed by
  `client_id`. Legacy bundle (no `client_id`) connects identically (token-resolved).
- **Delete / revoke**: cascade by `client_id` (existing behavior, re-keyed).

## Validation rules → requirements traceability

| Rule | FR |
|------|----|
| `ClientId` immutable, system-generated | FR-001 |
| `client_id` is canonical key everywhere | FR-002 |
| relaxed name validation | FR-003, FR-011 |
| rename identity-safe, session-safe | FR-004 |
| migrate + backfill, no loss | FR-005 |
| idempotent/crash-safe migration | FR-006 |
| legacy clients transparent | FR-007 |
| additive wire | FR-008 |
| id-based addressing | FR-009 |
| listings show name, disambiguate | FR-010 |
| unknown id → not-found | FR-012 |
| names non-unique, no warning | FR-013 |
| correlation stable across rename | FR-014 |
