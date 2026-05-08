# Contract: Wire Protocol Delta — SNI Routing

**Feature**: 009-tls-sni-routing
**Phase**: 1 (Design & Contracts)
**Authoritative reference**: [`../research.md`](../research.md) §R-005,
[`../design.md`](../design.md)

This contract enumerates every byte added to or read from the bidi gRPC
wire by v0.9. Anything not listed here is **byte-stable** with v0.8.

---

## 1. `proto/forward.proto` delta

### 1.1 `Rule` — add SNI selector

```proto
message Rule {
  // ... existing fields 1..=10 unchanged

  // Additive in v1.5 (spec 009-tls-sni-routing). Wire field number 11.
  // Absent → plain TCP forward / TLS-only fallback (depending on listener mode).
  // Present → host or `*.suffix` pattern; ASCII, lowercased, ≤ 253 chars.
  // v0.8 readers (no awareness of field 11) drop it on decode and see a
  // plain-TCP rule. The forward-server capability gate (sni_unsupported_by_client,
  // HTTP 422) prevents a server from ever sending such a rule down a v0.8
  // channel — see contracts/operator-api.md §3.
  optional string sni_pattern = 11;
}
```

### 1.2 `RuleStats` — add SNI hit counters

```proto
message RuleStats {
  // ... existing fields 1..=12 unchanged
  //   field 11 = target_failovers_total (v0.7) — DO NOT REUSE
  //   field 12 = per_target              (v0.7) — DO NOT REUSE

  // Additive in v1.5 (spec 009-tls-sni-routing). Field numbers 13/14/15
  // continue after v0.7. All three are monotonic; default-zero on the wire.
  // Reset on rule replace / client restart, matching the v0.7 convention
  // for runtime counters.
  uint64 sni_route_exact_total    = 13;
  uint64 sni_route_wildcard_total = 14;
  uint64 sni_route_fallback_total = 15;
}
```

### 1.3 New `SniListenerStats` message

```proto
// New in v1.5 (spec 009-tls-sni-routing). One per active SNI listener
// on the client. Carried alongside RuleStats in StatsReport, because miss
// and parse-failure events have no rule_id attribution — they happen
// before rule selection.
message SniListenerStats {
  uint32 listen_port                       = 1;
  uint64 sni_route_miss_total              = 2;
  uint64 client_hello_parse_failures_total = 3;
}
```

### 1.4 `StatsReport` — carry the new listener-level message

```proto
message StatsReport {
  uint64 sent_at_unix_ms = 1;
  repeated RuleStats stats = 2;

  // Additive in v1.5; empty for clients with no SNI listener.
  repeated SniListenerStats sni_listener_stats = 3;
}
```

### 1.5 `RuleUpdate` — UNCHANGED

`RuleUpdate { request_id, action, rule }` is unchanged. SNI rule
membership grouping is performed on the client (`PortGroupManager`)
without any new wire envelope (R-005).

---

## 2. Byte-stability invariant

When `Rule.sni_pattern` is absent (or empty) and
`StatsReport.sni_listener_stats` is empty, the v0.9 encoding for that
message MUST be byte-identical to the v0.8 encoding of the same logical
content.

This is testable with a deterministic round-trip:

```text
v0.8 encoded bytes  →  v0.9 decode  →  v0.9 encode  =  v0.8 encoded bytes
```

---

## 3. Capability declaration

A v0.9 forward-client MUST declare its version in the existing `Hello`
message (no new field):

```proto
message Hello {
  // ... existing fields
  string client_version = N;  // unchanged; e.g. "0.9.0"
}
```

The forward-server gates SNI rule pushes against this value (see
[`contracts/operator-api.md`](./operator-api.md) §3).

---

## 4. Field-number registry (audit trail for future specs)

| Message | Field | Tag | First seen | Spec |
|---|---|---|---|---|
| `Rule` | `sni_pattern` | 11 | v1.5 | 009-tls-sni-routing |
| `RuleStats` | `sni_route_exact_total` | 13 | v1.5 | 009-tls-sni-routing |
| `RuleStats` | `sni_route_wildcard_total` | 14 | v1.5 | 009-tls-sni-routing |
| `RuleStats` | `sni_route_fallback_total` | 15 | v1.5 | 009-tls-sni-routing |
| `SniListenerStats` | `listen_port` | 1 | v1.5 | 009-tls-sni-routing |
| `SniListenerStats` | `sni_route_miss_total` | 2 | v1.5 | 009-tls-sni-routing |
| `SniListenerStats` | `client_hello_parse_failures_total` | 3 | v1.5 | 009-tls-sni-routing |
| `StatsReport` | `sni_listener_stats` | 3 | v1.5 | 009-tls-sni-routing |

The next free field on `Rule` is **12**. The next free on `RuleStats` is
**16**. The next free on `StatsReport` is **4**.

---

## 5. Contract test plan

Tests live in `crates/forward-proto/tests/`.

| File | Asserts |
|---|---|
| `sni_wire_compat.rs::roundtrip_rule_field_11` | A `Rule` with `sni_pattern = Some("api.example.com")` round-trips; absent encoding equals v0.8. |
| `sni_wire_compat.rs::roundtrip_rule_stats_13_14_15` | `RuleStats` with all three counters non-zero round-trips; absent encoding equals v0.8. |
| `sni_wire_compat.rs::roundtrip_sni_listener_stats` | `StatsReport` with non-empty `sni_listener_stats` round-trips; empty equals v0.8. |
| `sni_wire_compat.rs::negative_rule_stats_11_12_unchanged` | A `RuleStats` whose v0.7 fields 11 (`target_failovers_total`) and 12 (`per_target`) are set to non-zero / non-empty values and **no SNI fields** are set produces the **exact same** byte sequence as v0.8 encoding of the same logical content. (HIGH-1 from round-3 review.) |
| `sni_wire_compat.rs::wire_layout_documented` | Reads this very file and asserts the field-number registry table matches the actual `forward.proto` declarations (uses `prost-build` reflection or string-grep the proto file). Prevents drift. |
