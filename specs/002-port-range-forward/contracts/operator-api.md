# Operator Interface Contract — v1.1 deltas

**Feature**: 002-port-range-forward
**Inherits from**: `specs/001-tcp-forward-mvp/contracts/operator-api.md`

This document records the **additive** changes to the v1 operator
surface (CLI + loopback HTTP API). Everything not mentioned here is
unchanged from v1. All v1 path prefixes, exit codes, and error code
strings remain frozen — new behaviors extend the existing verbs.

---

## CLI deltas

### `portunus-server push-rule <client> <listen> <target> [...]`

**Updated** to accept range syntax on `<listen>` and `<target>`. The
verb name and exit codes are unchanged from v1.

| Argument | Old | New |
|---|---|---|
| `<listen>` | `<port>` | `<port>` OR `<start>-<end>` |
| `<target>` | `<host>:<port>` | `<host>:<port>` OR `<host>:<start>-<end>` |

Examples:
```
# Single port (unchanged from v1)
portunus-server push-rule edge-01 18080 10.0.0.5:8080

# Range, same offset
portunus-server push-rule edge-01 30000-30050 10.0.0.5:30000-30050

# Range, shifted offset
portunus-server push-rule edge-01 30000-30050 10.0.0.5:40000-40050
```

**Validation** (FR-002):
- If `<listen>` carries a range, `<target>` MUST carry a range of the
  same length. Asymmetric forms (`30000-30050` vs single port, or
  ranges of different lengths) are rejected with exit code `3`
  (`invalid_target`) and a stderr message naming the mismatch.
- `start ≤ end` on both sides (exit `3`, message `range_inverted`).
- Range size MUST be `≤ range_rule_max_ports` (default 1024); larger
  ranges are rejected with exit `3` (`exceeds_cap`) and a message
  naming the cap.

**Conflict handling** (FR-010):
- Exit `5` (`port_in_use`) — frozen v1 code, reused. The stderr
  message names at least one offending port number (e.g.,
  `port_in_use: 30005 (overlaps rule 7 on edge-01)`).

**Activation timing**: A 100-port range push completes in roughly the
same wall-clock budget as a single-port push because all binds happen
in a single round-trip (the server pushes one `RuleUpdate`; the client
binds N listeners and reports one `RuleStatus`). The default
`--ack-timeout` (2 s) is sufficient for `range_rule_max_ports = 1024`
on a healthy system; operators on slow hardware may raise it.

### `portunus-server remove-rule <rule_id>`

**Unchanged.** Removing a range rule releases every listener under
that rule via the same drain path as today.

### `portunus-server list-rules [--client <name>] [--format text|json]`

**Updated** output to render ranges:

- `--format text`: the existing `PORT` column widens and shows
  `30000-30050` for range rules (and `18080` unchanged for single-port).
  A new `SIZE` column is appended.
- `--format json`: each rule object gains optional fields
  `listen_port_end` and `target_port_end` (omitted for single-port
  rules) plus a derived `range_size` integer (always present, equals
  1 for single-port).

### `portunus-server rule-stats <rule_id> [--per-port] [--format text|json]`

**Updated** with a new optional `--per-port` flag (FR-011).

- Default behavior (no flag): unchanged — one row of aggregate
  counters per rule.
- `--per-port` flag: returns the aggregate row PLUS one row per port
  in the range. Output:

```
# rule-stats 8 --per-port (text format, abbreviated)
rule_id=8 client=edge-01 range=30000-30050 bytes_in=4123987 bytes_out=4123987 active=12 updated_at=2026-05-07T12:00:00Z

PORT     BYTES_IN   BYTES_OUT  ACTIVE
30000    1024000    1024000    3
30001    0          0          0
30002    512000     512000     1
...
30050    0          0          0
```

JSON form adds a `per_port` field to the existing stats object:
```json
{
  "rule_id": 8,
  "client_name": "edge-01",
  "listen_port": 30000,
  "listen_port_end": 30050,
  "target_port": 30000,
  "target_port_end": 30050,
  "bytes_in": 4123987,
  "bytes_out": 4123987,
  "active_connections": 12,
  "updated_at": "2026-05-07T12:00:00Z",
  "per_port": [
    { "listen_port": 30000, "bytes_in": 1024000, "bytes_out": 1024000, "active_connections": 3 },
    { "listen_port": 30001, "bytes_in": 0,       "bytes_out": 0,       "active_connections": 0 }
  ]
}
```

For single-port rules, `--per-port` returns one row whose `listen_port`
equals the rule's `listen_port` (graceful degradation — operators can
script against the same shape).

---

## HTTP API deltas

### `POST /v1/rules` — accepts range fields

The request body gains two optional fields:

```json
{
  "client": "edge-01",
  "listen_port": 30000,
  "listen_port_end": 30050,
  "target_host": "10.0.0.5",
  "target_port": 30000,
  "target_port_end": 30050,
  "protocol": "tcp",
  "ack_timeout_secs": 5
}
```

Rules:
- `listen_port_end` / `target_port_end` are **co-required** —
  specifying one without the other returns 400 with a
  `mismatched_range` code (mapped from `OperatorError::InvalidTarget`).
- Length-mismatch and inverted ranges return 400.
- Cap exceeded returns 400 with a body whose `error.code` is
  `exceeds_cap` and `error.message` names the requested size and the
  cap.

Response is unchanged: `201` + `{"rule_id": N}`.

### `GET /v1/rules` — returns range fields

Each rule object in the response array gains optional
`listen_port_end` / `target_port_end` (omitted for single-port rules)
and a derived `range_size` integer (always present).

### `GET /v1/rules/{rule_id}/stats?per_port=true`

**New optional query parameter** `per_port=true` (FR-011). When
omitted, behavior is unchanged.

When set, the response gains a `per_port` array of objects with shape
`{ listen_port, bytes_in, bytes_out, active_connections }` — one entry
per port in the rule's listen range. For single-port rules, the array
contains exactly one entry (graceful degradation — see CLI section).

If the server has not yet received a per-port snapshot from the client
(e.g., immediately after activation, before the next stats interval),
the `per_port` array MAY be empty; clients of the API should retry
after `stats_report_interval_secs`.

### Error codes

No new top-level error codes. All range-specific failures map to
existing codes:

| Condition | HTTP | `error.code` | CLI exit |
|---|---|---|---|
| Inverted range, length mismatch, malformed range syntax | 400 | `invalid_target` | 3 |
| Range exceeds cap | 400 | `exceeds_cap` (NEW string under existing 400 family) | 3 |
| Range overlaps existing rule | 409 | `port_in_use` (reused; message names the port) | 5 |
| `range_rule_max_ports` config invalid at startup | (server fails to start) | n/a | n/a |

Note: `exceeds_cap` is a new error code string but reuses HTTP 400 and
exit 3 from the same family that already handles `invalid_target`. New
codes within an existing family are explicitly allowed by the v1
stability guarantee ("new codes may be added, existing codes will not
be renamed").

---

## Stability guarantees (v1.1)

- All v1.0 error codes, HTTP paths, and CLI exit codes remain frozen.
- New JSON fields (`listen_port_end`, `target_port_end`, `range_size`,
  `per_port`) are additive; v1.0 consumers that ignore unknown fields
  continue to work.
- New CLI flag `--per-port` is opt-in.
- New error code string `exceeds_cap` is additive within the existing
  HTTP 400 / CLI exit 3 family.
- New query parameter `?per_port=true` is opt-in; absent → identical
  behavior to v1.0.
