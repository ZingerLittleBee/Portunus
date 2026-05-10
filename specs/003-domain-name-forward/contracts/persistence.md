# Persistence schema deltas (v0.3.0)

Additive overlay on the v0.2.0 server-side rule store and on the
client-side credential bundle. Backward-compatible: a v0.2.0 on-disk
store loads unchanged; a v0.3.0 on-disk store with no DNS rules and
no `prefer_ipv6` flags written is byte-identical to a v0.2.0 store
(round-trip through serde).

---

## `rules.json` (server-side rule store)

### v0.2.0 entry shape (for context)

```json
{
  "rule_id": 42,
  "client_name": "edge-01",
  "listen_port": 8080,
  "target_host": "192.168.1.10",
  "target_port": 80,
  "listen_port_end": null,
  "target_port_end": null,
  "request_id": "01H...",
  "status": "Active"
}
```

### v0.3.0 entry shape — DELTA

```json
{
  "rule_id": 42,
  "client_name": "edge-01",
  "listen_port": 8443,
  "target_host": "api.example.com",         // CHANGE: may now be a DNS name
  "target_port": 443,
  "listen_port_end": null,
  "target_port_end": null,
  "request_id": "01H...",
  "status": "Active",
  "prefer_ipv6": false                      // NEW: optional, defaults to false on absence
}
```

**serde wire**:

```rust
// in portunus-server/src/rules.rs
struct PersistedRule {
    rule_id: RuleId,
    client_name: ClientName,
    listen_port: u16,
    target_host: String,
    target_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    listen_port_end: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_port_end: Option<u16>,
    request_id: RequestId,
    status: RuleStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefer_ipv6: Option<bool>,              // NEW
}
```

**Compatibility rules**:

- A v0.2.0 entry (no `prefer_ipv6` key) loads as `prefer_ipv6: None`,
  which the runtime treats as `false` (default IPv4-first).
- An IP-target rule with `prefer_ipv6: true` deserializes successfully
  but the flag is unused at runtime (resolver layer is skipped). On
  re-serialize the flag is preserved verbatim — we don't strip it,
  so an operator who sets it generically keeps consistent semantics.
- DNS-name `target_host` strings re-validate against RFC 1123 strict
  syntax on load. A bad entry on disk (e.g. handwritten with an
  underscore) MUST cause a server startup error with the offending
  rule_id named, NOT silently degrade the rule.

**Migration**: none required. v0.2.0 → v0.3.0 is a pure read of the
existing on-disk file. v0.3.0 → v0.2.0 downgrade also works (v0.2.0
serde tolerates and discards the extra `prefer_ipv6` key by default
in our `serde(deny_unknown_fields = false)` setting — which matches
the v0.2.0 PersistedRule).

---

## Credential bundle (client-side)

**No change.** The bundle carries server endpoint, server cert
fingerprint, and bearer token; DNS configuration is a *client-local*
concern and is not negotiated through the bundle. A future spec may
add resolver tunables to the bundle if a use case surfaces, but it
is out of scope here.

---

## Resolver cache

**Not persisted.** As noted in `data-model.md`, the resolver cache is
process-local and discarded across `portunus-client` restarts. This is
intentional — a restarted client should re-validate DNS state on
first traffic rather than acting on a stale cache from a previous
process generation.
