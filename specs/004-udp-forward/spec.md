# Feature Specification: UDP forwarding

**Feature Branch**: `004-udp-forward`
**Created**: 2026-05-07
**Status**: Draft
**Input**: User description: "UDP forwarding: forwarding rules may target UDP in addition to TCP. The existing `Protocol` enum already reserves the slot. A UDP rule binds a UDP socket on the client `listen_port` and proxies datagrams to a `target_host:target_port` (IP literal or DNS name, reusing the v0.3.0 resolver). Per-flow state is keyed by source `(addr, port)` with an idle-eviction timeout; no connection establishment, so 'active connections' becomes 'active flows'. Per-rule byte/datagram counters and the same `dns_failures` surface apply. Range rules can also be UDP. Additive on top of v0.3.0 — TCP rules keep working byte-identically."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Forward UDP traffic to a single upstream port (Priority: P1)

An operator pushes a forwarding rule with the UDP protocol. Datagrams arriving at the chosen edge port are relayed to a single upstream `host:port`, and replies are sent back to the original sender. End-users (e.g., a DNS resolver, a syslog client, a game client) interact with the edge and observe behaviour indistinguishable from talking directly to the upstream.

**Why this priority**: Without this, the platform can only carry TCP. UDP is the second universally needed transport (DNS, NTP, syslog, QUIC pre-handshake, RTP, game telemetry). Single-port UDP delivers an MVP that exercises the full new code path: socket binding, per-flow state, idle eviction, and metrics.

**Independent Test**: Bring up server + one client. Run a UDP echo on the upstream host. Push a UDP rule listening on edge port `P` targeting `upstream:Q`. From a third host, send a UDP datagram to edge `P`; assert the same payload arrives back from edge `P` within one network round-trip. Re-send from a different source port; assert both flows are tracked independently.

**Acceptance Scenarios**:

1. **Given** a connected client and an upstream UDP echo on `127.0.0.1:9999`, **When** the operator pushes a UDP rule `edge-01 6000 127.0.0.1:9999`, **Then** a UDP datagram of N bytes sent to `edge-01:6000` is echoed back from `edge-01:6000` to the original source within one round-trip.
2. **Given** an active UDP rule, **When** two end-users send datagrams from different source `(addr, port)` pairs, **Then** each receives only its own reply (replies are not cross-routed).
3. **Given** an active UDP rule and an upstream that does not reply, **When** an end-user sends a datagram, **Then** the rule absorbs the datagram (no error to the sender — UDP is best-effort) and the per-rule `bytes_in` counter advances.

---

### User Story 2 - UDP rule with a DNS-name target (Priority: P2)

An operator pushes a UDP rule whose `target_host` is a DNS name rather than an IP literal. The client resolves the name lazily on the first flow's first datagram, caches the result per the v0.3.0 resolver semantics, and serves subsequent flows from the cache. DNS failures surface through the same per-rule counter as TCP rules.

**Why this priority**: Without this, every UDP rule needs an IP literal — operationally inconvenient and inconsistent with the v0.3.0 TCP surface. Reusing the existing resolver layer keeps cardinality, single-flight, and stale-while-error behaviour identical between protocols.

**Independent Test**: Configure a hosts entry for `udp.test → 127.0.0.1`, push `edge-01 6001 udp.test:9999 --protocol udp`, send a datagram, observe the same `rule.dns_resolved` event the TCP path emits. Break DNS, observe `dns_failures` counter advance, restore DNS, observe the rule recover without operator action.

**Acceptance Scenarios**:

1. **Given** a UDP rule targeting `udp.test:9999`, **When** the first datagram arrives, **Then** the client emits one DNS resolution event and forwards the datagram to the resolved address.
2. **Given** a UDP rule whose target name fails to resolve, **When** an end-user sends a datagram, **Then** the datagram is dropped, the per-rule `dns_failures` counter advances by one, and the rule remains Active.

---

### User Story 3 - UDP port-range rule (Priority: P2)

An operator pushes a single rule that binds a contiguous UDP port window on the client and forwards each port's datagrams to the same-offset upstream port — equivalent to the v0.2.0 TCP range-rule surface but for UDP. A typical use case is a game-server port window or a multi-stream RTP receiver.

**Why this priority**: Without this, operators with N adjacent UDP ports must push N rules. The TCP range surface is already proven in v0.2.0; a UDP variant is mostly tracking changes in the data-plane code and reusing the rule schema.

**Independent Test**: Push `edge-01 6000-6010 upstream:7000-7010 --protocol udp`. Send a datagram to edge `6004`; assert it lands at upstream `7004` and replies route back. Repeat for two other ports in the range.

**Acceptance Scenarios**:

1. **Given** a UDP range rule `6000-6010 → upstream:7000-7010`, **When** an end-user sends a datagram to edge `6004`, **Then** it arrives at upstream `7004` and the reply is delivered to the original sender.
2. **Given** a UDP range rule, **When** the operator runs `rule-stats <id> --per-port`, **Then** per-port byte/datagram counters are returned for each port in the window.

---

### User Story 4 - Idle UDP flows are reaped automatically (Priority: P3)

A UDP rule maintains per-flow state (one entry per source `(addr, port)`). When a flow has been quiet for longer than the idle-eviction window, the client releases its resources without operator action. The per-rule `active_flows` gauge reflects the live count.

**Why this priority**: Without bounded eviction, a rule under heavy churn (each datagram from a new ephemeral source) leaks state until the per-rule cap is hit and new flows are dropped. With eviction, steady-state memory is bounded by the actual rate of distinct senders within the idle window.

**Independent Test**: Push a UDP rule. Send one datagram each from 100 distinct source ports. Wait the configured idle window. Observe `active_flows` returns to zero. Re-send from one of the original source ports; observe a fresh upstream socket is opened (the prior one was reaped) and the reply still routes back correctly.

**Acceptance Scenarios**:

1. **Given** a UDP rule with idle-window `T`, **When** a flow sends one datagram and goes silent for `> T`, **Then** the flow's per-flow state is released and `active_flows` decrements by one.
2. **Given** a UDP rule that has hit its `max_flows_per_rule` cap, **When** a new source attempts to send, **Then** the datagram is dropped and a per-rule `flows_dropped_overflow` counter advances.

---

### Edge Cases

- **Datagram larger than the receive buffer**: An end-user sends a datagram larger than the platform's UDP receive buffer (typical kernel default ~64 KiB but smaller on some systems). Behaviour: the datagram is dropped at the kernel boundary; a per-rule counter records the drop. UDP semantics make truncation undesirable — better to drop than partially forward.
- **Upstream ICMP "port unreachable"**: The kernel surfaces an asynchronous error on the upstream-facing socket. UDP is best-effort; the proxy logs and counts the event but does not fail the flow or the rule. The end-user sees no error (mirrors talking to the upstream directly).
- **Rule push for a UDP port already bound by a TCP rule on the same client**: Same port number on different protocols is allowed — UDP and TCP have independent port spaces at the kernel level. Port-conflict detection is per-protocol.
- **Mixed-protocol port range**: A range rule's `protocol` field applies to every port in the range; there is no notion of "ports 6000-6004 TCP, ports 6005-6010 UDP" within a single rule.
- **Operator pushes a UDP rule against a v0.3.0 client**: The client refuses with a typed error code (`unsupported_protocol`) so the operator gets a clear message, not a silent drop. (Downgrade safety — additive proto fields semantics.)
- **End-user keeps a flow active forever**: Idle eviction does not apply if traffic flows continuously. Memory per long-lived flow is bounded by per-flow constants, not by traffic volume.
- **Source spoofing / unsolicited reply from upstream**: A datagram arriving on the upstream-facing socket without a matching live flow is dropped. (No NAT-pinhole bypass.)

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: A forwarding rule MUST be able to specify UDP as its protocol in addition to TCP. The protocol selector is per-rule (single-port or range), not per-port within a range.
- **FR-002**: A UDP rule MUST bind a UDP socket on the client's `listen_port` and accept datagrams from any source.
- **FR-003**: For each new source `(addr, port)` observed on a UDP rule, the client MUST establish a per-flow upstream socket and forward the inbound datagram to `target_host:target_port`.
- **FR-004**: Replies from the upstream MUST be delivered back to the originating source `(addr, port)` and only that source.
- **FR-005**: UDP rule targets MUST accept either an IP literal or a DNS name. DNS-name targets MUST reuse the v0.3.0 resolver (lazy resolution on first flow, TTL-clamped cache, single-flight under burst, RFC 8767-style stale-while-error grace, multi-A fallback, `--prefer-ipv6` opt-in).
- **FR-006**: A UDP rule MUST maintain per-flow state keyed by source `(addr, port)` and MUST evict flows that have been idle (no datagrams sent or received) for longer than a configurable idle window. The default idle window MUST be no shorter than 30 seconds and no longer than 5 minutes; the per-server-config default is 60 seconds.
- **FR-007**: A UDP rule MUST enforce a per-rule cap on concurrent live flows. The default cap MUST be operator-configurable; when the cap is hit, the oldest idle flow MAY be reaped early or the new flow's first datagram MAY be dropped — choice of policy is implementation-defined but the dropped-overflow case MUST be counted in a per-rule metric.
- **FR-008**: Per-rule observability MUST include: cumulative bytes inbound/outbound, cumulative datagrams inbound/outbound, current live `active_flows` gauge, cumulative `flows_dropped_overflow` counter, and the existing `dns_failures` counter inherited from v0.3.0. The counter family for "active_connections" inherited from earlier releases MUST surface as `active_flows` for UDP rules and as `active_connections` for TCP rules — naming reflects protocol semantics.
- **FR-009**: A UDP rule MUST support port-range mapping. A range rule with protocol UDP behaves analogously to v0.2.0 TCP range rules: a contiguous listen-port window maps to a same-offset target-port window, with one UDP socket per port in the range.
- **FR-010**: TCP rule behaviour MUST be byte-identical to v0.3.0. The presence of UDP support MUST NOT change a TCP rule's hot path, wire format for TCP-only fields, or measured throughput/latency.
- **FR-011**: A v0.4.0 client paired with a v0.3.0 server MUST refuse a UDP rule push with a typed `unsupported_protocol` error so the operator sees a clear failure rather than a silent drop. A v0.3.0 client paired with a v0.4.0 server is a non-goal (operators upgrade clients first).
- **FR-012**: An asynchronous ICMP-like upstream error on a per-flow upstream socket MUST NOT fail the rule or the flow. The error MAY be counted in a per-rule diagnostic counter and logged, but the rule remains Active and the flow's per-flow state is preserved until idle eviction.
- **FR-013**: A datagram exceeding the receive buffer ceiling MUST be dropped (not truncated and forwarded) and the drop MUST be reflected in a per-rule diagnostic counter so operators can size their configuration to traffic.
- **FR-014**: Operator surfaces (CLI subcommands, HTTP API, persistence on-disk) MUST present UDP rules through the same paths as TCP rules. The `protocol` field is the only schema delta between the two; downstream surfaces (`list-rules`, `rule-stats`, `--per-port`) MUST handle either value uniformly.

### Key Entities *(include if feature involves data)*

- **UDP Rule**: A forwarding rule whose `protocol` is UDP. Distinguished from a TCP rule only by this field; otherwise carries the same `listen_port`/`listen_port_end`, `target_host`, `target_port`/`target_port_end`, and (for DNS-name targets) `prefer_ipv6` fields.
- **UDP Flow**: A logical connection between an end-user source `(addr, port)` and the upstream destination, mediated by one ephemeral upstream-facing socket on the client. Holds: source address, last-activity timestamp (for idle eviction), upstream socket handle, the parent rule's id.
- **UDP Flow Table**: A per-rule collection of live flows, capped at `max_flows_per_rule` and pruned by an idle-eviction sweep at a cadence faster than the idle window itself.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: An operator can take a fresh v0.4.0 deployment from "binaries on disk" to "first UDP datagram successfully forwarded end-to-end through a pushed rule" in under 60 seconds, on the same host pair the v0.3.0 quickstart targets.
- **SC-002**: A single UDP rule sustains at least 50 000 datagrams/second of bidirectional 512-byte datagrams on loopback for at least 60 seconds without dropping inside the proxy. (Datagram drops at the kernel boundary due to receive-buffer overflow are excluded — they predate the proxy.)
- **SC-003**: A rule under burst from 1 000 distinct simultaneous source `(addr, port)` pairs sustains all flows without any flow being misrouted (an end-user's reply MUST go to that end-user, never to a different source). Flow misrouting is a hard zero — any single instance is a release blocker.
- **SC-004**: Per-rule metric cardinality MUST remain one row per rule for every UDP-specific collector (`active_flows`, `flows_dropped_overflow`, datagram counters). A 1024-port UDP range rule produces the same number of Prometheus rows as a 1-port UDP rule (per collector) — the cardinality budget proven in v0.2.0/v0.3.0 is preserved.
- **SC-005**: After the configured idle window elapses, the live flow count for a quiescent rule returns to zero within one eviction-sweep cycle. Memory and file-descriptor consumption per quiescent rule is bounded by per-rule constants (no growth proportional to historical traffic).
- **SC-006**: TCP-only data-plane benchmarks (the `data_plane.rs` criterion baseline from v0.1.0 maintained through v0.3.0) MUST remain within ±5% of the v0.3.0 numbers. UDP shipping MUST NOT regress the TCP hot path.

## Assumptions

- **Idle-window default of 60 seconds** balances common UDP use cases. Short-flow protocols like DNS (1-2 datagrams per flow) tolerate aggressive reaping; long-flow protocols like RTP keep flows live by traffic. Operators may tune via server config.
- **Per-flow upstream socket model** (one client-side ephemeral source port per end-user flow) is assumed because it is the only model that makes reply routing trivially correct and source-isolated. The alternative (single shared upstream socket with inbound demux by `(saddr, sport)`) is rejected as adding error-prone application-level NAT for no observable end-user benefit.
- **No source-IP preservation upstream**: The upstream sees the client host's IP, not the original end-user's IP. Same posture as the TCP path — preserving the original source IP would require kernel-level transparent proxying (TPROXY) which is out of scope.
- **No multicast or broadcast**: Both ends are unicast UDP. Multicast group joins, anycast routing, and broadcast targeting are out of scope; the bind is to a unicast address only.
- **Receive-buffer ceiling reuses the kernel default** unless the operator raises it via `sysctl`/`SO_RCVBUF`. The proxy MAY surface a warning when received datagrams approach the configured ceiling but does not auto-tune the kernel.
- **Range rules carry per-rule protocol uniformly**: a UDP range rule binds N UDP sockets, a TCP range rule binds N TCP listeners. Mixed-protocol ranges are explicitly out of scope.
- **DNS resolver behaviour is inherited verbatim** from v0.3.0. UDP rules do not introduce a parallel resolver or independent cache — they share `LiveResolver` with TCP rules so the per-rule single-flight bound and stale-while-error grace continue to hold across protocols.
- **The TCP "active_connections" gauge becomes "active_flows" for UDP rules**: the underlying collector MAY be unified or split — observable behaviour is that operators see one numeric current-live value per rule with the protocol-appropriate name.
- **Wire-format additivity**: the `Protocol` enum slot is reserved in the proto3 schema today; no new wire-level fields are required for the basic UDP path beyond what is already defined. UDP-specific operator visibility (datagram counters, `flows_dropped_overflow`) follows the v0.3.0 additive-field pattern.
- **Downgrade safety, asymmetric**: Operators upgrade clients before pushing UDP rules from a newer server. A v0.3.0 client sees a UDP-protocol rule and refuses with a typed error; a v0.4.0 client paired with a v0.3.0 server is not a supported pairing.
