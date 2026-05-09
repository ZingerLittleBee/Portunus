# Phase 1 — Data Model: PROXY Protocol & Peek Histogram

**Feature**: 010-proxy-protocol-and-peek-histogram

## 1. Persistent / control-plane entities

### 1.1 `ProxyProtocolVersion`

```text
enum ProxyProtocolVersion {
  V1,
  V2,
}
```

Semantics:
- Absent: legacy v0.9 behaviour, no upstream prelude
- `V1`: emit ASCII PROXY line
- `V2`: emit binary PROXY header

### 1.2 `RuleTarget` (extended)

The existing per-target rule entry gains one optional attribute:

| Field | Type | Notes |
|---|---|---|
| `host` | string | existing |
| `port` | u16 | existing |
| `priority` | u32 | existing |
| `proxy_protocol` | optional enum | NEW, TCP-only |

Validation:
- Only valid for TCP rules
- Invalid on UDP rules
- Absent defaults to legacy behaviour

### 1.3 Rule push capability gate

If any target in a pushed rule has `proxy_protocol != None`, the target client
must have `client_version >= 0.10.0`.

## 2. Runtime entities on forward-client

### 2.1 Upstream connection context

The selected target dial path needs two socket-derived endpoints from the
accepted downstream connection:

| Field | Type | Meaning |
|---|---|---|
| `source_addr` | `SocketAddr` | Original client peer address |
| `dest_addr` | `SocketAddr` | Accepted local address on the forward-client listener |

These are fed into PROXY header encoding before byte forwarding begins.

### 2.2 Peek histogram accumulator

Per SNI listener:

```text
PeekHistogramCounters {
  buckets: [AtomicU64; N],
  sum_micros: AtomicU64,
  count: AtomicU64,
}
```

Observed once per peek attempt, including timeout and parse failure.
Finite buckets cover observations up to and including the 3-second deadline.
Observations above the deadline still increment `count` / `+Inf` but do not
increment finite bucket counters.

### 2.3 Listener stats extension

Existing `SniListenerCounters` gains histogram state. Listener-level stats remain
scoped to `(client, listen_port)`.

## 3. Wire model

Additive fields:
- `Target.proxy_protocol` enum/field
- `SniListenerStats` gains histogram payload for bucketed peek duration counts

Absent fields preserve v0.9 semantics.
