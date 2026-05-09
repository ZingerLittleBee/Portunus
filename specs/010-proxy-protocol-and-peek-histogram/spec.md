# Feature Specification: PROXY-Protocol Injection & SNI Peek-Duration Histogram

**Feature Branch**: `010-proxy-protocol-and-peek-histogram`
**Created**: 2026-05-09
**Status**: Draft
**Input**: User description: see [Inputs](#inputs)

## Clarifications

### Session 2026-05-09

- Q: Which destination endpoint should the emitted PROXY v1/v2 header report? → A: Original client→forward-client listener IP and port.
- Q: Which peek outcomes should the peek-duration histogram record? → A: All SNI-mode peek outcomes: success, timeout, and parse error.
- Q: For wildcard forward-client listeners, which destination IP should the PROXY header report? → A: The accepted socket's actual local IP and port.

## User Scenarios & Testing *(mandatory)*

### User Story 1 — Backends see the real client IP (Priority: P1)

An operator runs forward-rs in front of a TLS service (typically port 443) and
fans out to multiple upstream backends with v0.9 SNI routing. Today, every
backend's access log shows the forward-client's IP address as the client peer,
because forward-rs is a transparent L4 proxy that opens its own TCP connection
to the upstream. The operator wants the backend to see the **real end client's**
IP address (and source port) so existing access logging, geo-IP, abuse
analytics, and IP allow-lists keep working unchanged after introducing
forward-rs.

**Why this priority**: This is the single most painful gap any v0.9 deployment
hits on day one. Without it, every backend operator either has to instrument
their app to read a custom header (which forward-rs cannot inject because it
never decrypts TLS) or accept that their access logs are useless. PROXY
protocol is the universally accepted solution that nginx, HAProxy, Caddy,
Traefik, Postgres, Redis, and many TCP services already understand.

**Independent Test**: A two-host setup — forward-client → nginx with
`proxy_protocol on;` — receives a TLS connection from a known external IP.
Assert that nginx's `$proxy_protocol_addr` (or equivalent) matches that
external IP rather than the forward-client's bind address. The test passes
when the backend reports the real client IP for both IPv4 and IPv6
connections, and fails (no fall-through) if the operator opts in but the
backend has not been configured to expect PROXY protocol.

**Acceptance Scenarios**:

1. **Given** a target with PROXY-protocol opted in (version 1) and a TLS
   client connecting from `203.0.113.45:54321` to forward-client on
   `:443`, **When** forward-client opens the upstream TCP connection,
   **Then** the upstream receives a single PROXY v1 ASCII line carrying
   `203.0.113.45 <fwd-client-local-ip> 54321 <fwd-client-local-port>`
   before any TLS bytes flow.
2. **Given** the same setup but PROXY-protocol version 2, **When**
   forward-client opens the upstream TCP connection, **Then** the upstream
   receives the 12-byte PROXY v2 signature plus a TCP4 address block with
   the same source/dest values, again before any TLS bytes flow.
3. **Given** a v0.10 server is asked to push a rule whose target opts into
   PROXY-protocol to a forward-client whose self-reported version is older
   than 0.10, **When** the operator submits the push, **Then** the server
   refuses the push with a structured error indicating the client cannot
   support PROXY-protocol and the rule is **not** activated anywhere.
4. **Given** a target that does **not** opt into PROXY-protocol, **When**
   forward-client opens the upstream connection, **Then** the upstream byte
   stream is byte-identical to v0.9 (no header is prepended).
5. **Given** a target that opts in but the upstream resets the connection
   before the PROXY header finishes writing, **When** the write fails,
   **Then** the failure is counted as a connect failure for that target
   (driving the existing multi-target health/failover behaviour) and a
   diagnostic event is emitted via tracing.

---

### User Story 2 — Operators can see ClientHello peek-latency tails (Priority: P2)

An operator running v0.9 SNI routing in production wants to know the latency
distribution of the ClientHello peek phase — not just whether peeks
eventually succeed (already a counter) but how long they take. The peek phase
runs under a 3-second deadline; understanding the p50/p95/p99/p999 tail tells
the operator whether real clients are routinely close to that ceiling on
high-RTT networks, whether a particular listener has degraded recently, and
whether the deadline could safely be tightened or needs to be raised.

**Why this priority**: This is observability, not a behavioural change.
Useful for capacity planning and incident triage but not blocking the v0.9
adoption story. Slated below PROXY-protocol because v0.9 already ships peek
counters that flag whether peeks are failing — only the *distribution* of
all SNI-mode peek durations is new.

**Independent Test**: With a forward-client running an SNI-mode listener,
generate a controlled mix of low-RTT and high-RTT clients (e.g., via
artificial network delay). Scrape the server's Prometheus surface and
verify that bucketed peek-duration counts increase under load and that the
distribution shifts as expected when artificial delay is introduced. Plain
TCP (legacy) listeners must produce **no** peek-histogram observations.

**Acceptance Scenarios**:

1. **Given** an SNI-mode listener that has served some traffic, **When** an
   operator queries the server's Prometheus surface, **Then** they see a
   histogram series labelled by client and listen port that exposes
   bucketed peek-duration counts plus a sum and count, suitable for
   computing quantiles via standard Prometheus tooling.
2. **Given** a legacy plain-TCP listener that has served traffic, **When**
   an operator queries the same surface, **Then** the listener produces
   **no** peek-histogram observations (the histogram is SNI-mode-only).
3. **Given** a peek that times out at the 3-second deadline, **When** that
   peek terminates, **Then** the peek-failure counter from v0.9 increments
   as before *and* the histogram records the 3-second observation in the
   `le="3"` bucket. Peeks observed after the deadline remain visible via
   the histogram `_count` / `le="+Inf"` series without inflating finite
   buckets.

---

### User Story 3 — Per-target opt-in & per-rule mixing (Priority: P3)

An operator wants flexibility: within one rule that fans out to multiple
upstream targets, some targets are services they own (which expect PROXY
protocol) while other targets are third-party services or legacy backends
that would treat the PROXY bytes as a malformed handshake. The operator
must be able to opt in **per target**, not per rule and not per listener.

**Why this priority**: This is more a correctness constraint than a story
of its own — the PROXY-protocol opt-in must be at the target level, not
the rule level, otherwise mixed-target rules force the operator to either
over-include (sending PROXY to a service that does not expect it) or
under-include (losing the real client IP on backends that do expect it).
Listed as P3 because operationally it is a single configuration field, but
the **shape** of that field (per-target) is load-bearing.

**Independent Test**: Configure one rule with two targets — target A opts
into PROXY-protocol, target B does not — and direct two TLS connections
through forward-client such that the multi-target failover/selection
hands one to A and one to B. Capture upstream bytes; A's stream begins
with a PROXY header and B's does not.

**Acceptance Scenarios**:

1. **Given** a rule with two targets where only target A opts in, **When**
   a connection is dispatched to A, **Then** A's upstream stream is
   PROXY-prefixed and B's (when later dispatched to it) is not.
2. **Given** the same rule, **When** an operator inspects the rule via the
   operator API or web UI, **Then** the per-target PROXY-protocol setting
   is visible and editable per target without affecting unrelated targets.

---

### Edge Cases

- **Mixed address families**: TLS client connects over IPv6 to a
  forward-client listener on a dual-stack socket; the upstream connection
  may also be IPv6 or IPv4. The PROXY header must report the address
  family of the **client→forward-client** connection (the original
  client's view), not the upstream connection — that is the contract
  backends rely on.
- **Wildcard listener bind addresses**: if forward-client listens on a
  wildcard address such as `0.0.0.0` or `::`, the PROXY header must use
  the accepted socket's actual local IP address and port, not the
  configured wildcard bind address.
- **Loopback / unix-style upstream targets**: forward-rs only supports
  TCP/UDP targets; UNIX-domain sockets are not in scope. PROXY v2 has a
  UNIX address-family block; we will not emit it.
- **Capability mismatch on hot reload**: a v0.10 forward-client connects,
  the server activates a PROXY-enabled rule on it, then the client
  reconnects after a downgrade to v0.9. The server must re-evaluate the
  capability gate at reconnect and refuse to re-push that rule, leaving
  the client in a clean state for its other rules.
- **Wire-stable downgrade**: a v0.9 client encountering a `Target` proto
  message with the new opt-in field set must drop the unknown field
  silently and treat the target as v0.9. The server's capability gate is
  what prevents that case from happening in practice; the wire-level
  silence is the safety net.
- **Histogram on the legacy listener mode**: a port that started as legacy
  plain-TCP must never emit peek-histogram observations even if other
  ports on the same client are SNI-mode.
- **Bucket boundaries crossing the peek deadline**: the finite histogram
  tail bucket must include observations equal to the 3-second deadline.
  Observations measured above the deadline must remain visible via the
  histogram `_count` / `+Inf` bucket while preserving Prometheus finite
  bucket semantics (`le="3"` means `<= 3s`, not `timeout-ish`).
- **PROXY header on a target with multiple priorities**: when v0.7
  multi-target failover swaps from a primary target opted-in to PROXY to
  a secondary target that is not opted in, the secondary connection must
  not carry a PROXY header. Each target's setting is independent.

## Requirements *(mandatory)*

### Functional Requirements

#### PROXY-protocol injection

- **FR-001**: The system MUST allow an operator to opt a target into
  PROXY-protocol injection per individual target, choosing one of two
  versions (v1 ASCII line, or v2 binary header).
- **FR-002**: When an operator does **not** opt a target in, the upstream
  byte stream MUST be byte-identical to the v0.9 forwarding behaviour for
  that target — no header, no warm-up bytes, no behavioural drift.
- **FR-003**: When an opted-in target's upstream TCP connection succeeds,
  the system MUST write exactly one PROXY header to the upstream socket as
  the very first bytes of the upstream stream, before any client→upstream
  bytes are forwarded.
- **FR-004**: The PROXY header MUST carry the source IP and source port of
  the **original client→forward-client** connection and the destination IP
  and destination port that the original client targeted (the
  forward-client's listening socket), regardless of the upstream
  connection's address family. For wildcard listener bind addresses, the
  destination IP and port MUST be the accepted socket's actual local
  endpoint.
- **FR-005**: PROXY-protocol injection MUST work for both IPv4 and IPv6
  client connections.
- **FR-006**: PROXY-protocol injection applies to TCP rules only; UDP
  rules MUST reject any attempt to opt a target into PROXY-protocol at
  validation time with a clear, structured error.
- **FR-007**: If the PROXY header write to the upstream socket fails for
  any reason, the system MUST treat that as a connect failure for the
  target so that v0.7 multi-target health and failover apply, and MUST NOT
  fall back to forwarding without the header.
- **FR-008**: The system MUST refuse to push a rule whose target opts into
  PROXY-protocol to any forward-client whose self-reported version is
  older than v0.10, returning a structured `proxy_protocol_unsupported_by_client`
  error before activating the rule anywhere.
- **FR-009**: The per-target PROXY-protocol setting MUST be visible and
  editable in the operator API, the operator CLI, and the management web
  UI rule editor.
- **FR-010**: The per-target setting MUST persist across server restarts
  using the existing storage layer; on schema upgrade from v0.9 to v0.10,
  pre-existing targets MUST default to "not opted in" with no manual
  intervention.

#### SNI peek-duration histogram

- **FR-011**: SNI-mode listeners MUST observe the wall-clock duration of
  every ClientHello peek (whether the peek succeeded, hit the deadline,
  or aborted on a parse error) and record it in a per-listener histogram.
- **FR-012**: The peek-duration histogram MUST be exposed on the same
  Prometheus surface as v0.9's existing per-listener and per-rule
  metrics, labelled by client identity and listen port using the
  conventions established from v0.5 onward.
- **FR-013**: Plain-TCP (legacy) listeners MUST NOT contribute peek-
  duration observations.
- **FR-014**: The histogram bucket boundaries MUST cover the range from
  sub-millisecond to the 3-second peek deadline with enough resolution
  that operators can distinguish typical load (well under 10ms) from
  tail incidents (hundreds of milliseconds to seconds).
- **FR-015**: The histogram's finite tail bucket MUST include observations
  equal to the 3-second peek deadline. Observations greater than the
  deadline MUST increment the histogram's total count / `+Inf` series
  without incrementing any finite `le` bucket.

#### Cross-cutting invariants

- **FR-016**: forward-rs MUST remain a pure L4 byte-passthrough — no
  feature in v0.10 may decrypt, terminate, parse, or re-encrypt TLS
  application data.
- **FR-017**: The data-plane events emitted for PROXY-protocol failures
  and for peek-histogram observation MUST flow through the structured
  tracing log and the Prometheus surface only; they MUST NOT enter the
  SQLite operator audit ring (preserving the v0.9 D13 invariant).
- **FR-018**: Authentication and credential-handling seams from v0.5/v0.8
  (TLS + bearer token under Constitution v2.0.1) MUST NOT change.
- **FR-019**: The on-the-wire control-plane envelope, target-message
  shape, and forwarding hot-path layout MUST remain byte-stable for v0.9
  callers that do not opt into the new fields.
- **FR-020**: This feature MUST NOT introduce new workspace dependencies;
  PROXY-protocol encoding and peek-duration histogram reporting must use
  existing workspace dependencies and standard library facilities.

#### Out of scope (explicit non-requirements)

- **NR-001**: TLS termination, decryption, or re-encryption — the system
  remains a pure L4 byte-passthrough. Same posture as v0.9.
- **NR-002**: PROXY-protocol consumption on the **client→forward-client**
  ingress side (i.e., trusting an upstream load balancer's PROXY header
  on incoming connections). v0.10 only **injects** to upstreams.
- **NR-003**: PROXY-protocol on UDP rules. Although the v2 spec defines
  UDP/DGRAM blocks, UDP is a separate design conversation and is
  deferred.
- **NR-004**: A `/metrics` endpoint on forward-client. Metrics continue
  to flow through the existing client→server stats reporting channel.
- **NR-005**: TLV/extension fields in the PROXY v2 binary header (e.g.,
  authority, CRC32C, custom TLVs). v0.10 emits the minimum address block
  only.
- **NR-006**: Configurable peek-budget timeout. The 3-second deadline
  from v0.9 stays hard-coded.
- **NR-007**: L7 / HTTP-aware routing, rate-limiting, and QUIC SNI —
  still backlog items, unchanged from v0.9.

### Key Entities

- **Target PROXY-protocol setting**: a per-target value with three
  forms — absent (legacy v0.9 behaviour), version 1 (ASCII line), or
  version 2 (binary header). Stored alongside other per-target attributes
  (address, priority, health hints) in the same persistence path.
- **Upstream connection prelude**: the very first bytes written to an
  upstream socket after TCP connect. In v0.9 this is empty; in v0.10 it
  is empty for non-opted-in targets and a single PROXY header for
  opted-in targets. Distinct from the forwarded byte stream that follows.
- **Peek-duration observation**: a single (listener-id, duration) pair
  produced once per accepted SNI-mode connection (regardless of peek
  outcome). Aggregated client-side into a fixed-bucket histogram and
  reported to the server alongside existing stats.
- **Capability gate decision**: a server-side check performed at
  rule-push time that compares the requested target's PROXY-protocol
  setting against the destination forward-client's self-reported version
  before activating the rule. Same shape as v0.9's
  `sni_unsupported_by_client` precedent.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: For 100% of upstream connections to opted-in targets, the
  backend's access log shows the **end client's** IP and source port
  rather than the forward-client's bind address, across both IPv4 and
  IPv6 client connections, with zero observed cases of forward-client
  IP leakage in a representative test workload of at least 10,000
  connections.
- **SC-002**: Opting a target into PROXY-protocol adds no more than one
  millisecond of additional setup latency at the median compared to the
  same target without opt-in, measured client-side from accept to first
  forwarded byte under a normal-load fixture.
- **SC-003**: Targets that are not opted in produce upstream byte
  streams that are byte-identical to v0.9 across a regression replay of
  every fixture currently exercising the v0.9 forwarding path —
  measured as zero diff bytes against a captured v0.9 baseline.
- **SC-004**: An operator running an SNI-mode listener for at least one
  hour can compute peek-duration p50, p95, p99, and p999 from the
  Prometheus surface using only standard `histogram_quantile` queries,
  without needing per-connection log parsing or external tooling.
- **SC-005**: A v0.9 forward-client paired with a v0.10 server cannot
  inadvertently activate a PROXY-protocol-enabled target — the server
  refuses the push and the operator receives a clear, actionable error
  identifying the version mismatch within the same API call.
- **SC-006**: After a v0.9 → v0.10 server upgrade, every existing v0.9
  rule continues to forward without configuration changes, with zero
  observed behavioural differences in upstream byte streams or in the
  v0.9 metric surface.
- **SC-007**: A peek that times out at the 3-second deadline appears in
  both the v0.9 peek-failure counter and the new peek-duration histogram's
  `le="3"` bucket. A peek observed after the deadline appears in `_count`
  / `le="+Inf"` without inflating finite buckets, so an operator triaging
  a "peek failures rising" Prometheus alert can pivot to the latency tail
  in the same dashboard panel without corrupting quantile math.

## Assumptions

- The PROXY-protocol versions emitted are HAProxy v1 (ASCII) and v2
  (binary), per the published HAProxy specification frozen at v2.4. No
  vendor-specific extensions.
- The histogram bucket layout will use fixed log-spaced boundaries
  covering ≈ 100 µs to 3 s with roughly half-decade granularity in the
  middle of the range. The exact boundaries are an implementation
  detail to be pinned down during design.
- The new per-target setting is stored as an additive nullable column
  on the existing targets/rules persistence table; the schema-version
  handshake range advances by one minor step. Pre-existing rows
  read as "not opted in".
- The capability gate compares against the forward-client's
  self-reported version on its most recent control-plane reconnect
  (the v0.7 / v0.9 precedent); no new wire field is required for the
  comparison itself.
- The v0.7 multi-target health-and-failover behaviour applies
  unchanged: a PROXY-write failure is one form of connect failure,
  no different in priority or accounting from a TCP RST during connect.
- The existing v0.6 web UI rule editor surfaces per-target attributes
  in a tabular form already; the new opt-in field appears as one
  additional column / form input in that table.
- Operators monitoring v0.10 already use the same Prometheus stack as
  v0.9; no new monitoring infrastructure is introduced by this feature.

## Inputs

For traceability, the original feature description provided to
`/speckit-specify` is reproduced below verbatim:

> v0.10 — PROXY protocol injection on upstream connect (per-target
> opt-in) + SNI peek-duration histogram. Theme: "Give v0.9 users the
> missing pieces" — PROXY protocol so backends can see real client IPs;
> peek-duration histogram to close the v0.9 observability gap (D12 /
> NR-004). Hard invariants: forward-rs stays pure L4 byte-passthrough;
> byte-stable for v0.9 callers that don't opt in; zero new workspace
> deps; auth seam unchanged; data-plane events tracing-only (D13).
> Scope (PROXY): per-target opt-in `Option<ProxyVersion ∈ {V1, V2}>`,
> additive proto field on Target, additive SQLite column, IPv4+IPv6,
> TCP-only, capability gate `proxy_protocol_unsupported_by_client`,
> write-failure counted as connect failure. Scope (histogram):
> per-SNI-listener via new additive Prometheus channel, fixed log-
> spaced buckets, SNI-mode only. Out of scope: L7, rate-limit, QUIC
> SNI, port-range SNI, configurable peek-budget, ingress-side PROXY
> consumption, /metrics on forward-client. Inherited baselines: v0.9
> (SNI), v0.7 (multi-target failover), v0.5 (RBAC envelope, metric
> labels). Branch: 010-proxy-protocol-and-peek-histogram.
