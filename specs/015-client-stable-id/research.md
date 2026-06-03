# Phase 0 Research: Client Stable Identifier

All spec-level NEEDS CLARIFICATION were resolved with the operator before planning
(2026-06-02). This document records the technical decisions that flow from those answers
plus the codebase facts established by direct inspection.

## R-001: Identifier type and encoding

- **Decision**: `ClientId(Ulid)` newtype in `portunus-core/src/id.rs`, alongside the existing
  `RuleId`/`RequestId`. ULID, lowercase-Crockford-base32 26-char canonical string on the
  wire and in URLs.
- **Rationale**: The workspace already depends on `ulid` and uses it for `RequestId`. ULIDs
  are 128-bit, lexicographically sortable (creation-ordered), URL-safe, and collision-
  resistant — ideal for an opaque, stable key that also yields a natural default sort for
  client listings. `Copy` semantics make it cheaper than a `String` name as a map key.
- **Alternatives considered**: UUIDv4 (no time ordering, extra dep), monotonic integer (leaks
  count, not opaque), hashing the name (not stable across rename — defeats the purpose).

## R-002: Migration mechanism — refinery, not a hand-rolled table

- **Decision**: Add `crates/portunus-server/src/store/migrations/V011__client_id.sql`. No new
  migration runner; refinery picks it up automatically.
- **Rationale**: Inspection shows migrations are **refinery-embedded**
  (`refinery::embed_migrations!("src/store/migrations")` in `store/mod.rs:56`; runner at
  `mod.rs:171/185`). The handoff dossier's reference to a manual `schema_migrations` table is
  inaccurate — refinery owns its own `refinery_schema_history` table and applies `V###__*.sql`
  in order. Current head is `V010`.
- **Alternatives considered**: a Rust-code data migration — rejected; SQL-in-refinery is the
  established pattern and keeps the migration declarative and idempotent-by-construction
  (refinery records applied versions).

## R-003: SQLite primary-key change strategy

- **Decision**: For each re-keyed table, use the **create-new-table → copy → drop-old →
  rename** sequence inside `V011`, all within the single migration transaction refinery wraps.
  Order: (1) add `client_tokens.client_id` and backfill a fresh ULID per existing row;
  (2) rebuild dependent tables with a `client_id` column/PK/FK, populating it by joining back
  to `client_tokens` on `client_name`; (3) keep `client_name` as an ordinary (now
  non-unique-capable) column where still useful for display.
- **Rationale**: SQLite's `ALTER TABLE` cannot change a primary key or add a foreign key in
  place; the table-rebuild dance is the standard, documented SQLite approach. `client_tokens`
  is the authoritative roster of clients, so it anchors id assignment; every other table is a
  dependent that joins by name.
- **Idempotency / crash-safety**: refinery applies a migration in a transaction and only
  records the version on success, so an interrupted run rolls back and re-runs cleanly. The
  SQL must therefore be safe to run from a clean `V010` state exactly once; we rely on
  refinery's version gate rather than `IF NOT EXISTS` guesswork (SC-006 verified by a test
  that runs the runner twice).
- **ULID generation in SQL**: SQLite has no native ULID function. Decision: generate the ids
  in Rust at migration time via a refinery **runner hook / pre-step** that assigns ids to
  existing `client_tokens` rows before the SQL backfill, OR ship them as part of a Rust-side
  migration step. (Resolved at task time; the constraint is only that ids are real ULIDs, not
  `rowid` casts.) The seven tables to re-key: `client_tokens`, `rules`, `rate_limit_owner`,
  `traffic_quotas`, `traffic_usage_minute`, `traffic_usage_hour`, `client_enrollments`.

## R-004: Wire compatibility — additive

- **Decision**: Add a new `string client_id` field to `EnrollClientRequest`'s response path
  via `CredentialBundle` (new field, e.g. `client_id = 7`), and to `OwnerRateLimitUpdate`
  (new field) and `TrafficQuotaUpdate` (new field). Retain every existing `client_name` field
  for display. `Hello`/`Welcome` are unchanged.
- **Rationale**: Protobuf field additions are backward/forward compatible — old clients ignore
  the new field; new servers tolerate its absence. Inspection confirms `Hello`/`Welcome`
  carry **no** client name (`proto/portunus.proto` lines 98–122); the server resolves the
  connecting client's identity from the bearer token (`grpc/service.rs` uses
  `identity.client_name` derived from `token_store.verify` → `ClientIdentity`). Therefore the
  data-plane stream needs no schema change at all for transparent legacy connectivity.
- **Alternatives considered**: replacing `client_name` with `client_id` — rejected; breaks
  running clients and the dossier's preferred direction, and gains nothing since name is still
  needed for display.

## R-005: Transparent upgrade for already-enrolled clients

- **Decision**: No re-enrollment. After `V011` assigns each client a `ClientId`, the token
  store resolves an authenticated token to that client's `ClientId`. A pre-upgrade bundle
  (token, no `client_id`) connects exactly as before.
- **Rationale**: Identity on the data-plane stream is token-derived, not name/id-derived on
  the wire (R-004). The only requirement is that `token_store.verify` returns a
  `ClientIdentity` carrying the resolved `client_id`. This is a server-internal change.
- **Validation**: e2e test — enroll under the `V010` code path (or seed a `V010` DB), upgrade,
  reconnect with the old bundle, assert traffic flows (SC-005).

## R-006: `ClientName` relaxed validation rules

- **Decision**: `ClientName::new` rejects only: empty/whitespace-only, any Unicode control
  character (`char::is_control`), and length > 255 **bytes**. It accepts uppercase, spaces,
  dots, underscores, hyphens (including leading/trailing/consecutive), and non-Latin Unicode.
  Stored verbatim; no case-folding or NFC normalization.
- **Rationale**: Matches FR-003 and the operator decision. Byte-length cap (255) keeps storage
  and display bounded while comfortably allowing multibyte names. Control-character rejection
  prevents log/terminal injection and display corruption (Observability hygiene).
- **Compatibility note**: `CredentialBundle.client_name` in `portunus-client/src/bundle.rs:16`
  is typed `ClientName`, so relaxing the validator automatically relaxes bundle parsing — no
  separate change. The existing strict-shape unit tests in `id.rs` (`rejects_bad_shapes`,
  `accepts_dns_label_shapes`, etc.) MUST be rewritten to the new contract.
- **Alternatives considered**: char-count vs byte-count cap — byte cap chosen for storage
  predictability; allowing control chars — rejected for safety.

## R-007: Metric label form

- **Decision**: Keep the Prometheus `client` label as the human-readable **name**; correlate
  internally by `ClientId`. A rename updates the label value but is understood as the same
  logical client because internal series are keyed by id.
- **Rationale**: Operators read dashboards by name; opaque ULIDs hurt readability. Inspection
  shows `metrics.rs` carries `client_name: ClientName` on its handles (lines 39, 594) and
  documents `(client, rule)` / `(client, rule, owner)` label tuples — keeping name as the
  label value is the least-surprising change. Cardinality is unchanged.
- **Trade-off accepted**: a rename produces a label-value change (a new series) in Prometheus;
  this is acceptable and documented (rename is infrequent, operator-initiated).

## R-008: Operator surface (HTTP / CLI / Web UI) addressing

- **Decision**: Address a specific client by `ClientId`. HTTP: `/v1/clients/{id}/...`
  (the `owner.rs` comments already anticipate `/v1/clients/{id}/...`). CLI owner/quota/rule
  subcommands take a client id argument. Web UI route becomes `/clients/:clientId`
  (`webui/src/App.tsx:231` currently `/clients/:clientName`). A new rename affordance (HTTP
  `PATCH`/`PUT` on the client + a Web UI control) sets the display name.
- **Rationale**: Stable addressing (US3) requires id-based paths so links/scripts survive
  rename. Listings still render the name; duplicates disambiguate with a short id prefix.
- **Note**: `webui/src/components/UserCreateForm.tsx:44`'s `^[a-z][a-z0-9-_]*$` regex is for
  **userId**, not client name — must NOT be touched by this feature.

## R-009: Performance gate

- **Decision**: Run the existing `cargo bench -p portunus-client --bench data_plane` to
  confirm flatness; no new benchmark authored.
- **Rationale**: The forwarding hot path and `portunus-forwarder` are untouched. The only
  runtime-path change is `ConnectedClients` keying (`ClientName` → `ClientId`), which is on
  the control-plane session lifecycle, not the per-packet path, and is neutral-to-favorable.

## Resolved unknowns summary

| Topic | Resolution |
|-------|------------|
| Identifier | `ClientId(Ulid)` newtype |
| Migration | refinery `V011__client_id.sql` (+ Rust-side ULID assignment) |
| PK change | table-rebuild dance, `client_tokens` as source of truth |
| Wire | additive `client_id`; `Hello`/`Welcome` unchanged |
| Legacy clients | transparent, token-resolved, no re-enroll |
| Name rules | non-empty, no control chars, ≤255 bytes, Unicode OK, verbatim |
| Metric label | name (display); id (internal correlation) |
| Addressing | id in HTTP/CLI/UI paths; rename endpoint added |
| Perf | existing data-plane bench, expected flat |
