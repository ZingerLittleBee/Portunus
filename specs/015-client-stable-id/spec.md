# Feature Specification: Client Stable Identifier (name as display field)

**Feature Branch**: `015-client-stable-id`
**Created**: 2026-06-02
**Status**: Draft
**Input**: User description: "为 client 引入系统生成的稳定不透明 ID(建议 ULID),把 client_name 从唯一标识符/URL 段/数据库主键/关联键降级为纯显示字段。" (full background: `docs/refactor-client-id-handoff.md`)

## Overview

Today a client's user-supplied **name** is overloaded as four different things at once:
its unique identity, its URL path segment, its database primary key, and the correlation
key in logs and metrics. Because the name doubles as a machine identifier, the system
enforces a strict DNS-label rule on it (1–63 chars, lowercase alphanumerics, must start
and end with `[a-z0-9]`, no consecutive hyphens). Operators are therefore blocked from
giving clients human-friendly names — uppercase, spaces, dots, underscores, or non-Latin
characters all fail with `invalid_name: client name must start and end with [a-z0-9]`.

This feature separates **identity** from **label**. The system assigns every client a
stable, opaque identifier at creation time. That identifier becomes the single thing every
layer keys on. The name is demoted to a free-form **display field** with only minimal
validation, and (because identity no longer depends on it) it becomes safe to change a
client's display name without disturbing its rules, tokens, quotas, or history.

This is a workspace-wide change touching the identity model, the control-plane wire schema,
the persistence layer (with an in-place migration of existing data), the operator HTTP/CLI
surface, the management Web UI, the edge client, and the test suites.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Give a client a human-friendly name (Priority: P1)

An operator enrolls or creates a client and gives it a readable label such as
`Acme Prod – East`, `edge_01.lab`, or `北京边缘节点`. The system accepts the name as a
display field, assigns the client a stable opaque identifier behind the scenes, and shows
the friendly name everywhere the client is listed while using the identifier for all
internal references.

**Why this priority**: This is the core user-visible payoff of the whole refactor —
removing the DNS-label straitjacket. Without it the rest of the change has no observable
value to operators.

**Independent Test**: Create a client with a name containing uppercase, a space, a dot, an
underscore, and non-Latin characters. Confirm creation succeeds, the client appears in the
list with that exact name, and it has a distinct system-assigned identifier.

**Acceptance Scenarios**:

1. **Given** an operator on the client-creation flow, **When** they submit a name with
   mixed case / spaces / dots / underscores / Unicode within the length limit, **Then** the
   client is created successfully and the name is stored and displayed verbatim.
2. **Given** an operator submits an empty name, a name exceeding the maximum length, or a
   name containing control characters, **When** they confirm, **Then** the system rejects it
   with a clear, specific validation message.
3. **Given** a newly created client, **When** the operator inspects it, **Then** it has a
   stable identifier distinct from its display name.

---

### User Story 2 - Identity survives a name change (Priority: P1)

An operator renames a client (e.g. a typo fix, a re-branding, an environment move). All of
the client's rules, issued token, rate-limit / quota settings, and accumulated traffic
history remain attached to the same client. A currently-connected client is not disconnected
or invalidated by the rename.

**Why this priority**: A stable identity that cannot be safely renamed delivers only half
the benefit. Rename-safety is the headline reason for introducing an opaque identifier and
is what operators will reach for first once names are free-form.

**Independent Test**: Create a client with rules, a quota, and some recorded traffic; rename
it; confirm every rule / quota / history row and the live connection still resolve to the
same client under the new name.

**Acceptance Scenarios**:

1. **Given** a client with rules, a quota, and traffic history, **When** the operator changes
   its display name, **Then** the identifier is unchanged and all associated records still
   belong to the client.
2. **Given** a connected client, **When** its display name is changed, **Then** its session
   stays alive and continues forwarding.
3. **Given** two clients, **When** an operator renames one to a name another already uses,
   **Then** the rename is accepted (names are not required to be unique).

---

### User Story 3 - Stable links and references (Priority: P2)

An operator navigates to a client's detail view, bookmarks it, and shares the link with a
teammate. The link continues to resolve to the same client even after the client is renamed.
Operator API/CLI calls that target a specific client likewise address it by its stable
identifier, so automation does not break when names change.

**Why this priority**: Stable addressing is a direct consequence of identity/label
separation and is needed for durable bookmarks, scripts, and integrations, but it is only
meaningful once Stories 1 and 2 exist.

**Independent Test**: Open a client detail page, copy the URL, rename the client, reload the
copied URL, and confirm it still shows the same client. Repeat for an operator API/CLI call
that targets the client.

**Acceptance Scenarios**:

1. **Given** a client detail link captured before a rename, **When** the link is reopened
   after the rename, **Then** it resolves to the same client.
2. **Given** an operator script that references a client by identifier, **When** the client
   is renamed, **Then** the script continues to work without modification.

---

### User Story 4 - Existing deployment upgrades cleanly (Priority: P1)

An operator upgrades an existing server that already has clients, rules, tokens, quotas, and
traffic history. After upgrade, every existing client has been assigned a stable identifier,
all existing records are correctly re-associated, and previously-enrolled edge clients keep
working without manual intervention.

**Why this priority**: This change rewrites the persistence layer's keys and the wire
contract. If an upgrade loses data or knocks deployed clients offline, the feature cannot
ship regardless of how nice the new naming is. It is a release gate.

**Independent Test**: Populate a database in the previous schema with multiple clients (each
with rules / quota / history), run the upgrade path, and verify all clients have identifiers,
no rows are orphaned, and an already-enrolled client reconnects successfully.

**Acceptance Scenarios**:

1. **Given** a database from the prior version with clients, rules, tokens, quotas, and
   traffic history, **When** the server starts on the new version, **Then** each client is
   assigned a stable identifier and every dependent record is re-associated to it with no
   data loss.
2. **Given** an edge client that was enrolled before the upgrade, **When** it connects to the
   upgraded server, **Then** it authenticates and forwards as before, per the compatibility
   decision recorded in this spec.
3. **Given** the upgrade has already run once, **When** the server restarts again, **Then**
   the migration does not run twice or corrupt data (it is idempotent).

---

### Edge Cases

- **Duplicate display names**: two clients are allowed to share a display name (identity is
  the identifier), so all human-facing listings must remain unambiguous (e.g. by surfacing a
  short form of the identifier alongside the name).
- **Rename to an invalid name**: rejected with the same validation rules as creation; the
  prior name is preserved unchanged.
- **Whitespace-only or control-character name**: rejected.
- **Name at the exact maximum length / one over**: boundary accepted / rejected respectively.
- **Old edge client with a pre-upgrade credential bundle** (no identifier in it): handled per
  the backward-compatibility decision in this spec (FR-007).
- **Partially-failed migration** (process killed mid-upgrade): on restart the upgrade resumes
  or re-runs safely without partial/duplicate state.
- **Identifier appearing in a URL or API call that does not exist**: returns a clear
  not-found result, not a server error.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST assign every client a stable, opaque, system-generated
  identifier at creation/enrollment time. The identifier MUST be immutable for the life of
  the client.
- **FR-002**: The system MUST use the client identifier (not the name) as the canonical key
  for: persisted records (tokens, rules, rate limits, quotas, traffic usage, enrollment
  records), in-memory connected-client tracking, operator API/CLI addressing, Web UI routing,
  and internal log/metric correlation.
- **FR-003**: The system MUST treat the client **name** as a free-form display field with
  relaxed validation: it MUST reject only empty/whitespace-only names, control characters,
  and names exceeding a defined maximum length; it MUST accept uppercase, spaces, dots,
  underscores, and non-Latin (Unicode) characters.
- **FR-004**: Operators MUST be able to change a client's display name without changing its
  identifier and without detaching any of its associated records or dropping its live
  session.
- **FR-005**: Upgrading an existing deployment MUST assign identifiers to all existing
  clients and re-associate every dependent record to the correct client, with no data loss.
  The existing token records are the source of truth for which clients exist.
- **FR-006**: The upgrade/migration MUST be idempotent and safe to interrupt and re-run on an
  already-deployed database.
- **FR-007**: Already-enrolled edge clients MUST continue to authenticate and forward traffic
  after the upgrade **transparently, with no re-enrollment**: a pre-upgrade credential bundle
  (carrying a token but no identifier) MUST still connect, because the authenticated token
  resolves to the client's newly-assigned identifier on the server. Re-enrollment is never
  required as a result of this upgrade.
- **FR-008**: The control-plane wire contract MUST convey the client identifier where client
  identity is exchanged (enrollment request/response, credential bundle, and any per-client
  operator update messages). The change MUST be **additive**: the identifier is added as a
  new field while the existing name field is retained for display, so that clients running the
  prior wire contract are not broken.
- **FR-009**: Operator-facing surfaces (API paths, CLI arguments, Web UI routes/links) MUST
  address a specific client by its identifier rather than its name.
- **FR-010**: All operator- and user-facing listings of clients MUST display the client name,
  and MUST remain unambiguous even when two clients share a name.
- **FR-011**: Validation failures for client names MUST return a clear, specific message
  indicating which rule was violated.
- **FR-012**: Requests that reference a non-existent client identifier MUST return a clear
  not-found result rather than an internal error.
- **FR-013**: Display-name uniqueness — the system MUST NOT require client names to be
  unique and MUST NOT warn on collision; identical display names are freely allowed because
  identity is provided solely by the identifier. Listings disambiguate duplicates by surfacing
  a short form of the identifier alongside the name.
- **FR-014**: Log and metric correlation MUST be consistent across a rename: a renamed client
  remains the same correlated entity. (See Assumptions for the chosen metric-label form.)

### Key Entities *(include if feature involves data)*

- **Client Identifier**: a stable, opaque, system-generated value created with the client and
  never changed. The canonical reference for the client across every layer.
- **Client (Name)**: a free-form, human-readable display label attached to a client. Mutable,
  not required to be unique, minimally validated.
- **Client**: an enrolled edge node, identified by its Client Identifier, carrying a display
  name, an authentication token, zero or more forwarding rules, optional rate-limit/quota
  settings, and accumulated traffic history — all keyed by the identifier.
- **Credential Bundle**: the material an edge client uses to authenticate and connect; after
  this change it conveys the client identifier (plus the name for display).

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: An operator can create a client whose name contains uppercase, spaces, dots,
  underscores, and non-Latin characters, and the name is stored and shown verbatim — a case
  that is impossible before this change.
- **SC-002**: After renaming a client, 100% of its rules, tokens, quota settings, and traffic
  history rows still resolve to the same client, and an active session is not interrupted.
- **SC-003**: A client detail link or an identifier-based operator command captured before a
  rename still resolves to the same client after the rename.
- **SC-004**: Upgrading a populated prior-version database results in every client having an
  identifier and zero orphaned dependent records (verified by a migration test over a seeded
  database).
- **SC-005**: An edge client enrolled before the upgrade reconnects and forwards traffic after
  the upgrade without manual reconfiguration (subject to the FR-007 decision).
- **SC-006**: Re-running the upgrade path on an already-migrated database produces no change
  and no error (idempotency verified by a test).
- **SC-007**: No regression in the existing test suites across the workspace; data-plane
  performance stays within the project's established perf gate.

## Assumptions

- **Identifier form**: an opaque, sortable, collision-resistant identifier consistent with
  the identifiers the project already uses for other entities (rule/request identifiers). The
  exact encoding is an implementation detail chosen at plan time.
- **Name length limit**: a maximum of 255 characters is assumed unless changed during
  clarification; the minimum is one non-whitespace character.
- **Name normalization**: names are stored as entered (no case-folding or Unicode
  normalization beyond rejecting control characters); display is verbatim.
- **Metric label**: client metric labels continue to use the human-readable **name** for
  dashboard readability, while internal correlation uses the identifier; a renamed client is
  still treated as one logical entity. (Open to revision in clarification.)
- **URL/addressing form**: operator API paths and Web UI routes use the identifier as the
  client path segment (replacing the name segment).
- **Wire compatibility default**: the preferred direction is **additive** — add the
  identifier to the wire contract while keeping the name field for display — so that running
  clients are not broken; confirmed via FR-008 clarification.
- **No new operator capability beyond rename**: aside from relaxed naming and rename, this
  feature adds no new client-management feature; it is an identity/label separation refactor.
- **Auth model unchanged**: TLS + bearer token remains the authentication model; this feature
  does not alter how clients authenticate, only how the authenticated identity is keyed.
- **Historical spec contract copies are not modified**: prior `specs/*/contracts/` snapshots
  are historical and remain untouched.
