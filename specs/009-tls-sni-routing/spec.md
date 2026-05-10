# Feature Specification: TLS SNI-Based Routing for Forwarded Connections

**Feature Branch**: `009-tls-sni-routing`
**Created**: 2026-05-08
**Status**: Draft
**Input**: User description: "Add TLS Server Name Indication (SNI) based routing to Portunus. A single TCP listen port (typically 443) can fan out to different upstream targets based on the TLS hostname in the client's ClientHello. Portunus remains a pure L4 byte-passthrough — never decrypts, terminates, or re-encrypts TLS. Full design is in `specs/009-tls-sni-routing/design.md` (commits a556570 → 0142e43)."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Route Multiple TLS Hostnames Through a Single Port (Priority: P1) 🎯 MVP

An operator runs several backend services that all need to be reachable on
the public TCP port 443. Today each service must be assigned a separate
listen port (or live behind a separate IP). After this feature, the operator
attaches each service to the same port and disambiguates them by the TLS
hostname (`SNI`) the client requests, e.g. `api.example.com → api-backend`,
`web.example.com → web-backend`. Forwarded bytes are still passed through
unchanged — Portunus does not terminate TLS.

**Why this priority**: This is the single user-visible motivation for the
feature. Without it, the operator either runs another tool in front of
Portunus or burns one port per service. Solving this delivers all the
business value of v0.9 on its own.

**Independent Test**: Operator pushes two rules, both on TCP `:443`, with
distinct exact `sni_pattern` values pointing at two different upstreams.
Two clients open TLS connections, each requesting a different SNI; each
client lands on the correct upstream. Verifiable end-to-end with real TLS
clients (e.g. `openssl s_client -servername`).

**Acceptance Scenarios**:

1. **Given** two active rules on TCP `:443` with `sni_pattern = "api.example.com"`
   and `sni_pattern = "web.example.com"`, **When** a TLS client connects to
   `:443` with SNI `api.example.com`, **Then** its bytes are forwarded to
   the upstream associated with the `api.example.com` rule, byte-for-byte
   unchanged.
2. **Given** the same setup, **When** a TLS client connects with SNI
   `web.example.com`, **Then** its bytes are forwarded to the upstream
   associated with the `web.example.com` rule.
3. **Given** an operator pushes a second rule with the same `sni_pattern`
   already in use on the same client and port, **When** validation runs,
   **Then** the push is refused with a clear "duplicate SNI route" error
   and no rule is activated.
4. **Given** a SNI rule is active, **When** the operator queries the rule
   list (CLI or API), **Then** the `sni_pattern` is returned alongside the
   rule, so it can be reviewed and audited.

---

### User Story 2 - Wildcard SNI Routing for Subdomain Fan-Out (Priority: P1)

An operator runs a multi-tenant service whose tenants live at unique
subdomains (`tenantA.app.example.com`, `tenantB.app.example.com`, …). The
operator wants every subdomain of `app.example.com` to land on the same
upstream, without enumerating each tenant.

**Why this priority**: Wildcard support is the difference between SNI
routing being usable at scale and being a toy. Without it, the operator
adds one rule per tenant. Promoting this to P1 keeps it inside the MVP
because the implementation cost is small once exact matching exists, and
real deployments will demand it on day one.

**Independent Test**: Push one rule on TCP `:443` with
`sni_pattern = "*.app.example.com"`. Open a TLS connection with SNI
`tenantA.app.example.com` — lands on the upstream. Open another with SNI
`other.app.example.com` — also lands on the upstream. Open one with SNI
`app.example.com` (no left label) — does NOT match.

**Acceptance Scenarios**:

1. **Given** a rule with `sni_pattern = "*.app.example.com"`, **When** a
   client requests SNI `tenantA.app.example.com`, **Then** the connection
   is forwarded to the rule's upstream.
2. **Given** the same rule, **When** a client requests SNI
   `app.example.com` (no left label), **Then** the connection does NOT
   match the wildcard.
3. **Given** the same rule, **When** a client requests SNI
   `a.b.app.example.com` (extra label), **Then** the connection does NOT
   match the single-level wildcard.
4. **Given** rules with `sni_pattern = "api.app.example.com"` (exact) and
   `sni_pattern = "*.app.example.com"` (wildcard) on the same port,
   **When** a client requests SNI `api.app.example.com`, **Then** the
   exact rule wins.
5. **Given** rules with `sni_pattern = "*.app.example.com"` and
   `sni_pattern = "*.team.app.example.com"`, **When** a client requests
   SNI `x.team.app.example.com`, **Then** the more specific (longer
   suffix) wildcard wins.
6. **Given** an operator pushes `sni_pattern = "*.*.example.com"`,
   **When** validation runs, **Then** the push is refused with a "malformed
   SNI pattern" error (only single leading `*` is allowed).

---

### User Story 3 - Fallback Route for Valid TLS Without a Recognised SNI (Priority: P2)

An operator wants every well-formed TLS connection on `:443` that does NOT
match any specific SNI to land on a default upstream — for example a
"nothing here" landing service or a catch-all reverse proxy.

**Why this priority**: A recognised pattern in real TLS gateways. Not
strictly required for the first deployments (operators can omit the
fallback and accept that unmatched SNI is rejected), but high value at
low cost: it slots into the existing route table as the `NULL` slot.

**Independent Test**: Push two rules on TCP `:443` — one with
`sni_pattern = "api.example.com"`, one with `sni_pattern = NULL` (the
fallback). Three TLS clients connect: one with SNI `api.example.com` (hits
the named rule), one with SNI `unknown.example.com` (hits the fallback),
one with no SNI extension at all (hits the fallback).

**Acceptance Scenarios**:

1. **Given** an SNI port with a fallback rule (`sni_pattern = NULL`),
   **When** a client connects with valid TLS but no SNI extension, **Then**
   the connection is forwarded to the fallback rule's upstream.
2. **Given** the same configuration, **When** a client connects with a
   valid TLS ClientHello whose SNI matches no rule, **Then** the
   connection is forwarded to the fallback rule.
3. **Given** an SNI port WITHOUT a fallback rule, **When** a client
   connects without SNI or with a non-matching SNI, **Then** the
   connection is rejected (TCP closed) and a structured event is emitted
   for operator diagnostics.
4. **Given** an operator pushes a second `sni_pattern = NULL` rule on the
   same port, **When** validation runs, **Then** the push is refused with
   a "duplicate fallback" error.

---

### User Story 4 - Existing Plain-TCP Rules Continue to Work Unchanged (Priority: P2)

An operator has v0.7-style rules that don't use SNI at all (plain TCP
forwarding on a single port, port range, UDP, or TCP-with-domain-target).
After upgrading to v0.9, every existing rule continues to behave
byte-for-byte the same as it did under v0.7 / v0.8.

**Why this priority**: Backward compatibility is required by the project's
constitution (V. byte-stable control plane). The feature must not impose
overhead on rules that don't opt into SNI.

**Independent Test**: A baseline Portunus install is upgraded to v0.9.
None of the existing rules carry an `sni_pattern`. Run the v0.7 / v0.8
end-to-end test suite — every test passes unchanged. Capture a v0.7
control-plane wire trace and replay it against v0.9 — byte-stable.

**Acceptance Scenarios**:

1. **Given** an operator's v0.8 rule set with no `sni_pattern` anywhere,
   **When** the operator upgrades to v0.9, **Then** all rules activate and
   forward exactly as they did in v0.8.
2. **Given** the same upgrade, **When** v0.7-format gRPC traffic is
   replayed against the v0.9 server, **Then** the bytes on the wire are
   identical (the new `sni_pattern` field is omitted when absent).
3. **Given** a port has a single rule with `sni_pattern = NULL` (legacy
   plain-TCP), **When** any TCP traffic arrives — TLS, plain-text, or
   unknown — **Then** every byte is forwarded to the upstream without
   inspection (no peek, no parse).
4. **Given** a v0.8 client connects to a v0.9 server, **When** the
   operator attempts to push a rule with `sni_pattern` set, **Then** the
   push is refused with a clear "client too old" error before any rule is
   activated.

---

### User Story 5 - Operator Diagnostics for SNI Routing Health (Priority: P3)

An operator investigating routing problems needs to see how the SNI
listener is performing — how many connections matched exactly vs. via
wildcard vs. fallback, and how many were rejected because the bytes were
not TLS, the ClientHello was incomplete, or the SNI matched nothing.

**Why this priority**: Diagnostics matter, but the feature ships value
even without them — operators can capture pcap if needed. P3 keeps it in
scope without forcing histogram or per-listener latency tracking into v0.9.

**Independent Test**: After running mixed traffic (exact hits, wildcard
hits, fallbacks, non-TLS noise) for 5 minutes, the operator scrapes
`/metrics` and sees counters in expected proportions; the structured log
contains one event per failure case with enough fields for triage.

**Acceptance Scenarios**:

1. **Given** mixed traffic on an SNI listener, **When** the operator
   scrapes `/metrics`, **Then** per-rule counters reflect exact /
   wildcard / fallback hits and per-listener counters reflect miss and
   parse-failure totals.
2. **Given** a client connection that times out before sending a complete
   ClientHello, **When** the timeout fires, **Then** a structured event
   identifies the listener port, the peer address, and the bytes-read
   count, sufficient to diagnose a stuck client.

---

### Edge Cases

- A non-TLS payload (e.g. a plain HTTP request) arriving on an SNI listener
  port → connection is closed with a structured `tls.parse_failed` event;
  the listener does not silently treat the bytes as fallback.
- A client that opens TCP but sends nothing for 3 seconds → connection is
  closed with a `tls.client_hello_timeout` event.
- A ClientHello whose handshake message is fragmented across multiple TLS
  records → the SNI extraction reassembles the fragments and parses
  successfully (or fails with `tls.parse_failed` if the fragments are
  malformed).
- A ClientHello whose total size exceeds the read budget → connection is
  closed with `tls.parse_failed`; legitimate clients fit comfortably under
  the budget even with post-quantum hybrid key shares.
- An operator tries to convert a live legacy plain-TCP rule into a SNI
  rule by pushing a sibling SNI rule on the same port → push is refused;
  operator must remove the legacy rule first (which drains its
  connections) before the listener can be re-bound in SNI mode.
- An operator removes the only remaining rule on an SNI port → the
  listener is torn down using the existing v0.7 drain semantics; in-flight
  forwarded connections complete or hit the existing drain timeout.
- Two operators (separate `client` identities running forward-client) each
  bind their own `:443 + api.example.com` route → both succeed; SNI route
  uniqueness is scoped per-client, not global.
- An IDN hostname is configured: the operator submits the Punycode form
  (`xn--…`); SNI from the wire arrives in Punycode; comparison is case-
  insensitive ASCII.
- An operator pushes an `sni_pattern` whose grammar is malformed (multiple
  asterisks, a `*` not at the leftmost label, non-ASCII bytes, length over
  253 chars) → push is refused at validation with a specific error code.
- An old (v0.8 or earlier) forward-client connects to a v0.9 server →
  attempts to push any rule with `sni_pattern` set are refused with HTTP
  422 before reaching the wire; existing non-SNI rules still flow.

## Requirements *(mandatory)*

### Functional Requirements

#### Routing model

- **FR-001**: System MUST accept a new optional rule attribute that selects
  between three pattern shapes: an exact ASCII hostname (length 1-253),
  a single-leading-label wildcard `*.<suffix>`, or absent (no pattern).
- **FR-002**: System MUST accept this attribute only on TCP rules whose
  listen port is a single port (not a range); rules of other shapes
  (UDP, TCP port-range) MUST be refused with a specific validation error.
- **FR-003**: System MUST refuse a wildcard pattern that does not begin
  with exactly `*.`, contains additional `*` characters, or whose suffix
  has fewer than two labels.
- **FR-004**: System MUST normalise stored patterns and incoming SNI
  values to lowercase ASCII before comparison.
- **FR-005**: When a TLS client opens a connection to a single-port TCP
  listener whose set of active rules includes any rule with a non-empty
  pattern, the system MUST inspect the client's ClientHello to determine
  the requested hostname before forwarding.
- **FR-006**: The system MUST select the matching rule using priority:
  exact hostname > longest matching wildcard suffix (where the matched
  prefix contains no `.`) > the fallback rule (the rule with no pattern,
  if any).
- **FR-007**: A single connection MUST be forwarded to exactly one rule's
  upstream. Once selected, target selection and failover follow the
  existing v0.7 multi-target rules unchanged.
- **FR-008**: The bytes of the ClientHello and every byte that follows
  MUST reach the selected upstream byte-for-byte unchanged. The system
  MUST NOT decrypt, modify, re-encrypt, or buffer beyond what is required
  to peek the SNI.

#### Failure handling

- **FR-009**: If the client does not deliver a parseable ClientHello
  within 3 seconds of connection accept, the connection MUST be closed
  and a structured event MUST be emitted that names the listener port and
  the peer address.
- **FR-010**: If the bytes received are not a TLS ClientHello (wrong
  record type, wrong handshake type, malformed structure), the connection
  MUST be closed and a structured event MUST be emitted.
- **FR-011**: If the ClientHello is valid but contains no `server_name`
  extension (or an empty one), the connection MUST be routed via the
  fallback rule when one exists; if no fallback exists the connection
  MUST be closed and an event emitted.
- **FR-012**: If the ClientHello carries a `server_name` that matches no
  rule on this listener, the connection MUST be routed via the fallback
  rule when one exists; if no fallback exists the connection MUST be
  closed and an event emitted.
- **FR-013**: The peek read budget MUST allow ClientHello messages up to
  64 KiB and MUST account for handshake messages fragmented across
  multiple TLS records.

#### Listener lifecycle

- **FR-014**: A given `(client, single TCP port)` listener MUST be in
  exactly one mode for its lifetime: legacy plain-TCP forwarding, or SNI
  dispatch. The mode is fixed by the first rule pushed onto the port.
- **FR-015**: A push that would change the mode of a live listener (i.e.
  add a SNI sibling onto a legacy plain-TCP port, or vice versa) MUST be
  refused with a specific error code that names the operational
  workflow ("remove existing rule first").
- **FR-016**: On a legacy plain-TCP port, the system MUST NOT inspect
  client traffic at all — behaviour is byte-for-byte identical to the
  pre-feature implementation.
- **FR-017**: When all rules on a SNI listener are removed, the listener
  MUST be torn down using the existing drain semantics; the next push
  reopens the mode question.
- **FR-018**: Adding or removing a rule on a live SNI listener MUST NOT
  interrupt connections that are already being forwarded.
- **FR-019**: Removing a rule MUST be possible by rule identifier alone;
  the system MUST NOT require the operator to know which port the rule
  belongs to.

#### Uniqueness & overlap

- **FR-020**: Within one `(client, single TCP port)` listener, every
  active rule's pattern MUST be pairwise distinct. Two rules with the
  same exact hostname, the same wildcard, or both absent (the fallback
  slot) MUST NOT be active simultaneously.
- **FR-021**: Two different `client` identities MAY bind the same logical
  pattern on the same logical port — each client owns its own listener.
- **FR-022**: Adding any TCP single-port rule with a pattern onto a port
  whose existing range rule covers the same listen port MUST be refused
  using the existing v0.7 port-overlap error.

#### Wire & client compatibility

- **FR-023**: The control-plane wire format MUST remain backward
  compatible. A pre-v0.9 client receiving a rule with no pattern MUST
  decode and execute it identically to v0.8.
- **FR-024**: The system MUST refuse any push of a rule with a non-empty
  pattern targeted at a forward-client whose declared version is below
  v0.9.0, returning a specific error code that names the missing client
  capability. The refusal MUST occur before any rule is activated.
- **FR-025**: The control-plane MUST NOT introduce new top-level wire
  messages purely for SNI grouping; rule deltas continue to be exchanged
  one rule at a time.

#### Operator surfaces

- **FR-026**: The rule-creation HTTP API and the rule-creation CLI MUST
  both accept the new pattern attribute.
- **FR-027**: The rule-listing HTTP API and CLI MUST surface the pattern
  on each rule when present, and omit the field when absent.
- **FR-028**: The Web UI rule list MUST show a column reflecting the
  pattern; the rule editor MUST show an optional input for the pattern
  only when the rule is TCP and single-port, with helper text covering
  exact, wildcard, and fallback semantics.
- **FR-029**: All validation refusals MUST carry a stable, machine-readable
  error code distinguishing at least: malformed pattern, unsupported rule
  shape, duplicate pattern, duplicate fallback, mode-change attempt, and
  client-too-old.

#### Persistence

- **FR-030**: The pattern attribute MUST persist across server restarts
  alongside the rule it belongs to, using the project's existing
  persistence mechanism.
- **FR-031**: A persistence rollback to v0.8 MUST remain possible: rules
  without a pattern MUST be readable by a v0.8 binary, and the schema
  change MUST be additive only.

#### Observability

- **FR-032**: The system MUST surface per-rule counters distinguishing
  exact, wildcard, and fallback hits.
- **FR-033**: The system MUST surface per-listener counters for "SNI did
  not match any rule" and "ClientHello could not be parsed", keyed by
  listen port (since these events have no rule attribution).
- **FR-034**: All metric series MUST follow the existing label
  conventions (`client`, `rule`, `owner` for per-rule; `client`, `port`
  for per-listener) and MUST NOT introduce alternate label names for the
  same dimensions.
- **FR-035**: Data-plane events emitted by SNI routing MUST flow only
  through the structured tracing log and Prometheus counters; they MUST
  NOT be recorded in the operator allow/deny audit ring.

#### Out of scope (explicit non-requirements)

- **NR-001**: TLS termination, decryption, or re-encryption — the system
  remains a pure L4 byte-passthrough.
- **NR-002**: HTTP / HTTPS reverse-proxy semantics (Host header routing,
  path routing) — deferred to L7 backlog.
- **NR-003**: QUIC / HTTP-3 SNI inspection — different parsing path,
  deferred.
- **NR-004**: Per-listener latency histograms for ClientHello peek —
  monotonic counters only in v0.9; histogram deferred.
- **NR-005**: Online conversion between legacy plain-TCP and SNI mode for
  a live port — explicitly forbidden by FR-015.

### Key Entities

- **SNI Pattern**: a per-rule optional value that determines whether a
  rule participates in SNI routing and how its hostname comparison works.
  Three forms: exact host, single-level wildcard, absent. Stored
  lowercased ASCII. Subject to grammar validation.
- **SNI Listener**: the runtime construct on a forward-client that owns
  one bound TCP port, accepts connections, peeks ClientHello, dispatches
  to one of its member rules, and tracks per-listener counters that have
  no rule attribution. Lifetime is bounded by the membership of its
  group; mode is fixed when the first member activates.
- **Route Group**: the set of rules sharing a `(client, single TCP port)`
  key. The group's first member fixes the listener mode (legacy or SNI).
  Pattern uniqueness is enforced within a group.
- **Fallback Slot**: the at-most-one rule per group whose pattern is
  absent. On a SNI listener it acts as a destination for valid TLS
  connections whose SNI matches no other rule (or whose SNI is missing).
- **Per-Rule SNI Counters**: monotonic counters attached to each rule
  describing how many connections it received via exact / wildcard /
  fallback match.
- **Per-Listener SNI Counters**: monotonic counters attached to each
  active SNI listener describing how many connections were rejected
  because the SNI matched no rule, or because the ClientHello could not
  be parsed.
- **Client Capability**: the declared version of a connected forward-client,
  used to gate any SNI-bearing rule push.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: An operator can publish two TLS-bearing services on a single
  shared TCP port and reach each from a real TLS client by hostname,
  with bytes delivered to the correct backend on every request, in a
  one-page runbook (push two rules → connect with two SNIs → verify).
- **SC-002**: For non-SNI rules carried over from v0.8, end-to-end
  connection and forwarding behaviour is byte-for-byte identical to the
  v0.8 release as measured by the existing v0.8 e2e test suite passing
  unchanged on v0.9.
- **SC-003**: For an SNI listener under representative load, additional
  connection setup latency relative to a v0.8 plain-TCP listener on the
  same hardware is no more than 5 ms at the 99th percentile, where the
  baseline is the v0.7 connection_setup_latency benchmark.
- **SC-004**: Across an upgrade from v0.8 to v0.9 with no rule changes,
  the forward-client wire stream is byte-stable: a v0.8 capture replayed
  against a v0.9 server produces an identical response stream.
- **SC-005**: An operator using a v0.8 forward-client cannot accidentally
  activate a SNI rule: every such push is refused before any forwarding
  begins, and the refusal carries an error code that names the missing
  capability.
- **SC-006**: For a representative wildcard catalogue (≤ 100 rules per
  listener), the route-table lookup decision is made in under 100
  microseconds at the 99th percentile, so the peek pipeline is dominated
  by network time.
- **SC-007**: After 5 minutes of mixed traffic on an SNI listener, the
  operator can determine from the standard metrics surface (no logs
  required) the relative volumes of exact / wildcard / fallback / miss /
  parse-failure outcomes for triage.
- **SC-008**: A connection that is intentionally non-TLS, or that stalls
  before sending a ClientHello, is rejected within the configured 3-second
  budget and never reaches an upstream — verified by an integration test
  that asserts the upstream socket never observes the non-TLS bytes.

## Assumptions

- **A-001**: The Portunus deployment continues to be a pure L4 byte
  passthrough; this feature does not introduce TLS termination,
  certificate handling, or any obligation to read past the ClientHello.
- **A-002**: Operators submit Internationalised Domain Names already
  encoded as Punycode (`xn--…`). This matches what is on the wire in
  TLS, and avoids embedding a Unicode normalisation library.
- **A-003**: A 3-second wait for a parseable ClientHello is sufficient
  for legitimate TLS clients on real networks. Slowloris-style attackers
  are out of scope of this feature beyond enforcing the 3-second / 64 KiB
  budget.
- **A-004**: The set of SNI-bearing rules on any single listener is small
  enough (typical ≤ 100, hard limit ≤ 10 000) that an in-memory route
  table rebuilt on every change is preferable to incremental updates.
- **A-005**: Operators understand that converting a legacy plain-TCP
  port into a SNI port (or back) requires removing the existing rule
  first, draining its connections, then pushing the new rule. This is
  documented in the operator quickstart.
- **A-006**: Operators tolerate that listener-level miss / parse-failure
  metrics carry no `owner` label — these events have no rule attribution
  and adding `owner` would either be wrong or expand cardinality without
  signal.
- **A-007**: SNI uniqueness is scoped per `client` (the forward-client
  identity), not globally; two clients can each own their own listener
  on `:443 + api.example.com` without conflict.
- **A-008**: The existing v0.8 SQLite store is the authoritative
  persistence layer; no new file or store is introduced for SNI routing.
- **A-009**: The existing v0.7 multi-target failover semantics remain in
  place; SNI selects which rule to use, then the rule's per-target
  health logic decides which target receives the bytes.

## Dependencies

- **D-001**: v0.8 SQLite persistence — rule schema additive migration.
- **D-002**: v0.7 multi-target rule semantics — SNI plays before target
  selection.
- **D-003**: v0.7 client-version handshake — used by FR-024's gate.
- **D-004**: v0.6 Web UI rule pages — extended with the new column and
  form input.
- **D-005**: Existing forward-client `RuleStats` / `StatsReport` channel
  — carries the new counter fields.
