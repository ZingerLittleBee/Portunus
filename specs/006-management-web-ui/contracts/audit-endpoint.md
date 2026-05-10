# Contract: `GET /v1/audit`

Read endpoint for the in-memory audit ring buffer. Superadmin-only.
Source-of-truth for audit data is the existing structured log file
(`tracing` JSON layer); this endpoint is a convenience read of the
last N entries that the UI consumes.

## Request

```
GET /v1/audit?limit=<N>&outcome=<allow|deny>
Authorization: Bearer <operator-token>
```

| Query param | Type | Default | Validation |
|---|---|---|---|
| `limit` | integer | `100` | 1 ≤ limit ≤ 1000; out-of-range → 422 `invalid_limit` |
| `outcome` | string (optional) | none (return both) | one of `allow` / `deny`; other values → 422 `invalid_outcome` |

(`outcome` filtering is implemented client-side per spec FR-010, but
the query param is forwarded unchanged to the server for completeness.
The server applies the filter to keep the response small for
mobile / bandwidth-constrained tabs.)

## Authorization

- `auth_layer` middleware first: 401 on bad/missing bearer; 503
  `bootstrap_required` if no superadmin exists yet.
- Handler-side check: caller `role` MUST be `superadmin`. Otherwise
  return 403 `role_required` (matches the v0.5 `RbacError::RoleRequired`
  → HTTP 403 mapping in `auth_layer::http_status_for`).

## Response

### 200 OK

Newest-first array of entries:

```json
[
  {
    "timestamp": "2026-05-07T13:42:11.045Z",
    "actor": "alice",
    "role": "user",
    "method": "POST",
    "path": "/v1/rules",
    "outcome": "deny",
    "reason": "port_outside_grant"
  },
  {
    "timestamp": "2026-05-07T13:42:09.812Z",
    "actor": "_legacy",
    "role": "superadmin",
    "method": "POST",
    "path": "/v1/users",
    "outcome": "allow",
    "reason": null
  }
]
```

Array length = `min(limit, ring_buffer_len, count_after_outcome_filter)`.
If the ring buffer is empty the response is `[]`.

### 401 / 403 / 422

Standard `ApiError` body shape per `operator-api.md`:

```json
{ "error": { "code": "<RbacError code or invalid_*>", "message": "<human-readable>" } }
```

## Side effects

- A successful read emits one `operator.allow` audit entry like any
  other request (the audit endpoint audits itself — meta but
  consistent).

## Test plan

`crates/portunus-server/tests/audit_contract.rs` (new) covers:

1. Empty buffer returns `[]`.
2. Buffer with N entries returns at most `limit` newest-first.
3. `?limit=2000` → 422 `invalid_limit`.
4. `?outcome=deny` filters to deny rows only.
5. `?outcome=banana` → 422 `invalid_outcome`.
6. Caller with role `user` → 403 `role_required`.
7. No bearer → 401 `unauthenticated`.
8. Reading the audit endpoint after performing some operator actions
   shows those actions in the buffer in newest-first order.

## Implementation notes (non-binding for the contract)

- Handler is a thin wrapper around
  `state.audit.snapshot(limit, outcome_filter)`.
- The ring buffer must NOT contain raw bearer tokens — they were
  already redacted at the auth_layer emit site (Constitution IV).
- Response sets `Cache-Control: no-store`.
