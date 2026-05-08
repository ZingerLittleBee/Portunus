# Feature Specification: Multi-target failover

**Feature Branch**: `007-multi-target-failover`
**Created**: 2026-05-08
**Status**: Draft
**Input**: User description: "extend a forwarding rule to carry an ordered list of targets instead of a single host:port. Client-side health tracking with passive failure detection plus optional active TCP-connect probes. Priority-ordered failover to the next healthy target on primary failure; automatic fall-back to primary on recovery. Existing connections finish on their original target — no mid-flight migration. Single-target rules stay byte-identical to v0.6.0."

## User Scenarios & Testing *(mandatory)*

<!--
  Each user story is INDEPENDENTLY TESTABLE — implementing only US1 yields a
  viable MVP (manual failover via passive health detection); US2 and US3 add
  the recovery loop and observability surfaces respectively. Operator UX (US4)
  rides on top of all three.
-->

### User Story 1 - Primary failure transparently shifts to secondary (Priority: P1)

The operator pushes a forwarding rule with two targets: a primary upstream and a secondary upstream. End-users connecting to the listen port today reach the primary. When the primary becomes unreachable (host down, port closed, network partition), end-users continue to land on the secondary without operator intervention. The operator does not have to delete and re-push the rule, edit any config file, or restart any service.

**Why this priority**: This is the entire reason the feature exists. Without P1, the operator already has the v0.6.0 single-target rule shape — they just don't have failover. P1 alone delivers the value proposition.

**Independent Test**: Configure a rule whose primary points at a deliberately-unreachable host (e.g., a port nothing is listening on) and whose secondary points at a working echo server. Open a TCP connection to the rule's listen port from a third host and confirm the bytes round-trip through the secondary. No operator action required between primary failure and successful traffic on secondary.

**Acceptance Scenarios**:

1. **Given** a rule with `targets: [primary, secondary]` and primary unreachable since rule creation, **When** an end-user opens a new TCP connection, **Then** the connection succeeds via the secondary upstream within ≤1 connect retry budget.
2. **Given** a rule whose primary was reachable and 3 successive connect attempts to it have failed within 30 seconds, **When** the next end-user connection arrives, **Then** the connection is routed to the secondary without attempting the primary (the primary is in the Failed state).
3. **Given** a rule with two targets where both are reachable, **When** an end-user opens a new TCP connection, **Then** the connection lands on the primary (priority 0) and per-target counters reflect bytes through the primary, not the secondary.

---

### User Story 2 - Primary recovery resumes traffic to primary (Priority: P2)

The operator's primary upstream comes back online (someone restarts the box, the network heals, the upstream service is redeployed). End-user connections automatically shift back to the primary on the next new connection — without operator intervention. Existing connections that were already on the secondary are not interrupted; they finish naturally.

**Why this priority**: Without recovery, operating with multi-target rules is a one-way street — every rule eventually drains to its lowest-priority target after enough transient outages. P2 makes the failover policy stable over time. It is independently testable from P1 (you need P1 to fail over before recovery is observable, but P2 adds a separate state transition with its own contract).

**Independent Test**: Same setup as US1. After traffic has shifted to secondary, restart the primary upstream. Confirm that subsequent new connections land on the primary (per-target counters resume increasing on primary). Confirm that connections opened during the failover period and still in flight continue to send/receive bytes through their secondary upstream until they close naturally.

**Acceptance Scenarios**:

1. **Given** a rule whose primary was Failed (3 consecutive connect failures) and 2 consecutive successful connects have since occurred (active probe, or end-user connection that the forwarder retries when the secondary is also probed), **When** the next end-user connection arrives, **Then** the connection lands on the primary and per-target counters resume incrementing on the primary.
2. **Given** a long-running TCP connection opened during failover and currently flowing through the secondary, **When** the primary recovers, **Then** that specific connection continues on the secondary until the end-user closes it (no mid-flight migration).
3. **Given** the same recovery as scenario 1, **When** the operator queries per-rule stats, **Then** `target_failovers_total` shows two transitions (primary→secondary and secondary→primary), per-target byte counters reflect the split, and the per-target health status surfaces "Healthy" for both.

---

### User Story 3 - Operator observes which target traffic is using and why (Priority: P3)

The operator can see, at any moment, which targets a rule has, which one is currently selected for new connections, when each target last failed or last succeeded, and how many bytes have flowed through each target. The operator can spot a degraded target that is silently always-failing-over to the secondary without having to grep client logs or correlate timestamps across boxes.

**Why this priority**: P1+P2 deliver the behaviour. P3 makes the behaviour legible and operable. Without it, a primary that has been silently failing over for months looks identical to a healthy primary from the operator's CLI/UI — they only notice when the secondary also fails. P3 is a P2-not-P1 because the failover itself works without it; it is a P3-not-P2 because the existing per-rule byte counter from v0.4.0 is enough to confirm the rule is moving traffic at all.

**Independent Test**: With a multi-target rule under a mix of working primary and failed primary, query `rule-stats --per-target` (CLI) and open the rule's detail page in the Web UI. Confirm that both surfaces show: (a) the targets list in priority order; (b) each target's current health (Healthy / Failed); (c) each target's last-failure and last-success timestamps; (d) per-target byte counters that reconcile with the per-rule total; (e) the global `target_failovers_total` count.

**Acceptance Scenarios**:

1. **Given** a rule that has experienced 5 failovers since creation, **When** the operator queries `rule-stats --per-target`, **Then** the output lists every target with its priority, current health status, last-failure timestamp, last-success timestamp, consecutive-failure count, and per-target byte counters; and the rule-level `target_failovers_total` reads `5`.
2. **Given** the same rule, **When** the operator opens the rule's detail page in the Web UI, **Then** the targets list renders with health badges, and the per-target byte counters and `target_failovers_total` update live (5-second cadence) as traffic flows.
3. **Given** a multi-target rule whose primary has been Failed for 24 hours, **When** the operator scrapes Prometheus `/metrics`, **Then** `forward_rule_target_failovers_total{client,rule}` shows the total transition count and is suitable for a "primary-down rate over time" alert.

---

### User Story 4 - Operator builds, edits, and removes multi-target rules through the same surfaces as single-target rules (Priority: P3)

The operator pushes a multi-target rule via the same `push-rule` CLI subcommand, the same `POST /v1/rules` HTTP endpoint, and the same Web UI rule-push form they already use for single-target rules. Multi-target is a small extension of the existing surface — not a parallel command. Single-target rules continue to work exactly as before; the operator does not have to learn a second mental model.

**Why this priority**: P3 because the feature can ship without UI / CLI extensions (an operator could in principle issue raw HTTP POSTs with the new shape). But adoption depends on the operator surface mirroring the existing one — so this lands together with P1.

**Independent Test**: Push two equivalent rules: one via the legacy `push-rule edge-01 8080 example.com:80` CLI form, one via the new repeatable `--target` form (`push-rule edge-01 8080 --target example.com:80`). Both are accepted; the response shape carries `targets[]` of length 1 in both cases; the second shape is byte-identical at the data plane to the first. Repeat for the Web UI form: a single-target push using the existing form produces the same on-the-wire rule as the same push with one target row in the new "targets list" form.

**Acceptance Scenarios**:

1. **Given** a v0.6.0-shaped HTTP body (`target_host` + `target_port`), **When** the operator POSTs to `/v1/rules`, **Then** the rule is accepted, the response carries `targets[]` of length 1, and at the data plane the rule is byte-identical to a v0.6.0 rule (no failover state allocated, no extra branch in the hot path).
2. **Given** a new HTTP body shape (`targets: [{host, port, priority}, …]`), **When** the operator POSTs to `/v1/rules`, **Then** the rule is accepted and the response echoes the same targets list with current per-target health.
3. **Given** an HTTP body containing BOTH `target_host` and `targets[]`, **When** the operator POSTs to `/v1/rules`, **Then** the request is rejected with a clear validation error indicating the two shapes are mutually exclusive.
4. **Given** the Web UI rule push form, **When** the operator clicks "Add another target" and fills in a second target row, **Then** the resulting POST carries the new `targets[]` shape and the rule is created.

---

### Edge Cases

- **All targets are unhealthy at the moment a connection arrives**: a target with the lowest "best-known health" is still attempted (otherwise the rule would silently drop traffic). The operator's expectation is "do something; surface the failure" rather than "refuse to attempt". The connection then either succeeds (in which case the target's health flips back toward Healthy) or fails (in which case the end-user sees a connection failure, the target's consecutive-failure count increments, and the next connection follows the same logic).
- **Target list contains DNS names that fail to resolve**: failed DNS resolution counts as a connect failure for that target's health (the target stays in the Failed state until DNS recovers). The existing v0.3.0 resolver behaviour applies per-target — including the 30 s stale-while-error grace.
- **A rule is updated to remove a target that is currently the selected target for new connections**: existing in-flight connections on that target finish naturally; new connections route to the remaining targets per the same priority order.
- **A rule has only one target and that target fails repeatedly**: the rule behaves exactly like a v0.6.0 single-target rule against a failing upstream — every new connection attempt fails, the per-rule connection-failure counter increments, and `target_failovers_total` stays at 0 (no other target to fail over to).
- **Active health probe is enabled but the probe interval is shorter than typical TCP connect timeouts**: the probe respects the same connect timeout the data plane uses; probes never overlap for the same target (a probe in flight defers the next probe until it completes or times out).
- **A single target appears multiple times in the list at different priorities**: the validator rejects the rule at push time. Targets must be unique by `(host, port)`.
- **A UDP rule with multiple targets**: the upstream selection happens once per UDP flow on the first inbound packet from a given source `(addr, port)`. The chosen upstream sticks until the flow is idle-evicted. Failover to a new target applies only to NEW flows that have not yet been pinned to an upstream.

## Requirements *(mandatory)*

### Functional Requirements

**Rule shape and back-compat**

- **FR-001**: A forwarding rule MUST carry an ordered list of targets of length ≥ 1. Each target carries a host (IP literal or DNS name), a port (1..=65535), and a priority (lower number = higher priority).
- **FR-002**: A rule whose targets list is exactly length 1 MUST behave identically to a v0.6.0 single-target rule: same on-the-wire bytes, same activation events, same per-rule statistics — no failover state allocated.
- **FR-003**: The operator MUST be able to push a rule using the v0.6.0 single-target shape (`target_host` + `target_port`) and have the system implicitly construct a one-element targets list. Existing rules carried in persisted state from v0.6.0 MUST continue to work without any operator action.
- **FR-004**: The operator MUST be able to push a rule using the new shape (a `targets` array). The system MUST reject any push that supplies BOTH the legacy and the new shape, and MUST reject any push that supplies neither.
- **FR-005**: The system MUST reject any rule push whose targets list contains duplicate `(host, port)` pairs.

**Selection and failover behaviour**

- **FR-006**: For a multi-target rule, the system MUST select the highest-priority target whose current health is Healthy when accepting a new TCP connection or first inbound UDP packet from a new flow.
- **FR-007**: When all targets are non-Healthy, the system MUST still attempt the highest-priority target rather than silently dropping the connection. The end-user sees the connection failure if no target succeeds.
- **FR-008**: When a target's connect attempt fails, the system MUST treat that as a passive failure signal for that target. Three consecutive failures within a 30-second sliding window MUST mark the target as Failed.
- **FR-009**: When a target in the Failed state has two consecutive successful connects (whether end-user-driven or active-probe-driven), the system MUST flip it back to Healthy.
- **FR-010**: A health-state transition (Healthy→Failed or Failed→Healthy) MUST increment the rule's `target_failovers_total` counter.
- **FR-011**: When a higher-priority target becomes Healthy after being Failed, the system MUST route the next new connection to that target. Existing in-flight connections MUST continue on whatever target they originally chose — there is no mid-flight migration.
- **FR-012**: For UDP rules, the upstream selected for a given flow on its first packet MUST stick for the lifetime of that flow (until idle-evict). Failover to a new target MUST only apply to subsequent flows.

**Active health probing (optional)**

- **FR-013**: A rule MAY carry an `active_health_check_interval_secs` setting. When set, the system MUST periodically perform a TCP-connect probe to each target at that interval. Probe results feed the same passive health-state machine (FR-008, FR-009).
- **FR-014**: The active probe MUST share the same connect timeout the data plane uses. Probes for the same target MUST NOT overlap; if a probe is still in flight when the interval elapses, the next probe MUST be deferred until the in-flight one completes or times out.
- **FR-015**: When `active_health_check_interval_secs` is unset, the system MUST NOT consume resources or schedule probes for that rule.

**Operator observability**

- **FR-016**: The per-rule statistics surface MUST report, per target: current health state, last-failure timestamp, last-success timestamp, consecutive-failure count, bytes transferred in, bytes transferred out, connections accepted (TCP) or flows accepted (UDP). It MUST also report the rule-level `target_failovers_total`.
- **FR-017**: The CLI per-rule stats command MUST surface per-target counters only behind an explicit `--per-target` flag, mirroring how range rules surface `--per-port`. The default output MUST stay at the same per-rule cardinality as v0.6.0.
- **FR-018**: The operator metrics endpoint MUST expose `forward_rule_target_failovers_total{client, rule}` as a counter. Per-target byte counters MUST NOT be exported as default Prometheus series; they remain query-only via the per-rule stats surface to keep `/metrics` cardinality bounded.
- **FR-019**: The Web UI rule detail page MUST render the targets list in priority order with per-target current health badges, last-failure / last-success timestamps, and live per-target byte counters that update on the same 5-second cadence as the existing per-rule live stats.
- **FR-020**: The Web UI rule push form MUST allow the operator to add and remove target rows inline. Single-target pushes via this form MUST produce a request indistinguishable from the v0.6.0 single-target form at the data plane.

**Authorisation**

- **FR-021**: The targets list MUST NOT participate in the multi-tenant authorisation envelope (FR from 005-multi-user-rbac). Existing grants continue to gate `(client, listen-port range, protocol)`. An operator who is authorised to push a rule on a given listen port MAY freely choose targets without additional grant checks.

### Key Entities

- **Target**: a single (host, port) upstream within a rule, with a priority (lower = preferred). Carries current health state (Healthy / Failed), last-failure and last-success timestamps, and per-target lifetime counters.
- **Rule (extended)**: gains a non-empty ordered list of Targets, an optional active-probe interval, and a rule-level `target_failovers_total` counter. Inherits everything from the v0.6.0 rule shape (listen port range, protocol, owner, DNS preference, etc.).
- **Health state machine**: per-target state, governed by passive (data-plane connect outcomes) and optional active (probe outcomes) signals. Transitions emit audit-grade log events that the existing audit ring captures alongside operator allow/deny entries.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: When a primary upstream goes down, end-user connections shift to the secondary on the next new connection without any operator action — the operator does not have to log in, edit a file, or run a CLI command. Verified by an end-to-end test that kills the primary mid-traffic and observes the secondary handling the next 100 connections without intervention.
- **SC-002**: When the primary recovers, end-user connections shift back to it within at most one new-connection round-trip after the rule's recovery threshold is met (FR-009). Existing in-flight connections complete on their original target.
- **SC-003**: Single-target rule throughput regresses by ≤ 1% on the existing TCP forwarder data-plane benchmark vs the v0.6.0 baseline. Verified by the criterion benchmark gate currently enforced in CI.
- **SC-004**: An operator can push, edit, and delete a multi-target rule through the same CLI subcommand and the same Web UI form they use for single-target rules — no separate menu, no separate command. Verified by a UX walkthrough that creates a multi-target rule using only surfaces the operator already knows.
- **SC-005**: An operator can identify, in under 30 seconds of looking at the Web UI rule detail page, which target a multi-target rule currently sends new connections to and which targets have failed in the last hour. Verified by the per-target health badges and per-target byte counters surfacing on the detail page.
- **SC-006**: A `/metrics` scrape on a host running 100 multi-target rules adds ≤ 100 new series compared to the same 100 single-target rules — i.e., the per-target byte counters do not appear in the default `/metrics` payload (FR-018), only `forward_rule_target_failovers_total{client, rule}` is added per rule.
- **SC-007**: When all targets are unhealthy, the data plane attempts the highest-priority target rather than silently dropping. The end-user sees an explicit connection failure within the rule's connect timeout, and the per-rule connection-failure counter increments — no mystery silent drops.

## Assumptions

- **Static target list across rule lifetime**: a rule's targets list is set at push time and only changes on a push of a new rule with the same id (the existing v0.6.0 rule-update semantics). Per-target hot-add / hot-remove without reissuing the rule is out of scope.
- **TCP-connect health checks are sufficient for v1**: the active probe checks raw TCP reachability of `host:port`. L7 health (HTTP `GET /health`, gRPC health-check protocol, etc.) is out of scope. Operators who need L7 should put a v1 health endpoint in front of their target.
- **Strict priority order; no weighted distribution**: every new connection prefers index 0 if Healthy. Weighted round-robin, hashed source-IP affinity, and least-connections strategies are out of scope. The next release may revisit if there's demand.
- **Targets are not part of the authorisation envelope**: 005-multi-user-rbac grants gate which clients an operator can push rules on and which listen-port ranges they can use. The targets list is an operator-side concern within those grants — not a separate gate.
- **Failover state is in-process and ephemeral**: per-target health is held by the running forwarder and reset on client restart. The first connection after a restart treats every target as Healthy. Persisting health across restarts is out of scope.
- **Single-target rules are byte-identical to v0.6.0**: the Constitution Principle II hard guarantee. The implementation strategy is "branch on `targets.len() == 1` at the rule-activation layer; multi-target lives in a separate code path that single-target rules never enter".
- **Existing v0.3.0 DNS resolver behaviour applies per-target**: each target with a DNS name resolves on first connect and caches per the resolver's existing `[5 s, 5 min]` TTL clamp + 30 s stale-while-error grace. Resolution failures count as connect failures for that target's health.
- **The 006 Web UI is the operator's primary surface for the per-target detail view**. The CLI `rule-stats --per-target` exists for headless / scripted use but the bar for "operator can identify which target is healthy in 30 s" (SC-005) is set against the Web UI.
