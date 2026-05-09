# Contract: Wire Delta — Rate Limiting & QoS

## 1. New messages

### `RateLimit`

Eight optional scalar fields. Absent fields = uncapped on that
dimension. `*_burst` fields override the default `burst = 1 × rate`.

| Tag | Field | Type |
|---|---|---|
| 1 | `bandwidth_in_bps` | optional uint64 |
| 2 | `bandwidth_out_bps` | optional uint64 |
| 3 | `new_connections_per_sec` | optional uint32 |
| 4 | `concurrent_connections` | optional uint32 |
| 5 | `bandwidth_in_burst` | optional uint64 |
| 6 | `bandwidth_out_burst` | optional uint64 |
| 7 | `new_connections_burst` | optional uint32 |

### `RateLimitStats`

| Tag | Field | Type |
|---|---|---|
| 1 | `reject_total` | repeated `RateLimitRejectCount` |
| 2 | `throttle_micros_in` | uint64 |
| 3 | `throttle_micros_out` | uint64 |
| 4 | `active_connections` | uint32 |

### `RateLimitRejectCount`

| Tag | Field | Type |
|---|---|---|
| 1 | `reason` | `RateLimitRejectReason` |
| 2 | `total` | uint64 |

### `OwnerRateLimitStats`

| Tag | Field | Type |
|---|---|---|
| 1 | `owner_id` | string |
| 2 | `stats` | `RateLimitStats` |

### `OwnerRateLimitUpdate` (server → client push variant)

| Tag | Field | Type |
|---|---|---|
| 1 | `client_name` | string |
| 2 | `owner_id` | string |
| 3 | `rate_limit` | optional `RateLimit` |  // null = unset
| 4 | `action` | `OwnerRateLimitAction` |  // SET or REMOVE

### Enums

```text
enum RateLimitRejectReason {
  RATE_LIMIT_REJECT_REASON_UNSPECIFIED = 0;
  CONN_CONCURRENT       = 1;
  CONN_RATE             = 2;
  UDP_FLOW_RATE         = 3;
  OWNER_CONCURRENT      = 4;
  OWNER_CONN_RATE       = 5;
  OWNER_UDP_FLOW_RATE   = 6;
}

enum OwnerRateLimitAction {
  OWNER_RATE_LIMIT_ACTION_UNSPECIFIED = 0;
  SET    = 1;
  REMOVE = 2;
}
```

## 2. Extended messages

### `Rule`

Adds:

| Tag | Field | Type |
|---|---|---|
| 12 | `rate_limit` | optional `RateLimit` |

`Rule.targets[].proxy_protocol` (v0.10) is unchanged. Per-target sub-
caps are explicitly out of scope for v0.11.

### `RuleStats`

Adds:

| Tag | Field | Type |
|---|---|---|
| 16 | `rate_limit` | optional `RateLimitStats` |

### `StatsReport`

Adds:

| Tag | Field | Type |
|---|---|---|
| 4 | `owner_rate_limit_stats` | repeated `OwnerRateLimitStats` |

### `ServerMessage.payload`

Adds (next free oneof tag at proto edit time):
- `OwnerRateLimitUpdate owner_rate_limit_update`

Server pushes one of these whenever an operator API call to
`PUT | DELETE /v1/clients/{id}/owners/{owner_id}/rate-limit` mutates an
owner cap envelope on a connected client.

## 3. Compatibility

- v0.10 clients (no awareness of `Rule.rate_limit = 12` or
  `OwnerRateLimitUpdate`) silently ignore the new field/variant.
  The server's capability gate (R-006, FR-006) prevents that scenario
  from ever shipping by refusing pushes to pre-v0.11 clients with
  `422 rate_limit_unsupported_by_client` *before* the rule activates
  anywhere.
- A rule with no `rate_limit` field is byte-identical to v0.10 on the
  wire — gate enforced by the regression bench (SC-004 / Constitution II).
- `RuleStats.rate_limit` and `StatsReport.owner_rate_limit_stats`
  are absent for clients with no caps; v0.10 servers handling such
  reports see no behaviour change.

## 4. Validation surface

| Code | Reason |
|---|---|
| `400 validation.rate_limit_cap_zero` | A cap value was 0 |
| `400 validation.rate_limit_burst_without_rate` | `*_burst` set without companion `rate` |
| `400 validation.rate_limit_burst_range` | `burst` outside `[rate/100, rate*60]` |
| `400 validation.rate_limit_burst_unsupported` | `concurrent_connections_burst` set |
| `422 rate_limit_unsupported_by_client` | Push targets a pre-v0.11 client |
