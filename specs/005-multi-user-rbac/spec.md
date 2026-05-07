# Feature Specification: Multi-User RBAC for the Forward Server

**Feature Branch**: `005-multi-user-rbac`
**Created**: 2026-05-07
**Status**: Draft
**Input**: User description: "Multi-user RBAC for the forward server: identity model (users with credentials), role hierarchy, and per-user grants for client machines, listen-port ranges, and forwarding protocols. Every operator push-rule and stats request is authorized against the caller's grants before reaching the rule store. Additive on top of v0.4.0 — response bodies become byte-supersets (gain an `owner` field); the only operator-visible breaking change is that an `Authorization: Bearer <token>` header becomes mandatory. No web UI in this feature."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Constrained user pushes a rule within their grants (Priority: P1)

A platform operator (superadmin) onboards a tenant by creating a user, issuing a credential, and granting that user the right to push rules on a specific client machine, within a specific listen-port range, for a specific set of protocols. The tenant then uses the operator CLI with their own credential to push a rule that fits the grant; the server accepts the rule and stores it under the tenant's identity. A second push from the same tenant that violates any grant dimension (different client, port outside the range, disallowed protocol) is rejected with a clear reason and no rule is created.

**Why this priority**: This is the smallest end-to-end slice that delivers the core value — multiple identities can share one server with isolated, bounded forwarding capability. Without it, the RBAC model has no observable user-facing effect. Every other story builds on this enforcement path.

**Independent Test**: Create one constrained user via the existing superadmin credential; push a compliant rule with the new user's credential and observe success; push four violation variants (wrong client, listen port too low, listen port too high, wrong protocol) and observe four distinct rejection reasons. The superadmin's own push-rule path keeps working with the same wire shape (response bodies gain an `owner` field; no other change) throughout.

**Acceptance Scenarios**:

1. **Given** a user `alice` with grant `{client: client-a, listen_ports: 30000..30010, protocols: [tcp]}`, **When** alice pushes a TCP rule on client-a listen port 30005, **Then** the rule is accepted and recorded as owned by alice.
2. **Given** the same alice grant, **When** alice pushes a TCP rule on client-a listen port 30099, **Then** the request is rejected with reason `port_outside_grant` and no rule is created or activated.
3. **Given** the same alice grant, **When** alice pushes a UDP rule on client-a listen port 30005, **Then** the request is rejected with reason `protocol_not_granted`.
4. **Given** the same alice grant, **When** alice pushes a TCP rule on client-b listen port 30005, **Then** the request is rejected with reason `client_not_granted`.
5. **Given** the deployment uses the `operator_token` config-shortcut bootstrap path (see FR-006) and presents that token, **When** any rule is pushed, **Then** the request is treated as the built-in `_superadmin` identity and accepted regardless of grants.

---

### User Story 2 - Superadmin manages users and grants (Priority: P1)

A platform operator needs to create, list, and remove users; issue and rotate credentials; and create, list, and revoke per-user grants. Every state change is durable across server restarts. Listing surfaces enough detail (user → grants → credential metadata) for the operator to audit who can do what without grepping through raw config.

**Why this priority**: P1 because Story 1 has no usable workflow without administrative CRUD for users and grants. Without this, the only way to seed identities would be manual config-file edits, which is error-prone and doesn't meet the "machine onboarding" goal.

**Independent Test**: Through the operator CLI, create a user, list users (verify presence), issue a credential (verify a token is returned exactly once and is not retrievable later), create a grant, list grants (verify), revoke the grant, list again (verify removal). Restart the server and re-list; the user and any non-revoked state must persist.

**Acceptance Scenarios**:

1. **Given** an empty user table, **When** the superadmin calls `user-add alice`, **Then** alice exists and is listed by `user-list`.
2. **Given** alice exists with no credential, **When** the superadmin calls `credential-issue alice`, **Then** a token is returned in the response body exactly once; subsequent `user-list` shows credential metadata (created_at, last_used_at) but never the raw token.
3. **Given** alice has a grant, **When** the superadmin calls `grant-revoke <grant_id>`, **Then** the grant is removed from listings and any rule alice owned that depended on the revoked grant is removed within the propagation window described in SC-005.
4. **Given** the server is stopped after creating users and grants, **When** the server restarts, **Then** all users, grants, and credentials are restored.
5. **Given** a user with active rules, **When** the superadmin calls `user-remove alice`, **Then** all of alice's rules are removed and her credentials are invalidated immediately.

---

### User Story 3 - Tenant inspects only their own rules and stats (Priority: P2)

A tenant uses the operator CLI with their own credential to list rules, view per-rule stats, and view per-port stats. The response includes only rules owned by that tenant — never rules owned by other tenants or by the superadmin. The superadmin sees everything.

**Why this priority**: P2 because Story 1 covers writes; reads are required for tenants to verify their own deployments but are independently testable and not on the critical first-pass path.

**Independent Test**: Create two users `alice` and `bob`, push one rule under each, then call `rule-list` with each credential and verify each tenant sees only their own rule. Call the same with the superadmin credential and verify both rules are visible.

**Acceptance Scenarios**:

1. **Given** alice owns rule R-A and bob owns rule R-B, **When** alice calls `rule-list`, **Then** the response contains R-A only.
2. **Given** the same setup, **When** bob calls `rule-stats R-A`, **Then** the request is rejected with reason `not_owner` and no stats are returned.
3. **Given** the same setup, **When** the superadmin calls `rule-list`, **Then** both R-A and R-B are returned with an `owner` field identifying the responsible user.

---

### User Story 4 - Tenant rotates their own credential (Priority: P3)

A tenant whose credential may have leaked rotates it themselves without requiring superadmin intervention. The new credential is returned exactly once; the old credential stops working immediately.

**Why this priority**: P3 because the superadmin can already accomplish this in Story 2 (issue a new credential, revoke the old). Self-service is a usability improvement, not a blocker.

**Independent Test**: With alice's current credential, call `credential-rotate`; capture the new token; verify the old token is rejected on the next call and the new token is accepted.

**Acceptance Scenarios**:

1. **Given** alice holds credential C-old, **When** alice calls `credential-rotate`, **Then** a new credential C-new is returned exactly once and the response indicates C-old is invalidated.
2. **Given** alice has rotated, **When** any subsequent request uses C-old, **Then** the request is rejected with reason `credential_invalid`.

---

### Edge Cases

- A user has multiple overlapping grants (e.g., `client-a, ports 30000..30010, [tcp]` AND `client-a, ports 30005..30020, [udp]`). A push that satisfies any single grant is allowed. The system MUST NOT require all grants to match.
- A grant references a client that does not currently exist (offline or never connected). Pushing rules under that grant succeeds; the rule sits in the existing pending state and activates when the client appears, identical to current v0.4.0 behavior.
- A grant is revoked while a rule it authorized is actively forwarding traffic. The rule is removed within the SC-005 propagation window; in-flight datagrams/connections complete or fail per existing v0.4.0 rule-removal semantics, no new connections accepted.
- A user is removed while they hold an active CLI session (long-running stats stream). The next request after invalidation MUST be rejected; the server MUST NOT panic or leak the prior session.
- Neither the `operator_token` config-shortcut nor a prior `bootstrap-superadmin` invocation has been run. The server still starts, but every operator request is rejected with `bootstrap_required` until one of the two bootstrap paths runs. The server MUST NOT silently grant superadmin to any unauthenticated request.
- A grant is created with `operator_token` config-shortcut already applied AND a different superadmin already exists in `identity.json`. The config-shortcut is a no-op on subsequent starts (the existing superadmin is honored; the config token is NOT minted as a second superadmin) — startup emits one INFO log noting the shortcut was ignored.
- A grant is requested with `protocols: []` (empty set). The grant-add request is rejected at validation time with `empty_protocol_set`; no grant is persisted. (No "no-op grant" mode exists — the codebase fails closed.)
- Two users hold credentials that happen to collide due to a hash truncation. Credential lookup MUST disambiguate by full credential, not by prefix.

## Requirements *(mandatory)*

### Functional Requirements

**Identity & Credentials**

- **FR-001**: The system MUST support creating, listing, and removing user identities, each with a unique stable identifier and a human-readable name.
- **FR-002**: The system MUST issue per-user bearer credentials such that the raw credential value is shown to the operator exactly once at issuance and is never retrievable afterward; only non-secret credential metadata (created_at, last_used_at, optional label) is queryable later.
- **FR-003**: The system MUST support credential rotation initiated by either the credential's owner or the superadmin, atomically issuing the replacement and invalidating the prior credential.
- **FR-004**: The system MUST persist users, credentials, and grants durably across restarts; restart MUST NOT silently lose or downgrade any authorization state.

**Roles & Authorization**

- **FR-005**: The system MUST recognize at least two roles: `superadmin` (unrestricted) and `user` (restricted to their grants). Future roles MAY be added without breaking existing role assignments.
- **FR-006**: The system MUST support an optional `operator_token` key in `server.toml` (introduced by v0.5.0) that, when set on a deployment with no existing superadmin in the identity store, mints a built-in `_superadmin` user backed by the supplied token on first start. This is one of two bootstrap paths to obtain the first superadmin (the other is the `bootstrap-superadmin` CLI subcommand per FR-017). After first start, removing the config key does NOT revoke the token (it has been persisted to the identity store as a hash). Wire-shape compatibility: success-path response bodies are byte-supersets of v0.4.0 (only the additive `owner` field is new); the only operator-visible breaking change is that an `Authorization: Bearer <token>` header becomes mandatory on every `/v1/*` request.
- **FR-007**: The system MUST authorize every operator request (push-rule, remove-rule, list-rules, rule-stats, per-port-stats, user/grant management, AND the existing v0.4.0 client-provisioning endpoints `/v1/clients*`) against the caller's identity and grants before any side effect occurs. Unauthorized requests MUST be rejected with a categorized reason and MUST NOT mutate state. Client provisioning becomes superadmin-only by default (non-superadmin operators receive `role_required`).
- **FR-008**: The system MUST distinguish at least the following authorization rejection reasons in machine-readable form: `unauthenticated`, `credential_invalid`, `client_not_granted`, `port_outside_grant`, `protocol_not_granted`, `not_owner`, `role_required`. Each reason MUST map to a stable error code usable by the operator CLI.

**Grants**

- **FR-009**: The system MUST allow the superadmin to create, list, and revoke grants. A grant MUST specify (a) the user it applies to, (b) the target client machine identifier or `any`, (c) a contiguous listen-port range (one or more ports, with start ≤ end), and (d) a non-empty set of permitted protocols drawn from the protocols supported by the underlying forwarding engine (currently TCP and UDP).
- **FR-010**: The system MUST allow a user to hold multiple grants. A push request is authorized if at least one grant satisfies all dimensions (client, every port in the rule's listen range, protocol). Range rules MUST require the entire listen range to fit within a single grant's port range; partial coverage by multiple grants is NOT sufficient.
- **FR-011**: The system MUST permit grants that reference a client identifier that has not yet connected. Such grants are stored and become effective as soon as that client connects, with no further operator action.
- **FR-012**: When a grant is revoked, the system MUST remove any rule whose authorization depended exclusively on that grant within the time bound stated in SC-005. Rules that remain authorized by another grant the user still holds MUST continue running. The owning user is notified through the standard rule-removed audit channel.
- **FR-013**: When a user is removed, the system MUST remove all rules owned by that user, invalidate all of that user's credentials, and revoke all of that user's grants. The identity-side state changes (credentials revoked, grants revoked, user removed) MUST commit as one atomic write to the identity store BEFORE the rule-removal cascade begins; the rule-removal cascade then runs synchronously and completes within the SC-005 5 s bound. A crash between the two phases MUST leave the identity store consistent and MUST NOT leak capability through still-running rules (the rule-removal cascade is idempotent and replays cleanly on restart, although as of v0.5.0 rules do not survive restart at all per the v0.4.0 design).

**Ownership & Visibility**

- **FR-014**: The system MUST record the owning user identifier on every rule at creation time. Rules created by the superadmin are recorded as owned by the built-in superadmin identity.
- **FR-015**: A non-superadmin user's read requests (list rules, get rule, rule stats, per-port stats) MUST be filtered to only rules they own. Attempting to read a rule owned by another user MUST be rejected with `not_owner`. The superadmin sees all rules with their `owner` field populated.
- **FR-016**: Per-rule and aggregate metrics already exposed in v0.4.0 MUST gain an `owner` dimension so operators can break stats down by user. Per-port and per-rule cardinality limits established in v0.4.0 MUST NOT regress (one row per rule per collector).

**Bootstrap & Operability**

- **FR-017**: The system MUST provide a documented `bootstrap-superadmin` CLI subcommand that creates the first superadmin identity in environments where the `operator_token` config-shortcut (FR-006) was not used. This subcommand MUST be safe to invoke at deployment time, MUST print the freshly-minted token to stdout exactly once (never to logs), and MUST exit non-zero with `already_bootstrapped` if any superadmin already exists in the identity store.
- **FR-018**: All authorization decisions (grant, deny) MUST be logged at a level operators can route to standard observability sinks, and MUST include the actor, the resource, the decision, and (on deny) the rejection reason. Logging MUST NOT include raw credentials.

### Key Entities *(include if feature involves data)*

- **User**: A named identity that can hold credentials and grants. Attributes: stable identifier, human-readable name, role, created_at, optional disabled flag. Relationships: 1→N credentials, 1→N grants, 1→N owned rules.
- **Credential**: A bearer token that authenticates a user. Attributes: opaque secret value (stored in a form that does not allow retrieval and supports safe verification of presented tokens), created_at, last_used_at, optional label, status (active / revoked). Relationships: N→1 user.
- **Grant**: A capability the superadmin attaches to a user, scoping what they can push. Attributes: stable identifier, target client identifier or wildcard `any`, listen-port range (start..end inclusive), permitted protocols (non-empty subset of {TCP, UDP}), created_at, optional note. Relationships: N→1 user.
- **Rule (existing, extended)**: The v0.4.0 forwarding rule entity gains an `owner_user_id` attribute populated at creation time. Existing rule attributes (id, protocol, listen ports, target, …) are unchanged.
- **Audit event** *(implied by FR-018; minimal shape)*: timestamp, actor user identifier, action, resource identifier, decision, optional reason. Storage durability and retention may follow existing logging conventions; persistent audit DB is out of scope for this feature.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A superadmin can onboard a new tenant from "user does not exist" to "tenant has pushed their first rule successfully" in under 60 seconds of wall-clock time, following the documented quickstart, on a single-host loopback environment.
- **SC-002**: Adding the authorization check to the push-rule and stats-read paths increases their median end-to-end latency by no more than 5 ms compared to the v0.4.0 baseline measured on the same hardware.
- **SC-003**: 100% of push-rule, remove-rule, list-rules, and stats requests that violate any grant dimension or ownership constraint are rejected with the categorized reason codes enumerated in FR-008. Verified by the per-story acceptance scenarios.
- **SC-004**: After this feature lands, v0.4.0 operator CLI invocations require exactly one additive change to keep working: setting `FORWARD_OPERATOR_TOKEN` (or passing `--token`). With that token in place, success-path response bodies are byte-supersets of v0.4.0 (only the additive `owner` field is new — existing v0.4 client decoders ignore unknown JSON fields per superset rules) and CLI exit codes for the success path are unchanged. Verified by replaying the v0.4.0 quickstart end-to-end after exporting `FORWARD_OPERATOR_TOKEN`.
- **SC-005**: When a grant is revoked or a user is removed, every dependent rule is removed within 5 seconds of the revocation request returning success, measured from the operator's clock.
- **SC-006**: After a server restart, 100% of users, non-revoked grants, and active credentials are present and usable. No bootstrap step beyond starting the binary is required.
- **SC-007**: Operators can answer "who pushed rule X?" via a single `rule-list --format table` invocation (the table includes an `owner` column) and "what can user Y do?" via a single `grant-list --user Y --format table` invocation. Both queries are answerable from the operator CLI without log grepping or accessing any other surface.

## Assumptions

- The operator surface remains the existing operator CLI / HTTP API. No web UI is in scope for this feature; a later feature may add one on top of the identity model defined here.
- Credentials are bearer tokens consistent with the v0.2 operator model. Password-based login, MFA, and SSO are out of scope. Adding them later does not require changing the user/grant data model.
- "Client machine identifier" reuses the existing v0.1+ client identifier the server already assigns at handshake. No new device-attestation mechanism is introduced.
- A grant's port range and protocol set are matched exactly as written; the system does not infer port-protocol combinations (e.g., granting `[tcp]` does not implicitly allow UDP on the same port).
- Revoking a grant removes any rule whose authorization depended on it (fail-closed posture). This is preferable to leaving orphaned rules running, even though it may surprise operators; the behavior is explicit in FR-012 and surfaced in audit logs.
- Aggregate metric cardinality remains bounded by the v0.4.0 rule established for port-range rules (one row per rule per collector). Adding the `owner` dimension does not multiply rows because each rule already has exactly one owner.
- Bootstrap path: when the `operator_token` config-shortcut (FR-006) is absent, a one-shot `bootstrap-superadmin` CLI subcommand (FR-017) creates the initial superadmin and prints its credential exactly once.
- The audit event stream piggybacks on the existing structured-logging facility (Constitution Principle IV); a separate persistent audit database is out of scope and may be added in a future feature.
- Per-grant rate limiting and per-user quota beyond port-range / protocol scoping are out of scope; they may be added in a future feature without changing the entities defined here.
