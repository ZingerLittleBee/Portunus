# Persistence delta — v0.4.0 UDP forwarding

**Inheritance**: This document is an **overlay** on
`specs/001-tcp-forward-mvp/contracts/persistence.md` and the v0.2.0 /
v0.3.0 deltas.

---

## `rules.json`

The on-disk rule schema gains nothing new structurally — `protocol` is
already a top-level field. v0.4.0 simply allows a new value:

| Field | v0.3.0 values | v0.4.0 values |
|---|---|---|
| `protocol` | `"tcp"` | `"tcp"` \| `"udp"` |

Example v0.4.0 `rules.json` entry for a UDP single-port rule:

```jsonc
{
  "rule_id": 0,
  "client_name": "edge-01",
  "listen_port": 6000,
  "target_host": "echo.example",
  "target_port": 9999,
  "protocol": "udp",
  "prefer_ipv6": false
}
```

Range form is unchanged (just `"protocol": "udp"`):

```jsonc
{
  "rule_id": 1,
  "client_name": "edge-01",
  "listen_port": 6000,
  "listen_port_end": 6010,
  "target_host": "host",
  "target_port": 7000,
  "target_port_end": 7010,
  "protocol": "udp"
}
```

---

## Downgrade safety

A v0.3.0 server reading a `rules.json` that contains any UDP rule
**MUST refuse to load and exit with a typed error**, never silently
coerce to TCP. This is enforced in the deserialiser by an **exhaustive
match** on the protocol string — adding `"udp"` to the v0.4.0
deserialiser is a one-line change; v0.3.0's deserialiser had only
`"tcp"` in its match, so an unknown value naturally surfaces as a
typed error.

Operator workflow for downgrade:

1. Identify UDP rules: `forward-server list-rules | grep udp`.
2. Either remove them (`remove-rule <id>`) or rewrite them as a TCP
   rule against an alternate upstream that fronts UDP for you.
3. Save `rules.json`. Then downgrade.

---

## `tokens.json`

Unchanged. UDP introduces no new credentials; the existing per-client
bearer-token model carries UDP rules over the same control-plane
gRPC stream as TCP rules.

---

## `server.toml`

Two new keys (both optional; both ignored by v0.3.0 servers, both
provided to clients via the `Welcome` message at connect time — see
`contracts/forward.proto`):

```toml
# Per-flow idle window for UDP rules (FR-006). After this many seconds
# of no datagrams in either direction, the flow is reaped and its
# upstream socket released. Default: 60 s. Range: 30..=300 s.
# udp_flow_idle_secs = 60

# Maximum concurrent live UDP flows per rule (FR-007). When the table
# is at this cap, new-flow first-datagrams are dropped and counted in
# `forward_rule_flows_dropped_overflow_total`. Default: 1024. Each
# flow consumes one upstream socket fd on the client; raise only after
# raising LimitNOFILE in the systemd unit accordingly. Range: 1..=65535.
# udp_max_flows_per_rule = 1024
```

Both keys live at the top level of `server.toml`. Clients receive
their values inside the per-connect `Welcome` message; values of `0`
in the wire message tell the client to use its compile-time defaults
(60 / 1024). This means a v0.4.0 client paired with a v0.3.0 server
(which does not send these fields) gracefully falls back to the
defaults — no client-side knob, no per-edge-host config.

---

## TLS / certificates / token store

Unchanged. UDP introduces no new key material, no new file paths, no
permission changes.
