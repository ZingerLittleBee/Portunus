# Operator API & CLI deltas (v0.3.0)

Additive overlay on the v0.2.0 operator surfaces (HTTP at
`127.0.0.1:7080/v1/...` + `portunus-server` CLI subcommands). All deltas
are backward-compatible: a v0.2.0-shaped HTTP request body or CLI
invocation continues to work; new fields and flags default to the
v0.2.0 behavior.

---

## HTTP API

### `POST /v1/rules` — push a rule

**Body (additive)**:

```json
{
  "client_name": "edge-01",
  "listen_port": 8443,
  "target_host": "api.example.com",        // CHANGE: now accepts a DNS name in addition to IP literal
  "target_port": 443,
  "listen_port_end": null,                 // v0.2.0
  "target_port_end": null,                 // v0.2.0
  "prefer_ipv6": false                     // NEW, optional, default false
}
```

**Validation deltas**:

- `target_host` is classified at push time. IP literals follow the
  v0.2.0 path verbatim. DNS names are validated against RFC 1123
  strict syntax (FR-001 / R-005); failures return:

  ```json
  HTTP 400
  {
    "code": "invalid_target_host",
    "message": "DNS name 'foo_bar.example' contains invalid character '_' (RFC 1123 strict)"
  }
  ```

  Other validator-driven `code` values:
  `invalid_target_host_label_too_long` (>63 chars per label),
  `invalid_target_host_too_long` (>253 chars total),
  `invalid_target_host_label_hyphen` (leading/trailing hyphen).

- `prefer_ipv6` MAY be set on any rule (IP or DNS target). Setting
  it on an IP-target rule is a no-op at runtime; the API accepts
  it for symmetry so generic operator tooling can always include
  the field.

- `prefer_ipv6` is rejected with `HTTP 400 { "code":
  "prefer_ipv6_invalid_value" }` if it is anything other than `true`
  or `false`. JSON `null` is treated as absent (= default `false`).

**Response (additive)**:

```json
HTTP 201
{
  "rule_id": 42,
  "status": "Active",
  "target_host": "api.example.com",        // echoed back as written, not resolved
  "prefer_ipv6": false                     // echoed back, even when client did not send it
}
```

The response always includes `prefer_ipv6` so operator tools that
list rules can rely on the field's presence.

### `GET /v1/rules` — list rules

**Body (additive)**: each entry carries `target_host` (verbatim, may
be an IP or a DNS name) and `prefer_ipv6` (bool). No new top-level
field.

### `GET /v1/rules/{id}/stats` — per-rule stats snapshot

**Body (additive)**:

```json
{
  "rule_id": 42,
  "bytes_in": 12345,                       // v0.1.0
  "bytes_out": 67890,                      // v0.1.0
  "active_connections": 3,                 // v0.1.0
  "per_port": [...],                       // v0.2.0 (only present with ?per_port=true)
  "dns_failures": 0                        // NEW, always present, 0 for IP-target rules
}
```

`dns_failures` is the running per-rule DNS-failure counter (FR-008,
SC-006). For IP-target rules it is always 0 — the resolver layer is
skipped entirely, so it can never increment. The field is unconditionally
present so operators can poll one endpoint and not branch on rule type.

### `GET /metrics` — Prometheus scrape

**New collector**:

```
# HELP portunus_rule_dns_failures_total Per-rule monotonic count of end-user connections refused due to DNS resolution failure (NXDOMAIN, SERVFAIL, timeout, full multi-A exhaustion).
# TYPE portunus_rule_dns_failures_total counter
portunus_rule_dns_failures_total{client="edge-01",rule="42"} 7
```

Cardinality contract: one row per `(client, rule)` pair, never per
attempt, per address, or per failure-mode reason (R-008 / SC-006).

---

## CLI

### `portunus-server push-rule <client_name> <listen_spec> <target_spec> [--prefer-ipv6]`

**Argument shapes (additive)**:

- `<target_spec>` accepts the v0.2.0 forms (`host:port`,
  `host:start-end`) where `host` was previously required to be an IP
  literal. After this change `host` MAY be either an IP literal or a
  DNS name.
- New flag `--prefer-ipv6` (no value) sets `prefer_ipv6 = true` on the
  pushed rule. Default is `false` (omit the flag).

**Examples**:

```sh
# v0.2.0 single-port IP target — unchanged
portunus-server push-rule edge-01 8080 192.168.1.10:80

# v0.2.0 port-range IP target — unchanged
portunus-server push-rule edge-01 30000-30099 192.168.1.10:30000-30099

# v0.3.0 single-port DNS target, IPv4-first (default)
portunus-server push-rule edge-01 8443 api.example.com:443

# v0.3.0 single-port DNS target, IPv6-preferred
portunus-server push-rule edge-01 8443 api.example.com:443 --prefer-ipv6

# v0.3.0 port-range DNS target — one resolution per range (FR-011)
portunus-server push-rule edge-01 8000-8009 api.example.com:8000-8009

# Validation failure surfaces immediately
portunus-server push-rule edge-01 8080 'foo_bar.example:80'
# → exit 1, stderr: invalid_target_host: DNS name 'foo_bar.example' contains
#   invalid character '_' (RFC 1123 strict)
```

### `portunus-server rule-stats <id> [--per-port]`

**Output (additive)**: gains a `dns_failures` row alongside the
v0.1.0/v0.2.0 fields:

```text
rule 42 (client edge-01)
  bytes_in           12345
  bytes_out          67890
  active_connections 3
  dns_failures       0          ← NEW
```

Same field is exposed through `--json` mode (v0.2.0).

### `portunus-server list-rules`

**Output (additive)**: gains a `target_host` (already present in
v0.2.0 — now legitimately may be a hostname) and a `prefer_ipv6`
column when `--wide` is set, or as a JSON field in `--json` mode.
The default narrow human-readable output stays the same shape so
operators' eyeballs don't have to re-parse it.
