# On-Disk Persistence Contract — v1.1 deltas

**Feature**: 002-port-range-forward
**Inherits from**: `specs/001-tcp-forward-mvp/contracts/persistence.md`

This document records the **additive** persistence changes for range
rules. The token store, TLS material, and `server.toml` schema are
otherwise unchanged — the only new field is the optional cap config
on `ServerConfig` plus optional fields on persisted rules (when /
where rules are persisted).

---

## ServerConfig (`server.toml`) — one new optional field

```toml
# v0.1.0 fields unchanged. New optional field below.
range_rule_max_ports = 1024   # default; loaded with serde(default)
```

- Type: `u32`.
- Default: `1024` (matches Linux default soft `RLIMIT_NOFILE`; see
  spec § Clarifications Q1).
- Loader behavior: missing field → 1024. Present with value `0` → fail
  to start with a clear error (a cap of 0 means "no rules ever
  succeed", which is almost certainly a misconfig).
- Operators on hosts with raised `RLIMIT_NOFILE` may raise this; on
  stricter hosts (containerized, hardened systemd) they should lower
  it accordingly.

---

## Rules persistence — additive optional fields

If/when the server persists its rule store on disk (currently in-
memory per spec 001-tcp-forward-mvp `data-model.md` § Rule "Storage"),
the additive shape MUST be:

```json
{
  "version": 1,
  "rules": [
    {
      "id": 7,
      "client_name": "edge-01",
      "listen_port": 18080,
      "target_host": "10.0.0.5",
      "target_port": 8080,
      "protocol": "Tcp",
      "state": { "kind": "active" },
      "created_at": "2026-05-06T12:00:00Z",
      "last_state_change_at": "2026-05-06T12:00:00Z"
    },
    {
      "id": 8,
      "client_name": "edge-01",
      "listen_port": 30000,
      "listen_port_end": 30050,
      "target_host": "10.0.0.5",
      "target_port": 30000,
      "target_port_end": 30050,
      "protocol": "Tcp",
      "state": { "kind": "active" },
      "created_at": "2026-05-07T12:00:00Z",
      "last_state_change_at": "2026-05-07T12:00:00Z"
    }
  ]
}
```

**Invariants**:

- `version` stays `1`. The new fields are optional and decode to
  `None` when absent (`#[serde(default)]` on the Rust struct), so a
  v0.1.0 rules file loads on a v0.2.0 server unmodified — **no
  migration step is required** (FR-005).
- `listen_port_end` is `null` / omitted (single-port rule, equivalent
  to v0.1.0 shape) OR an integer `>= listen_port` (range rule).
- `target_port_end` MUST be present iff `listen_port_end` is present,
  and the resulting target range MUST have the same length as the
  listen range (loader rejects mismatched files).
- The atomic write protocol (tmp file + fsync + rename + parent
  fsync) is unchanged.

**Forward compatibility**: a future v0.3.0 server reading a v0.2.0
rules file behaves identically; future schema changes that introduce
incompatible shapes MUST bump the `version` field per the existing
migration model.

---

## Token store — unchanged

`tokens.json` schema, layout, and write protocol are unchanged from
v0.1.0.

## TLS material — unchanged

`server.crt` / `server.key` shape and lifecycle are unchanged.

---

## Migration model — no migration in v1.1

A server upgrade from v0.1.0 to v0.2.0 needs **no data migration**:

- `tokens.json`: v1, no change.
- Rules persistence (when present): v1, additive optional fields.
  v0.1.0 records load as `range_size = 1` rules and behave identically
  to today.
- `server.toml`: existing fields unchanged. `range_rule_max_ports` is
  optional and defaults to 1024.

A downgrade from v0.2.0 back to v0.1.0 is **not supported** if any
range rule (`listen_port_end != null`) has been written to disk; the
v0.1.0 server's deserializer will not understand the new fields. This
is documented as an operator constraint; range rules are an opt-in
feature, so operators who avoid pushing them retain bidirectional
upgrade safety.
