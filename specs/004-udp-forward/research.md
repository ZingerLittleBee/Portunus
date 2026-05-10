# Phase 0 Research — UDP forwarding

## R-001 — UDP socket model in Tokio

**Decision**: Use `tokio::net::UdpSocket` for both the listener and per-flow upstream sockets. Listener calls `recv_from(&mut buf)` in a loop, dispatches to the flow table, and forwards via the per-flow upstream socket using `send_to(buf, target_addr)` (NOT `connect` + `send`). One Tokio task per UDP rule (or per port in a range rule); per-flow read loop is a child task spawned at flow-creation time and ended at idle eviction.

**Rationale**: `tokio::net::UdpSocket` is already in the workspace's `tokio` `net` feature — no new dependency. `recv_from` returns `(bytes, SocketAddr)` in one syscall, which is the only data we need to key the flow table. Avoiding `connect` on the upstream socket sidesteps a macOS quirk where a connected `UdpSocket` rejects asymmetric replies in some kernel versions; `send_to` always works the same way and lets one upstream socket serve many target addrs (relevant for multi-A DNS — the flow can re-dial to the next address on async error without a re-bind).

**Alternatives considered**:
- `socket2` raw socket with `IP_RECVERR` to surface ICMP errors synchronously: feature flag drift, Linux-only, and FR-012 explicitly says ICMP errors do NOT fail the flow. Skip.
- `mio` directly: bypasses Tokio's reactor, doesn't compose with the existing `JoinSet`/`CancellationToken` lifecycle. No upside.
- Single shared upstream socket with userspace demux of `(saddr, sport)`: requires the proxy to invent application-level NAT, plus reply routing must scan a table on every datagram. Per-flow socket gives O(1) reply routing (kernel does the demux). Memory cost is one fd per flow which the per-rule cap bounds.

## R-002 — Idle window and reaper cadence

**Decision**: Default idle window 60 s, operator-tunable in `[30 s, 5 min]` via `server.toml` key `udp_flow_idle_secs`. Reaper runs once per `idle_window / 4` (default 15 s) and evicts flows whose `last_seen` is older than `now - idle_window`. Insertion path also lazily evicts the oldest expired entry if the table is at cap, before deciding whether to drop the new flow.

**Rationale**: 60 s is the median of common UDP usage (DNS sub-second flows tolerate aggressive reaping; RTP/RTSP at typical 20 ms cadence keeps flows alive by traffic). The `[30 s, 5 min]` clamp prevents pathological config (1 s would burn CPU; 1 h would let a flood balloon to OS limits). `idle_window / 4` reaper cadence guarantees an idle flow is reaped within at most `1.25 × idle_window` of its last datagram — bounded enough for SC-005's "returns to zero within one eviction-sweep cycle" assertion.

**Alternatives considered**:
- Per-flow Tokio timer (`tokio::time::sleep(idle_window)` per flow): one timer per active flow, bookkeeping cost grows with churn. Reaper sweep is O(N) per cadence which is identical amortised cost but with a single timer per rule.
- Probabilistic eviction (Redis-style): unnecessary at the scale we expect (≤ 1024 flows per rule). Deterministic sweep is easier to reason about and to test.

## R-003 — Per-rule flow cap and overflow policy

**Decision**: Default `udp_max_flows_per_rule = 1024`, operator-tunable. Overflow policy: **drop the new flow's first datagram** and increment `portunus_rule_flows_dropped_overflow_total{client,rule}`. Existing flows are unaffected.

**Rationale**: 1024 mirrors the v0.2.0 `range_rule_max_ports` default — same magnitude as the documented per-rule fd budget, so an operator who set `LimitNOFILE` for v0.2.0 already has headroom. Drop-new (rather than reap-oldest-idle on insert) gives deterministic backpressure: the same source key always either succeeds or always fails until traffic drops below the cap, which is easier to diagnose than churning evictions. A future spec can add an `overflow_policy = drop_new | reap_oldest` config knob if a real workload demands it.

**Alternatives considered**:
- Reap oldest idle flow on insert overflow: hides cap-hit from the operator; the metric goes up only if the table is "fully active" (no idle entries). Less observable.
- Hard process-level cap: shared budget across all rules. Couples otherwise-independent rules' fates. Per-rule cap matches the v0.2.0 isolation model.

## R-004 — Datagram receive buffer

**Decision**: 65 535-byte stack buffer per `recv_from` call (UDP max payload is 65 507 bytes for IPv4, 65 527 for IPv6 — round to 65 535 for safety). Allocated once per listener task (reused across iterations).

**Rationale**: A `[u8; 65535]` on the stack is 64 KiB; one per listener task (one per rule, or one per port for range rules). At the worst-case 1024-port range rule that's 64 MiB stack, which exceeds Tokio's default 2 MiB worker thread stack. Solution: heap-allocate the buffer once via `vec![0u8; 65535]` at task spawn, reuse for the lifetime of the listener. Every `recv_from` call writes into the same buffer in place (no per-datagram allocation).

**Alternatives considered**:
- Per-datagram `BytesMut::with_capacity(65535)`: per-datagram allocation in the hot path. Fails Constitution II.
- Use `recvmmsg` (Linux batched recv) via `socket2`: not exposed by `tokio::net::UdpSocket`. Marginal throughput gain at very high pps; not needed for SC-002's 50 k/s. Defer to a future optimisation if needed.

## R-005 — Routing replies back to the original source

**Decision**: Each per-flow upstream socket has its own dedicated reply-reader task spawned at flow creation. The task loops on `upstream_sock.recv_from(&mut buf)` and calls `listener_sock.send_to(buf, source_addr)` to deliver the reply. The flow's `last_seen` is updated on both inbound (from end-user) and outbound (reply from upstream). Eviction cancels the reply-reader task via the per-flow `CancellationToken`.

**Rationale**: One task per flow is the simplest correct model — the reply task owns the upstream socket, the listener task owns the listen socket, and the only shared state is the flow table (`last_seen` update + lookup). No userspace demux of replies (kernel does it). On flow eviction, dropping the upstream socket and cancelling the task releases the fd immediately.

**Alternatives considered**:
- Single multiplexed reply task per rule polling all upstream sockets via `select!`/`tokio::select!`: doesn't scale to 1024 flows in a single `select!`. `JoinSet` would work but adds bookkeeping for a model that's no simpler than per-flow tasks.
- `epoll`-style reactor in the listener loop: that IS the Tokio reactor. No reason to reinvent.

## R-006 — Sharing the v0.3.0 resolver layer

**Decision**: UDP rules call the same `LiveResolver::connect_target(rule_id, &Target, port, prefer_ipv6)` entrypoint that TCP rules use, but in a **UDP-aware shim**: the shim takes the resolved address (or address list, on multi-A) and uses it as a dial target for `send_to` / per-flow upstream `bind+send_to`, rather than `TcpStream::connect`. Effectively `connect_target` becomes a "resolve + first-IP" call returning `(SocketAddr, AnswerSource)` for UDP, vs `(TcpStream, AnswerSource)` for TCP. To avoid splitting the resolver API, we add a sibling method `resolve_target(rule_id, &Target, port, prefer_ipv6) -> Result<(Vec<SocketAddr>, AnswerSource), ConnectError>` that the UDP path consumes; TCP keeps using `connect_target`.

**Rationale**: The resolver layer's interesting work — TTL clamping, single-flight, stale-while-error grace, multi-A ordering, family preference — is identical between protocols. Cache and policy are the property of `LiveResolver`, not of the dial mechanism. Splitting at "give me addresses" preserves all that work without duplicating it.

**Alternatives considered**:
- Build a parallel `UdpResolver`: code duplication; risk of cache divergence; doubles the per-rule single-flight bound (worst-case 2 queries per rule per cache window).
- Make `connect_target` generic over a "Dialer" trait: adds a generic parameter to a hot-path method for one extra implementation. Sibling method is less invasive.

The v0.3.0 `dns_failures` counter increments on `Resolution`/`AllAddrsUnreachable` errors today. The UDP path increments the same counter in the same conditions, so SC-006-equivalent semantics hold for UDP rules with no further wiring.

## R-007 — `active_connections` vs `active_flows` naming

**Decision**: Split at **both** the wire layer and the operator
surface. The existing `RuleStats.active_connections` (proto field 4)
remains TCP-only — UDP rules emit zero on it. A new
`RuleStats.active_flows` (proto field 9) carries the live UDP flow
count — TCP rules emit zero on it. Prometheus ships two distinct
collector names (`portunus_rule_active_connections` and
`portunus_rule_active_flows`), each with the same `{client, rule}`
label set. Operator text surfaces (`rule-stats` text, JSON) select
the field based on `rule.protocol`.

**Rationale**: Reusing `active_connections` for both protocols
sounds elegant but breaks the v0.3.0 wire contract: a v0.3.0
operator dashboard reading `active_connections` for an `edge-01:0`
that the v0.4.0 server now treats as UDP would silently start
showing UDP flow counts under a label that historically meant TCP
connections. Worse, a v0.3.0 reader (operator dashboard, scraping
sidecar) decoding a v0.4.0 RuleStats sees an `active_connections`
value with no protocol context — the field's meaning depends on
out-of-band rule metadata. Splitting at the wire level keeps every
field's semantics stable: `active_connections > 0` ⇒ TCP rule,
`active_flows > 0` ⇒ UDP rule, both fields are zero for an idle
rule of either protocol. This is the same pattern v0.2.0 used for
`bytes_in_total` vs `bytes_out_total` (and v0.3.0 for
`dns_failures_total`) — distinct collectors with shared labels, no
cardinality penalty, dashboards stay self-documenting.

The wire cost is one `uint32` per `RuleStats` message (4 bytes per
stats tick, every 5 s, per rule) — well within the additive proto3
budget.

**Alternatives considered**:
- Reuse `active_connections` field for both protocols (the
  initially-recorded R-007 decision): silently overloads a stable
  v0.3.0 field's semantics. Rejected as a wire-contract violation.
- One collector `portunus_rule_active_sessions{client,rule,protocol}`:
  adds a label, breaks the v0.3.0 cardinality contract for dashboards
  that key by the existing collector name.
- Rename `active_connections` to `active_sessions` on the wire: not
  byte-compat with v0.3.0. Hard no.

## R-008 — Prometheus metric cardinality (SC-004)

**Decision**: Four new collectors, each with the `{client, rule}` label set, materialised lazily on first non-zero increment (Prometheus default for `IntCounterVec` / `GaugeVec`). On `remove-rule`, all four labels (matching the dropped rule_id) are removed via `IntCounterVec::remove_label_values` — same idiom v0.3.0 introduced for `portunus_rule_dns_failures_total`.

**Rationale**: One row per rule per collector. A 1024-port UDP range rule appears as a single row in each of the four collectors. The per-port detail (relevant for range rules under `--per-port`) lives in the existing `PerPortStats` repeated field on `RuleStats`, NOT in a new Prometheus row — same separation v0.2.0 established. SC-004 holds by construction.

**Alternatives considered**:
- Per-port Prometheus rows for range rules: spectacular cardinality explosion (1024 rows × 4 collectors = 4096 rows for one rule). Already rejected by v0.2.0's design and the constitution's observability principle.

## R-009 — Wire byte-compatibility with v0.3.0

**Decision**: All four UDP-specific `RuleStats` fields (`datagrams_in`, `datagrams_out`, `active_flows`, `flows_dropped_overflow`) are added at proto field numbers 7, 8, 9, 10 — past the highest v0.3.0 field number (6). They are non-`optional` proto3 scalars (`uint64` / `uint32`), so:

- A v0.4.0 sender setting all four to zero (TCP rule) emits the same wire bytes as a v0.3.0 sender (proto3 default-zero is wire-absent).
- A v0.3.0 reader receiving a v0.4.0 message with non-zero UDP fields drops the unknown fields per proto3 semantics. The TCP-relevant fields are unchanged.
- A v0.4.0 reader reading a v0.3.0 message sees default-zero for all four UDP fields, which is correct (a TCP-only ruleset has no UDP datagrams).

`Protocol::UDP = 2` joins the existing enum. Adding a value to a proto3 enum is non-breaking; v0.3.0 readers receiving `protocol = 2` decode it as the integer 2 and any switch over the enum hits the default arm — which is where FR-011's `unsupported_protocol` error code surfaces. The `portunus-proto/tests/udp_wire_compat.rs` test pins both directions of the round-trip.

**Rationale**: Standard proto3 additive evolution. No new versioning required.

**Alternatives considered**:
- Define a separate `UdpRuleStats` message: would force the server to switch on rule.protocol when feeding metrics, plus need a `oneof`. More moving parts than four extra scalar fields on the existing message.

## R-010 — Operator HTTP API: protocol field

**Decision**: `POST /v1/clients/{client}/rules` accepts an optional `"protocol": "tcp" | "udp"` field in the JSON body, defaulting to `"tcp"` if absent. The response body echoes the resolved protocol. `GET /v1/rules` includes `"protocol"` in each rule entry. Error code `unsupported_protocol` (HTTP 422) is returned if the client target version doesn't support UDP (FR-011).

**Rationale**: Mirrors the v0.3.0 `prefer_ipv6` field shape — optional in, mandatory out. JSON enums use the lowercase string form for human readability (CLI arg parity). HTTP 422 for unsupported protocol is the v1 mapping in `operator-api.md` — same family as `range_invalid` and `invalid_protocol`.

**Alternatives considered**:
- Numeric protocol enum in JSON: less discoverable, more error-prone.
- New endpoint `POST /v1/clients/{client}/udp-rules`: doubles the surface for one field. Ugly and wire-incompatible with the existing `list-rules`.

## R-011 — Per-port datagram counters under `--per-port`

**Decision**: Extend `PerPortStats` (the message used by `--per-port` for range rules in v0.2.0) with two additive optional fields: `uint64 datagrams_in = 4`, `uint64 datagrams_out = 5`. TCP per-port stats leave them at zero. The HTTP `?per_port=true` query and the CLI `--per-port` flag include them in the output for UDP range rules.

**Rationale**: `--per-port` is the existing escape hatch for "I really want detail beyond what Prometheus carries"; UDP operators want the same view. Additive fields preserve v0.2.0 wire compatibility.

**Alternatives considered**:
- Separate `PerPortUdpStats` message: forces a `oneof` on `RuleStats`. Two scalar fields are simpler.

## R-012 — Graceful drain on shutdown / rule remove

**Decision**: On SIGINT/SIGTERM, the existing `portunus-client` shutdown coordinator cancels the per-rule UDP listener tasks, which in turn cancel all per-flow reply-reader tasks via their `CancellationToken`. The drain timeout (`shutdown_drain_timeout_secs`) is shared with TCP — but UDP has no in-flight notion to wait for, so cancellation completes immediately and TCP gets the full timeout. On `remove-rule`, the same per-rule cancellation cascade fires.

**Rationale**: UDP is connectionless. There's nothing to "drain" in the TCP sense. Closing the listener stops accepting new datagrams; closing per-flow upstream sockets stops the reply path. End-users see datagrams silently dropped — same as if the upstream went away. This is the correct UDP behaviour; pretending otherwise would invent semantics that aren't there.

**Alternatives considered**:
- Per-flow "linger" period after listener close: invents a wait-for-no-replies window. UDP has no end-of-stream signal so this would always time out. Skip.

## R-013 — Benchmarks and Constitution II gates

**Decision**: Two new criterion benches, harness=false (matching `data_plane.rs`/`range_install.rs`/`dns_resolver.rs`):

- `portunus-client/benches/udp_data_plane.rs::single_flow_throughput` — open one flow, send 60 s of 512-byte datagrams, measure dgrams/s. Asserts ≥ 50 000/s (SC-002).
- `portunus-client/benches/udp_data_plane.rs::single_flow_rtt` — single 512-byte datagram round-trip, measure median latency. Used to track regression but no hard threshold for v0.4.0 (SC-002 is throughput-driven).

The existing `data_plane.rs` baseline (`v0.1.0` JSON in `crates/portunus-client/benches/baselines/`) is the **TCP regression gate** — re-run on every PR touching `forwarder/`. Threshold: ±5% per Constitution II. CI's `bench_regression_gate.py` already enforces this.

**Rationale**: Two complementary metrics — sustained throughput catches scheduler/syscall overhead drift, single RTT catches per-flow setup/teardown bugs. Constitution II's "must ship reproducible bench" rule is satisfied by both.

**Alternatives considered**:
- One bench measuring "flows/s" (rate of new-flow setup): measures the opening hot path but doesn't catch steady-state regressions. Add only if a future spec touches per-flow setup; current spec leaves that path as one HashMap insert + one socket bind.

## R-014 — macOS vs Linux UDP behaviour

**Decision**: Develop and test on both. Where behaviour differs (e.g., `IP_RECVERR`-style ICMP error visibility, `SO_REUSEPORT` semantics for the listener socket), the proxy uses the lowest-common-denominator behaviour: no ICMP error inspection (FR-012 — errors don't fail flows anyway), no `SO_REUSEPORT` for the listener (one process binds the port, no multi-listener load balancing in v0.4.0).

**Rationale**: macOS support is a development-experience requirement (Constitution: "macOS for development"), not a production target. Avoiding platform-specific socket options keeps the test surface uniform. If a future spec needs per-CPU `SO_REUSEPORT` listeners on Linux for performance, that's a Linux-only optimisation behind `#[cfg(target_os = "linux")]`.

**Alternatives considered**:
- Linux-only with macOS support behind a feature flag: makes local dev painful for Mac-using contributors. Skip.
