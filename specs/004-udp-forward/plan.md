# Implementation Plan: UDP forwarding

**Branch**: `004-udp-forward` | **Date**: 2026-05-07 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/004-udp-forward/spec.md`

## Summary

Extend the data plane to a second transport. A rule whose `protocol = UDP`
binds a UDP socket on the client `listen_port` and proxies datagrams to
`target_host:target_port`. There is no connection in UDP, so the client
maintains a **per-rule flow table** keyed by source `(addr, port)`: the
first datagram from a new source spawns one ephemeral upstream socket,
subsequent datagrams from the same source reuse it, and an idle reaper
sweeps stale entries on a cadence faster than the configured idle
window. Replies on the upstream socket route back to that one source
and only that source.

DNS-name targets reuse the **v0.3.0 resolver layer verbatim** —
`LiveResolver` is shared between TCP and UDP rules, so single-flight,
TTL-clamped caching, stale-while-error grace, multi-A fallback, and the
per-rule `dns_failures` counter all carry over without a parallel
implementation. The IP-literal short-circuit in `connect_target` is the
same.

The wire / persistence / HTTP / CLI surfaces evolve **additively**:

- `Protocol::UDP = 2` joins the existing enum (already reserved comment
  on field 1 — adding a value to a proto3 enum is non-breaking).
- `RuleStats` gains four UDP-specific fields (datagrams in/out,
  active_flows, flows_dropped_overflow) — proto3 `optional`/default-zero
  for cross-version safety. TCP rules emit zeros; v0.3.0 readers drop
  unknown fields per proto3 semantics.
- The Prometheus collector family grows by four counters/gauges with
  the existing `{client,rule}` label set. **Cardinality budget
  preserved**: one row per rule per collector regardless of range size.
- Operator surface change: `push-rule --protocol udp`, server-side
  port-conflict detection becomes per-protocol (TCP and UDP have
  independent kernel port spaces).

The TCP forwarder hot path is **unchanged**: `accept → copy_bidirectional`,
the resolver layer, the per-connection setup. The UDP data plane is a
new sibling of `forward-client/src/forwarder/proxy.rs` that does not
touch `proxy.rs`. The criterion `data_plane.rs` baseline is the
regression gate (Constitution II — must stay within ±5%).

## Technical Context

**Language/Version**: Rust 1.88 (constitution-pinned MSRV via `tonic`).
**Primary Dependencies**: existing — `tokio` (`net::UdpSocket` is in the
                          baseline `net` feature, no new feature flag),
                          `tonic` 0.14, `prost`, `rustls`, `prometheus`,
                          `axum`, `hickory-resolver` (from v0.3.0). **No
                          new external dependency** for this feature.
**Storage**: existing rules persistence (`rules.json`) accepts a new
             `protocol` value; `RuleStats` proto fields land additively
             so the on-disk shape is forward-compatible at the wire
             level (rules.json itself is not stats — stats are runtime).
**Testing**: `cargo test` per crate (unit + integration); `forward-e2e`
             integration crate exercises server + client + real UDP
             sockets (loopback echo); new UDP flow table has its own
             unit tests with real `UdpSocket` pairs (mocks for sockets
             are forbidden by Constitution III — "real sockets,
             loopback acceptable, mocks not"). The resolver layer
             continues to use the v0.3.0 `MockResolver` for hostname
             cases since that mocks an external system, not a socket.
**Target Platform**: Linux primary (musl static binary). macOS for
                     development. UDP behaviour on macOS loopback is
                     close enough to Linux for the test surface; any
                     platform-specific quirk (e.g., `IP_RECVERR` for
                     ICMP error visibility) lives behind `cfg`.
**Project Type**: Multi-crate Cargo workspace (unchanged): `forward-core`,
                  `forward-proto`, `forward-auth`, `forward-server`,
                  `forward-client`, `forward-e2e`. No frontend.
**Performance Goals**:
  - **UDP throughput**: ≥ 50 000 datagrams/s of bidirectional 512-byte
    datagrams on loopback for ≥ 60 s with zero in-proxy drops (SC-002).
    Will be measured by a new criterion bench
    `forward-client/benches/udp_data_plane.rs` (per-datagram median +
    sustained throughput).
  - **TCP regression budget**: existing `data_plane.rs` baseline must
    stay within ±5% (SC-006 + Constitution II). Re-run on every PR
    that touches `forwarder/`.
  - **Flow table operations**: insert/lookup/evict at ≥ 100 k ops/s
    per rule (mirror of the per-rule churn ceiling 1 k flows × 100 Hz
    burst). Standard `HashMap<SocketAddr, FlowState>` over
    `tokio::sync::Mutex` is more than adequate; we will measure to
    confirm rather than micro-optimise speculatively.
**Constraints**:
  - **Idle-eviction window**: default 60 s, range \[30 s, 5 min\]
    (FR-006). Operator-tunable via server config
    (`udp_flow_idle_secs`).
  - **Per-rule flow cap**: default 1024 (mirrors `range_rule_max_ports`
    intent — one bound per rule), tunable
    (`udp_max_flows_per_rule`). Overflow policy: **drop the new
    flow's first datagram and increment `flows_dropped_overflow`**
    (FR-007). Reaping the oldest idle flow early would be a future
    optimisation; for v0.4.0 deterministic backpressure is preferred.
  - **Datagram size ceiling**: receive into a 65 535-byte stack-or-pool
    buffer (UDP max). Anything larger is impossible (kernel rejects).
    `flows_recv_truncated_total` counts kernel `MSG_TRUNC` events
    (FR-013). No fragmentation tracking — UDP fragments are kernel
    business.
  - **No connect-on-upstream-socket**: each per-flow upstream socket
    is `bind(0)` then `send_to(target_addr, ...)`. Using
    `UdpSocket::connect` would make the upstream socket unidirectional
    on macOS in some kernel versions; `send_to` keeps the model
    simple and portable.
  - **DNS resolver**: shared with TCP. The first datagram from each
    new flow on a DNS-target rule triggers `connect_target` exactly
    as the TCP path does — same single-flight, same cache, same
    `dns_failures` increment on resolution failure.
  - **Cardinality**: one Prometheus row per rule for every new
    collector (SC-004). Range-rule per-port datagram detail surfaces
    via `--per-port` only (mirrors v0.2.0 byte counters).
**Scale/Scope**: A client running 100 UDP rules each at 1024-flow
                 capacity holds at most 102 400 per-flow upstream
                 sockets. At one fd per upstream socket plus one fd
                 per listener (1 per port for single-port rules, N per
                 N-port range rule), worst-case fd usage is the sum
                 plus the existing TCP and DNS fds. The systemd
                 `LimitNOFILE` documentation note from v0.2.0 already
                 covers this — bump `LimitNOFILE` proportionally
                 before raising `udp_max_flows_per_rule`.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Pass? | Notes |
|---|---|---|
| I. Security by Default (TLS + bearer token, no plaintext) | ✅ | No control-plane transport changes. UDP rules ride the same `RuleUpdate` over the existing TLS+bearer-token gRPC stream as TCP rules. Per-rule policy (per-tenant protocol whitelist) is the natural enforcement seam when multi-tenant lands; for v0.4.0 it is a hard rule that a client accepts both protocols. **No new credentials, no new auth surface, no new key material.** UDP per-flow upstream sockets bind to ephemeral ports on `0.0.0.0` (or operator-pinned interface); no inbound port is opened for end-users beyond the rule's `listen_port`. |
| II. Performance Is a Feature | ✅ | TCP hot path unchanged: `forwarder/proxy.rs` is not modified. UDP gets its own data-plane module behind a protocol dispatch in `forwarder/mod.rs::activate`. Two new criterion benches: `udp_data_plane.rs` measures per-datagram p50/p99 and sustained dgrams/s (must beat SC-002's 50 k/s loopback floor); the existing `data_plane.rs` continues as the TCP regression gate (must stay within ±5%). The flow-table lookup is one HashMap get under a `tokio::sync::Mutex`; the alternative (lock-free dashmap) is on the table only if benches show contention — measured first, optimised second. |
| III. Test-First Discipline | ✅ | (a) Wire byte-compat: `forward-proto/tests/udp_wire_compat.rs` (NEW) — a TCP-only `Rule` and a TCP-only `RuleStats` (with all UDP fields at default zero) MUST encode byte-identical to v0.3.0 (proto3 zero-default + unknown-field-drop semantics). (b) UDP data plane unit tests: real `UdpSocket` pairs over loopback drive the flow-table state machine — flow creation, idle eviction, overflow drop, reply routing isolation. (c) End-to-end: `forward-e2e/tests/udp_smoke.rs` (NEW) wires a real `forward-client` against a real UDP echo and asserts US1+US3+US4 acceptance scenarios. (d) DNS-target UDP reuses the v0.3.0 `MockResolver` machinery to assert US2 without depending on a live DNS path. (e) **Constitution III "no socket mocks"**: enforced — UDP tests use real `UdpSocket` everywhere, the only mock is `Resolve` (which mocks an external network service, not a socket). |
| IV. Observability & Operability | ✅ | Four new metrics, all `{client,rule}`-labelled (one row per rule, SC-004): `forward_rule_udp_datagrams_in_total` (counter), `forward_rule_udp_datagrams_out_total` (counter), `forward_rule_active_flows` (gauge), `forward_rule_flows_dropped_overflow_total` (counter). Existing `forward_rule_bytes_{in,out}_total` carry UDP byte counts as well — same semantics across protocols. Audit logs gain `rule.udp_flow_opened` (one per new flow, includes rule_id + source addr + chosen upstream addr) at INFO and `rule.udp_flow_evicted` at DEBUG (rate-limit-friendly). DNS resolution events (`rule.dns_resolved`/`rule.dns_failed`) are emitted by the shared resolver layer, identical to TCP. **No log line per datagram** — that would dwarf the data plane. **Graceful drain**: SIGINT/SIGTERM signals tear down UDP listeners after the existing TCP drain timeout; per-flow state is dropped synchronously (UDP has no "in-flight" notion to wait for). |
| V. Multi-Tenant Isolation | ✅ | Protocol is a per-rule property, owned by one `(client_name, rule_id)` pair. The flow table is per-rule, so one tenant's churn cannot evict another tenant's flows. UDP listener bind happens through the same per-port allocator as TCP — when multi-tenant lands, the per-tenant port-range and protocol whitelist enforce at the same seam. Upstream sockets are per-flow and bound to ephemeral ports owned by the client process; no shared upstream socket means no cross-flow interference. |

**Gate result**: PASS. No constitutional violations; nothing to track in
the Complexity Tracking table.

**Post-Phase-1 re-check**: Re-evaluated after `data-model.md`,
`contracts/forward.proto`, `contracts/operator-api.md`, and
`contracts/persistence.md` landed. UDP-specific stats fields are
additive optional/default-zero, so v0.3.0 wire-compat holds (R-009).
The new metrics retain the v0.2.0/v0.3.0 cardinality budget (R-008).
The UDP data plane lives in a new module (`forwarder/udp/`) that does
not touch the TCP proxy file (R-006). PASS, gate clear for
`/speckit-tasks`.

## Project Structure

### Documentation (this feature)

```text
specs/004-udp-forward/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/
│   ├── forward.proto    # Phase 1: additive proto diff (overlay vs v0.3.0)
│   ├── operator-api.md  # Phase 1: HTTP + CLI surface deltas
│   └── persistence.md   # Phase 1: rules persistence schema deltas
├── checklists/
│   └── requirements.md  # Already produced by /speckit-specify
└── tasks.md             # /speckit-tasks output (not created here)
```

### Source Code (repository root)

```text
proto/
└── forward.proto                         # add Protocol::UDP = 2; add datagrams_in/out, active_flows,
                                          #   flows_dropped_overflow to RuleStats (fields 7-10)

crates/
├── forward-core/src/
│   └── (no changes — protocol is a proto enum; Target/Hostname unchanged)
├── forward-proto/
│   └── tests/udp_wire_compat.rs          # NEW: TCP-only Rule + RuleStats encode byte-identical to v0.3.0
├── forward-server/src/
│   ├── operator/cli.rs                   # add --protocol udp on push-rule subcommand
│   ├── operator/rule_cli.rs              # parse protocol value; wire it into the gRPC Rule
│   ├── operator/http.rs                  # accept + return optional protocol field (default tcp)
│   ├── rules.rs                          # port-conflict check becomes per-protocol; active_flows surfaced like active_connections
│   ├── metrics.rs                        # NEW collectors: udp_datagrams_in/out, active_flows, flows_dropped_overflow
│   ├── grpc/service.rs                   # observe() also feeds the four new collectors per StatsReport tick
│   └── config.rs                         # NEW operator config: udp_flow_idle_secs (default 60), udp_max_flows_per_rule (default 1024)
├── forward-client/src/
│   ├── forwarder/
│   │   ├── mod.rs                        # activate(): dispatch on rule.protocol → spawn TCP or UDP task
│   │   ├── proxy.rs                      # UNCHANGED — TCP byte-identical hot path
│   │   ├── stats.rs                      # add datagrams_in/out, active_flows, flows_dropped_overflow counters
│   │   └── udp/                          # NEW module
│   │       ├── mod.rs                    # UDP listener loop: recv_from → flow lookup/create → upstream send
│   │       ├── flow.rs                   # UdpFlow struct (per-source ephemeral upstream socket + last_seen)
│   │       ├── table.rs                  # FlowTable: capped HashMap + idle reaper task
│   │       └── tests/                    # unit tests with real loopback UdpSockets
│   ├── control.rs                        # send_stats_report: include UDP fields in RuleStats
│   ├── benches/udp_data_plane.rs         # NEW: criterion bench (per-datagram + sustained throughput)
│   └── benches/data_plane.rs             # UNCHANGED — regression gate
├── forward-e2e/tests/
│   ├── common/mod.rs                     # add push_rule_http_with_protocol helper
│   └── udp_smoke.rs                      # NEW: real client, real UDP echo, US1+US2+US3+US4 acceptance
└── deploy/
    └── server.toml.example               # document new udp_flow_idle_secs + udp_max_flows_per_rule keys
```

**Structure Decision**: same multi-crate workspace as v0.1.0 / v0.2.0 /
v0.3.0; the only structural addition is the
`forward-client/src/forwarder/udp/` module — kept inside the existing
`forwarder` parent so the TCP and UDP listeners share the lifecycle
machinery (activate/deactivate/drain) without leaking implementation
across the protocol boundary. The TCP hot path is in a sibling file
that this feature does not modify.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

(none)
