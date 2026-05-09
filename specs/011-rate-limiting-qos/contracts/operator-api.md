# Contract: Operator API — Rate Limiting & QoS

## 1. Per-rule cap on rule create / update

`POST /v1/rules` and `PUT /v1/rules/{rule_id}` request body gains an
optional `rate_limit` object:

```jsonc
{
  "client": "edge-01",
  "listen_port": 443,
  "protocol": "tcp",
  "targets": [
    { "host": "10.0.1.5", "port": 8443, "priority": 0 }
  ],
  "rate_limit": {
    "bandwidth_in_bps": 1048576,
    "bandwidth_out_bps": 1048576,
    "new_connections_per_sec": 50,
    "concurrent_connections": 100,
    "bandwidth_in_burst": 524288   // optional override
  }
}
```

All four caps + four burst overrides are independently optional.
Omitted = uncapped on that dimension. `concurrent_connections_burst`
field is present in the schema but rejected if non-null.

Validation errors and the capability gate are listed in
[`wire.md`](./wire.md) §4.

## 2. Per-owner cap envelope

Nested under client (Q5):

### `GET /v1/clients/{client_id}/owners/{owner_id}/rate-limit`

Returns the owner's cap envelope on this client, or 404 if unset.

```jsonc
{
  "client_name": "edge-01",
  "owner_id": "alice",
  "rate_limit": {
    "bandwidth_in_bps": 10485760,
    "concurrent_connections": 500
  },
  "updated_at_unix_ms": 1715292000000
}
```

### `PUT /v1/clients/{client_id}/owners/{owner_id}/rate-limit`

Body is a `RateLimit` object (same shape as per-rule). Idempotent;
overwrites any existing envelope for this `(client, owner)` pair.

Capability gate: the server checks the target client's last reported
`Hello.client_version` and refuses with `422
rate_limit_unsupported_by_client` if `< 0.11.0`.

### `DELETE /v1/clients/{client_id}/owners/{owner_id}/rate-limit`

Removes the envelope. Idempotent. The client receives an
`OwnerRateLimitUpdate{action: REMOVE}` push and drops the
`OwnerRateLimiter` for that owner.

### `GET /v1/clients/{client_id}/owners`

Lists every owner with rules on this client, plus a `has_rate_limit`
boolean. Used by the Web UI to populate the Owner quotas tab.

## 3. Rule read surfaces

`GET /v1/rules`, `GET /v1/rules/{rule_id}`, the CLI rule-list
rendering, and the Web UI rules table all expose the per-rule
`rate_limit` object verbatim (or omit when null).

## 4. Prometheus surface (forward-server)

Additive collectors (folded from `RuleStats` and
`StatsReport.owner_rate_limit_stats`):

```
forward_rate_limit_reject_total{client, rule, owner, reason}
forward_rate_limit_throttle_seconds_total{client, rule, owner, direction}
forward_rate_limit_active_connections{client, rule, owner}
forward_rate_limit_owner_active_connections{client, owner}
```

`reason` ∈ {`conn_concurrent`, `conn_rate`, `udp_flow_rate`,
`owner_concurrent`, `owner_conn_rate`, `owner_udp_flow_rate`}.
`direction` ∈ {`in`, `out`}. The `owner` label is the empty string
on per-rule reasons that have no owner attribution (defensive — they
all do today, but the label space is reserved).

Label cardinality envelope: `rules × owners × 6` for reject,
`rules × 2 + owners × 2` for throttle / active. Same order of
magnitude as v0.10's label envelope.

No new collectors are emitted for rules with no cap fields — the
no-cap fast path doesn't produce a `RuleStats.rate_limit` payload.

## 5. CLI delta

`forward-cli rule create` / `rule update` accept four cap flags and
four burst-override flags (`--bandwidth-in-bps`,
`--bandwidth-out-bps`, `--new-connections-per-sec`,
`--concurrent-connections`, plus `--*-burst`). All optional.

`forward-cli owner-cap` (new sub-command) wraps
`PUT/GET/DELETE /v1/clients/{id}/owners/{owner_id}/rate-limit`.

`forward-cli rule list` adds a `CAPS` column with a compact summary
(`1 MB/s in · 100 conn`); the long form adds a full `Rate limit`
section under each rule.
