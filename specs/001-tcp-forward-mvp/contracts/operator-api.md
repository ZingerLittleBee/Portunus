# Operator Interface Contract

**Feature**: 001-tcp-forward-mvp
**Surface**: CLI subcommands of `portunus-server` AND a loopback HTTP API.
The two surfaces are 1:1 — the CLI is a thin wrapper that calls the HTTP
API when one is reachable on `operator_http_listen`, and falls back to
in-process execution otherwise. This contract defines both.

Authorisation in MVP: local UNIX shell access on the server host
(FR-022). The HTTP listener binds to loopback only and performs no
additional authentication.

---

## CLI

### `portunus-server provision-client <name> [--out <path>]`

Creates a fresh credential bundle for `<name>`.

- `<name>`: 1–63 chars, DNS-label shape (lowercase, alphanum + hyphen).
- `--out <path>`: default `<cwd>/<name>.bundle.json`.

**Exit codes**:
- `0` — bundle written; absolute path printed to stdout.
- `2` — `client_already_exists`. No file written. (FR-004 / Q2)
- `3` — invalid `<name>`.
- `1` — any other error (e.g., I/O writing the bundle).

**Stderr** (on success): one structured log line of `event=audit.provision`.

### `portunus-server revoke <name>`

Marks the token for `<name>` as revoked. Disconnects the client if currently
connected. Idempotent — revoking an already-revoked or non-existent name
returns exit `0` (with a stderr note for the non-existent case).

**Exit codes**:
- `0` — revocation persisted.
- `1` — I/O error.

### `portunus-server list-clients [--format text|json]`

Lists the union of {provisioned clients, currently-connected clients}.

`--format text` (default): table.
`--format json`: array of objects:
```json
[
  {
    "client_name": "edge-01",
    "provisioned_at": "2026-05-06T12:00:00Z",
    "revoked_at": null,
    "connected": true,
    "remote_addr": "203.0.113.5:51234",
    "connected_at": "2026-05-06T12:01:30Z"
  }
]
```

### `portunus-server push-rule <client> <listen_port> <target_host>:<target_port> [--protocol tcp]`

Pushes a forwarding rule to the named client. Returns the assigned `rule_id`.

**Exit codes**:
- `0` — rule pushed and acknowledged as `Active` within `--ack-timeout` (default 2 s). Prints `rule_id` to stdout.
- `4` — `client_not_connected` (FR-014).
- `5` — `port_in_use` (a rule with the same `(client, listen_port)` already exists in `Active` or `Failed` state — Q4).
- `6` — `activation_failed`: client returned a `FAILED` outcome within the ack window. The rule IS retained in `Failed` state on the server (Q4); operator must `remove-rule` to free the port. Stderr names the reason.
- `7` — `ack_timeout`: client did not respond within `--ack-timeout`. The rule remains in `Pending`. Operator may `list-rules` and decide whether to `remove`.
- `1` — other.

### `portunus-server remove-rule <rule_id>`

Removes a rule (any state). Idempotent.

**Exit codes**:
- `0` — removed.
- `8` — `rule_not_found`.

### `portunus-server list-rules [--client <name>] [--format text|json]`

Lists rules with their state. Useful for finding `Failed` rules to clean up.

### `portunus-server rule-stats <rule_id> [--format text|json]`

Returns current per-rule stats from the server's cache (last known
`bytes_in`, `bytes_out`, `active_connections`). Stats are refreshed by the
client every `stats_report_interval_secs` (default 5 s).

---

## HTTP API

Bound to `operator_http_listen` (default `127.0.0.1:7080`). All endpoints
return JSON. Errors use the shape:
```json
{ "error": { "code": "client_already_exists", "message": "…" } }
```
The `code` values are stable; the `message` is human-oriented and may
change.

| Method | Path | Body | 2xx response | Maps to CLI |
|---|---|---|---|---|
| `POST` | `/v1/clients` | `{"name": "edge-01"}` | `201` + bundle JSON | `provision-client` |
| `POST` | `/v1/clients/{name}/revoke` | (none) | `204` | `revoke` |
| `GET` | `/v1/clients` | — | `200` + list | `list-clients` |
| `POST` | `/v1/rules` | `{"client": "edge-01", "listen_port": 18080, "target_host": "10.0.0.5", "target_port": 8080, "protocol": "tcp"}` | `201` + `{ "rule_id": 7 }` | `push-rule` |
| `DELETE` | `/v1/rules/{rule_id}` | — | `204` | `remove-rule` |
| `GET` | `/v1/rules` | optional `?client=…` | `200` + list | `list-rules` |
| `GET` | `/v1/rules/{rule_id}/stats` | — | `200` + stats | `rule-stats` |

| HTTP status | When |
|---|---|
| 400 | malformed body, invalid name shape |
| 404 | `client_not_found` / `rule_not_found` |
| 409 | `client_already_exists` / `port_in_use` |
| 422 | `client_not_connected`, `activation_failed` |
| 504 | `ack_timeout` |

---

## Stability guarantees (MVP)

- The set of error `code` strings is **frozen** for v1 of the API; new
  codes may be added, existing codes will not be renamed.
- The HTTP path prefix is `/v1/`; a breaking redesign of the HTTP surface
  becomes `/v2/`.
- The CLI exit-code numbers above are **frozen** for v1.
