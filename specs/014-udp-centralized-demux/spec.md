# Feature Specification: UDP Centralized Reply Demux

**Feature Branch**: `014-udp-centralized-demux`
**Created**: 2026-05-21
**Status**: Draft
**Input**: Brainstorming session 2026-05-21 — direction "deepen L4 → UDP runtime
boundary correction + memory/scheduler footprint reduction"

## Clarifications

### Session 2026-05-21

- Q: What is the real bottleneck this addresses — fd count, tokio task count, or
  heap memory? → A: Heap memory. Each per-flow reply pump owns a 64 KiB receive
  buffer (`UDP_BUFFER_BYTES`). Worst-case `100 rules × 1024 flows/rule = 102_400
  flows` ⇒ ≈ 6.4 GiB of receive buffers alone. fd usage is secondary; tokio
  task count is tertiary.
- Q: Did the existing per-flow-socket design have a load-bearing reason? → A:
  Only macOS portability (see `specs/004-udp-forward/plan.md:106-110`). The
  `portunus-client` target deployments do not include macOS, so the constraint
  no longer applies.
- Q: Should the centralized demux run per-listener, per-rule, or per-process?
  → A: **Per-rule.** A per-rule runtime is the correct boundary: the existing
  `udp_max_flows_per_rule` config name has always promised rule-wide
  semantics, but the implementation enforces it per-port-listener. Centralising
  at rule scope corrects that boundary at the same time as fixing the memory
  footprint.
- Q: Should the new path ship behind an env kill switch like
  `PORTUNUS_DISABLE_SPLICE`? → A: **No.** Direct replacement. UDP correctness
  semantics change (per-rule cap, ICMP-driven evict); a dual code path would
  not be a useful comparison and would leave dead code we cannot delete. The
  new path is the only path.
- Q: Should upstream sockets be `connect(target)`ed? → A: **Yes.** Two
  benefits: (a) the kernel filters reply packets to the chosen peer,
  preventing reply-source spoofing; (b) ICMP errors (`ECONNREFUSED`,
  `EHOSTUNREACH`, `ENETUNREACH`) bind to the connected socket and reflect
  through `recv`/`send`, enabling deterministic flow eviction on peer death.
  Trade-off: multi-A fallback **after** flow creation is removed. Multi-A
  fallback still runs on the first datagram; once a target is chosen, the
  flow's lifetime commits to it. Mid-flow target switching is a dubious
  semantic for UDP NAT state anyway and we drop it.
- Q: What is the demux fairness model? → A: Per-`Ready` budget of **32
  datagrams**, then re-arm the readable future. Prevents a hot flow from
  starving the rest of the rule.
- Q: What metrics / wire surfaces change? → A: Wire and Prometheus surfaces
  are unchanged. Four new `tracing` events are added (`udp_upstream_connect_failed`,
  `udp_addflow_dropped`, `udp_flow_evicted_icmp`, `udp_reply_wouldblock`).
  `active_flows` gauge meaning is corrected (see FR-014).

## User Scenarios & Testing *(mandatory)*

### User Story 1 — High-flow-count UDP forwarding stops melting heap (Priority: P1) 🎯 MVP

An operator runs `portunus-client` as a UDP front-end for a workload with
thousands of concurrent NAT flows per rule (DNS reverse-proxy, game-server
relay, voice/video TURN-style relay). Today each flow owns a dedicated upstream
`UdpSocket` **and** a dedicated tokio reply-pump task that holds a 64 KiB
receive buffer in its future state. A modest 20 rules × 500 flows = 10 000
flows already costs ≈ 640 MiB of receive buffers; an aggressive 100 rules ×
1024 flows touches ≈ 6.4 GiB. The operator wants the same data plane and the
same rule semantics, but without the heap explosion that scales linearly with
flow count.

**Why this priority**: This is the single load-bearing reason to do the work.
Memory footprint per active UDP flow drops by roughly 3 orders of magnitude
(64 KiB receive buffer becomes one shared buffer per rule). fd count is
unchanged; scheduler pressure drops sharply because per-flow tokio tasks
disappear.

**Independent Test**: On a Linux host, push a single UDP rule. Drive 1 000
concurrent client source addresses through it (each sending one datagram
every 30 s, well within the idle window). Measure RSS of `portunus-client`
before and after the flows establish. New design: ΔRSS ≪ 100 MiB. v1.4.x
baseline: ΔRSS ≈ 64 MiB (1 000 × 64 KiB). The improvement is not visible
at 1 000 flows but is the same factor at 100 000.

**Acceptance Scenarios**:

1. **Given** a plain UDP rule with `udp_max_flows_per_rule = 4096` and 4 096
   concurrent client source addresses each maintaining a long-running flow,
   **When** `portunus-client` is running on Linux, **Then** the process RSS
   attributable to UDP receive buffers is bounded by a small constant
   (single-digit MiB per rule), independent of the flow count. The
   v1.4-baseline RSS for the same workload exceeds 256 MiB for the buffers
   alone.

2. **Given** the same rule under steady-state traffic with N concurrent
   flows, **When** an operator queries `rule-stats` over the operator HTTP
   API, **Then** per-`(rule, listen_port)` `datagrams_in_total` and
   `datagrams_out_total` continue to advance with the same semantics as
   v1.4.x. Counter values across the rule are byte-equal to the v1.4.x
   counters for the same input traffic (`SC-003` byte-stability gate).

3. **Given** a rule whose target process is killed while the rule is under
   traffic, **When** the next outbound datagram triggers an ICMP
   `port unreachable`, **Then** the affected flow is evicted from the
   registry immediately (without waiting for the idle window) and the next
   inbound datagram from the same source builds a new flow, which the
   resolver re-resolves and which may pick a different multi-A target.

---

### User Story 2 — Range UDP rules cap correctly at rule scope (Priority: P1)

An operator pushes a UDP range rule (`listen_port_start..listen_port_end`)
expecting `udp_max_flows_per_rule` to bound the whole rule. Today the cap is
applied independently per port listener: a 100-port range rule with cap 1024
actually permits 102 400 concurrent flows, the `flows_dropped_overflow`
counter only triggers when *one specific port* hits 1024, and the
`active_flows` gauge under-reports because each listener writes its own
`flow_table.len()` into a shared `AtomicU32` (last-writer-wins, see
`forwarder/stats.rs:212` `set_active_flows` → `store(n, Relaxed)`). The
operator wants the cap and gauge to mean what the field name says.

**Why this priority**: Documented-behaviour correction. Operators sizing
capacity by reading the field name today get a cap that is silently
multiplied by `range_size`, and the gauge they monitor under-reports under
load. Both are quiet correctness bugs; both surface only on range rules.

**Independent Test**: Push a range UDP rule covering 4 ports with
`udp_max_flows_per_rule = 3`. Send first-packets from 4 distinct sources,
each to a different port. v1.4.x accepts all 4 (each port has its own cap-3
table). New design accepts 3 and drops the 4th, bumping
`flows_dropped_overflow`.

**Acceptance Scenarios**:

1. **Given** a range UDP rule with `udp_max_flows_per_rule = 3` spanning 4
   listen ports, **When** 4 distinct client sources each send a first
   datagram to a different listen port, **Then** exactly 3 flows are
   created, the 4th datagram is silently dropped (FR-009-UDP), and
   `flows_dropped_overflow_total{rule}` advances by 1.

2. **Given** the same rule under any traffic pattern, **When** an operator
   reads the `active_flows` gauge / `rule-stats` HTTP field, **Then** the
   reported value equals `registry.len()` — the true count of `(listen_port,
   src)` live entries across the rule. v1.4.x reports the last listener's
   `flow_table.len()` instead, which under-reports by a factor up to
   `range_size`.

3. **Given** a single-port (non-range) UDP rule, **When** any workload runs
   against it, **Then** the cap behaviour and `active_flows` value are
   byte-equal to v1.4.x for the same input. The correction applies only
   to range rules.

---

### User Story 3 — Reply spoofing is dropped at the kernel (Priority: P2)

An adversary on the upstream network learns the ephemeral source port of a
specific UDP flow's upstream socket and injects a forged reply spoofing the
real target. Today the upstream socket is `bind(0)` only; any packet that
reaches that local port from any source is delivered to the reply pump and
forwarded to the original client. After this feature, the upstream socket is
`bind(0) + connect(target)`, so the Linux kernel drops packets that don't
come from the chosen target address.

**Why this priority**: Hardening that is essentially free once we already do
`connect()` for ICMP reflection (US1 acceptance #3). Not a known CVE; the
attack requires off-path port discovery and is mostly a concern on shared
upstream segments. Worth recording as a positive consequence rather than a
goal.

**Independent Test**: Run a UDP rule, establish a flow, then send a UDP
datagram from an unrelated source IP to the flow's upstream ephemeral
local port (loopback test using a third process). v1.4.x: the datagram
reaches the original client. New design: the datagram is dropped by the
kernel and never observed at the original client.

**Acceptance Scenarios**:

1. **Given** an established UDP flow with chosen target `T`, **When** any
   process on the upstream-reachable network sends a UDP datagram to the
   flow's upstream local port with source address `≠ T`, **Then** the
   datagram is not delivered to the original client and no flow counter
   advances.

---

### Edge Cases

- **Channel back-pressure**: the listener → demux mpsc channel for `AddFlow`
  is bounded (1 024). If demux is wedged or extreme first-packet burst
  saturates the channel, the listener rolls back the just-built flow
  (`registry.remove + flow.cancel.cancel()`) and drops the payload. Emits
  `rule.udp_addflow_dropped` warn. Choosing 1024 means a real first-packet
  storm needs ≥ 1024 cold flows in one demux poll cycle to trigger this —
  that is itself a sign rate-limiting needs tuning, not a transparent
  failure.
- **EMSGSIZE on upstream send**: PMTU discovery returns `EMSGSIZE` for a
  too-large datagram. Drop the datagram, do not evict the flow, do not
  touch `last_seen`, do not consume quota. Emit `rule.udp_emsgsize` debug.
  Evicting would not help — the next datagram is just as large.
- **WouldBlock on upstream `try_send`**: rare on Linux (default unbounded
  send buf) but possible under extreme upstream queue pressure. Drop the
  datagram, no evict. Same policy applies to demux `listener.try_send_to`
  WouldBlock for reply direction. Logged at TRACE to avoid log DoS.
- **Idle reap vs in-flight fast path**: reaper may evict a flow at the
  exact moment a listener has already taken its `Arc<UdpFlow>` for fast
  path. The send proceeds; the reply may be dropped because the demux's
  read_wait has already resolved Cancelled. Documented as at-most-one
  idle-boundary datagram loss; UDP semantics permit it.
- **Same client source addressing multiple listen ports of the same rule**
  (range rule): each `(listen_port, src)` is a distinct flow with its own
  upstream socket and own target selection. v1.4.x already does this per
  port-listener, but the cap and gauge treat them inconsistently. After
  this feature the keying is canonical and the cap covers all of them.
- **Listener task panic / demux task panic / reaper task panic**: the
  supervisor task on `JoinSet` observes the unexpected exit, fires
  `rule_cancel`, and reports `RuleStatusEvent::Failed` to control plane.
  Server-side re-PUSH semantics handle the restart.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001** Each active UDP rule MUST run as a single `UdpRuleRuntime`
  owning exactly one `UdpFlowRegistry`, one `ReplyDemuxTask`, one
  `RuleReaper` task, and N `PerListenerLoop` tasks where N is the count of
  listen ports.
- **FR-002** Flow identity MUST be the tuple `(listen_port: u16, src:
  SocketAddr)`. The same client source addressing two ports of the same
  range rule MUST resolve to two independent flows with two independent
  upstream sockets.
- **FR-003** `UdpFlowRegistry` MUST enforce `udp_max_flows_per_rule` as a
  single rule-wide cap. A range rule of M ports MUST NOT permit
  `cap × M` flows.
- **FR-004** First-packet path order MUST be: `(1) registry.get(key) →
  hit/miss`; on miss `(2) quota.is_exhausted? → drop`; `(3)
  registry.try_reserve(key) → cap exceeded? drop + dropped_overflow++`;
  `(4) rate_limit.acquire_first_packet → reject? drop`; `(5)
  resolver.resolve_target`; `(6) UdpSocket::bind family-matching +
  connect(target)`; `(7) registry.commit(reservation, flow)`; `(8)
  demux_tx.try_send(AddFlow); on full → remove + cancel + drop payload`;
  `(9) upstream.try_send(payload); on failure → remove + cancel + drop`.
  Steps 2 and 3 MUST precede step 4: cap-exceeded MUST NOT consume a
  rate-limit token.
- **FR-005** Upstream sockets MUST be `connect()`ed to the chosen target
  after `bind(0)`. The local bind address family MUST match the target
  family (IPv4 target → `0.0.0.0:0`, IPv6 target → `[::]:0`). Once
  connected, multi-A fallback within the same flow MUST NOT switch
  targets. Multi-A fallback at first-packet selection is preserved.
- **FR-006** A flow MUST be evicted (registry remove + cancel) when its
  upstream socket reports `ECONNREFUSED`, `EHOSTUNREACH`, or
  `ENETUNREACH` on either `try_send` or `try_recv`, irrespective of idle
  window. The next datagram from the same source rebuilds the flow.
- **FR-007** A flow MUST NOT be evicted on `WouldBlock` (either direction)
  or on `EMSGSIZE`. These are transient / per-datagram conditions; the
  datagram is dropped and the flow continues.
- **FR-008** `ReplyDemuxTask` MUST drain at most 32 datagrams from a single
  upstream socket per `Ready` outcome before re-arming the readable future,
  to maintain fairness across flows within the rule.
- **FR-009** `ReplyDemuxTask` MUST use `listener.try_send_to(buf, src)`
  (non-blocking) for replies. `WouldBlock` MUST drop the reply, MUST NOT
  evict the flow, MUST NOT advance `datagrams_out_total`, MUST NOT
  consume reply-direction quota, MUST NOT update `last_seen`.
- **FR-010** `RuleReaper` MUST scan the registry every `idle_window / 4`
  and evict flows whose `last_seen` exceeds `idle_window`. Eviction is
  `registry.remove + flow.cancel.cancel()`. The reaper MUST run once per
  rule, not once per listener.
- **FR-011** A supervisor task MUST hold a `JoinSet` over (listeners,
  demux, reaper). On any unexpected task exit or panic, the supervisor
  MUST fire `rule_cancel` and emit `RuleStatusEvent::Failed` to the
  control plane.
- **FR-012** Shutdown sequence MUST be: `(1) rule_cancel.cancel()`; `(2)
  await listener handles`; `(3) await reaper handle`; `(4) registry.drain
  → remove + cancel each remaining flow`; `(5) drop demux_tx or send
  Shutdown`; `(6) await demux handle`. Shutdown MUST be idempotent.
- **FR-013** `last_seen` MUST be updated only on successful send (either
  direction). It MUST NOT be updated on `recv_from` before send.
- **FR-014** The `active_flows` gauge / `rule-stats` field MUST reflect
  `registry.len()` (true rule-wide unique-flow count). v1.4.x's
  last-writer-wins behaviour (each listener stores its own
  `flow_table.len()` into a shared `AtomicU32`) MUST be replaced.
- **FR-015** Wire protocol, operator HTTP API schema, Prometheus row set,
  and Web UI surfaces MUST NOT change. The configuration field
  `udp_max_flows_per_rule` keeps its name; only its scope is corrected.
- **FR-016** New `tracing` events MUST be added:
  `rule.udp_upstream_connect_failed` (warn),
  `rule.udp_addflow_dropped` (warn),
  `rule.udp_flow_evicted_icmp` (info, once per evict),
  `rule.udp_reply_wouldblock` (trace),
  `rule.udp_emsgsize` (debug),
  `rule.udp_runtime_started` (info, once per rule with `listen_port_start,
  listen_port_end, range_size, rule_cap, cap_scope="per_rule"`).
- **FR-017** Per-`(rule, listen_port)` `datagrams_in_total` and
  `datagrams_out_total` MUST continue to be reported with the same
  granularity as v1.4.x. Inbound is counted in the listener loop; outbound
  is counted in the demux task using `key.listen_port`.

### Key Entities

- **`UdpRuleRuntime`**: per-rule top-level handle. Owns the registry,
  listener socket map, demux command sender, cancel token, and join
  handles. Constructed by `control.rs::handle_server_message` on PUSH,
  destructed on REMOVE.
- **`UdpFlowRegistry`**: per-rule shared flow table.
  `inner: Mutex<HashMap<FlowKey, Slot>>` where `Slot ::= Pending |
  Live(Arc<UdpFlow>)`. Exposes `try_reserve` / `commit` / `remove` /
  `get` / `len` / `drain`. Enforces the rule-wide cap.
- **`Reservation`**: RAII guard returned by `try_reserve`. Drops `Slot::
  Pending` and decrements the cap counter if not consumed by `commit`.
  Listener early-return paths between reserve and commit MUST drop the
  guard, never call an explicit release.
- **`ReplyDemuxTask`**: single tokio task per rule. Holds a
  `FuturesUnordered<ReadWait>` over all live flows' upstream sockets.
  Each `ReadWait` is a `select!` over `flow.cancel.cancelled()` and
  `flow.upstream.readable()`. On `Ready` it drains the socket via
  `try_recv` up to a 32-datagram budget, then re-arms. Single 64 KiB
  receive buffer reused per iteration (boxed once at task start).
- **`RuleReaper`**: single tokio task per rule. Periodic idle scan; emits
  `rule.udp_flow_closed_idle` per evict.
- **`PerListenerLoop`**: one tokio task per listen port. `recv_from` on
  the listener socket; on hit, fast path; on miss, cold path (FR-004).
- **`FlowKey`**: `(u16 listen_port, SocketAddr src)`.
- **`DemuxCommand`**: `enum { AddFlow(FlowKey, Arc<UdpFlow>), Shutdown }`.
  No `RemoveFlow` variant — eviction is `registry.remove + flow.cancel`;
  the demux's `ReadWait` resolves to `Cancelled` and naturally drops its
  `Arc<UdpFlow>`.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001 (memory)**: At 1 000 concurrent flows on a single rule, RSS
  attributable to UDP receive buffers MUST be bounded by `≤ 16 × 64 KiB +
  registry overhead per flow (≤ 1 KiB)`. v1.4-baseline buffers alone are
  ~64 MiB at this scale. Measurement target: `cargo bench -p
  portunus-forwarder --bench udp_data_plane` with new high-flow-count
  scenario.
- **SC-002 (per-rule cap)**: A range UDP rule of N listen ports with
  `udp_max_flows_per_rule = K` MUST admit exactly K concurrent flows
  across all ports, not `N × K`. Validated by integration test
  `udp_range_rule_cap_is_per_rule`.
- **SC-003 (byte stability)**: The full existing `cargo test --workspace`
  suite MUST pass on the new code path with byte-identical data-plane
  content. Tests whose structure couples to the v1.4 per-listener
  `UdpFlowTable` (asserting `flow_table.len()` from a single port's
  perspective, etc.) are rewritten in this work to assert the equivalent
  rule-wide invariant; the implementation plan enumerates which tests
  are deleted vs rewritten. Validated end-to-end by `udp_smoke`
  (preserved) plus new `udp_rule_round_trip_byte_equal`.
- **SC-004 (throughput)**: Single-flow UDP throughput on the existing
  `udp_data_plane` benchmark MUST be within ±5 % of the v1.4-baseline
  median. The optimization is **not** primarily a throughput play;
  regression beyond ±5 % blocks ship. Measurement is Linux perf host
  only, not CI gated (same policy as splice).
- **SC-005 (ICMP eviction latency)**: When a target process is terminated
  during steady-state UDP traffic, the affected flow MUST be evicted
  within the round-trip time of the next outbound datagram (≤ 2 × RTT
  practical). Validated by e2e `udp_smoke_icmp_evict`.
- **SC-006 (channel saturation)**: Under a synthetic first-packet storm
  of 10 000 new flows arriving within 100 ms, no fewer than 90 % of the
  reservations within `udp_max_flows_per_rule` MUST successfully
  commit. AddFlow channel drops are permissible but MUST emit
  `rule.udp_addflow_dropped` and MUST clean up the failed reservation
  (no leaked counter, no leaked socket).

## Assumptions

- `portunus-client` deploy targets are Linux only (confirmed for v1.5
  scope). The new code path uses `UdpSocket::connect()` and relies on
  Linux's ICMP-error reflection semantics; both work on macOS but are
  not explicitly tested.
- `udp_max_flows_per_rule` defaults to a small number (current default
  is 1024). Operators relying on the v1.4 inflated cap of
  `cap × range_size` will see effective capacity drop on upgrade and
  must either raise the cap or split the rule. Migration is called
  out in `## Out of Scope` and in the release notes.
- The existing `udp_flow_idle_secs` semantics are unchanged. Reaper
  granularity (`idle_window / 4`) is unchanged.
- No new external dependencies. `tokio` (already in workspace) provides
  `UdpSocket::try_send`, `try_send_to`, `try_recv`, `readable`,
  `connect`. The `FuturesUnordered` primitive lives in `futures-util`,
  already in the workspace.

## Out of Scope

- **`recvmmsg` / `sendmmsg` batching**. Linux-only batched syscalls
  could yield 10-20 % throughput improvement on top of this work, but
  require unsafe FFI scaffolding and a separate perf-host validation.
  Tracked as a follow-up.
- **Single-socket-per-rule (Option A)**. Considered and rejected:
  multiple clients hitting the same upstream target cannot be demuxed
  by source address alone, since replies all carry the target's
  address as source. The connected per-flow socket model is the only
  one that supports generic UDP forwarding semantics without
  protocol-aware demux.
- **Per-process global demux**. Considered and rejected: cross-rule
  isolation, dynamic registration/cancellation, and per-rule
  fault containment all become harder for a marginal task-count
  reduction (single-digit count) over per-rule.
- **io_uring**. A different reactor model would require replacing
  tokio's UDP integration; out of scope at this size.
- **Migration tooling for operators relying on inflated cap**.
  Operators raise the cap themselves; rules with `range_size ×
  udp_max_flows_per_rule > 65535` (the cap upper bound) MUST be
  split into smaller ranges. No automated detection or rewrite.
- **UDP-side prometheus per-port breakdown** for the four new
  tracing events. They remain log-only, surfaced through the
  operator's existing log aggregation. Adding per-port Prometheus
  rows is a separate, low-priority follow-up.

## Dependencies

- **v0.4.0 UDP forward** (`specs/004-udp-forward/`): this spec replaces
  the per-listener `UdpFlowTable` + per-flow `spawn_reply_pump` model.
  v0.4 contracts on FR-001..FR-019 are inherited where not explicitly
  superseded.
- **v0.11.0 rate-limiting / QoS** (`specs/011-rate-limiting-qos/`): the
  first-packet rate-limit gate (owner + rule) keeps its position in the
  cold path. Step ordering relative to cap check is FR-004 (cap precedes
  rate-limit).
- **v1.4.0 traffic quotas** (013-traffic-quotas; lives in
  `docs/superpowers/plans/2026-05-14-traffic-quotas-and-history.md`,
  no `specs/013-*` directory): per-(user, client) byte quota check
  remains in the same positions in the listener fast/cold paths and
  in the demux reply path. `QuotaHandle::quota_consume_after_send`
  call sites translate verbatim from per-flow reply pump to demux
  task with no semantic change.
- **v1.3.0 splice** (`specs/012-tcp-zero-copy-splice/`): unrelated to
  UDP; coexists. Establishes the precedent for `try_io + readable()`
  reactor integration (no `AsyncFd`), which this spec follows.

## Open Questions

None at spec-acceptance time. Implementation-level details (exact
`mpsc` channel capacity, exact reaper sweep granularity, exact buffer
strategy for sub-`Box<[u8; 65535]>` allocation) are settled by the
implementation plan, not the spec.
