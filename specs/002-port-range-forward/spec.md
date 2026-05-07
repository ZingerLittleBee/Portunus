# Feature Specification: Port-Range Forwarding Rules

**Feature Branch**: `002-port-range-forward`
**Created**: 2026-05-07
**Status**: Draft
**Input**: User description: "Add port-range forwarding rules. A single rule maps a contiguous range of listen ports on the client (e.g., 30000-30050) to a corresponding range of target ports on a single upstream host (e.g., upstream:30000-30050 or shifted to upstream:40000-40050). The operator pushes one rule, manages one rule, observes one rule — replacing N individual rules for ranges that today require N CLI calls. TCP only for v1; UDP is separate."

## Clarifications

### Session 2026-05-07

- Q: Default value of the per-rule port-range cap (FR-008) → A: 1024 ports per range (matches Linux default soft `RLIMIT_NOFILE`; covers typical NodePort / SIP / game-server use cases while leaving fd headroom for accepted connections).
- Q: Minimum per-port observability for a range rule → A: Prometheus stays strict aggregate (cardinality matches single-port rules), AND `rule-stats <rule_id> --per-port` CLI exposes an on-demand per-port breakdown for diagnosis. Best-of-both: clean cardinality budget, operators retain "which port is wedged?" diagnostic without splitting the rule.
- Q: Wire / persistence backward-compat shape for range rules → A: Additive optional fields on the existing `Rule` schema (`listen_port_end`, `target_port_end`). Absent = today's single-port behavior; present = range. One CLI verb (`push-rule`) accepts both shapes, one proto message evolves, zero migration of existing persisted rules. Internally a single-port rule is treated as a range of size 1 (unified forwarder code path).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Push a port-range rule in a single operation (Priority: P1)

An operator manages an edge host that needs to expose a contiguous range
of listen ports (typical examples: a Kubernetes NodePort range, a SIP/RTP
media gateway, a game server cluster, a passive-FTP data port pool). With
v0.1.0 they would run `push-rule` once per port — 51 invocations for a
50-port range, 51 entries in `list-rules`, 51 separate revocations on
teardown. With this feature they push a single rule that names the
listen-port range and the target-port range; the system binds every port
in the range and forwards each to the corresponding upstream port.

**Why this priority**: This is the entire reason the feature exists. The
core value is operator-side: one CLI/HTTP call, one entry in inventory,
one revoke. Any subsequent stories build on top of this.

**Independent Test**: With a fresh server + connected client, push a
range rule for 100 contiguous ports, confirm `list-rules` shows exactly
one entry, then drive a TCP connection through any port in the range
and verify it reaches the corresponding target port.

**Acceptance Scenarios**:

1. **Given** a connected edge client and an upstream reachable on ports
   30000–30050, **When** the operator pushes a rule mapping
   `30000–30050 → upstream:30000–30050`, **Then** the rule reaches the
   `Active` state within the standard ack timeout, all 51 listen ports
   are bound on the client, and `list-rules` shows one entry.
2. **Given** a rule mapping `30000–30050 → upstream:40000–40050`, **When**
   a TCP connection arrives on the client at port `30025`, **Then** it
   is forwarded to `upstream:40025` (matching offset preserved).
3. **Given** a rule mapping a single port via the range syntax (e.g.,
   `30000–30000`), **When** the operator pushes it, **Then** it behaves
   identically to the existing single-port rule API.

---

### User Story 2 - Remove a port-range rule cleanly (Priority: P1)

The same operator removes the previously pushed range rule. All ports
in the range release their listeners, in-flight forwarded connections
get the existing graceful drain, and follow-up operator queries show
the rule and all ports gone.

**Why this priority**: Without correct teardown the feature traps ports
across operator mistakes / migrations and forces manual recovery
(restart the client). Same priority as US1 because operators need both
sides of the lifecycle to ship the feature.

**Independent Test**: Push a range rule, drive traffic through it,
remove it. Verify (a) every port in the range is no longer bound on the
client within the documented drain window, (b) `list-rules` no longer
shows the entry, (c) attempting to bind any of those ports from another
process succeeds.

**Acceptance Scenarios**:

1. **Given** an `Active` range rule and at least one in-flight
   connection on a port in that range, **When** the operator removes the
   rule, **Then** the rule is removed from `list-rules` immediately, the
   in-flight connection is allowed to complete (or hits the drain
   timeout), and after drain every port in the range is unbound on the
   client.
2. **Given** a range rule that has been removed, **When** the operator
   pushes a new rule using any subset of those same ports, **Then** the
   new rule activates without a `port_in_use` rejection.

---

### User Story 3 - Operator sees per-rule observability without label explosion (Priority: P2)

The operator runs `rule-stats <rule_id>` for a 100-port range rule and
sees aggregate `bytes_in / bytes_out / active_connections` for the
entire range as a single row. Prometheus `/metrics` exposes one time
series per existing collector per rule (not one per port), so a 100-port
range adds the same number of series as a single-port rule.

**Why this priority**: Without aggregate stats, a 100-port range bloats
Prometheus cardinality 100× and operator output becomes unreadable —
defeating the inventory-simplification value of US1. P2 instead of P1
because operators can ship the rule before observability lands and the
existing per-rule cardinality is still useful in the interim.

**Independent Test**: Push a 50-port range rule, drive bytes through
several distinct ports in the range, wait one stats interval, confirm
`rule-stats` returns one row whose byte counts equal the sum of bytes
sent across all ports, and `/metrics` exposes exactly one
`forward_rule_bytes_in_total{client,rule}` series for the range
(not 50 series).

**Acceptance Scenarios**:

1. **Given** a 50-port range rule with traffic driven through ports
   `30000`, `30025`, and `30049`, **When** the operator runs
   `rule-stats <rule_id>` after the next stats tick, **Then** one row is
   returned and `bytes_in` / `bytes_out` equal the sum of bytes sent
   across all three ports (within the existing ±1 KB tolerance).
2. **Given** the same rule, **When** scraping `/metrics`, **Then**
   exactly one entry per existing per-rule collector
   (`forward_rule_bytes_in_total`, `forward_rule_bytes_out_total`,
   `forward_rule_active_connections`) appears for that `rule_id` —
   not 50.

---

### User Story 4 - Range conflicts are rejected with a useful error (Priority: P2)

The operator attempts to push a range that overlaps an already-active
rule's listen port (single or range). The push is rejected with an
error that names the offending port(s); no listeners are partially
bound; the existing rule is unaffected.

**Why this priority**: All-or-nothing push is the only sane semantic
for ranges — partial activation would leave the operator with a half-
broken rule and unclear inventory. P2 because the simple rejection is a
follow-up to US1's core mechanism.

**Independent Test**: Push rule A on `30000–30010`, then push rule B on
`30005–30015`. Confirm rule B is rejected, the error references port
`30005` (or the overlap range), no port in `30011–30015` is bound on
the client, and rule A's status is unchanged.

**Acceptance Scenarios**:

1. **Given** an `Active` rule mapping `30000–30010 → upstream:...`,
   **When** the operator pushes a second rule mapping `30005–30015`,
   **Then** the second push is rejected with an error identifying the
   conflict and the second rule never enters `Active` state.
2. **Given** the same starting state, **When** the second rule's range
   is `30011–30020` (immediately adjacent, no overlap), **Then** the
   push succeeds and both rules are `Active`.
3. **Given** a port outside any rule that is already taken by an
   unrelated process on the client, **When** the operator pushes a
   range that includes that port, **Then** the push is rejected with an
   error naming the offending port and no ports in the range are left
   bound.

---

### Edge Cases

- **Inverted range** (`listen_start > listen_end`, or target start >
  target end): rejected at validation before any state change.
- **Length mismatch** between listen and target ranges: rejected at
  validation. The range size on both sides MUST be identical.
- **Single-port range** (`30000–30000`): accepted; behaves identically
  to a single-port rule.
- **Range size at or beyond the configured cap** (see FR-008): push is
  rejected with an error naming the limit; no ports are bound.
- **Operator pushes the same range twice**: the second push is
  rejected with the existing port-conflict error against the operator's
  own first rule (no implicit idempotency / replace semantics — same as
  current single-port behavior).
- **Client disconnects mid-bind**: any partially bound ports release
  before the rule reports `Active`; rule transitions to `Failed` per
  the existing rule state machine.
- **Server restart with persisted range rules**: each persisted range
  rule is re-pushed exactly once on client reconnect (same recovery
  path as today's single-port rules).
- **Per-port stats drilldown when the operator wants it**: not in v1
  scope — see Out of Scope.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The operator MUST be able to push a single forwarding
  rule that names a contiguous listen-port range and a corresponding
  target-port range on a single upstream host, replacing what would
  otherwise require one push per port.
- **FR-002**: A range rule MUST validate that `listen_start ≤
  listen_end`, `target_start ≤ target_end`, and
  `listen_end − listen_start == target_end − target_start`. Failures
  MUST be reported synchronously and MUST NOT bind any port.
- **FR-003**: A successfully activated range rule MUST bind every port
  in `[listen_start, listen_end]` on the client and MUST forward
  traffic from each listen port `p` to the corresponding target port
  `target_start + (p − listen_start)` on the configured target host.
- **FR-004**: Range rule activation MUST be all-or-nothing: if any port
  in the range cannot be bound (already in use externally, conflict
  with another active rule, hit the configured size cap, etc.), the
  rule MUST NOT enter the `Active` state, MUST NOT leave any listener
  bound, and MUST surface an error that names at least one offending
  port (or the violated limit).
- **FR-005**: Single-port forwarding rules from v0.1.0 MUST continue to
  work without changes for existing operators and existing persisted
  rules. The wire and persistence schema MUST evolve **additively**:
  the existing `Rule` representation gains optional `listen_port_end`
  and `target_port_end` fields whose absence means single-port behavior
  (FR-005 backward compat) and whose presence means a range rule.
  Persisted rules from v0.1.0 MUST load on a v0.2.0 server with no
  migration. The operator MUST be able to push both shapes through a
  single CLI verb (`push-rule`) and a single HTTP endpoint, and both
  representations MUST produce observationally identical behavior in
  the degenerate case where the range size is 1.
- **FR-006**: Removing a range rule MUST release every listener in the
  range and MUST follow the existing per-rule graceful drain semantics
  (in-flight connections get the configured drain window before the
  kernel reaps the socket).
- **FR-007**: The operator surface (CLI subcommands and HTTP API) MUST
  represent a range rule as one entity — one ID, one entry in
  `list-rules`, one row in `rule-stats`, one revoke call — regardless
  of range size.
- **FR-008**: The system MUST enforce a configurable upper bound on the
  number of ports per range rule. The default cap MUST be **1024
  ports** (matching the Linux default soft `RLIMIT_NOFILE`, leaving
  ample fd headroom for accepted connections at 2 fds each). Operators
  MUST be able to override the cap via configuration. Pushes that
  exceed the active cap MUST be rejected with an error that names the
  cap and the requested range size.
- **FR-009**: Per-rule byte and active-connection counters MUST
  aggregate across all ports in the range. Prometheus `/metrics`
  collectors and the default `rule-stats <rule_id>` output MUST expose
  exactly one series / one row per range rule per collector (not one
  per port), preserving Prometheus label cardinality with single-port
  rules.
- **FR-011**: For on-demand diagnosis the operator MUST be able to
  request a per-port breakdown of a range rule's counters via a CLI
  flag (e.g., `rule-stats <rule_id> --per-port`). This per-port view
  MUST report the same counters the aggregate does
  (`bytes_in`, `bytes_out`, `active_connections`) for each port in the
  range. The breakdown MUST NOT be exposed via Prometheus (no extra
  per-port time series) so that the cardinality budget in FR-009 / SC-002
  is preserved.
- **FR-010**: A range rule conflict (a pushed range overlapping any
  already-active rule's listen port) MUST be detected before any
  listener is bound, MUST reject the push, and MUST report the
  conflicting port(s).

### Key Entities

- **Range Rule**: A forwarding rule whose listen side and target side
  are each a contiguous closed port range of equal size. Represented
  on the wire and on disk as the existing single-port `Rule` plus two
  optional fields (`listen_port_end`, `target_port_end`). Absent
  fields = single-port behavior (preserves v0.1.0 persistence
  verbatim); present fields = range. Carries the same identity (rule
  ID), state machine, ack semantics, and persistence shape as today's
  single-port rule. Single-port rules are the degenerate case where
  the range size is 1.
- **Range Cap**: An operator-configurable integer bounding the maximum
  port count of any one range rule. Enforced server-side at push time;
  exists to protect operators from accidentally exhausting client file
  descriptors or ephemeral ports.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: An operator can deploy a 100-port contiguous range with a
  single push, with end-to-end activation (push → all 100 listeners
  bound → first byte through any port in the range) completing in the
  same wall-clock budget as today's single-port quickstart for SC-001
  (under 5 minutes from zero on a fresh host pair, under 5 seconds for
  the push step alone on an already-running pair).
- **SC-002**: A range rule of 100 ports adds the same number of
  Prometheus time series as a single-port rule (one per per-rule
  collector). Cardinality of per-rule collectors is independent of
  range size.
- **SC-003**: For a representative range size (50 ports), the operator
  workflow to push, list, observe, and remove the rule requires
  exactly one CLI/HTTP call per step — equal to the single-port
  workflow — versus 50 calls per step in v0.1.0.
- **SC-004**: Removing a range rule releases every port in the range
  within the existing per-rule drain window (default 30 s); after the
  drain, an unrelated process can bind any of those ports without
  collision.
- **SC-005**: A push that violates validation (length mismatch,
  inverted range, overlap with an existing rule, or exceeding the
  configured cap) is rejected synchronously with no listener bound and
  no change to existing rules' state.

## Assumptions

- **TCP only for v1.** UDP forwarding is a separate feature (tracked
  under future spec work). Range rules accept the same protocol field
  as single-port rules but currently only `tcp` is valid.
- **Single upstream host per rule.** A range rule maps to one
  destination host; load balancing across multiple upstreams is out of
  scope.
- **Same-offset mapping.** Listen port `p` always forwards to target
  port `target_start + (p − listen_start)`. Other mappings (mux N
  ports onto one target port, scatter, hash) are out of scope.
- **Per-port stats drilldown is on-demand via CLI only** (clarified
  2026-05-07 — see FR-011). Default `rule-stats` output and all
  Prometheus series stay aggregate per FR-009 / SC-002.
  `rule-stats <rule_id> --per-port` returns the per-port breakdown for
  ad-hoc diagnosis without inflating Prometheus cardinality. Operators
  who want continuous per-port time series in Prometheus must split
  the range into smaller rules.
- **Default cap on range size is 1024 ports** (clarified
  2026-05-07 — see FR-008). Matches Linux default soft
  `RLIMIT_NOFILE`. Operators with raised ulimits can override via
  configuration; operators on hosts with stricter limits should
  lower it to match.
- **Existing persistence and recovery semantics extend to range rules.**
  Range rules are persisted in the same store as single-port rules and
  re-pushed on client reconnect using the same code path.
- **No implicit replace / merge semantics.** Pushing a range that
  overlaps an existing one is a conflict, not an update. Operators
  remove and re-push to change a range — same semantics as single-port
  rules today.

## Out of Scope

- UDP range forwarding (separate feature).
- Multiple upstream targets for a single range rule (load balancing,
  failover).
- Non-contiguous port lists (`30000,30005,30010`) — only contiguous
  ranges in v1.
- Per-port observability drilldown within a range.
- Rebinding ports on rule edit / hot-reload.
- Range rules across multiple clients (a single rule still binds on
  one client only).
