# Contract: operator HTTP + CLI

**Phase**: 1 (design) | **Feature**: 007-multi-target-failover | **Date**: 2026-05-08

This contract defines the operator-facing surface for multi-target rules. All changes are additive: every v0.6.0 request body and every v0.6.0 response shape continues to work unchanged. The new `targets[]` shape rides alongside.

---

## 1. `POST /v1/rules` — create or replace a rule

### Accepted request bodies

The HTTP layer accepts EITHER the legacy shape OR the new shape. Supplying both, or neither, is a 400 (FR-004).

#### Legacy shape (v0.6.0, still accepted)

```json
{
  "client": "edge-01",
  "listen_port": 8080,
  "listen_port_end": 8080,
  "target_host": "example.com",
  "target_port": 80,
  "target_port_end": 80,
  "protocol": "tcp",
  "prefer_ipv6": false
}
```

#### New shape (v0.7.0)

```json
{
  "client": "edge-01",
  "listen_port": 8080,
  "listen_port_end": 8080,
  "protocol": "tcp",
  "prefer_ipv6": false,
  "targets": [
    {"host": "primary.example.com",   "port": 80, "priority": 0},
    {"host": "secondary.example.com", "port": 80, "priority": 1}
  ],
  "health_check_interval_secs": 30
}
```

`priority` is optional in the request — server fills it from the row index when omitted (so `[{host:"a", port:80}, {host:"b", port:80}]` is equivalent to priorities `[0, 1]`).

### Validation (server-side, before reaching the wire)

| Code | Condition | Status |
|---|---|---|
| `rule_shape_conflict` | both `target_host`/`target_port` AND `targets` are present | 400 |
| `rule_shape_missing` | neither `target_host` nor `targets` is present | 400 |
| `targets_empty` | `targets` is present but empty | 400 |
| `targets_too_many` | `targets.len() > 8` | 400 |
| `target_invalid_host` | any `target.host` fails resolver-syntax validation | 400 |
| `target_invalid_port` | any `target.port` not in `1..=65535` | 400 |
| `targets_duplicate` | two targets share `(host, port)` (FR-005) | 400 |
| `health_check_interval_out_of_range` | `health_check_interval_secs` set and not in `1..=3600` | 400 |
| `multi_target_unsupported_by_client` | targets.len() ≥ 2 but the target client's last-known `Hello.client_version` < `0.7.0` (R-007) | 422 |
| `forbidden_listen_port` | RBAC envelope check fails on `(client, listen_port..=listen_port_end, protocol)` (unchanged from 005) | 403 |

The targets list is NOT subject to RBAC checks (FR-021).

### Response

```json
{
  "rule_id": 42,
  "client": "edge-01",
  "listen_port": 8080,
  "listen_port_end": 8080,
  "protocol": "tcp",
  "prefer_ipv6": false,
  "targets": [
    {"host": "primary.example.com",   "port": 80, "priority": 0},
    {"host": "secondary.example.com", "port": 80, "priority": 1}
  ],
  "health_check_interval_secs": 30,
  "activation": {"outcome": "activated", "request_id": "01J..."}
}
```

A v0.6.0-shaped request always echoes back a length-1 `targets` array — operators that want to keep the old single-target mental model can ignore the field; operators that want to upgrade to multi-target can re-push with a longer list.

## 2. `GET /v1/rules/{id}` — fetch a single rule

Response carries `targets[]` (length ≥ 1) plus per-target current health snapshot. Default response is per-target health summary; raw counters live on the stats endpoint (§4).

```json
{
  "rule_id": 42,
  "client": "edge-01",
  "listen_port": 8080,
  "listen_port_end": 8080,
  "protocol": "tcp",
  "targets": [
    {"host": "primary.example.com", "port": 80, "priority": 0,
     "health": {"state": "healthy", "consecutive_failures": 0,
                "last_failure_at": null, "last_success_at": "2026-05-08T10:42:11Z"}},
    {"host": "secondary.example.com", "port": 80, "priority": 1,
     "health": {"state": "healthy", "consecutive_failures": 0,
                "last_failure_at": "2026-05-08T10:30:55Z", "last_success_at": "2026-05-08T10:42:11Z"}}
  ],
  "health_check_interval_secs": 30
}
```

For single-target rules, the `health` field on the only target is `null` (no health state allocated; FR-002 byte-identity).

## 3. `GET /v1/rules` — list rules

Each entry carries the same `targets[]` shape as §2. No new query parameters; existing pagination unchanged.

## 4. `GET /v1/rules/{id}/stats[?per_target=true]`

### Default response (no `per_target` query param)

Identical shape to v0.6.0, plus one new field:

```json
{
  "rule_id": 42,
  "bytes_in": 12345678,
  "bytes_out": 9876543,
  "active_connections": 17,
  "dns_failures": 0,
  "datagrams_in": 0, "datagrams_out": 0,
  "active_flows": 0, "flows_dropped_overflow": 0,
  "target_failovers_total": 3
}
```

For single-target rules `target_failovers_total` is always 0.

### With `?per_target=true`

Response gains a `per_target[]` array. Single-target rules return `per_target: []` regardless of the flag (FR-002 invariant I-3).

```json
{
  "rule_id": 42,
  "bytes_in": 12345678,
  "bytes_out": 9876543,
  "active_connections": 17,
  "dns_failures": 0,
  "datagrams_in": 0, "datagrams_out": 0,
  "active_flows": 0, "flows_dropped_overflow": 0,
  "target_failovers_total": 3,
  "per_target": [
    {"index": 0, "host": "primary.example.com", "port": 80, "priority": 0,
     "health": "healthy", "consecutive_failures": 0,
     "last_failure_at": "2026-05-08T10:30:55Z", "last_success_at": "2026-05-08T10:42:11Z",
     "bytes_in": 8000000, "bytes_out": 6000000, "connections_accepted": 12},
    {"index": 1, "host": "secondary.example.com", "port": 80, "priority": 1,
     "health": "healthy", "consecutive_failures": 0,
     "last_failure_at": null, "last_success_at": "2026-05-08T10:30:42Z",
     "bytes_in": 4345678, "bytes_out": 3876543, "connections_accepted": 5}
  ]
}
```

## 5. `GET /v1/rules/{id}/stats/stream` (SSE, from spec 006)

The 5-second-cadence SSE channel from spec 006 gains the same two fields. Each `data:` event payload is the §4 default body shape unless the SSE subscriber added `?per_target=true` on subscribe (the per-target detail rides the same channel — no second stream). The Web UI rule detail page subscribes with `?per_target=true` (FR-019).

## 6. `GET /metrics` — Prometheus

ONE new series per rule, regardless of target count:

```text
# HELP forward_rule_target_failovers_total Total Healthy<->Failed target health transitions on this rule.
# TYPE forward_rule_target_failovers_total counter
forward_rule_target_failovers_total{client="edge-01",rule="42"} 3
```

No per-target time series are exported (FR-018, SC-006). Per-target counters are query-only via §4.

## 7. CLI: `push-rule`

### Legacy form (v0.6.0, still works)

```sh
forward-server push-rule edge-01 8080 example.com:80 --protocol tcp
```

Equivalent to a v0.6.0-shaped POST. Builds a one-element targets list under the hood.

### New repeatable `--target` form

```sh
forward-server push-rule edge-01 8080 \
    --target primary.example.com:80 \
    --target secondary.example.com:80 \
    --protocol tcp \
    --health-check-interval-secs 30
```

`--target` is repeatable; first occurrence has priority 0, next has priority 1, etc. Operator can override with `host:port@priority` syntax: `--target secondary.example.com:80@5`.

### `--targets-json` form (machine-friendly)

```sh
forward-server push-rule edge-01 8080 --protocol tcp \
    --targets-json '[{"host":"primary.example.com","port":80,"priority":0},
                     {"host":"secondary.example.com","port":80,"priority":1}]'
```

`--target`, `--targets-json`, and the legacy positional form are mutually exclusive — providing more than one form is a CLI error before any HTTP request is issued.

## 8. CLI: `rule-stats [--per-target]`

### Default output (unchanged from v0.6.0, plus one line)

```text
Rule 42 (edge-01, tcp, listen 8080)
  bytes_in:                12345678
  bytes_out:                9876543
  active_connections:            17
  target_failovers_total:         3
```

### `--per-target` output

```text
Rule 42 (edge-01, tcp, listen 8080)
  bytes_in:                12345678
  bytes_out:                9876543
  active_connections:            17
  target_failovers_total:         3
  per-target detail:
    [0] primary.example.com:80   priority=0  health=healthy  fails=0  in=8000000  out=6000000  conns=12  last_fail=2026-05-08T10:30:55Z  last_ok=2026-05-08T10:42:11Z
    [1] secondary.example.com:80 priority=1  health=healthy  fails=0  in=4345678  out=3876543  conns=5   last_fail=-                      last_ok=2026-05-08T10:30:42Z
```

For single-target rules, `--per-target` prints `per-target detail: (single-target rule, no per-target state)` and exits 0 (no error).

## 9. Auth and audit

- All endpoints continue to require the existing operator bearer token (RBAC envelope from spec 005).
- `POST /v1/rules` continues to write an `audit_event` row with `actor_id`, `client`, `listen_port_range`, `protocol`, `outcome`. The targets list is NOT part of the audit envelope (mirrors FR-021 — targets are operator-side detail, not a privilege boundary).
- Health-state transitions emit `tracing` JSON events `event = "rule.target.health_changed"` on the client side. These are not part of the operator HTTP audit ring (which gates on operator actions), but flow into the central log pipeline for forensic correlation.

## 10. Contract tests covering this surface

| Test | Crate | What it asserts |
|---|---|---|
| `rules_multi_target_contract::accept_legacy_shape` | forward-server | legacy POST round-trips to a length-1 `targets[]` response |
| `rules_multi_target_contract::accept_new_shape` | forward-server | new POST round-trips and echoes targets in priority order |
| `rules_multi_target_contract::reject_both_shapes` | forward-server | both fields present → 400 `rule_shape_conflict` |
| `rules_multi_target_contract::reject_neither` | forward-server | neither present → 400 `rule_shape_missing` |
| `rules_multi_target_contract::reject_duplicates` | forward-server | duplicate (host,port) → 400 `targets_duplicate` |
| `rules_multi_target_contract::reject_old_client` | forward-server | multi-target push to v0.6.0 client → 422 `multi_target_unsupported_by_client` |
| `rules_multi_target_contract::stats_default_back_compat` | forward-server | default stats response carries `target_failovers_total` field even for single-target rules |
| `rules_multi_target_contract::stats_per_target_query` | forward-server | `?per_target=true` populates `per_target[]` for multi-target rules and returns `per_target: []` for single-target |
| `rules_multi_target_contract::metrics_cardinality` | forward-server | `/metrics` adds exactly 1 new series per rule regardless of `targets.len()` |
