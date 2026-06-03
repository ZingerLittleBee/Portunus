# Contract: Operator HTTP / CLI / Web UI surface

The client path segment changes from a **name** to a **client id** everywhere a specific
client is addressed. Listings continue to return the display name (plus the id).

## HTTP routes

| Before | After |
|--------|-------|
| `…/clients/{client_name}/owners` | `/v1/clients/{client_id}/owners` |
| `…/clients/{client_name}/owners/{owner_id}/rate-limit` | `/v1/clients/{client_id}/owners/{owner_id}/rate-limit` |
| (client-scoped rule/quota routes by name) | same routes keyed by `{client_id}` |

New route — **rename** (display name change):

```
PATCH /v1/clients/{client_id}
Body: { "name": "<new display name>" }
→ 200 { "client_id": "...", "name": "<new display name>" }
→ 400 if name fails relaxed validation (empty / control char / >255 bytes)
→ 404 if client_id unknown
```

Behavior contracts:
- Unknown `{client_id}` → `404` (FR-012), not a 5xx, and the message MUST NOT reveal whether a
  client with a *different* id has a colliding name (FR-013 / Constitution V).
- Rename does not affect any rule/owner/quota/usage row and does not drop a live session
  (FR-004).

## List responses

Each client entry MUST include both `client_id` and `name`. Because names are non-unique,
clients SHOULD render a short id form (e.g. last 6 chars of the ULID) alongside the name to
disambiguate (FR-010).

## CLI

The operator CLI subcommands that target a client (owner-cap, rule, quota) take a
`--client-id <ULID>` argument instead of a client name. A rename subcommand is added:
`client rename --client-id <ULID> --name "<new display>"`.

## Web UI

- Route: `/clients/:clientId` (was `/clients/:clientName`).
- List/detail/forms key requests on `client_id`, display `name`.
- A rename control on the client detail view calls `PATCH /v1/clients/{id}`.
- **Do not touch** `UserCreateForm.tsx:44`'s `^[a-z][a-z0-9-_]*$` regex — it validates
  **userId**, not client name.

## Validation message contract

Client-name validation failures return a clear, specific message naming the violated rule
(empty / contains control character / exceeds 255 bytes) — FR-011.
