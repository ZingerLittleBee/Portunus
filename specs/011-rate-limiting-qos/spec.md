# Feature Specification: Connection Rate Limiting & QoS

**Feature Branch**: `011-rate-limiting-qos`
**Created**: 2026-05-09
**Status**: Draft
**Input**: User description: see [Inputs](#inputs)

## Inputs

> Connection rate limiting / QoS for forward-rs. Add per-rule and per-client
> caps on: (a) bandwidth (bytes per second, ingress/egress), (b)
> packet/connection rate (new TCP connections per second, new UDP flows per
> second), and (c) concurrent connection / flow count. Caps are configurable
> through the existing operator API and Web UI, persisted in SQLite alongside
> rules, and enforced in the data plane on forward-client. When a cap is hit,
> new connections are rejected (TCP RST or UDP drop) and bandwidth caps shape
> via in-flight throttling. Surface counters in `/metrics` and per-rule stats
> so operators can see throttle/reject events. Must preserve byte-stable
> wire/auth seams from v0.10; rate-limit fields are additive on `Rule` and
> stats messages. Tenant isolation: per-client caps prevent one operator's
> rules from starving another's. Hot-reload of cap values without dropping
> in-flight forwarding.

## User Scenarios & Testing *(mandatory)*

### User Story 1 — Per-rule bandwidth and connection caps (Priority: P1)

An operator owns a rule that exposes a backend service through forward-rs.
Without caps, a noisy or runaway client can saturate the upstream link or
exhaust the backend's connection budget, which is invisible to other rules
sharing the same forward-client. The operator wants to attach explicit
**bandwidth** (bytes/second, both directions), **new-connection rate** (TCP
conn/s or UDP flow/s), and **concurrent connection** caps to a single rule
through the operator API or Web UI. When a cap is exceeded, new connections
or packets that would breach it are rejected immediately (TCP RST after
accept; UDP packet dropped before forwarding) and bandwidth-capped flows are
**shaped** by in-flight throttling rather than killed. Per-rule counters
expose throttle and reject events so the operator can confirm the cap is
working and tune it.

**Why this priority**: This is the smallest unit that delivers value on its
own. A single capped rule is a complete, testable improvement over v0.10 and
addresses the most common operational pain (one rule starving the host or the
backend). Per-client / per-owner caps and Web UI polish are additive on top
of this slice.

**Independent Test**: A two-host setup — one forward-client with a single TCP
rule capped at 1 MB/s ingress and 100 concurrent connections — sustained
load from `iperf3` and `hey` against that rule. Assert that (a) measured
throughput converges within ±10% of the cap over a 30s window, (b) the 101st
concurrent connection attempt receives a TCP RST and increments the
`rate_limit_reject_total{reason="conn_concurrent"}` counter, (c) other
unrelated rules on the same forward-client see no throughput regression.

**Acceptance Scenarios**:

1. **Given** a TCP rule with `bandwidth_in = 1 MB/s` and a steady client
   sending at 5 MB/s, **When** traffic flows for 30 seconds, **Then** the
   measured ingress throughput on that rule is ≤ 1 MB/s ± 10% and
   `rate_limit_throttle_seconds_total{rule, direction="in"}` increases
   monotonically.
2. **Given** a TCP rule with `concurrent_connections = 100` and 100 active
   connections, **When** the 101st `connect()` attempt is accepted by the
   listener, **Then** forward-client closes it with a RST before any bytes
   are forwarded upstream and `rate_limit_reject_total{rule, reason="conn_concurrent"}`
   increments by exactly 1.
3. **Given** a TCP rule with `new_connections_per_second = 50`, **When** a
   client opens 200 connections inside 1 second, **Then** roughly 50 of them
   are accepted per second and the surplus is rejected with RST and counted
   under `reason="conn_rate"`.
4. **Given** a UDP rule with `new_flows_per_second = 20`, **When** 100 unique
   source-port flows arrive in 1 second, **Then** roughly 20 new NAT
   bindings are created per second and surplus first-packets are dropped
   silently and counted under `reason="udp_flow_rate"`.
5. **Given** a rule with no rate-limit fields set, **When** v0.11
   forward-client services it, **Then** the data path is byte-equivalent to
   v0.10 (no token-bucket overhead is observable in the bench harness).

---

### User Story 2 — Per-owner caps prevent cross-tenant starvation (Priority: P2)

A single forward-client agent typically hosts rules from several operators
(distinct RBAC owners). Without an owner-level cap, one operator can attach
many high-traffic rules and consume the host's NIC, CPU, or socket budget,
silently degrading every other operator's rules on the same node. The
operator administrator wants to attach **per-owner caps** on a forward-client
that aggregate across all that owner's rules and bound the slice each
operator can consume. Per-rule caps still apply individually below the
owner-level ceiling.

**Why this priority**: Multi-tenant isolation is a frequently asked
operational property but is meaningful only once per-rule caps exist. Many
small deployments run a single owner per client and don't need this layer.

**Independent Test**: A forward-client with two owners, each with three
rules. Owner A is capped at 10 MB/s aggregate; owner B has no aggregate cap.
Drive owner A's rules to a combined 50 MB/s offered load and owner B's rules
to 20 MB/s. Assert owner A's combined throughput converges to 10 MB/s ± 10%
while owner B's combined throughput stays unaffected, and the
`rate_limit_throttle_seconds_total{owner="A"}` counter grows while
`{owner="B"}` does not.

**Acceptance Scenarios**:

1. **Given** owner A with aggregate `bandwidth_in = 10 MB/s` and three of A's
   rules each offered 20 MB/s, **When** traffic runs for 30s, **Then** the
   sum of A's rules' measured ingress is ≤ 10 MB/s ± 10% and the cap is
   shared among the three rules in proportion to offered load.
2. **Given** the same setup plus owner B with no aggregate cap and a single
   rule offered 20 MB/s, **When** traffic runs for 30s, **Then** B's rule
   reaches its own per-rule cap (or line-rate if uncapped) and is unaffected
   by A's throttling.
3. **Given** a per-owner concurrent-connection cap of 500, **When** A's rules
   collectively hold 500 connections and a new connection attempt arrives
   on any of A's rules, **Then** it is rejected with RST and counted under
   `rate_limit_reject_total{owner="A", reason="owner_concurrent"}`.

---

### User Story 3 — Hot-reload caps without dropping in-flight forwarding (Priority: P2)

The operator wants to tune a cap (raise or lower) on a live rule without
disturbing existing connections. Today, raising a per-rule cap should let
in-flight throttled connections immediately benefit; lowering a cap should
take effect on the next packet/token-refill cycle without forcibly closing
established connections. New connections that would breach the new cap are
rejected normally.

**Why this priority**: Tuning happens during incidents (e.g., "we're being
DoS'd, drop the cap to 100 KB/s right now"). If a cap update kills live
connections, operators won't trust it.

**Independent Test**: A single rule with 10 MB/s cap and a steady iperf3
flow. The operator pushes a rule update lowering the cap to 1 MB/s. Assert
the existing connection's measured throughput drops to ≤ 1 MB/s within 2
seconds of the push, the connection is **not** closed by forward-client, and
no error appears in the operator audit ring.

**Acceptance Scenarios**:

1. **Given** a rule capped at 10 MB/s with one in-flight connection at line
   rate, **When** the operator pushes a rule update setting the cap to
   1 MB/s, **Then** the connection's measured throughput converges to
   ≤ 1 MB/s ± 10% within 2 seconds and the TCP connection state remains
   `ESTABLISHED`.
2. **Given** a rule capped at 1 MB/s, **When** the operator raises the cap
   to 10 MB/s, **Then** an in-flight throttled connection's throughput rises
   toward 10 MB/s within one token-refill cycle.
3. **Given** an inflight rule update lowering the cap, **When** the update
   is applied, **Then** the audit ring records exactly one `rule_updated`
   event and the data plane records zero `connection_reset_by_us` events
   attributed to the change.

---

### User Story 4 — Web UI exposes caps and visualises throttle activity (Priority: P3)

The Web UI rule editor gains optional inputs for the cap fields, displays
current cap values on the rules table, and the rule detail / metrics view
shows the throttle and reject counters in human-readable form (last 5
minutes, last 1 hour). Operators can confirm at a glance that a cap is
active and how often it is hit.

**Why this priority**: The operator API + `/metrics` already give a complete
control & observation surface; the UI is a usability improvement that makes
caps approachable for operators who don't scrape Prometheus.

**Independent Test**: With a P1 capped rule actively throttling, open the
rule detail page and assert: cap values are displayed with units, throttle
seconds in last 5 minutes is non-zero and matches `/metrics` within ±5%, the
rules table column shows a "throttling" badge.

**Acceptance Scenarios**:

1. **Given** the rule editor open in create mode, **When** the operator sets
   `bandwidth_in = 1 MB/s` and submits, **Then** the rule is created with
   the cap stored and visible on the rules table.
2. **Given** an active throttled rule, **When** the operator opens its
   detail page, **Then** the throttle and reject counts for the last 5
   minutes and 1 hour are displayed alongside the configured caps.

---

### Edge Cases

- **Cap = 0 vs cap unset**: Cap fields are optional. Unset = no limit (v0.10
  behaviour). Cap explicitly set to 0 means "no traffic allowed" and SHOULD
  be rejected at validation time as an obvious operator mistake; operators
  who want to disable a rule should toggle `enabled` instead.
- **Cap = u32::MAX or very large values**: Treated as effectively unlimited;
  no overflow in token-bucket math.
- **Per-owner cap with zero matching rules**: Cap exists but has no traffic;
  counters stay at zero and metrics expose the cap with usage = 0.
- **Per-rule cap raised above per-owner cap**: Per-owner ceiling still
  binds; no error at validation time (the per-owner cap simply masks the
  per-rule one). The Web UI MAY warn but not block.
- **Bandwidth cap very low (< 1 KB/s) with TCP**: Token-bucket math must
  still allow at least one MTU per refill cycle to avoid permanent stall;
  the spec defines a minimum effective cap (see Assumptions).
- **Connection-rate cap of 0**: Rejected at validation time as above.
- **NAT table eviction interaction (UDP)**: Concurrent flow count tracks the
  v0.4 NAT table size; flows that age out of NAT decrement the count
  immediately so a rule's flow count reflects live state.
- **Rule deleted while throttled connections exist**: v0.10 behaviour is
  unchanged — existing connections continue to completion; the per-rule
  token bucket is dropped after the last connection closes.
- **Hot-reload races with token-bucket fill**: A cap update while a refill
  tick is in flight MUST NOT cause a token-count discontinuity that
  artificially boosts or starves a connection; the new cap takes effect on
  the next refill tick.
- **Per-owner cap on a forward-client whose owner has been deleted**:
  Per-owner caps are anchored to the owner's stable RBAC ID; cap is
  retained until the owner's last rule on that client is removed, then GC'd.
- **Multi-target rules** (v0.7): Caps are applied at the rule level
  (aggregate across all targets); per-target sub-caps are out of scope for
  v0.11 — see Assumptions.

## Requirements *(mandatory)*

### Functional Requirements

**Cap configuration & persistence**

- **FR-001**: A rule MUST accept optional cap fields covering: bandwidth
  ingress (bytes/sec), bandwidth egress (bytes/sec), new-connection rate
  (TCP conn/sec or UDP flow/sec), and concurrent connection / flow count.
  Each field is independently optional.
- **FR-002**: Cap values MUST be persisted in SQLite as additive columns
  on the existing rules table; no new tables are required for per-rule
  caps. Schema-version range advances by exactly one minor version.
- **FR-003**: A per-owner aggregate cap envelope (same four dimensions)
  MUST be configurable on a `(client, owner)` pair through the operator
  API and persisted alongside RBAC state.
- **FR-004**: Caps MUST be expressible with units the operator UI can
  display (bytes/sec with K/M/G suffixes, integer counts).

**Wire compatibility**

- **FR-005**: Cap fields on `Rule` MUST be additive proto fields with
  numeric tags ≥ 16 (next free range after v0.10) so v0.10 forward-clients
  receiving a v0.11-encoded `Rule` decode without error and ignore unknown
  fields.
- **FR-006**: A v0.11 server pushing a rule with any cap field set to a
  forward-client whose self-reported version is < 0.11 MUST refuse the
  push with a structured error (capability gate analogous to v0.10's
  `proxy_protocol_unsupported_by_client` and v0.9's
  `sni_unsupported_by_client`); the rule MUST NOT activate anywhere.
- **FR-007**: When no cap fields are set on any rule, the server's
  on-the-wire `RuleUpdate` MUST be byte-identical to v0.10 (regression
  bench gate, analogous to v0.9 → v0.8 byte-stability).

**Data-plane enforcement**

- **FR-008**: Bandwidth caps MUST be enforced via in-flight throttling
  (delaying reads/writes) without closing established connections.
- **FR-009**: Concurrent-connection and connection-rate caps MUST be
  enforced by **rejecting** new connection attempts: TCP listener accepts
  the socket and closes it with RST before any bytes flow to the upstream;
  UDP first-packets are dropped silently before NAT binding is created.
- **FR-010**: Reject decisions MUST happen before the v0.7 multi-target
  selection / failover step so a rejected connection never counts as a
  target connect failure.
- **FR-011**: Hot-reload of cap values MUST take effect within one
  token-refill cycle (≤ 1 second by spec) without closing existing
  connections.
- **FR-012**: Data-plane events emitted by the rate limiter (throttle,
  reject) MUST be tracing-only and MUST NOT enter the SQLite operator
  audit ring (consistent with v0.9 D13).

**Tenant isolation**

- **FR-013**: When a per-owner cap is set, the per-owner ceiling MUST
  bind before per-rule caps; a rule whose per-rule cap is higher than
  the per-owner ceiling MUST still be throttled by the per-owner ceiling.
- **FR-014**: Reject and throttle events caused by per-owner caps MUST
  be attributable to the owner via the `owner` metric label distinct
  from per-rule cap events.

**Observability**

- **FR-015**: The forward-client `/metrics` endpoint MUST expose:
  - `rate_limit_reject_total{client, rule, owner, reason}` counter,
    where `reason ∈ {conn_concurrent, conn_rate, udp_flow_rate, owner_concurrent, owner_conn_rate, owner_udp_flow_rate}`.
  - `rate_limit_throttle_seconds_total{client, rule, owner, direction}`
    counter, where `direction ∈ {in, out}`, summing wall-clock time
    spent blocking reads/writes due to bandwidth caps.
  - `rate_limit_active_connections{client, rule, owner}` gauge.
  - `rate_limit_owner_active_connections{client, owner}` gauge.
- **FR-016**: Per-rule `RuleStats` MUST gain additive fields counting
  reject events by reason and total throttle seconds, mirroring the
  metrics surface, with proto tags ≥ 16.
- **FR-017**: A new `OwnerRateLimitStats` message MUST be added to
  `StatsReport` as an additive top-level field carrying per-owner reject
  and throttle counts and active-connection gauges.

**Operator API & UI**

- **FR-018**: The operator API rule create / update endpoints MUST
  accept the new cap fields with backward-compatible JSON / proto
  encoding (omitted = no cap).
- **FR-019**: The operator API MUST expose a separate endpoint or sub-
  resource for setting per-owner cap envelopes per `(client, owner)`.
- **FR-020**: Validation MUST reject cap = 0 and negative values at
  API boundary with a 400 / structured error before persistence.
- **FR-021**: The Web UI rules table MUST add a column showing
  configured cap values for each rule (compact form), and the rule
  editor MUST add optional inputs for each cap field with unit
  suffixes.

### Key Entities

- **Rate-limit envelope**: A bundle of four optional caps
  `{bandwidth_in, bandwidth_out, new_connections_per_second, concurrent_connections}`.
  Attached to a `Rule` (per-rule envelope) or to a `(client, owner)`
  pair (per-owner envelope). Each cap is null = unlimited.
- **Token bucket**: Per-cap state living on forward-client; tracks current
  tokens, last refill time, and configured `{rate, burst}`. One bucket
  per cap per scope (rule × direction, owner × direction).
- **Reject reason**: Enumerated label distinguishing why a new
  connection / flow was refused (per-rule concurrent, per-rule rate,
  UDP flow rate, per-owner concurrent, per-owner conn rate, per-owner
  UDP flow rate).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A rule capped at X bytes/sec ingress, driven by a steady
  client offering ≥ 5×X for 30 seconds, MUST measure within ±10% of X
  averaged over the last 20 seconds of the run, for X ∈ {100 KB/s,
  1 MB/s, 10 MB/s, 100 MB/s}.
- **SC-002**: A rule capped at N concurrent connections MUST accept
  exactly N (±0) and reject the (N+1)th within 50 ms of accept, for
  N ∈ {10, 100, 1000}.
- **SC-003**: A rule capped at R new connections per second MUST accept
  R ± 10% and reject the surplus, sustained over a 60-second window,
  for R ∈ {10, 100, 1000} (P1).
- **SC-004**: A forward-client with **no** rule using rate-limit fields
  MUST show ≤ 2% throughput regression and ≤ 5% per-connection setup
  latency regression vs. v0.10 on the existing data-plane bench harness
  (regression gate analogous to v0.9 vs v0.7).
- **SC-005**: A cap update pushed via the operator API MUST take
  measurable effect on an in-flight throttled connection within 2
  seconds end-to-end (operator API call to throughput convergence) and
  MUST NOT cause any TCP RST attributable to the change.
- **SC-006**: Per-owner caps MUST limit owner A's combined throughput
  while owner B's rules remain unaffected, with cross-talk ≤ 5% measured
  over 30 seconds (P2).
- **SC-007**: A v0.10 forward-client MUST refuse to register / accept
  rules carrying any cap field, with a clear capability error returned
  to the operator within 100 ms of the push.

## Assumptions

- **Tenant identity**: "Per-client" in the user input is interpreted as
  **per-RBAC-owner within a forward-client** because that is the
  tenant boundary the user explicitly named ("one operator's rules
  from starving another's"). The forward-client agent itself is a
  shared host and does not get its own aggregate cap in v0.11; node-
  level caps can be added later if needed.
- **Bucket model**: Token-bucket with `{rate, burst}` is the
  enforcement mechanism for both bandwidth and connection-rate caps.
  Default burst = 1 second of rate when the operator does not specify
  a burst explicitly. Smaller burst trades smoother shaping for worse
  small-message latency.
- **Throttle behaviour**: Bandwidth-cap exhaustion **blocks** the
  affected direction's read/write loop (preserves connections, adds
  latency). Connections are never closed by the rate limiter directly;
  drops happen only at upstream socket buffer overflow, which is the
  same backpressure path that exists in v0.10.
- **Multi-target** (v0.7): Per-rule caps apply at the rule aggregate
  (sum of all that rule's targets). Per-target sub-caps are out of
  scope for v0.11. Failover continues to operate normally on top of
  caps.
- **UDP flow definition**: A "flow" is one entry in the v0.4 NAT table
  (keyed by source IP + source port + listener). Concurrent flow count
  reads directly from the NAT table size for a rule.
- **Scope locked out**:
  - Rate limiting on SNI dispatch / per-SNI sub-rule (v0.9) — the cap
    is on the matched rule, not the listener.
  - Adaptive / congestion-aware caps (e.g., AIMD on observed loss).
  - L7-aware caps (per-Host or per-path) — out of scope until / unless
    L7 mode is added.
  - Per-source-IP caps (DDoS-style fairness) — explicitly deferred.
  - Cluster-wide caps that require coordination across forward-clients.
- **Constitution** (v2.0.1): TLS + bearer token auth seam unchanged. No
  new workspace dependencies (token-bucket is hand-rolled in
  forward-client using existing `tokio` primitives).
- **SQLite schema**: Existing `rules` table gains optional cap columns
  (NULL = unlimited). Per-owner envelopes live in a new
  `rate_limit_owner` table keyed by `(client_id, owner_id)`. Schema
  version range shifts `[1,2] → [1,3]` (v0.10 → v0.11).
- **Metrics cardinality**: New labels (`rule`, `owner`, `reason`,
  `direction`) follow v0.5+ conventions. Cardinality is bounded by
  `(rules × owners × 6 reason values)` which is the same envelope as
  v0.10.
- **Hot-reload**: Reuses v0.10's rule-update push path and the
  hot-reload semantics validated for v0.9 (T072: rule reload preserves
  in-flight forwarding). Cap update is implemented as a swap of the
  token-bucket configuration pointer; no allocation on the hot path.
