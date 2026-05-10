# Contract: proto wire extension (portunus.v1, additive in v1.4)

**Phase**: 1 (design) | **Feature**: 007-multi-target-failover | **Date**: 2026-05-08

This contract defines the additive proto3 changes to `proto/portunus.proto`. All changes are non-breaking. v0.6.0 readers decode the new wire as a v0.6.0 single-target rule (per back-compat encoding rule, see below). v0.7.0 readers decode either shape.

---

## 1. New message: `Target`

```proto
// New in v1.4 (spec 007-multi-target-failover).
// One upstream within a Rule. Inline-on-rule, not addressable on its
// own. See data-model.md for the Healthy/Failed state machine that
// the client maintains per Target.
message Target {
  string host     = 1;  // IP literal or DNS name (resolver applies
                        // per-target, same TTL clamp as v0.3.0).
  uint32 port     = 2;  // 1..=65535
  uint32 priority = 3;  // lower = higher priority. Two targets MAY
                        // share a value; row order breaks ties.
}
```

## 2. Extended message: `Rule`

Two new fields, both additive. Field numbers `9` and `10` are the next free slots after v1.2's `optional bool prefer_ipv6 = 8`.

```proto
message Rule {
  uint64 rule_id = 1;
  uint32 listen_port = 2;
  string target_host = 3;       // legacy single-target (kept)
  uint32 target_port = 4;       // legacy single-target (kept)
  Protocol protocol = 5;
  uint32 listen_port_end = 6;
  uint32 target_port_end = 7;
  optional bool prefer_ipv6 = 8;

  // Additive in v1.4 (spec 007-multi-target-failover).
  //
  // Encoding contract (back-compat):
  //   - Single-target rule: `targets` is empty; `target_host` /
  //     `target_port` carry the upstream. Wire bytes identical to
  //     v0.6.0.
  //   - Multi-target rule (length >= 2): `targets` carries every
  //     upstream including index 0; `target_host` is "" and
  //     `target_port` is 0. Readers detect "multi-target" by
  //     `!targets.is_empty()`.
  //
  // A v0.6.0 reader (no awareness of field 9) drops `targets` on
  // decode and sees a rule with empty target_host/0 target_port —
  // which is invalid by v0.6.0 validation. The server-side guard
  // (R-007 in research.md) refuses to push a multi-target rule down
  // a channel whose Hello.client_version is < 0.7.0, returning HTTP
  // 422 `multi_target_unsupported_by_client` instead.
  //
  // Server enforces: targets.len() in 1..=8 (for the new shape).
  repeated Target targets = 9;

  // Additive in v1.4 (spec 007-multi-target-failover).
  // Active TCP-connect probe interval. 0 (proto3 default) means
  // "passive only — do not schedule any probe task" (FR-015).
  // Server enforces: 1..=3600 when nonzero.
  uint32 health_check_interval_secs = 10;
}
```

### Server-side push guard (HTTP layer)

When the operator submits a multi-target rule for a client whose `Hello.client_version` is `< 0.7.0`:

- HTTP response: `422 Unprocessable Entity`
- Body: `{"error":"multi_target_unsupported_by_client","client_version":"0.6.0","required":">=0.7.0"}`
- The rule is NOT persisted; no `RuleUpdate` is sent on the channel.

This avoids shipping a wire payload the receiver can't validate.

## 3. Extended message: `RuleStats`

Two additive fields, both at the end. Field numbers `11` and `12`.

```proto
message RuleStats {
  uint64 rule_id = 1;
  uint64 bytes_in = 2;
  uint64 bytes_out = 3;
  uint32 active_connections = 4;
  repeated PerPortStats per_port = 5;
  uint64 dns_failures = 6;
  uint64 datagrams_in = 7;
  uint64 datagrams_out = 8;
  uint32 active_flows = 9;
  uint64 flows_dropped_overflow = 10;

  // Additive in v1.4 (spec 007-multi-target-failover).
  //
  // Cumulative count of Healthy <-> Failed transitions across all
  // targets of this rule. Feeds the
  // `portunus_rule_target_failovers_total{client,rule}` Prometheus
  // counter (FR-018). Always 0 for single-target rules (no target
  // health state allocated).
  uint64 target_failovers_total = 11;

  // Additive in v1.4 (spec 007-multi-target-failover).
  //
  // Per-target detail. Empty for single-target rules. For multi-
  // target rules, the client populates this on every 5 s
  // StatsReport tick (see PerTargetStats below). Mirrors the
  // per-port pattern from v1.1: present in StatsReport so the
  // server can answer `--per-target` CLI / Web UI queries from
  // the same in-memory snapshot, never re-exported as Prometheus
  // series (cardinality budget — SC-006).
  repeated PerTargetStats per_target = 12;
}
```

## 4. New message: `PerTargetStats`

```proto
// New in v1.4 (spec 007-multi-target-failover).
// One per Rule.targets entry, in priority order. Indexes are
// stable for the rule's lifetime — clients of this message can
// pair `index` with `Rule.targets[index]`.
message PerTargetStats {
  uint32 index    = 1;     // row index in Rule.targets
  string host     = 2;     // mirror of Target.host
  uint32 port     = 3;     // mirror of Target.port
  uint32 priority = 4;     // mirror of Target.priority

  // Health state. 0 = Healthy (default), 1 = Failed. Encoded as
  // an enum-like uint32 instead of an enum because the v1
  // proto's enum slot for "TargetHealth" would have to live in
  // the file's enum top-level namespace, and a uint32 keeps the
  // diff additive. Future states (Degraded, etc.) extend the
  // values.
  uint32 health = 5;       // 0 = Healthy, 1 = Failed

  uint32 consecutive_failures = 6;
  // SystemTime since UNIX epoch in ms. 0 → never observed.
  uint64 last_failure_at_unix_ms = 7;
  uint64 last_success_at_unix_ms = 8;

  // Per-target lifetime byte counters (rule-lifetime, reset on
  // rule replace / client restart).
  uint64 bytes_in              = 9;
  uint64 bytes_out             = 10;
  uint64 connections_accepted  = 11;  // TCP connections OR UDP flows
}
```

## 5. Wire-compat assertions (covered by `tests/targets_wire_compat.rs`)

The following round-trip properties MUST hold and are enforced by contract tests:

- **W-1**: a v0.6.0-shaped `Rule` (only `target_host` + `target_port` populated) decoded by a v0.7 reader decodes identically; the v0.7 reader synthesises a one-element `targets` list at a higher layer (NOT in proto decode itself — proto decode leaves `targets` empty).
- **W-2**: a v0.7-multi-target `Rule` (only `targets` populated, `target_host` empty, `target_port` 0) decoded by a v0.6.0 reader (i.e. the same .proto compiled before adding field 9) silently drops field 9, leaving a rule with empty `target_host` — which v0.6.0 server-side validation rejects. The server-side guard (R-007) prevents this from ever being sent.
- **W-3**: a `Rule` with `target_host` populated AND `targets` populated MUST be rejected by the v0.7 server before it reaches the wire (FR-004).
- **W-4**: encoding a v0.6.0-shaped rule with v0.7 proto produces a `bytes_eq` payload to the same rule encoded with v0.6.0 proto.
- **W-5**: `RuleStats` round-trip: a v0.6.0-shaped `RuleStats` (no `target_failovers_total`, no `per_target`) decoded by v0.7 reads `target_failovers_total = 0` and `per_target = []`. A v0.7 `RuleStats` with both new fields populated, decoded by v0.6.0, drops fields 11 and 12 silently — the v0.6.0 reader sees the rule-level totals untouched.
- **W-6**: encoding a single-target rule's `RuleStats` (no per-target state) with v0.7 proto produces a `bytes_eq` payload to v0.6.0 proto.

## 6. What the contract does NOT change

- **Service definition** (`Control.Channel`) unchanged.
- **Hello / Welcome** unchanged.
- **RuleStatus / RuleAction** unchanged.
- **PerPortStats** unchanged (UDP per-port byte counters from v1.3 remain orthogonal to per-target counters).
- **No new RPC**.
- **No new error / status codes** at the gRPC layer — wire-level rejections continue to surface via `RuleStatus.outcome = FAILED` with a string `reason`.

## 7. Tests that ride this contract

| Test | Crate | What it asserts |
|---|---|---|
| `targets_wire_compat::roundtrip_legacy_single_target` | portunus-proto | W-1 |
| `targets_wire_compat::v06_reader_drops_targets` | portunus-proto | W-2 (uses captured v0.6.0 descriptor bytes) |
| `targets_wire_compat::reject_both_shapes` | portunus-server (HTTP layer) | W-3 / FR-004 |
| `targets_wire_compat::byte_eq_single_target` | portunus-proto | W-4, W-6 |
| `targets_wire_compat::stats_back_compat` | portunus-proto | W-5 |
