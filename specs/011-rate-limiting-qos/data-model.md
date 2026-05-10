# Phase 1 — Data Model: Rate Limiting & QoS

**Feature**: 011-rate-limiting-qos

## 1. Persistent / control-plane entities

### 1.1 `RateLimit` envelope

A bundle of four optional caps. Used both as `Rule.rate_limit` and as
the value side of the per-owner table.

| Field | Type | Notes |
|---|---|---|
| `bandwidth_in_bps` | optional u64 | bytes/sec ingress (client → upstream); null = unlimited |
| `bandwidth_out_bps` | optional u64 | bytes/sec egress (upstream → client); null = unlimited |
| `new_connections_per_sec` | optional u32 | TCP conn/sec or UDP first-packet flow/sec; null = unlimited |
| `concurrent_connections` | optional u32 | concurrent TCP conn or UDP NAT bindings; null = unlimited |
| `bandwidth_in_burst` | optional u64 | override for `bandwidth_in_bps` burst pool size in bytes |
| `bandwidth_out_burst` | optional u64 | override for `bandwidth_out_bps` burst pool size in bytes |
| `new_connections_burst` | optional u32 | override for `new_connections_per_sec` burst pool size |
| `concurrent_connections_burst` | optional u32 | unused (concurrent is a hard ceiling, not a token bucket) |

Validation:
- All cap values MUST be `> 0`. Zero is rejected at the API boundary
  with `400 validation.rate_limit_cap_zero` (FR-020).
- `*_burst` field is meaningful only when its companion cap is set;
  otherwise rejected with `400 validation.rate_limit_burst_without_rate`.
- `*_burst` MUST satisfy `rate / 100 ≤ burst ≤ rate × 60` (R-011);
  outside bounds rejects with `400 validation.rate_limit_burst_range`.
- `concurrent_connections_burst` is reserved for future use; current
  validation rejects any non-null value.

### 1.2 `Rule` (extended)

| Field | Type | Notes |
|---|---|---|
| (existing v0.10 fields) | … | unchanged |
| `rate_limit` | optional `RateLimit` | NEW; absent = legacy uncapped behaviour |

### 1.3 `OwnerRateLimit`

| Field | Type | Notes |
|---|---|---|
| `client_name` | string | matches `Rule.client` |
| `owner_id` | string | stable v0.5 RBAC owner ID |
| `rate_limit` | `RateLimit` | required body of the envelope |
| `updated_at_unix_ms` | u64 | server-set on each PUT |

Stored in new SQLite table `rate_limit_owner` keyed
`(client_name, owner_id)`.

Lifecycle:
- Created by `PUT /v1/clients/{id}/owners/{owner_id}/rate-limit`.
- Removed by explicit `DELETE` or by garbage collection when the
  owner's last rule on this client is removed (`portunus-server`
  background sweep).

### 1.4 Capability gate

If any pushed `Rule` carries a non-null `rate_limit`, OR any owner-cap
mutation targets a `client_name` whose last reported
`Hello.client_version` is below `0.11.0`, the server returns
`422 rate_limit_unsupported_by_client` and the change does not take
effect anywhere.

## 2. Runtime entities on portunus-client

### 2.1 `TokenBucket`

```text
TokenBucket {
  rate_per_sec: u64,
  burst: u64,
  tokens: AtomicU64,        // current pool, capped at `burst`
  last_refill_micros: AtomicU64,  // monotonic-clock micros
}
```

Refill is lazy: each `acquire(n)` call computes
`elapsed = now - last_refill`, mints `min(burst, tokens + elapsed × rate)`
tokens, and either decrements `n` from the pool (success) or returns
the deficit (caller sleeps `deficit / rate`).

### 2.2 `RuleRateLimiter`

| Field | Type | Notes |
|---|---|---|
| `bandwidth_in` | optional `TokenBucket` | one bucket per direction |
| `bandwidth_out` | optional `TokenBucket` |   |
| `new_connections` | optional `TokenBucket` | shared between TCP accepts and UDP first-packets |
| `concurrent_max` | optional u32 | static cap; not a bucket |
| `active_connections` | `AtomicU64` | live count, decremented on close |

Held under `Arc<RuleRateLimiter>` so a hot-reload swap is atomic.

### 2.3 `OwnerRateLimiter`

Same shape as `RuleRateLimiter`. One per owner. Looked up via
`HashMap<OwnerId, Arc<OwnerRateLimiter>>` keyed by owner id, owned
by the `portunus-client` rate-limit subsystem.

### 2.4 Reject reason enum

```text
enum RejectReason {
  ConnConcurrent,
  ConnRate,
  UdpFlowRate,
  OwnerConcurrent,
  OwnerConnRate,
  OwnerUdpFlowRate,
}
```

Used as the `reason` label on `rate_limit_reject_total`. Maps 1:1 to
proto enum `RateLimitRejectReason`.

### 2.5 Reporting buffer

Per rule and per owner, the client accumulates:

```text
RateLimitStatsAccumulator {
  reject_total_by_reason: [AtomicU64; 6],
  throttle_micros_in: AtomicU64,
  throttle_micros_out: AtomicU64,
  active_connections: AtomicU64,   // mirror of the limiter's gauge
}
```

Drained into `RuleStats.rate_limit` (per-rule) and
`StatsReport.owner_rate_limit_stats` (per-owner) on every report tick.
Fully additive to v0.10's reporting cadence.

## 3. Wire model

Additive proto entities (see `contracts/wire.md` for full text):
- New message `RateLimit` (8 optional scalar fields).
- New message `RateLimitStats` (per-reason reject counts, throttle
  micros, active gauge).
- New message `OwnerRateLimitStats` (`owner_id` + `RateLimitStats`).
- New `Rule.rate_limit = 12` (optional `RateLimit`).
- New `RuleStats.rate_limit = 16` (optional `RateLimitStats`).
- New `StatsReport.owner_rate_limit_stats = 4` (repeated
  `OwnerRateLimitStats`).
- New server-push variant `OwnerRateLimitUpdate` carried in
  `ServerMessage.payload` next free oneof tag.
- New enum `RateLimitRejectReason`.

Absent fields preserve v0.10 byte-for-byte semantics.

## 4. Storage schema delta

`V005__add_rate_limit_columns.sql`:

```sql
ALTER TABLE rules ADD COLUMN rl_bandwidth_in_bps INTEGER;
ALTER TABLE rules ADD COLUMN rl_bandwidth_out_bps INTEGER;
ALTER TABLE rules ADD COLUMN rl_new_connections_per_sec INTEGER;
ALTER TABLE rules ADD COLUMN rl_concurrent_connections INTEGER;
ALTER TABLE rules ADD COLUMN rl_bandwidth_in_burst INTEGER;
ALTER TABLE rules ADD COLUMN rl_bandwidth_out_burst INTEGER;
ALTER TABLE rules ADD COLUMN rl_new_connections_burst INTEGER;
-- (concurrent_connections_burst reserved for future, not stored)

CREATE TABLE rate_limit_owner (
    client_name TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    rl_bandwidth_in_bps INTEGER,
    rl_bandwidth_out_bps INTEGER,
    rl_new_connections_per_sec INTEGER,
    rl_concurrent_connections INTEGER,
    rl_bandwidth_in_burst INTEGER,
    rl_bandwidth_out_burst INTEGER,
    rl_new_connections_burst INTEGER,
    updated_at_unix_ms INTEGER NOT NULL,
    PRIMARY KEY (client_name, owner_id)
);
```

Schema-version range `[1,3] → [1,4]`.

## 5. State transitions

- **Cap added to a previously-uncapped rule**: hot-reload swaps
  `Arc<RuleRateLimiter>` from `None` to `Some(...)`. Any in-flight
  connections start observing the cap on the next packet boundary;
  no connection is closed.
- **Cap raised**: bucket retains its current `tokens` value; new
  refill ticks fill at the new higher `rate` toward the new higher
  `burst`. In-flight throttled connections see throughput rise.
- **Cap lowered**: bucket retains its current `tokens` value capped
  at the new (lower) `burst`; new refill ticks fill at the lower
  `rate`. In-flight throttled connections converge to the new cap on
  the next refill cycle. **Concurrent cap lowered below live count**:
  new connections are rejected against the lower cap; existing
  connections continue to natural completion (Q4, FR-011).
- **Rule deleted**: `RuleRateLimiter` is dropped after the last live
  connection closes; reporting buffer flushes one final
  `RateLimitStats` entry.
- **Owner cap deleted**: `OwnerRateLimiter` is dropped at next
  rule-push tick if no remaining rule on this client carries that
  owner; otherwise it converts to `None` (per-owner uncapped).
