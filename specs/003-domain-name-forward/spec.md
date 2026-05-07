# Feature Specification: Domain-name forwarding targets

**Feature Branch**: `003-domain-name-forward`  
**Created**: 2026-05-07  
**Status**: Draft  
**Input**: User description: "domain-name forwarding: target host in a forwarding rule may be a DNS name (e.g. `api.example.com:443`) instead of an IP. The client resolves the name on first connect, caches the result honoring DNS TTL with a sensible upper bound, falls back gracefully if resolution fails (rule stays Active, individual conn fails with a recorded reason), and prefers IPv4 when both A and AAAA are available unless the operator opts in to v6. Surface DNS resolution failures as a per-rule counter so operators can spot bad targets without grepping logs. Additive on top of v0.2.0 — IP targets keep working byte-identically."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Push a rule whose target is a DNS name (Priority: P1)

An operator manages a fleet of forwarding clients that point at upstream
services hosted behind dynamic IPs (cloud load balancers, anycast
endpoints, services that publish round-robin A records). Today the
operator must look up the current IP, push a rule with the literal
address, and re-push whenever the upstream's IP changes. The operator
wants to push a rule once with the upstream's stable DNS name and have
the client follow the name as it moves.

**Why this priority**: This is the single behavior the feature exists
to deliver — without it none of the other stories can be tested. A
working MVP is "operator pushes one rule with a DNS target, traffic
reaches the upstream."

**Independent Test**: Spin up an upstream echo on `127.0.0.1:41000`,
add a hosts-file or local DNS entry mapping `echo.test → 127.0.0.1`,
push a rule `8080 → echo.test:41000`, then `curl localhost:8080` from
the client side and verify the bytes round-trip. The same recipe with
no DNS entry (literal `127.0.0.1:41000`) MUST still work byte-for-byte
identically — that proves the additive promise.

**Acceptance Scenarios**:

1. **Given** a rule with target `echo.test:41000` where `echo.test`
   resolves to `127.0.0.1`, **When** a client opens a TCP connection
   to the rule's listen port, **Then** the bytes are forwarded to
   `127.0.0.1:41000` and the response returns over the same socket.
2. **Given** a rule with target `127.0.0.1:41000` (literal IPv4),
   **When** the client opens a connection, **Then** the behavior is
   indistinguishable from the v0.2.0 baseline — same latency
   characteristics, same observability, same persistence shape.
3. **Given** a rule with target `[::1]:41000` (literal IPv6),
   **When** the client opens a connection, **Then** the bytes round-trip
   to `[::1]:41000`. (IPv6 literals are still allowed; the IPv6 opt-in
   from US3 only governs DNS resolution preference.)

---

### User Story 2 - DNS resolution fails or recovers without manual intervention (Priority: P2)

An operator points a rule at `staging.example.com:5432`. Sometime
later, the staging DNS record is deleted (or the resolver itself is
flapping). Without this story, the rule's listener would still accept
connections and stall indefinitely while the client retries hopelessly.
The operator wants:

- The rule to stay Active so it auto-recovers the moment DNS comes
  back — no operator re-push required.
- Each affected end-user connection to fail fast with a clear,
  named reason ("dns_resolution_failed" or similar) rather than
  hanging or returning a generic timeout.
- The previous successful resolution to keep working until either its
  TTL elapses or the upstream truly moves — DNS hiccups MUST NOT cause
  traffic to drop while a cached answer is still fresh.

**Why this priority**: Defines the failure mode. Without this, DNS-name
rules are strictly worse than IP rules under partial failure (they hang
where IP rules at least surface a reachable/unreachable answer
immediately). With it, DNS-name rules degrade gracefully and recover
automatically.

**Independent Test**: Push a rule with a DNS target, drive one
connection to confirm the cached answer works, then break DNS (point
the resolver at a black hole) and confirm: the rule stays Active in
`list-rules`, fresh connections fail with reason `dns_resolution_failed`
within a bounded time, and connections that fall within the cached-TTL
window keep succeeding. Restore DNS, wait at most one TTL, and confirm
new connections succeed again without operator action.

**Acceptance Scenarios**:

1. **Given** a rule with target `flaky.test:80` whose DNS record is
   currently broken, **When** an end-user opens a connection through
   the rule, **Then** the connection is refused or RST'd within
   3 seconds with a structured reason recorded against the rule, AND
   the rule's status remains `Active` (not `Failed`, not `Removed`).
2. **Given** a rule whose target was successfully resolved within the
   last cache window, **When** the upstream DNS resolver becomes
   unreachable, **Then** subsequent connections to that rule continue
   to succeed using the cached answer until the TTL expires.
3. **Given** a rule whose DNS target was failing, **When** DNS
   resolution starts working again, **Then** the next connection
   attempt succeeds without any operator action and without restarting
   the client.

---

### User Story 3 - Operator opts a rule in to IPv6 (Priority: P3)

Most operators run dual-stack upstreams where the AAAA record is
authoritative but transit between client and upstream is not yet IPv6
clean. The default — pick A when both A and AAAA exist — keeps these
deployments quiet. A smaller cohort runs IPv6-first or IPv6-only
upstreams (cloud regions, modern internal services); they need to opt
in.

**Why this priority**: Strictly an unblocker for IPv6-first deployments.
Default behavior covers the majority case correctly without operator
input, so this story can ship after US1+US2 without blocking the MVP.

**Independent Test**: Add a host entry mapping `dual.test` to both
`127.0.0.1` and `::1`. Push two rules: one default
(`8080 → dual.test:41000`) and one IPv6-opted-in
(`8081 → dual.test:41000`, IPv6-preferred). Verify the first connects
to `127.0.0.1:41000` and the second to `[::1]:41000`. Same DNS name,
two rules, two address families — observably different by inspecting
the proxy connection logs and connection counters.

**Acceptance Scenarios**:

1. **Given** a rule pushed with the default address-family preference
   and a target name that resolves to both an A and an AAAA record,
   **When** the client opens a connection, **Then** it dials the IPv4
   address.
2. **Given** the same rule re-pushed with `prefer_ipv6` opted in,
   **When** the client opens a connection, **Then** it dials the IPv6
   address.
3. **Given** a rule with `prefer_ipv6` opted in and a target that
   only has an A record (no AAAA), **When** the client opens a
   connection, **Then** it falls back to the A record rather than
   failing — opt-in expresses *preference*, not *requirement*.

---

### User Story 4 - Operator spots bad DNS targets without grepping logs (Priority: P4)

When something goes wrong in production, the operator's first move is
the metrics dashboard, not the log stream. A rule whose DNS target is
slowly going bad (intermittent NXDOMAIN, occasional SERVFAIL, slow
resolver) should show up as a rising counter on a per-rule chart, not
as a needle in the JSON-log haystack.

**Why this priority**: Pure observability — the system is correct
without it (US2 already prevents bad DNS from breaking unrelated
rules). It just makes the operator faster at spotting which rule is
the bad one. Lowest priority because the data already exists in logs;
the story is about presentation.

**Independent Test**: Push a rule pointing at a deliberately broken
DNS name, drive 10 connection attempts through it, then read the
metrics endpoint. Verify exactly one row of `forward_rule_dns_failures_total`
exists per rule with a count of 10 — single row per rule (matches the
SC-002 cardinality budget inherited from v0.2.0), one column for the
rule id, no per-attempt explosion.

**Acceptance Scenarios**:

1. **Given** a rule with a DNS target that consistently fails to
   resolve, **When** end-users drive N connection attempts through it,
   **Then** the per-rule DNS-failure counter rises by exactly N.
2. **Given** a rule with a healthy DNS target, **When** any number of
   connections succeed through it, **Then** its DNS-failure counter
   stays at 0 (i.e. transient cache-hit successes do not count as
   failures).
3. **Given** any number of rules with mixed DNS health, **When** the
   operator queries the metrics endpoint, **Then** the cardinality of
   `forward_rule_dns_failures_total` is exactly one row per rule that
   has ever had a resolution attempted — never one row per attempt,
   per address-family, or per resolved IP.

---

### Edge Cases

- **Target string that is ambiguous between IP and name** (e.g.
  `0:0:0:0:0:0:0:1` vs `[::1]` vs `localhost`): the parser MUST
  unambiguously classify each input as either "IP literal" (no
  resolution path) or "DNS name" (resolution path) at rule push time,
  using the same disambiguation rule that the standard library / OS
  resolver applies. A literal IPv6 address without brackets when a
  port is also expected is rejected at push time with a clear error.
- **Resolver timeout**: a single resolution attempt that hangs MUST
  not block the connection forever; it MUST surface as
  `dns_resolution_failed` after a bounded wait so the end-user gets
  a fast error and the client moves on.
- **TTL of 0** (DNS record explicitly says "do not cache"): the
  client honors it — every connection re-resolves — but never
  exceeds the configured minimum cache floor (so a malicious
  zero-TTL record cannot turn the client into a DNS-amplification
  source).
- **Very long TTL** (e.g. 24h): clamped to the configured upper
  bound so that operators are not surprised when a re-deployed
  upstream stays unreachable for a day.
- **Multiple A records returned**: the client MUST attempt the
  remaining addresses on per-connection failure (`Connection refused`,
  timeout) rather than giving up after the first. This is
  industry-standard "happy eyeballs"-style behavior; without it,
  rolling upstream restarts cause user-visible failures even though
  half the addresses are healthy.
- **Mixing IP and DNS rules on one client**: a client can hold an
  arbitrary mix of IP-target and DNS-target rules; one bad DNS
  resolution MUST NOT degrade unrelated rules' latency, throughput,
  or accept loop responsiveness.
- **Rule push with a syntactically valid but never-resolving name**
  (e.g. `not-a-real-host.invalid`): the rule push succeeds and the
  rule is `Active` (resolution is lazy, deferred to first connect).
  This matches US2's "fail individual connections, not the rule"
  promise.
- **DNS over upstream the operator does not trust**: out of scope.
  This feature uses whatever resolver the host operating system is
  configured with; hardening the resolver path is a separate spec.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: A forwarding rule's target host MUST accept either an IP
  literal (IPv4 or bracketed IPv6, current v0.2.0 behavior) **or** a
  DNS name. The accepted-input syntax for both forms is unambiguous
  and validated at rule push time.
- **FR-002**: When a rule's target is a DNS name, the client MUST
  perform name resolution **lazily**, on the first end-user connection
  through the rule (not at rule push, not at rule activation). Rule
  push and activation MUST NOT block on or fail because of DNS state.
- **FR-003**: The client MUST cache successful resolutions for the
  duration of the record's TTL, clamped to a configured cache floor
  (lower bound) and ceiling (upper bound). Cache hits MUST be served
  without consulting the resolver.
- **FR-004**: When DNS resolution fails (NXDOMAIN, SERVFAIL, timeout,
  resolver unreachable, or any other resolver-reported error), the
  client MUST: (a) leave the rule's status `Active`, (b) refuse the
  triggering end-user connection within a bounded wall-clock time
  with a structured reason that names the failure mode, (c) increment
  the per-rule DNS-failure counter, and (d) continue serving any
  still-valid cached answer for *other* connections, if one exists.
- **FR-005**: When the cached answer expires (TTL or ceiling reached),
  the next connection MUST trigger a fresh resolution. If the new
  resolution fails but the previous answer is still topologically
  reachable, the client MAY continue serving the previous answer for
  a bounded grace period to absorb transient resolver outages without
  cascading the failure to end users — the grace bound is part of the
  spec's measurable budgets, not a runtime knob.
- **FR-006**: When a DNS query returns multiple addresses, the client
  MUST attempt them in order on per-connection dial failure (refusal,
  timeout) before reporting the connection failed. Address ordering
  follows the address-family preference (FR-007); within a family the
  client MUST follow the order returned by the resolver.
- **FR-007**: When a DNS query returns both A and AAAA records, the
  client MUST prefer A records by default. A per-rule "prefer IPv6"
  opt-in inverts the preference for that one rule. If only one
  family is returned, the preference is moot — the client uses what
  it gets.
- **FR-008**: The client MUST expose a per-rule DNS-failure counter
  in its observability surface that increments exactly once per
  connection attempt that failed because of DNS resolution. The
  counter's cardinality budget MUST match v0.2.0's per-rule budget:
  one row per rule, never per attempt, per address, or per failure
  reason.
- **FR-009**: A rule that uses a DNS name MUST persist across server
  restarts in a form that survives a v0.2.0-shaped server reading it
  unchanged (forward-compat) and that allows a v0.3.0+ server to
  fully reconstruct the rule including its address-family preference
  on reload.
- **FR-010**: Existing v0.2.0 single-port and port-range rules with
  literal-IP targets MUST behave byte-identically after this feature
  ships: same latency profile, same persistence shape, same wire
  encoding bytes when no DNS-only field is set, same observability
  rows. No v0.2.0-shipped behavior may regress.
- **FR-011**: For port-range rules (v0.2.0), the DNS name resolves
  once per range and applies to every port in the range — i.e. all
  ports in the range share the same upstream IP for the duration of
  the cache lifetime. Re-resolution after TTL applies range-wide.
- **FR-012**: Resolution attempts MUST not block the client's accept
  loop or any unrelated connection. Concurrent connection attempts
  to the same DNS-target rule that arrive during an in-flight
  resolution MUST coalesce to a single resolver query (no thundering
  herd at the resolver).

### Key Entities

- **Target host**: the upstream destination expressed as either an IP
  literal (existing) or a DNS name (new). Carries the
  address-family-preference flag (default A-preferred). Lives inside
  a forwarding rule; one rule has exactly one target host.
- **Resolution cache entry**: a per-DNS-name record holding the
  resolver's answer set (one or more addresses), the resolver-reported
  TTL, the wall-clock instant the answer was received, and the
  effective expiry time after clamping. Owned by the client process;
  scoped to the client's lifetime; not persisted across restarts.
- **DNS-failure counter**: a per-rule monotonic count of end-user
  connections that failed because of DNS resolution. Lives in the
  client's observability surface alongside v0.2.0's
  bytes_in/bytes_out/active_connections counters.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: An operator can replace a literal-IP target on an
  existing rule with the upstream's DNS name in **under 60 seconds
  of operator wall-clock time** (push the new rule, remove the old)
  without restarting the server or the client and without dropping
  in-flight connections on unrelated rules.
- **SC-002**: When a DNS target's underlying IP changes (the
  upstream is replaced and the DNS record updated), end-user traffic
  through the rule starts hitting the new IP within **at most one
  configured cache ceiling**, with **zero operator action**.
- **SC-003**: When DNS for a target rule is fully broken (NXDOMAIN
  on every query), end-users hitting that rule see a connection
  failure within **3 seconds** (not a minutes-long hang), and the
  same operator with no DNS-debugging tools can identify the
  offending rule from the metrics dashboard alone in **under 30
  seconds**.
- **SC-004**: A DNS-target rule serving from a warm cache adds
  **no observable latency** to the end-user connection setup
  compared to a literal-IP rule at the same upstream — i.e. cache
  hits are amortized to effectively zero per-connection cost.
- **SC-005**: Across a fleet of 100 mixed rules (50 literal-IP, 50
  DNS-target), the per-process resolver query rate stays at or
  below **one query per rule per cache window** under steady-state
  traffic — coalescing and caching together prevent the resolver
  from being treated as a per-connection dependency.
- **SC-006**: The Prometheus surface for a forwarding client running
  any number of DNS-target rules emits exactly **one row of
  `forward_rule_dns_failures_total` per rule that has ever attempted
  resolution** — cardinality MUST NOT grow with attempt count or
  resolved-address count.

## Assumptions

- **TTL clamp defaults**: cache floor of 5 seconds (so a malicious
  zero-TTL record cannot DDoS the resolver), cache ceiling of 5
  minutes (so a re-deployed upstream is reachable within a bounded
  window even if its old TTL was a day). These are operator-tunable
  via server-side defaults applied at rule install.
- **Resolution timeout**: a single resolver attempt is bounded at
  3 seconds — enough for transcontinental round trips with a slow
  resolver, short enough to fit inside the 3-second SC-003 budget.
- **Stale-while-error grace**: when a fresh resolution fails but a
  previous answer exists past its TTL, the client MAY serve the
  stale answer for up to 30 seconds beyond expiry to absorb
  transient resolver outages.
- **Resolver source**: the client uses the host operating system's
  configured resolver (`/etc/resolv.conf` or the OS equivalent).
  Custom DNS-over-HTTPS, DNS-over-TLS, or operator-supplied
  resolver lists are out of scope for this feature.
- **DNS-target rules persist with the same operator-surface
  semantics as v0.2.0 rules** — `list-rules`, `rule-stats`, and
  the operator HTTP API all continue to work; the only new field
  visible at the operator surface is the optional address-family
  preference and the new failure counter.
- **Port-range rules with DNS targets** share one resolution per
  range (FR-011) — this matches operator intuition that
  `8080-8089 → api.example.com:80-89` points at one logical
  upstream identified by name.
- **Address-family opt-in is per-rule, not global**, so an operator
  can stage IPv6 migration one rule at a time without flipping the
  whole client.
- **No DNSSEC validation** is performed by the client. The trust
  boundary remains at the OS resolver — operators who need DNSSEC
  configure it on the resolver they point the client at.
- **Existing constitution stays in force**: TLS + bearer-token auth
  on the control plane, no mTLS, no per-end-user identity. DNS
  resolution is a client-local concern; nothing about it crosses
  the control plane.
