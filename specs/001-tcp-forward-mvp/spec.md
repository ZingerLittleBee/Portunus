# Feature Specification: Single-Tenant Token-Authenticated Control Plane with Single-Port TCP Forwarding (MVP)

**Feature Branch**: `001-tcp-forward-mvp`
**Created**: 2026-05-06
**Status**: Draft
**Input**: User description: "Single-tenant TLS + per-client bearer token control plane with single-port TCP forwarding MVP — the thin end-to-end vertical slice that proves the C/S architecture, transport security, control protocol, rule distribution, and the data plane all work together."

> **Authentication note**: This spec aligns with Constitution v2.0.0. The
> control channel uses TLS for transport (server certificate pinned by the
> client) and a per-client bearer token for authentication. mTLS / X.509
> client certificates are explicitly NOT used in this MVP.

## Clarifications

### Session 2026-05-06

- Q: How are bearer tokens handled across server restarts? → A: Token hashes (with client name and revoked flag) and the server TLS cert + key are persisted to disk at fixed paths; rule state and connection state remain in-memory only. Already-issued tokens stay valid across restarts; rules must be re-pushed.
- Q: What happens when the operator runs `provision-client` with a name that already exists? → A: Error out with `client_already_exists`; the operator must explicitly revoke the existing client before re-provisioning. No auto-rotation in MVP.
- Q: What is the target server scale (concurrent connected clients) for this MVP? → A: ≤100 concurrent clients per server. `list-clients` MAY return the full set in a single response; pagination, per-client task-pool tuning, and bounded-channel sizing for >100 clients are deferred to a future spec.
- Q: When a rule activation fails on the client (e.g., `port_in_use`), what is its lifecycle? → A: The rule is retained server-side in state `failed(<reason>)`. No automatic retry — neither immediately nor on control-connection reconnect. The operator must explicitly remove the failed rule before pushing a new rule that targets the same listen port.

## User Scenarios & Testing *(mandatory)*

### User Story 1 — Provision a Trusted Client (Priority: P1)

The operator runs a freshly installed server, asks it to provision a new client (giving the client a name), and receives a credential bundle containing the server endpoint, the server's TLS certificate fingerprint (for pinning), the client's name, and a high-entropy bearer token. They copy that bundle to a target machine, start the client process there, and within seconds see the new client appear in the server's "connected clients" listing — proving that transport security and authenticated identity are correctly established end-to-end.

**Why this priority**: Trust is the load-bearing foundation. Without verifiable authentication, every later capability (rule distribution, traffic stats) is built on sand. Until provisioning + handshake works, nothing else is meaningful.

**Independent Test**: From a clean install, run server, run `provision-client edge-01`, transfer the produced bundle to a second host, run client, then run `list-clients` on the server and confirm `edge-01` appears with its remote address and connect timestamp. No rule push required.

**Acceptance Scenarios**:

1. **Given** a freshly initialised server with no clients, **When** the operator provisions a client named `edge-01` and starts the client process on a separate host using the issued bundle, **Then** within 5 seconds `edge-01` appears in the server's connected-client listing with its remote address and connect time.
2. **Given** a client process started with a bundle containing an invalid token (or a token belonging to a different client name), **When** the client attempts to connect, **Then** the server rejects the authentication and emits a structured log event of type `auth_failure` naming the rejection reason, and the client never appears in the connected-client listing.
3. **Given** an operator has revoked a previously-issued token, **When** a client process attempts to connect using that token, **Then** the server rejects the authentication with reason `token_revoked` in the audit log.
4. **Given** a client started with a bundle whose pinned server-certificate fingerprint does not match the certificate the server actually presents, **When** the client attempts to connect, **Then** the client refuses to complete the TLS handshake and exits with a `server_cert_mismatch` error (without sending the token), and the failure is observable in the client's structured log.

---

### User Story 2 — Push a TCP Forwarding Rule and Verify Traffic (Priority: P2)

With a connected client, the operator pushes a single forwarding rule (listen port → target host:port, TCP). The client begins listening on the specified port; any inbound TCP connection is bidirectionally proxied to the target. The operator can verify the forward by performing a real TCP transfer (e.g., a file copy) through the listen port and observing it arrive at the target unchanged.

**Why this priority**: This is the actual product capability — moving bytes. P1 only proved trust; P2 proves the forwarder works.

**Independent Test**: After User Story 1, run a target service (any TCP echo or HTTP server) on a known host:port reachable from the client. Push a rule on the server. From a third host, transmit at least 100 MB of arbitrary data through the client's listen port and confirm bit-for-bit delivery to the target.

**Acceptance Scenarios**:

1. **Given** a connected client `edge-01` and a reachable target service on `10.0.0.5:8080`, **When** the operator pushes a rule `listen=18080, target=10.0.0.5:8080, protocol=TCP` to `edge-01`, **Then** within 1 second the client acknowledges the rule and begins accepting connections on port `18080`, and a subsequent inbound TCP connection is forwarded bidirectionally to `10.0.0.5:8080`.
2. **Given** an active rule, **When** a client transmits a 100 MB stream through the listen port, **Then** the bytes received at the target are byte-for-byte identical to the bytes sent and the connection completes without truncation.
3. **Given** the operator pushes a rule whose listen port is already bound on the client (port conflict), **When** the client attempts to activate the rule, **Then** the client reports `rule_activation_failed` with reason `port_in_use` to the server, the rule is not marked active, and the operator sees the failure surfaced in their interface.
4. **Given** a connected client, **When** the operator pushes a rule for a different client name that is not currently connected, **Then** the server rejects the push with `client_not_connected` and the rule is not stored.
5. **Given** an active rule, **When** the operator removes the rule, **Then** the client stops accepting new connections on that port within 1 second; in-flight connections are allowed to drain (see edge cases).

---

### User Story 3 — Observe Activity via Structured Logs and Per-Rule Stats (Priority: P3)

Once traffic is flowing, the operator can audit what is happening: structured log events for connect/disconnect/rule lifecycle, plus per-rule cumulative byte counters and active connection counts available on demand.

**Why this priority**: Operability is required by the constitution and is what makes the MVP operable in any real setting, but a working forwarder *technically* delivers value before observability is wired up.

**Independent Test**: With Story 1 + 2 active and a rule transferring traffic, run a stats query for the rule and confirm reported byte counts grow in line with actual traffic (within tolerance). Tail the structured log stream and confirm one event per rule activation, removal, and authentication failure.

**Acceptance Scenarios**:

1. **Given** an active rule with traffic flowing, **When** the operator queries the rule's stats, **Then** the response contains cumulative `bytes_in`, `bytes_out`, and current `active_connections`, each updated within 5 seconds of the underlying traffic.
2. **Given** any control-plane event (connect, disconnect, rule activate, rule remove, auth failure), **When** the event occurs, **Then** a structured log record is emitted on the relevant process containing at minimum: ISO-8601 timestamp, event type, client name (when applicable), rule identifier (when applicable), and reason (for failures).
3. **Given** a rule push, **When** it completes (success or failure), **Then** the server emits an `audit` log record naming the operator action, target client, rule details, and outcome.

---

### Edge Cases

- **Invalid or revoked client token**: Server rejects on the authentication exchange (after TLS but before any rule traffic); auth failure logged with the offered client name and a reason of `token_invalid` or `token_revoked`; client never appears in connected list.
- **Server certificate fingerprint mismatch on client**: Client refuses TLS handshake before sending its token; client exits or backs off and retries (depending on operator policy); failure visible in client log.
- **Listen port already bound on the client host**: Rule activation fails with `port_in_use`; the rule is retained server-side in state `failed(port_in_use)` and is not auto-retried (FR-012). Operator must explicitly remove it before reusing that listen port.
- **Privileged listen port (<1024) without privilege**: Same failure path as `port_in_use` but with a distinguishable reason such as `permission_denied`.
- **Target host:port unreachable when an inbound connection arrives**: Client closes the inbound connection promptly; failure does not crash the rule; failure is countable in stats and surfaces in logs at a reasonable rate (not per-connection spam).
- **Control connection drops while traffic is flowing**: In-flight forwarded connections continue uninterrupted. The client attempts reconnection with bounded backoff. New rules cannot be pushed until reconnection.
- **Server restart while clients are connected**: Clients reconnect on backoff and re-authenticate using their unchanged tokens (token store and TLS cert are persisted — see FR-006a). Rules are NOT restored automatically (no rule persistence in MVP); operator must re-push. This is documented behaviour, not a bug.
- **Rule removal during active forwarding**: Client stops accepting new connections within 1 second; in-flight connections drain up to the shutdown timeout (default 30s) then are forcibly closed.
- **Client process termination signal**: Client drains in-flight forwarded connections up to the shutdown timeout before exiting.
- **Two rules on the same client targeting the same listen port**: Second push fails with `port_in_use` (rules do not silently override).
- **Re-provisioning an existing client name**: Fails with `client_already_exists`; the operator must explicitly revoke the existing client first. This guards against accidental token rotation that would silently lock out a working machine.

## Requirements *(mandatory)*

### Functional Requirements

**Authentication & Trust**

- **FR-001**: Server MUST issue a per-client bearer token at provisioning time. Tokens MUST be generated from a cryptographically secure random source with at least 128 bits of entropy.
- **FR-002**: Server MUST present a TLS certificate (self-signed by default, or operator-supplied) on the control listener. Server MUST refuse all non-TLS control-plane connections.
- **FR-003**: Server MUST authenticate every client by verifying that the offered bearer token matches a known, non-revoked token, and MUST identify the client by the name registered against that token. Authentication MUST happen after TLS is established but before any rule message is processed.
- **FR-004**: Operator MUST be able to provision a new client by name; provisioning MUST produce a credential bundle containing: server endpoint (host:port), server TLS certificate fingerprint (for client-side pinning), client name, and the freshly issued bearer token. Provisioning a name that already exists in the token store (whether the existing entry is active or revoked) MUST fail with error `client_already_exists` and MUST NOT mutate the existing entry; the operator must explicitly revoke the existing client first if their intent is rotation.
- **FR-005**: Server MUST store tokens only as a non-reversible hash; the original token value MUST be returned to the operator exactly once (at provisioning) and MUST NOT be retrievable thereafter.
- **FR-006**: Operator MUST be able to revoke a previously-issued token. Subsequent connection attempts using a revoked token MUST be rejected.
- **FR-006a**: Server MUST persist the token store (token hash, client name, issued timestamp, revoked flag) and the server's TLS certificate + private key to disk at operator-configurable paths. After a server restart, all previously-issued non-revoked tokens MUST remain valid and the server MUST present the same TLS certificate (so client-side fingerprint pinning continues to succeed).

**Control Channel**

- **FR-007**: Client MUST establish and maintain a long-lived TLS-encrypted authenticated connection to the server. Client MUST verify the server's certificate against the pinned fingerprint in its bundle and MUST refuse the handshake on mismatch (without transmitting its token).
- **FR-008**: Client MUST automatically attempt to reconnect with bounded exponential backoff if the control connection drops.
- **FR-009**: Server MUST expose, to the operator, the set of currently connected clients with at least: client name, remote address, connect timestamp.

**Rules**

- **FR-010**: Operator MUST be able to push a TCP forwarding rule to a named, currently-connected client. A rule MUST specify: listen port (on the client host), target host, target port, and protocol (which MUST be TCP in this MVP).
- **FR-011**: Operator MUST be able to remove a previously-pushed rule.
- **FR-012**: Client MUST attempt to activate a rule (begin listening) within 1 second of receiving it and MUST report the activation result (success or named failure reason) back to the server. Activation failures MUST NOT be automatically retried — neither immediately nor on subsequent control-connection reconnect. The rule MUST be retained server-side in state `failed(<reason>)` and remain visible to the operator until explicitly removed; pushing a new rule that would conflict with a `failed` rule's listen port MUST also fail with `port_in_use` until the failed rule is removed.
- **FR-013**: For every accepted inbound TCP connection on a rule's listen port, the client MUST open a fresh outbound TCP connection to the rule's target host:port and bidirectionally proxy bytes until either side closes.
- **FR-014**: Server MUST reject a rule push targeting a client name that is not currently connected, returning a `client_not_connected` error to the operator without retaining the rule.
- **FR-015**: A client MUST support multiple concurrent rules on different listen ports; rules MUST NOT interfere with each other (failure of one MUST NOT affect others).
- **FR-016**: Rule removal MUST cause the client to stop accepting new inbound connections on the listen port within 1 second; in-flight forwarded connections MUST be drained subject to the shutdown timeout.

**Observability**

- **FR-017**: Server and client MUST emit structured (machine-parseable) log events for at least: process start, control connection established, control connection lost, authentication failure (with reason — `token_invalid`, `token_revoked`, `unknown_client`, etc.), token issued, token revoked, rule pushed, rule activated, rule removed, rule activation failed (with reason).
- **FR-018**: Client MUST maintain per-rule cumulative `bytes_in`, `bytes_out`, and current `active_connections` counters, queryable by the operator via the server.
- **FR-019**: Server MUST emit an audit log record for every operator action affecting a client or rule, naming the action, the affected client, the rule (where applicable), and the outcome.

**Lifecycle, Operator Interface & Architectural Seam**

- **FR-020**: Both server and client MUST drain in-flight forwarded connections on shutdown subject to a configurable timeout (default 30 seconds) before terminating.
- **FR-021**: Operator MUST be able to perform every MVP action (provision client, revoke token, list connected clients, push rule, remove rule, query rule stats) via a single command-line or local HTTP interface — no GUI required.
- **FR-022**: In the MVP, operator authorisation is satisfied by local-shell access to the server host; no separate operator login system is required.
- **FR-023**: The authentication mechanism MUST be encapsulated behind a single, well-defined interface in the codebase such that swapping it for an alternative (e.g., mTLS) does not require modifications to the rule-distribution logic, the data plane, or the operator interface. (Constitutional requirement, Principle I.)

### Key Entities

- **Client**: A registered remote machine identified by the name supplied at provisioning. Holds 0 or more active Rules at any time. Has a connection state (connected / disconnected) and, when connected, a remote address and connect timestamp.
- **Token**: The opaque, per-client bearer secret. Stored on the server as a hash plus metadata (client name, issued timestamp, revoked flag). The plaintext value exists only in the credential bundle delivered to the operator at provisioning time.
- **Rule**: A forwarding intent attached to a single Client, comprising: listen port, target host, target port, protocol (`TCP` in MVP), activation state (pending / active / failed with reason), and counters (bytes_in, bytes_out, active_connections).
- **Credential Bundle**: The artefact produced by client provisioning — server endpoint, server TLS certificate fingerprint, client name, and bearer token — that an operator transfers to a target machine to bootstrap a Client.
- **Operator**: The single trusted human role with shell access to the server host. No separate identity model in MVP.
- **Audit Event**: A structured log record describing an operator action (provision, revoke, push, remove) and its outcome.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Starting from a clean installation on two hosts, an operator completes the end-to-end happy path (provision client → start client → push rule → verify forwarded traffic with a real client tool) in under **5 minutes**.
- **SC-002**: A **100 MB** TCP transfer through a forwarding rule arrives at the target byte-for-byte identical to what was sent, in 100% of trial runs (≥10 trials).
- **SC-003**: Clients presenting a valid (non-revoked) token are accepted on **100%** of connection attempts; clients presenting a token that is malformed, unknown, or revoked are rejected on **100%** of attempts. Clients presenting any token over a TLS connection whose server certificate does not match the pinned fingerprint are rejected on **100%** of attempts (without the token ever reaching the server).
- **SC-004**: A single client sustains **at least 5 concurrent rules** and **at least 100 concurrent forwarded TCP connections per rule** under steady-state traffic without dropping connections.
- **SC-004a**: A single server sustains **at least 100 concurrently connected clients** without dropping control connections, and `list-clients` returns the full set in a single response within **1 second** at this scale.
- **SC-005**: After a client process restart, the operator can re-push the same rule and forwarded traffic resumes within **5 seconds** of the rule push.
- **SC-006**: Every authentication failure, token issuance, token revocation, rule activation, rule removal, and audit event is observable via structured logs that include at minimum a timestamp, event type, and (where applicable) client name and rule identifier — verifiable by automated log-shape assertion in tests.
- **SC-007**: Per-rule byte counters reported by the stats query are within **±1 KB** of actual transferred bytes (allowing for in-flight buffering) when measured at least 5 seconds after the last write.

## Assumptions

- **Single trusted operator** with local shell access on the server host; no operator-login system, no RBAC, no audit of operator identity beyond "the shell user".
- **Linux** is the target runtime for both server and client; **macOS** is supported for development only. Windows is out of scope.
- **No CA / no PKI**: server TLS is self-signed by default; operator may supply their own certificate. Clients pin the server certificate fingerprint at provisioning time. Per-client authentication uses bearer tokens, not X.509 client certificates. (See Constitution v2.0.0 Principle I for rationale and the deferred TODO on adding mTLS as an alternative auth seam later.)
- **Rule state is in-memory only** in MVP. Rules are NOT persisted across server restarts; the operator must re-push rules. The token store, in contrast, IS persisted (FR-006a) so that already-provisioned clients survive a server restart without intervention.
- **Rule scope is strictly TCP** with a single listen port per rule. UDP, domain-name resolution, port ranges, and protocol detection are explicitly deferred to subsequent specs.
- **Multi-tenancy is out of scope.** A single operator, single trust domain, no quotas, no per-tenant isolation logic in this MVP (the broader project goal of multi-tenancy is captured in the Constitution and will be addressed in a later spec).
- **No web UI**, no dashboard. CLI plus a local HTTP interface (operator-only, loopback) is sufficient.
- **Metrics endpoint IS in MVP** (Prometheus-compatible, loopback-bound) per Constitution Principle IV. Structured logs and on-demand stats queries are additionally provided. (Earlier drafts of this spec said the opposite; corrected during `/speckit-analyze`.)
- **Benchmark harness IS shipped in MVP** (criterion, data-plane single-rule throughput + p99 latency on loopback) per Constitution Principle II. No regression *threshold* is enforced yet because no baseline exists; the next hot-path-touching spec locks in numbers and CI gates. (Earlier drafts claimed a "v1 carve-out" that does not exist in the constitution; corrected during `/speckit-analyze`.)
- **No automatic token rotation** in MVP; rotation is performed by re-provisioning (issue new token, revoke old).
- **Server scale ceiling**: MVP targets ≤100 concurrently connected clients per server (SC-004a). Designs that would prematurely add pagination, per-client task pools, or bounded-channel tuning for higher fan-out are out of scope and should be deferred.
- **Connectivity assumption**: The client can reach the server on the configured control port; standard TCP connectivity from the operator host to the server's operator interface (CLI/HTTP) is available.
