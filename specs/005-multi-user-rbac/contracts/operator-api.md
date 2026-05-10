# Operator API & CLI Contract: Multi-User RBAC

**Feature**: 005-multi-user-rbac
**Phase**: 1 (design)

This document defines the operator-facing surface introduced or
modified by feature 005. Sections marked **NEW** describe surfaces
that did not exist in v0.4.0; sections marked **CHANGED** describe
additive deltas to existing v0.4.0 surfaces.

The HTTP API is the canonical surface. The CLI is a thin wrapper that
calls the same HTTP endpoints with the bearer token from
`PORTUNUS_OPERATOR_TOKEN` (env) or `--token <value>` (flag).

## Authentication

**CHANGED — every existing endpoint, plus all new endpoints**

Every request to `/v1/*` MUST present:

```http
Authorization: Bearer <token>
```

The token is the raw 43-char URL-safe-base64 string returned by
credential issuance (see § Credential Endpoints) or by
`bootstrap-superadmin` (see § Bootstrap). The server hashes the
presented token with blake3 and constant-time-compares against the
stored hashes in `identity.json`.

**Failure modes** (HTTP `401 Unauthorized`):

| `reason` field | Meaning |
|---|---|
| `unauthenticated` | `Authorization` header missing or not `Bearer …` |
| `credential_invalid` | Token shape valid, hash not found, or credential revoked |
| `user_disabled` | Hash matches but the owning user is disabled |
| `bootstrap_required` | No superadmin exists and no `operator_token` in config — server returns `503 Service Unavailable` instead of `401`, with `reason = "bootstrap_required"` |

**Failure body shape** (uniform across all `/v1/*`):

```json
{ "error": { "code": "credential_invalid", "message": "human-readable detail" } }
```

The `code` field is the machine-readable enum (FR-008). The `message`
field is operator-friendly text. CLI maps `code` → exit code per § CLI
Exit Codes.

## Authorization (RBAC) reasons

When authentication succeeds but the request is denied (HTTP `403
Forbidden`), the `error.code` is one of:

| `code` | Meaning | When |
|---|---|---|
| `client_not_granted` | None of the caller's grants reference this `client` | push-rule on a client the user has no grant for |
| `port_outside_grant` | No single grant fully covers the requested listen-port range | push-rule on a port outside the user's grants |
| `protocol_not_granted` | Caller has no grant for this protocol on this client+port range | push-rule with TCP when grant says `[udp]` |
| `not_owner` | Caller is not superadmin AND does not own the resource | rule-stats on someone else's rule |
| `role_required` | Endpoint requires `superadmin` and caller is `user` | user-add by a non-superadmin |

## Bootstrap (NEW)

**CLI**: `portunus-server bootstrap-superadmin --name "<display name>"`

- Refuses with exit `2` and `reason = "already_bootstrapped"` if any
  user with role `superadmin` exists in `identity.json`.
- On success, mints a new `User { id: "_superadmin", role:
  Superadmin, display_name: <given> }` and a new `Credential` for
  that user, writes both to `identity.json` atomically, and prints
  the raw token to stdout exactly once. Stdout shape (parseable):

  ```text
  superadmin user_id=_superadmin token=<43 chars>
  ```

- The token is NOT logged. It appears only on stdout.
- No HTTP equivalent — bootstrap is an out-of-band operation.

**Config-shortcut alternative**: setting `operator_token = "<token>"`
in `server.toml` causes the server to mint the same `_superadmin`
user on first startup if none exists. The token in the config is the
raw token (the server hashes it on first read and stores the hash).
Removing the config line after first start does NOT revoke the token
(it is now persisted to `identity.json` as a hash).

## User Endpoints (NEW)

All require role `superadmin`.

### `POST /v1/users`

**Request body**:

```json
{
  "id": "alice",
  "display_name": "Alice — payments team",
  "role": "user"
}
```

- `id`: regex `^[a-z][a-z0-9_-]{0,31}$`. Reserved IDs starting with
  `_` are rejected with `code = "reserved_user_id"`.
- `display_name`: UTF-8, ≤ 64 chars.
- `role`: `"user"` or `"superadmin"`. Defaults to `"user"`.

**Response** `201 Created`:

```json
{
  "id": "alice",
  "display_name": "Alice — payments team",
  "role": "user",
  "created_at": "2026-05-07T12:34:56Z",
  "disabled": false
}
```

**Errors**:

- `409 Conflict` `code = "user_already_exists"` — `id` is taken.
- `422 Unprocessable Entity` `code = "invalid_user_id"` /
  `"invalid_display_name"` / `"reserved_user_id"`.
- `403 Forbidden` `code = "role_required"` — caller is not superadmin.

**CLI**: `portunus-server user-add <id> --display-name "<name>"
[--role superadmin]`

### `GET /v1/users`

**Response** `200 OK`:

```json
{
  "users": [
    {
      "id": "_superadmin",
      "display_name": "Built-in superadmin",
      "role": "superadmin",
      "created_at": "...",
      "disabled": false,
      "credential_count": 1,
      "grant_count": 0
    },
    {
      "id": "alice",
      "display_name": "Alice — payments team",
      "role": "user",
      "created_at": "...",
      "disabled": false,
      "credential_count": 1,
      "grant_count": 2
    }
  ]
}
```

`credential_count` counts only Active credentials. `grant_count`
counts all grants (no Revoked grants exist — revoke removes).

**CLI**: `portunus-server user-list [--format json|table]`

### `GET /v1/users/{id}`

Same shape as one element of the list, plus a `grants` and `credentials`
array (credential entries omit `token_hash`; only metadata is exposed).

**Errors**:

- `404 Not Found` `code = "user_not_found"`.

**CLI**: `portunus-server user-get <id> [--format json|table]`

### `DELETE /v1/users/{id}`

Cascade: revoke all credentials, revoke all grants, remove all owned
rules. Synchronous; the response returns after the cascade.

**Response** `200 OK`:

```json
{
  "user_id": "alice",
  "removed_credentials": 1,
  "revoked_grants": 2,
  "removed_rules": 5
}
```

**Errors**:

- `404 Not Found` `code = "user_not_found"`.
- `409 Conflict` `code = "cannot_remove_self"` — superadmin cannot
  remove their own user. Use a different superadmin.
- `409 Conflict` `code = "last_superadmin"` — refuses to remove the
  last superadmin (would brick the operator surface). Bootstrap
  another first.

**CLI**: `portunus-server user-remove <id>`

## Credential Endpoints (NEW)

### `POST /v1/users/{id}/credentials`

Issues a new credential for the user. Caller is either the user
themselves OR a superadmin.

**Request body** (optional):

```json
{ "label": "ci runner #3" }
```

**Response** `201 Created`:

```json
{
  "credential_id": "01HEXY...",
  "user_id": "alice",
  "label": "ci runner #3",
  "created_at": "...",
  "token": "<43-char token, returned exactly once>"
}
```

The `token` field appears in the response body **exactly once** at
issuance. Subsequent reads of this credential (via `GET
/v1/users/{id}` or `GET /v1/users/{id}/credentials`) MUST NOT
include it.

**Errors**:

- `404 Not Found` `code = "user_not_found"`.
- `403 Forbidden` `code = "not_owner"` — caller is `user` and
  `{id}` is not them.

**CLI**: `portunus-server credential-issue <user-id> [--label "<text>"]`

### `POST /v1/users/{id}/credentials/{cred_id}/rotate`

Atomic: mints a new credential for the user, marks the named one
Revoked. Both writes commit in one `identity.json` snapshot.

**Response** `200 OK`:

```json
{
  "credential_id": "01HEZ...",      // the new one
  "user_id": "alice",
  "label": "ci runner #3 (rotated 2026-05-07)",
  "created_at": "...",
  "token": "<43-char NEW token>"
}
```

The old `cred_id` is now Revoked; verifying it returns
`credential_invalid`.

**Errors**:

- `404 Not Found` `code = "credential_not_found"` — `cred_id` doesn't
  belong to this user.
- `403 Forbidden` `code = "not_owner"`.

**CLI**: `portunus-server credential-rotate <cred-id>` (the user-id
is inferred from the cred-id; the call presents *the cred being
rotated* as the bearer).

### `DELETE /v1/users/{id}/credentials/{cred_id}`

Marks the credential Revoked without issuing a replacement.
Superadmin-only OR same-user.

**Response** `204 No Content`.

**CLI**: `portunus-server credential-revoke <cred-id>`

### `GET /v1/users/{id}/credentials`

Lists credentials (metadata only — `token_hash` and `token` are NEVER
emitted).

**Response** `200 OK`:

```json
{
  "credentials": [
    {
      "credential_id": "01HEXY...",
      "label": "ci runner #3",
      "created_at": "...",
      "last_used_at": "...",
      "status": "active"
    },
    {
      "credential_id": "01HABC...",
      "label": "old laptop",
      "created_at": "...",
      "last_used_at": "...",
      "status": "revoked",
      "revoked_at": "..."
    }
  ]
}
```

**CLI**: `portunus-server credential-list <user-id> [--format json|table]`

## Grant Endpoints (NEW)

All require role `superadmin`.

### `POST /v1/grants`

**Request body**:

```json
{
  "user_id": "alice",
  "client": "client-a",
  "listen_port_start": 30000,
  "listen_port_end": 30010,
  "protocols": ["tcp"],
  "note": "payments staging"
}
```

- `client`: a client name OR the literal string `"*"` for
  `ClientScope::Any`.
- `listen_port_start <= listen_port_end`, both in `1..=65535`.
- `protocols`: non-empty array of `"tcp"` and/or `"udp"`.
- `note`: optional, ≤ 128 chars.

**Response** `201 Created`:

```json
{
  "grant_id": "01HFGH...",
  "user_id": "alice",
  "client": "client-a",
  "listen_port_start": 30000,
  "listen_port_end": 30010,
  "protocols": ["tcp"],
  "note": "payments staging",
  "created_at": "..."
}
```

**Errors**:

- `404 Not Found` `code = "user_not_found"`.
- `422 Unprocessable Entity` `code = "invalid_port_range"` /
  `"empty_protocol_set"` / `"invalid_client"`.
- `403 Forbidden` `code = "role_required"`.

**CLI**: `portunus-server grant-add <user-id> --client <name|*>
--listen-ports <start>..<end> --protocols tcp,udp [--note "<text>"]`

### `GET /v1/grants[?user_id=<id>]`

**Response** `200 OK`:

```json
{
  "grants": [
    { "grant_id": "...", "user_id": "alice", "client": "client-a",
      "listen_port_start": 30000, "listen_port_end": 30010,
      "protocols": ["tcp"], "note": "payments staging",
      "created_at": "..." }
  ]
}
```

**CLI**: `portunus-server grant-list [--user <id>]
[--format json|table]`

### `DELETE /v1/grants/{grant_id}`

Cascade per R-006: scan owned rules of `grant.user_id`, remove any
rule no longer covered. Synchronous.

**Response** `200 OK`:

```json
{
  "grant_id": "01HFGH...",
  "removed_rules": [42, 43, 47]
}
```

**Errors**:

- `404 Not Found` `code = "grant_not_found"`.

**CLI**: `portunus-server grant-revoke <grant-id>`

## Rule Endpoints (CHANGED — additive)

The existing `/v1/rules` and `/v1/rules/{id}` endpoints remain at the
same paths with the same request bodies. Two changes:

1. Auth header is now mandatory (see § Authentication).
2. Responses include a new `owner` field (the `UserId` of the rule's
   creator). Existing v0.4.0 clients tolerate the extra JSON field
   per JSON-superset rules.

### `POST /v1/rules` (CHANGED)

Request body unchanged.

**Authorization**: superadmin always allowed. Non-superadmin: the
request is denied with one of `client_not_granted`,
`port_outside_grant`, `protocol_not_granted` if no grant the caller
holds covers the (client, listen-port range, protocol) tuple.

**Response** `201 Created` — adds `owner`:

```json
{
  "rule_id": 42,
  "status": "pending",
  "target_host": "10.0.0.5",
  "prefer_ipv6": false,
  "protocol": "tcp",
  "owner": "alice"
}
```

### `GET /v1/rules` (CHANGED)

**Filtering**: non-superadmin sees only rules where `owner == caller`.
Superadmin sees all rules. The query param `?owner=<id>` is
honored only for superadmin callers; non-superadmin callers ignore
the param (their view is hard-filtered to themselves).

Response objects gain `owner`.

### `GET /v1/rules/{id}/stats` (CHANGED)

Auth: superadmin always allowed; non-superadmin allowed iff
`rule.owner == caller`. Otherwise `403 not_owner`.

Response unchanged from v0.4 (no `owner` injected — the rule_id in the
URL already implies ownership context for the caller).

### `DELETE /v1/rules/{id}` (CHANGED)

Auth: superadmin always allowed; non-superadmin allowed iff
`rule.owner == caller`. Otherwise `403 not_owner`.

Response unchanged.

## CLI Exit Codes (CHANGED — additive)

The existing v0.4.0 exit-code map is preserved. New codes for the new
failure modes:

| Exit code | When |
|---|---|
| 0 | Success |
| 1 | Generic error (unchanged) |
| 2 | `already_bootstrapped` (bootstrap subcommand only) |
| 3 | Validation / argument error (unchanged) |
| 4 | `unauthenticated` / `credential_invalid` / `user_disabled` |
| 5 | RBAC denial (`client_not_granted`, `port_outside_grant`, `protocol_not_granted`, `not_owner`, `role_required`) |
| 6 | `bootstrap_required` (server returned 503 — the operator must bootstrap before any operation) |

Operators can grep for the exit code or parse the JSON `error.code`
from the CLI's stderr (the CLI passes the response body through
unchanged).

## Token plumbing in the CLI

Order of precedence:

1. `--token <value>` flag on the subcommand.
2. `PORTUNUS_OPERATOR_TOKEN` env var.
3. (none — error: `unauthenticated` before any HTTP call).

The CLI does NOT prompt for the token interactively. Pipelines and
scripts use env vars; humans paste into a one-off subshell.

## Backwards-compat summary

| v0.4.0 caller | What happens in v0.5.0 |
|---|---|
| `curl /v1/rules` (no Authorization header) | `401 unauthenticated`. |
| `curl -H "Authorization: Bearer $T" /v1/rules` (T issued via bootstrap or operator-issued superadmin) | Identical wire shape to v0.4.0 success path; response gains `owner` field. |
| `portunus-server push-rule …` (no env, no flag) | Exits `4 unauthenticated`. |
| `portunus-server push-rule …` (with `PORTUNUS_OPERATOR_TOKEN`) | Identical to v0.4.0 success path; response includes `owner`. |
| Operator's existing `server.toml` (no `operator_token`, no `identity.json`) | Server starts; every operator request returns `503 bootstrap_required`. Data plane (gRPC) is unaffected. |

The "additive on top of v0.4.0" promise from spec FR-006 is honored
at the wire-shape level (success-path bodies are byte-supersets of
v0.4) and at the data-plane level (no client→server gRPC change). The
breaking change is the mandatory auth header on the operator
surface — unavoidable by the nature of the feature, mitigated by the
two bootstrap paths in § Bootstrap.
