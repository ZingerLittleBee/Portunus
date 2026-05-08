# Contract — Operator HTTP API Delta

v0.8 is **additive only** on the operator HTTP surface (FR-008,
FR-011 spirit). This document lists every endpoint touched, the
exact byte-stable promise to v0.7 callers, and the new query
parameters allowed on `GET /v1/audit`.

Anything not listed here keeps its v0.7 contract verbatim.

---

## Touched endpoints

### `GET /v1/audit` — extended

| Aspect | v0.7 (baseline) | v0.8 (this spec) |
|--------|-----------------|------------------|
| Auth | superadmin-only bearer | unchanged |
| Existing query params | `limit` (1..1000, default 100), `outcome` ('allow'\|'deny') | unchanged |
| New query params | — | `since` (RFC3339), `until` (RFC3339), `cursor` (opaque token from prior page's `next_cursor`) |
| Response shape (root) | JSON array, newest-first | JSON object: `{ "entries": [...], "next_cursor": <string|null> }` **only when `cursor` or `since`/`until` are passed**; otherwise the root is the same JSON array as v0.7 |
| Response entry shape | unchanged fields | unchanged fields; reserved-for-future fields MUST be present (empty / null) so v0.7 parsers do not break |

**Compatibility rule** (FR-008, SC-006): a v0.7 client passing
`?limit=N&outcome=O` (or no params) MUST receive a response shape
identical to v0.7 — a bare JSON array, not the new envelope. The
new envelope is **opt-in** via the new params.

#### Pagination semantics

When the client opts into the envelope (any of `since`, `until`,
`cursor` present):

- `entries` is the page; ordering is `ts DESC, seq DESC`.
- `next_cursor` is opaque (server-generated; encodes `seq`); the
  client MUST treat it as a string.
- The server MUST NOT change the cursor encoding within a major
  version such that prior pages' cursors become invalid.
- An expired or unknown cursor returns HTTP 400 with
  `{"error":"invalid_cursor"}`; the client SHOULD restart pagination
  from `cursor` omitted.
- `since` / `until` accept any RFC3339 timestamp. Inclusive on
  `since`, exclusive on `until`.

#### Examples

```http
GET /v1/audit?limit=50&outcome=deny           # v0.7 behaviour, JSON array root
GET /v1/audit?since=2026-04-01T00:00:00Z&limit=50
                                              # opt-in envelope; first page
GET /v1/audit?cursor=<from-prior>&limit=50    # next page
```

---

### Endpoints with NO change to wire shape

These all keep their v0.7 contracts byte-stable; only the storage
backend changes:

- `GET /v1/users` — unchanged
- `GET /v1/users/{id}` — unchanged
- `GET /v1/users/me` — unchanged
- `POST /v1/users` — unchanged
- `DELETE /v1/users/{id}` — unchanged
- `POST /v1/users/{id}/credentials` — unchanged
- `DELETE /v1/credentials/{id}` — unchanged
- `POST /v1/grants` — unchanged
- `DELETE /v1/grants/{id}` — unchanged
- `GET /v1/rules` — unchanged
- `GET /v1/rules/{id}` — unchanged
- `POST /v1/rules` — unchanged
- `DELETE /v1/rules/{id}` — unchanged
- `GET /v1/rules/{id}/stats` — unchanged
- `GET /v1/rules/{id}/stats/stream` (SSE) — unchanged
- `GET /metrics` — unchanged shape; new series listed below
- `GET /v1/clients` — unchanged

Regression coverage: a v0.7-shape contract test runs every endpoint
above on a v0.8 build with a populated store and asserts byte-for-byte
JSON equality (modulo timestamp-driven fields).

---

## Prometheus series delta

Reused (semantic-identical):

- `forward_audit_buffer_drops_total{}` — was: ring-buffer overflow.
  Now: hand-off-queue overflow. Operators with existing alerts on
  this series keep working.

Added:

- `forward_audit_durable_writer_lag_seconds{}` — gauge; the age of
  the oldest entry currently sitting in the hand-off queue (0 when
  the queue is empty). Useful for diagnosing burst-overrun before
  drops happen.
- `forward_store_open_connections{}` — gauge; current pool checkout
  count. Operators can monitor pool exhaustion.
- `forward_store_busy_total{}` — counter; cumulative `SQLITE_BUSY`
  occurrences mapped to `ForwardError::Transient`. Should stay near
  zero in a healthy deployment thanks to `BEGIN IMMEDIATE`.

No counter is renamed or removed. No label cardinality is added on
existing series.

---

## Error response envelope

Unchanged from v0.7. The new failure modes from this spec map to
existing HTTP statuses:

| Condition | HTTP | Body |
|-----------|------|------|
| Invalid pagination cursor | 400 | `{"error":"invalid_cursor"}` |
| `since` after `until` | 400 | `{"error":"invalid_time_range"}` |
| Invalid RFC3339 in `since` / `until` | 400 | `{"error":"invalid_timestamp","field":"since"\|"until"}` |
| Conflict (e.g., duplicate `client_name` on a SQLite UNIQUE failure) | 409 | `{"error":"conflict","detail":"..."}` |
| Internal store error (corruption-class — should be unreachable at runtime per `contracts/persistence.md`) | 500 | `{"error":"internal","request_id":"..."}` |

---

## Quick contract test plan

| Test | Asserts |
|------|---------|
| `audit_v07_array_root` | `GET /v1/audit` (no params) returns a JSON array root, not an envelope |
| `audit_v07_limit_outcome` | `GET /v1/audit?limit=10&outcome=deny` returns a JSON array root, length ≤ 10, all `outcome=deny` |
| `audit_v08_envelope_since` | `GET /v1/audit?since=...` returns an envelope with `entries`, `next_cursor` |
| `audit_v08_pagination_round_trip` | Following `next_cursor` across N pages reaches every entry exactly once |
| `audit_v08_invalid_cursor` | Random string cursor → HTTP 400 invalid_cursor |
| `users_byte_stable` | Snapshot the v0.7 response of `GET /v1/users` → byte-identical on v0.8 (modulo seeded timestamps) |
| `prom_series_present` | `/metrics` exposes the new series and keeps the old `forward_audit_buffer_drops_total` |

These are authored before implementation per Constitution Principle
III.
