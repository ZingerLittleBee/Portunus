# Operator API delta — v0.4.0 UDP forwarding

**Inheritance**: This document is an **overlay** on
`specs/001-tcp-forward-mvp/contracts/operator-api.md` and the v0.2.0 /
v0.3.0 deltas. Endpoints, exit codes, and HTTP status mappings not
mentioned here are unchanged. The frozen v1 stability guarantee
applies.

---

## Push rule

### CLI

```text
forward-server push-rule [OPTIONS] <CLIENT> <LISTEN> <TARGET>

  --protocol <PROTOCOL>      tcp | udp   [default: tcp]
  --prefer-ipv6              (v0.3.0)
  --ack-timeout <SECONDS>    (existing, default 2)
  --http-endpoint <ENDPOINT> (existing)
```

The `--protocol udp` flag is the **only new CLI surface** for this
release. It is accepted on both single-port and range push-rule
invocations:

```sh
# Single-port UDP rule
forward-server push-rule edge-01 6000 echo.example:9999 --protocol udp

# UDP range rule
forward-server push-rule edge-01 6000-6010 host:7000-7010 --protocol udp

# UDP rule with IPv6 preference (combines with v0.3.0 flag)
forward-server push-rule edge-01 6000 echo.example:9999 --protocol udp --prefer-ipv6
```

### HTTP

`POST /v1/clients/{client}/rules` accepts an optional `protocol` field:

```jsonc
{
  "listen_port": 6000,
  "target_host": "echo.example",
  "target_port": 9999,
  "protocol": "udp",            // NEW (optional, default "tcp")
  "prefer_ipv6": true            // v0.3.0
}
```

Range form (UDP):

```jsonc
{
  "listen_port_start": 6000,
  "listen_port_end":   6010,
  "target_host": "host",
  "target_port_start": 7000,
  "target_port_end":   7010,
  "protocol": "udp"
}
```

Response body adds the resolved protocol:

```jsonc
{
  "rule_id": 0,
  "status": "active",
  "target_host": "echo.example",
  "protocol": "udp",            // NEW (always present on the response)
  "prefer_ipv6": true
}
```

### Error codes (additions)

| Code | HTTP | CLI exit | Meaning |
|---|---|---|---|
| `invalid_protocol` | 400 | 3 | `protocol` value was not `"tcp"` or `"udp"`. (Existing v0.1.0 code, scope widened to the new value.) |
| `unsupported_protocol` | 422 | 3 | The connected client is v0.3.0 and cannot accept a UDP rule. (NEW.) |

`port_in_use` becomes **per-protocol**: pushing UDP `:6000` on a client
that already has TCP `:6000` is **legal** and succeeds. Conflict only
fires when the requested `(protocol, listen_port)` matches an existing
rule on the same client.

`mismatched_range` (existing v0.2.0 code) does not need new semantics —
range protocol is per-rule, not per-port within a range, so there is no
"mixed protocol range" failure mode beyond what `invalid_protocol`
already catches.

---

## List rules

`GET /v1/rules` adds the `protocol` field to each rule entry:

```jsonc
[
  {
    "rule_id": 0,
    "client_name": "edge-01",
    "listen_port": 6000,
    "target_host": "echo.example",
    "target_port": 9999,
    "protocol": "udp",          // NEW (always present, defaults to "tcp" for v0.3.0 rules)
    "state": "active"
  }
]
```

CLI text output for `list-rules` adds a column:

```text
ID     CLIENT               PROTO  PORT   TARGET                           STATE
0      edge-01              udp    6000   echo.example:9999                active
1      edge-01              tcp    8080   api.example.com:443              active
```

(The exact column widths follow the existing v0.3.0 layout —
`PROTO` slots in between `CLIENT` and `PORT`.)

---

## Rule stats

`GET /v1/rules/{rule_id}/stats` JSON adds:

```jsonc
{
  "rule_id": 0,
  "client_name": "edge-01",
  "protocol": "udp",                    // NEW
  "bytes_in": 1024,
  "bytes_out": 512,
  "active_connections": 0,              // 0 for UDP rules
  "active_flows": 3,                    // NEW (0 for TCP rules)
  "datagrams_in": 42,                   // NEW (0 for TCP rules)
  "datagrams_out": 21,                  // NEW (0 for TCP rules)
  "dns_failures": 0,
  "flows_dropped_overflow": 0,          // NEW (0 for TCP rules)
  "updated_at": "2026-05-08T03:14:15Z"
}
```

CLI text output is protocol-aware (driven by the `protocol` field):

UDP rule:
```text
rule_id=0 client=edge-01 protocol=udp bytes_in=1024 bytes_out=512 active_flows=3
  datagrams_in=42 datagrams_out=21 dns_failures=0 flows_dropped_overflow=0
  updated_at=2026-05-08T03:14:15Z
```

TCP rule (v0.3.0 layout, `protocol=tcp` field added but otherwise
unchanged):
```text
rule_id=1 client=edge-01 protocol=tcp bytes_in=… bytes_out=… active=…
  dns_failures=0 updated_at=…
```

`?per_port=true` query (range rules): each entry of the `per_port`
array gains `datagrams_in` / `datagrams_out`.

---

## `/metrics` (Prometheus, loopback)

Four new collectors, with the same `{client, rule}` label set as the
v0.3.0 `forward_rule_dns_failures_total`:

```text
# HELP forward_rule_active_flows Current live UDP flows per rule
# TYPE forward_rule_active_flows gauge
forward_rule_active_flows{client="edge-01",rule="0"} 3

# HELP forward_rule_udp_datagrams_in_total Cumulative UDP datagrams ingressing each rule
# TYPE forward_rule_udp_datagrams_in_total counter
forward_rule_udp_datagrams_in_total{client="edge-01",rule="0"} 42

# HELP forward_rule_udp_datagrams_out_total Cumulative UDP datagrams egressing each rule
# TYPE forward_rule_udp_datagrams_out_total counter
forward_rule_udp_datagrams_out_total{client="edge-01",rule="0"} 21

# HELP forward_rule_flows_dropped_overflow_total Cumulative new-flow datagrams dropped because the per-rule flow table was at capacity
# TYPE forward_rule_flows_dropped_overflow_total counter
forward_rule_flows_dropped_overflow_total{client="edge-01",rule="0"} 0
```

**Cardinality**: one row per rule per collector. A 1024-port UDP range
rule produces exactly one row in each of the four collectors above
(SC-004). `remove-rule` drops the labels for ALL four (extending the
v0.3.0 `dns_failures_total` cleanup pattern); test in
`forward-server/src/metrics.rs::tests` covers this.

---

## Stability guarantee

The HTTP error codes, CLI exit codes, and Prometheus metric names
landed in this release are **frozen at v1** per the existing operator
API stability contract. Future releases may add columns to text
output, fields to JSON responses, and new metric collectors, but MUST
NOT rename or repurpose anything listed above without a major-version
operator-API bump.
