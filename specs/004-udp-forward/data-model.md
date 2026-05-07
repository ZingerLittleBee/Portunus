# Data Model — UDP forwarding (v0.4.0 delta)

**Inheritance**: This document is an **overlay** on
`specs/003-domain-name-forward/data-model.md`. Entities not mentioned
here are unchanged. The v0.3.0 `Target` enum (`Ip | Dns`), `Hostname`
validator, `Cache`, and `LiveResolver` carry over verbatim.

---

## Entity additions

### Protocol (enum, wire-stable)

| Variant | Wire value | Status |
|---|---|---|
| `PROTOCOL_UNSPECIFIED` | 0 | Reserved (proto3 default; rule push rejects with `invalid_protocol`) |
| `TCP` | 1 | v0.1.0+ |
| `UDP` | 2 | **NEW in v0.4.0** |

Capability negotiation is **proactive** (push-time), not reactive
(after RuleStatus.failed):

1. **Client → server (Hello)**: The client declares its supported
   protocols in a new `Hello.supported_protocols repeated Protocol`
   field (see `contracts/forward.proto`). v0.4.0 clients send
   `{TCP, UDP}`. v0.3.0 clients don't send the field; the server
   treats absence as `{TCP}`.
2. **Server (ConnectedClient)**: The Control RPC handler reads the
   first inbound `ClientMessage`. If it is a Hello, the server
   stores `supported_protocols: HashSet<Protocol>` on the
   `ConnectedClient` row created by `ClientRegistry::register`. If
   the first message is not a Hello (Hello is documented as
   optional in v1), the field defaults to `{TCP}`. Welcome is sent
   AFTER this read (in v0.3.0 it was sent immediately on register —
   v0.4.0 reorders so Welcome can carry capability-derived tunables
   in the future).
3. **Server (push validation)**: `RuleStore::push_*` rejects a UDP
   rule with HTTP 422 / exit 3 / typed code `unsupported_protocol`
   if the target client's `supported_protocols` does not contain
   `UDP`. Rejection happens **before** the rule is persisted to
   `rules.json` and **before** any RuleUpdate is sent over the
   wire — operators see the failure at the CLI / HTTP response.
4. **Client (defence-in-depth)**: The client's RuleUpdate handler
   inspects `rule.protocol`. Unknown variants (any value other than
   the protocols this client was compiled with) return
   `RuleStatus.failed` with `reason="unsupported_protocol"`. A v0.3
   client paired with a v0.4 server that bypassed step 3 (e.g.
   server downgrade between Hello and push) still fails safely.

This explicitly replaces the reactive RuleStatus.failed-only path
that v0.3.0 implicitly relied on (and that did not actually trigger,
since v0.3.0 clients silently constructed a TCP forwarder for any
protocol value).

### UdpFlow (new, client-side runtime)

A logical session between an end-user source `(addr, port)` and the
upstream destination of a UDP rule. **Not persisted, not on the wire.**

| Field | Type | Notes |
|---|---|---|
| `source_addr` | `SocketAddr` | Primary key within the parent rule's flow table |
| `upstream_socket` | `Arc<UdpSocket>` | Per-flow ephemeral upstream socket. `bind(0)`, never `connect`'d. |
| `upstream_addr` | `SocketAddr` | Resolved target. May change across reconnects on multi-A; per-flow it is fixed at creation. |
| `last_seen` | `Instant` | Updated on EVERY datagram in either direction. Read by the reaper. |
| `bytes_in` | `AtomicU64` | This flow's contribution to the rule's `bytes_in` (rolled up at stats tick). |
| `bytes_out` | `AtomicU64` | Same, outbound. |
| `datagrams_in` | `AtomicU64` | This flow's count of inbound datagrams. |
| `datagrams_out` | `AtomicU64` | Same, replies. |
| `cancel` | `CancellationToken` | Reaper / shutdown / rule-remove cascades through this. |

**Lifetime**: created on a listener `recv_from` whose source is not in the
flow table. Released when (a) idle reaper finds `last_seen < now -
idle_window`, or (b) the parent rule is removed, or (c) the client
shuts down, or (d) the per-rule cap is hit and the table cannot make
room. Released = `cancel.cancel()` + `Drop` on the `UdpSocket`.

### UdpFlowTable (new, per-rule)

| Field | Type | Notes |
|---|---|---|
| `entries` | `tokio::sync::Mutex<HashMap<SocketAddr, Arc<UdpFlow>>>` | Source → flow |
| `cap` | `usize` | From `udp_max_flows_per_rule` config (default 1024) |
| `idle_window` | `Duration` | From `udp_flow_idle_secs` config (default 60 s, clamp \[30 s, 5 min\]) |
| `reaper` | `JoinHandle<()>` | One reaper task per flow table; loops on `idle_window / 4` |
| `dropped_overflow` | `AtomicU64` | Cumulative count of new-flow datagrams dropped because table was at cap (rolled up at stats tick) |

**Operations**:

- `lookup_or_insert(source) -> Result<Arc<UdpFlow>, OverflowDropped>` — fast path is a HashMap get under the mutex. Slow path bind+spawn happens with the mutex held only long enough to `insert`; the spawn is outside the critical section.
- `evict(source)` — release the cancel token, remove the entry. Called by reaper and by rule-remove.
- `len() -> usize` — for the `active_flows` gauge tick.
- `drain()` — cancel all entries, await reaper, drop the table. Called on rule remove and on shutdown.

**Invariants**:

1. `entries.len() <= cap` at all times (insert-after-check protects this).
2. A `(rule_id, source_addr)` pair maps to exactly one `UdpFlow` instance until that flow is evicted.
3. The reaper holds the mutex only across the eviction sweep loop, never across an `await`.

### UdpListener (new, per-rule-port)

| Field | Type | Notes |
|---|---|---|
| `socket` | `Arc<UdpSocket>` | Bound to `0.0.0.0:listen_port` (or operator-pinned interface) |
| `flow_table` | `Arc<UdpFlowTable>` | Shared between this listener task and the per-flow reply tasks |
| `target` | `Target` | `Ip(SocketAddr)` or `Dns(Hostname, port)` — drives the resolver path |
| `prefer_ipv6` | `bool` | Per-rule address-family preference (FR-005) |
| `cancel` | `CancellationToken` | Cascades from the parent rule's cancellation |

**Range rules** instantiate one `UdpListener` per port in the range,
sharing the parent rule's metric labels but each owning its own flow
table. Per-port byte/datagram counters are kept on the listener and
rolled up to the parent rule's `RuleStats` for both Prometheus
(aggregate row) and `--per-port` (detailed array).

---

## State transitions

### UdpFlow lifecycle

```
                 first datagram from new source
                              │
                              ▼
                       [resolve target]
                              │
                  ┌───────────┴──────────┐
                  │                      │
              resolve OK             resolve FAIL
                  │                      │
                  ▼                      ▼
         [bind upstream sock]    [drop datagram,
                  │              dns_failures += 1]
                  ▼                      │
            [insert into                 ▼
             flow table]              [end]
                  │
                  ▼
            ┌───── Active ─────┐
            │                  │
   datagram in either dir    idle (now - last_seen > window)
            │                  │
            ▼                  ▼
    [forward + bump        [evict: cancel,
     last_seen + counters]   close upstream sock,
            │               remove from table]
            └─── back to Active
```

### Stats roll-up tick (every 5 s, existing cadence)

For each rule:

```
RuleStats {
  rule_id,
  bytes_in:    Σ bytes_in across all flows in all listeners,
  bytes_out:   Σ bytes_out,
  active_connections: 0  // for UDP
  active_flows:       Σ flow_table.len() across listeners (NEW field 9)
  per_port:    [PerPortStats for each listener if range rule],
  dns_failures: rule-level counter (existing, FR-005),
  datagrams_in:  Σ across all flows (NEW field 7),
  datagrams_out: Σ across all flows (NEW field 8),
  flows_dropped_overflow: Σ across all flow tables (NEW field 10),
}
```

**Invariant**: `bytes_in / bytes_out` are protocol-agnostic. The same
collector family (`forward_rule_bytes_in_total{client,rule}`) carries
both TCP and UDP byte counts. Operators distinguish by the
**protocol-specific** collectors that are non-zero on each rule.

---

## Server-side configuration additions (`server.toml`)

| Key | Type | Default | Range | Notes |
|---|---|---|---|---|
| `udp_flow_idle_secs` | u32 | 60 | 30..=300 | Per-flow idle window |
| `udp_max_flows_per_rule` | u32 | 1024 | 1..=65535 | Per-rule flow table cap |

Both are **server-side** config (the server pushes them to the client
inside the `Welcome` message at control-plane connect time). This
keeps the operator surface single-pane: an operator running 50 edge
hosts tunes one `server.toml`, not 50 `client.toml`.

**Welcome additions (additive proto3)** — canonical v0.3.0 Welcome
has fields 1 (`server_version`) and 2 (`server_time_unix_ms`); the
new tunables claim fields 3 and 4:

```protobuf
message Welcome {
  string server_version       = 1;
  uint64 server_time_unix_ms  = 2;

  // Additive in v1.3 (spec 004-udp-forward).
  uint32 udp_flow_idle_secs     = 3;  // 0 → use client's compile-time default (60)
  uint32 udp_max_flows_per_rule = 4;  // 0 → use client's compile-time default (1024)
}
```

A v0.3.0 client ignores these fields (proto3 default). A v0.4.0 client
talking to a v0.3.0 server gets zero on both fields and falls back to
the compile-time defaults — same observable behaviour.

---

## Persistence delta (`rules.json`)

The rules persistence file gains nothing fundamentally new — `protocol`
is already a field. UDP rules serialise with `"protocol": "udp"` in the
JSON form (was `"tcp"` in v0.3.0). A v0.3.0 server reading a
`rules.json` written by v0.4.0 with a `"udp"` rule MUST refuse to load
and log a typed error — never silently coerce to TCP. Implementation:
deserialiser does an exhaustive match on the protocol string. Operators
who downgrade must first remove or rewrite UDP rules.

---

## Validation rules (rule push)

Server-side at push time (mirrors v0.2.0/v0.3.0 push validation):

1. `protocol ∈ { "tcp", "udp" }` — anything else → `invalid_protocol` (HTTP 400, exit 3).
2. **Port conflicts are per-protocol.** A UDP rule on `:9000` and a TCP rule on `:9000` for the same client are both legal — kernel maintains independent port spaces. Conflict detection iterates only rules with the same `protocol` value.
3. UDP rule range size cap reuses the existing `range_rule_max_ports` (default 1024).
4. UDP rule on a v0.3.0 client → `unsupported_protocol` (HTTP 422, exit 3) — server checks the connected client's `Welcome.protocol_version` against the rule's protocol.
5. All v0.3.0 hostname validation rules apply unchanged to UDP rule targets.

---

## Observability surfaces

### Prometheus (loopback `/metrics`, one row per rule per collector)

| Collector | Type | Labels | Notes |
|---|---|---|---|
| `forward_rule_bytes_in_total` | counter | `client,rule` | **Existing.** Now carries TCP+UDP. |
| `forward_rule_bytes_out_total` | counter | `client,rule` | Same. |
| `forward_rule_active_connections` | gauge | `client,rule` | **Existing.** TCP only — UDP rules report 0. |
| `forward_rule_active_flows` | gauge | `client,rule` | **NEW.** UDP only — TCP rules report 0 (or are absent if never set). |
| `forward_rule_dns_failures_total` | counter | `client,rule` | **Existing v0.3.0.** Now also incremented by UDP rules. |
| `forward_rule_udp_datagrams_in_total` | counter | `client,rule` | **NEW.** UDP only. |
| `forward_rule_udp_datagrams_out_total` | counter | `client,rule` | **NEW.** UDP only. |
| `forward_rule_flows_dropped_overflow_total` | counter | `client,rule` | **NEW.** UDP only. |

`remove-rule` removes the labels for ALL collectors (extending the v0.3.0
cleanup pattern that drops the `dns_failures` row). A 1024-port UDP
range rule produces exactly **one row per collector** — SC-004 holds.

### CLI `rule-stats <id>`

Text output for a UDP rule:

```text
rule_id=0 client=edge-01 bytes_in=1024 bytes_out=512 active_flows=3
  datagrams_in=42 datagrams_out=21 dns_failures=0
  flows_dropped_overflow=0 updated_at=2026-05-08T03:14:15Z
```

For a TCP rule (unchanged from v0.3.0):

```text
rule_id=1 client=edge-01 bytes_in=… bytes_out=… active_connections=…
  dns_failures=0 updated_at=…
```

Field selection is driven by `rule.protocol`, not by zero-checks.

### CLI `rule-stats <id> --per-port` (range rules only)

UDP range rule per-port array adds `datagrams_in` / `datagrams_out`
to the existing `bytes_in` / `bytes_out` per-port object.

---

## Edge cases (refer to spec § Edge Cases)

| Case | Storage / runtime treatment |
|---|---|
| Datagram larger than recv buffer | Impossible at the IP level — the per-listener buffer is sized at 65 535 bytes (the IPv4/IPv6 UDP payload ceiling). No truncation can occur, so no counter is required. (Tokio's `UdpSocket::recv_from` does not expose `MSG_TRUNC` anyway — a counter would not be implementable on the current API surface.) |
| Upstream `EHOSTUNREACH`/`ENETUNREACH` from `send_to` | Per-flow state preserved (FR-012). One log at WARN level the first time per flow, count in a per-rule diagnostic counter (reserved field, not a Prometheus row in v0.4.0). |
| TCP rule on `:9000` AND UDP rule on `:9000` for same client | Both legal (per-protocol port spaces). |
| Mixed-protocol port range | Rejected at push time with `mismatched_range` (existing v0.2.0 error code reused). |
| v0.3.0 client + UDP rule push | Rejected with `unsupported_protocol` before the rule is persisted; rule never appears in `list-rules`. |
| Source spoofing on listener | Each datagram source is the kernel's view; no application can forge it without kernel-level capability. Reply routing keys on this same view, so spoofed source = reply-to-spoofer (correct UDP NAT semantics). |
| Sustained source-port churn | Per-rule cap activates → drops counted in `flows_dropped_overflow_total`. Not a flow leak. |

---

## Wire compatibility summary

| Direction | Behaviour |
|---|---|
| **v0.4.0 client + v0.4.0 server** | UDP rules work end-to-end with all observability fields. |
| **v0.4.0 client + v0.3.0 server** | Not a supported pairing. Operators upgrade clients last (server rejects `unsupported_protocol` for UDP pushes against a v0.4.0 client when the server has no UDP-aware push validation; client falls back to compile-time defaults for missing `Welcome` fields). |
| **v0.3.0 client + v0.4.0 server** | TCP rules unchanged. UDP rule push fails fast with `unsupported_protocol` before the rule is persisted; operator sees the typed error. |
| **v0.4.0 reader + v0.3.0 writer** | All UDP-specific `RuleStats` fields default to zero — correct for a TCP-only fleet. |
| **v0.3.0 reader + v0.4.0 writer** | UDP-specific fields are unknown and dropped per proto3 semantics. TCP-relevant fields unchanged. |
