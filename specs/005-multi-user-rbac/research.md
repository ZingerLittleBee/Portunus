# Research: Multi-User RBAC for the Forward Server

**Feature**: 005-multi-user-rbac
**Phase**: 0 (research, before design)
**Date**: 2026-05-07

This document resolves design unknowns surfaced during plan-level
analysis. Each item is a binary decision the implementer would
otherwise have to relitigate during `/speckit-tasks` or
`/speckit-implement`.

## R-001 — Persistent store for users/credentials/grants

**Question**: Should v0.5.0 introduce SQLite/Postgres for operator
identity, or stick with the JSON-file + atomic-write pattern proven by
`forward-auth::file_store::FileTokenStore`?

**Decision**: **Stick with JSON file**, single document
`identity.json`, same atomic-write protocol as `FileTokenStore`
(write tmp → fsync → rename → fsync(parent)). New file path is
`operator_store_path` in `server.toml`, defaulting to
`<config_dir>/identity.json`.

**Rationale**:

- Realistic operator-side data shape: tens of users, hundreds of grants.
  At this size, a single 10–100 KB JSON document loads in < 1 ms and
  writes in < 10 ms (dominated by fsync). Querying happens in-memory
  against a `HashMap<UserId, User>` and `Vec<Grant>` — there is no
  query workload SQL would help.
- Operational consistency. The codebase already runs one atomic-write
  JSON store (`FileTokenStore`); adding a SQL dependency would
  introduce a second backup/restore story, a second connection-pool
  tuning surface, and a second migration mechanism for one feature
  whose data shape doesn't justify it.
- Forward door is open. If a future feature (persistent audit log
  search, multi-host operator coordination, etc.) needs SQL, this
  decision does not block it: `OperatorAuthenticator` is a trait, the
  store impl is swappable. The constitution's `TODO(STORAGE_CHOICE)`
  remains deferred for the right reason — no driving requirement yet.

**Alternatives considered**:

- **SQLite via `rusqlite`**: rejected for v0.5.0. Adds a C dependency
  (constitution's musl-static-binary preference makes this awkward),
  introduces `?` parameter binding & migration tooling for one feature.
- **Embedded `sled` / `redb`**: rejected for v0.5.0. Same musl friction,
  smaller community, on-disk format more opaque to manual inspection
  during incident response.
- **Two separate files** (`users.json`, `grants.json`): rejected. Loses
  atomicity across user-removal-with-cascading-grant-revoke. One file,
  one transaction.

## R-002 — Bootstrap UX and the "one breaking change"

**Question**: The operator HTTP API is unauthenticated today (R-007).
Adding mandatory auth is technically a breaking change for any
operator workflow that talks to `/v1/*`. How do we minimize friction
while not introducing a silent unauthenticated mode?

**Decision**: **No silent unauthenticated mode in v0.5.0.** Two
explicit paths to obtain the first superadmin token:

1. **Config-file shortcut** (recommended for fresh deployments): set
   `operator_token = "<token>"` in `server.toml`. On first startup
   the server creates a built-in `_superadmin` user backed by that
   token if no superadmin exists in `identity.json`. Removing the
   key after first start does NOT revoke the token (it has been
   persisted to `identity.json`); rotating it requires the regular
   credential-rotate flow.
2. **One-shot CLI** (recommended for existing v0.4 deployments
   upgrading in place): `forward-server bootstrap-superadmin
   --name <human-name>`. Generates a fresh token, writes the user +
   credential to `identity.json`, prints the token to stdout
   exactly once. Refuses to run (exit code `2`,
   reason=`already_bootstrapped`) if any superadmin already exists.

A server with neither path used AND no entries in `identity.json`
**starts but rejects every operator request** with
`bootstrap_required` (HTTP 503). The data plane is unaffected — the
gRPC client channel uses its own auth (unchanged). Operators see a
clear log line at startup: `bootstrap required — see
docs/runbook.md#bootstrap`.

**Rationale**:

- A silent unauthenticated mode is exactly the security posture v0.5.0
  is meant to fix. Keeping it as a fallback would defeat Constitution
  V (multi-tenant isolation). Operators who deploy v0.5.0 must
  affirmatively opt into authentication.
- Two paths cover the two realistic operator profiles: greenfield
  (declarative config in `server.toml`) and brownfield (imperative
  CLI over an existing config). Both end with the same on-disk state
  (`identity.json` with one superadmin entry).
- Refusing repeat bootstrap (`already_bootstrapped`) prevents the
  failure mode where a forgotten config flag silently mints additional
  superadmin tokens.

**Alternatives considered**:

- **Implicit superadmin from absent config**: rejected. Same security
  posture as v0.4.0; would not satisfy SC-003's "100% of unauthorized
  requests rejected" claim.
- **Separate `bootstrap` daemon**: rejected. Out of proportion with
  the data shape; also creates a second long-lived process operators
  must remember exists.

## R-003 — Owner stamp on Rule

**Question**: Where does the `owner_user_id` live, and how does it
survive (or not) operations like restart and grant revocation?

**Decision**: `Rule` gains a non-optional `owner_user_id: UserId`
field. The field is **in-memory only** (rules are not persisted —
verified by reading `crates/forward-server/src/rules.rs` doc comment).
On a server restart, both rules and their owner stamps are gone;
operators re-push rules under their own identity, and the new push
re-stamps owner.

For superadmin-pushed rules, owner is the well-known
`UserId("_superadmin")` value (or the legacy `_superadmin_legacy`
created by the bootstrap shortcut in R-002 — same role, distinct
identifier so audit logs can distinguish bootstrap-era pushes).

**Rationale**:

- v0.5.0 inherits the rules.rs design: rules are runtime state, not
  persistent state. Adding persistence here would balloon the feature
  scope. The owner stamp follows the same lifetime as the rule it's
  on.
- Restart "loses" rules but operators re-push them. Re-stamping owner
  on re-push is the obvious behavior; no migration logic needed.

**Alternatives considered**:

- **Persist rules with their owner in a new `rules.json`**: rejected
  for v0.5.0. Out of scope; the v0.4.0 rules.rs comment explicitly
  defers rule persistence as future work. v0.5.0 should not be the
  feature that conflates two persistence migrations.
- **Optional owner field**: rejected. A None owner is ambiguous
  (legacy? superadmin? bug?) and complicates rbac code. Required +
  well-known sentinel for superadmin is simpler.

## R-004 — Grant matching semantics: closed-set vs union-cover

**Question**: A user has two grants on `client-a`: one for ports
30000..30010 / `[tcp]`, another for ports 30011..30020 / `[tcp]`.
Should a push of a TCP range rule on listen ports 30005..30015 be
allowed (union covers it) or rejected (no single grant covers it)?

**Decision**: **Closed-set**: a push is authorized iff exactly one
single grant covers all dimensions of the request. Range rules whose
listen range straddles two grants are rejected with
`port_outside_grant`, even if the union would cover them.

**Rationale**:

- Operator predictability. "Build me a grant that covers this rule"
  is a single mental operation; "build me a set of grants whose union
  covers this rule" requires a fixpoint search and surprises operators
  when adding a third grant invalidates previous rules.
- Grant intent is usually "this user owns this whole region". When
  they want a contiguous capability, they should express it as a
  contiguous grant. Splitting into two grants implies two distinct
  intents (e.g., two different services) — letting the user mint a
  rule that sits in both is a leak of intent.
- Symmetric with how port ranges work elsewhere in the codebase
  (v0.2.0 range rules require the whole range to be unallocated; no
  partial coverage). Keeps the mental model consistent.

**Alternatives considered**:

- **Union-cover**: rejected for the reasons above. Would also require
  the rbac function to compute set-cover, which is more code than the
  trivial closed-set predicate.
- **Hybrid (single-port rules union-cover, range rules closed-set)**:
  rejected. Adds branch logic for negligible UX gain.

## R-005 — Cardinality of the new `owner` Prometheus label

**Question**: Adding an `owner` label to the existing per-rule
collectors — does this multiply rows?

**Decision**: **No**. Each rule has exactly one owner, and the
existing per-rule cardinality is "one row per rule per collector". The
label `owner` simply joins `{client, rule}` to make
`{client, rule, owner}` — same row count.

**Rationale**: Prometheus cardinality is the product of label
*cardinalities*, but in practice it's the product of *observed
combinations*. Because owner is functionally determined by rule (one
owner per rule), the observed combinations are still one per rule.

**Validation**: `forward-server/tests/rbac_metric_cardinality.rs` (a
tasks-phase artifact) will assert the row count of
`forward_rule_bytes_in_total` after pushing N rules from M users
equals N (not N×M).

## R-006 — Grant-revoke and user-remove side effects on active rules

**Question**: When a grant is revoked or a user removed, what
happens to rules whose authorization depended on the revoked
capability?

**Decision**:

- **Grant revoke**: server-side scan of the in-memory rules table.
  For each rule owned by the affected user, recompute authorization
  against the user's *remaining* grants. Rules that no longer have
  any covering grant are removed (state transitions to `Removed` via
  the existing remove-rule code path; same audit semantics as
  operator-initiated remove). Rules still covered by another grant
  the user holds continue running.
- **User remove**: every rule owned by that user is removed
  unconditionally; every credential they hold is invalidated; every
  grant they hold is revoked. The whole transaction is committed to
  `identity.json` once before any rule removal lands, so a crash
  mid-cascade leaves the auth state consistent (rule-removal is
  in-memory and idempotent — replaying the cascade after restart is
  a no-op because the rules are already gone with the restart).

**Time bound**: SC-005 demands all dependent rules removed within 5 s.
The scan is O(rules) which is bounded by O(1 k) at our target scale,
so the bound is comfortable. The operator HTTP response for the
revoke / remove call returns *after* the scan completes (synchronous
cascade), so SC-005 is measured from the response timestamp, not from
some background task.

**Rationale**:

- Fail-closed posture. A revoked grant should not leave residual
  capability.
- Synchronous cascade gives operators a deterministic outcome
  ("revoke succeeded → no rules remain that depended on it"). A
  background reaper would be more code and a less observable contract.

**Alternatives considered**:

- **Leave rules running, block new pushes only**: rejected, fail-open.
  Documented in spec assumption.
- **Background reaper**: rejected for the deterministic-outcome
  reason above.

## R-007 — Operator API auth: wired today vs introduced now

**Question**: The plan claims "the operator HTTP API is
unauthenticated today" — is that strictly true? What is the actual
v0.4.0 baseline?

**Decision**: After grepping the v0.4.0 source
(`crates/forward-server/src/operator/{http.rs,cli.rs}` and
`crates/forward-server/src/serve.rs`), the answer is **yes,
unauthenticated**. The router has no `tower::Layer` for auth; the
listener binds to whatever address `server.toml` says (loopback by
default per the v0.4 example config).

**Implication**: spec FR-006 / SC-004 wording about "preserving v0.4.0
operator workflows byte-identically" is honored at the wire-shape
level (request body, response body, status codes, exit codes are
unchanged for the success path; new fields are additive); but every
operator request in v0.5.0 must add an `Authorization: Bearer <token>`
header, which is the unavoidable new requirement of the feature. This
is an additive HTTP header, not a wire-shape change. The v0.4 quickstart
walkthrough will be updated to include the `bootstrap-superadmin` step
+ `--token` plumbing; existing v0.4 e2e fixtures get a
`bootstrap_then_token()` helper added in `forward-e2e/tests/common/mod.rs`.

This is captured in the SC-004 wording in the spec ("byte-identical
request/response wire shapes") which does NOT promise that no header
is added. The spec is internally consistent; only the FR-006
"legacy operator bearer token" phrasing was aspirational. **Tasks
phase will sharpen FR-006 wording during the analysis pass** to
say "the `operator_token` configured in `server.toml` (R-002 path 1)
becomes the built-in superadmin identity".

## R-008 — Audit log shape

**Question**: What exactly does an audit log line contain, and how
do we guarantee tokens never appear in it?

**Decision**: One structured log line per allow/deny decision via the
existing `tracing` macros. Fields:

```text
event       = "operator.allow" | "operator.deny"
ts          = ISO-8601 UTC
actor       = user_id (or "_anonymous" for pre-auth failures)
role        = "superadmin" | "user" | "_anonymous"
action      = "push_rule" | "remove_rule" | "list_rules" | "rule_stats" |
              "user_add" | "user_remove" | "credential_issue" |
              "credential_rotate" | "credential_revoke" | "credential_list" |
              "grant_add" | "grant_revoke" | "grant_list" | "bootstrap"
resource    = string identifier (rule_id, user_id, grant_id, ...)
outcome     = "allow" | "deny"
reason      = (deny only) one of FR-008's enumerated codes
```

Token-redaction guarantee: `audit_log_redaction.rs` test loads the
captured stream, generates a fresh credential, runs a credential-issue
+ rotate flow, and asserts the literal token string never appears in
any captured log record. Implementation invariant: the audit emitter
takes `&OperatorIdentity` and `&str` action labels — never a raw
credential. Tokens are produced by `forward-auth::token::generate_token`
and returned to the HTTP response body only; they never traverse the
audit code path.

**Rationale**: Operators run `journalctl` / `kubectl logs` against
this output; a token leaking into INFO/WARN would be an immediate
incident. Cheaper to design out than to patch out.

## R-009 — SIGHUP reload of identity.json

**Question**: Constitution IV requires graceful config reload. Does
the operator store need a SIGHUP handler?

**Decision**: **Yes, but limited**. SIGHUP triggers a re-read of
`identity.json` from disk. The reload swaps the in-memory snapshot
under `FileOperatorStore`'s `RwLock`. In-flight HTTP requests
complete against the snapshot they entered with (no mid-request
re-evaluation). New requests after the swap see the new state.

**Out of scope** for v0.5.0:

- Cascading rule-removal on reload-detected grant revoke: writes via
  the CLI are the supported path for revocation; out-of-band edits to
  `identity.json` are NOT a supported workflow (and we add a doc note
  in `runbook.md` saying so).
- Multi-process file-watch coordination: there is exactly one
  forward-server process per host (Constitution Tech Constraints).

**Rationale**: Constitution IV's "graceful reload" intent is honored
at minimum cost. The full cascade-on-reload story would require
journaling the previous snapshot and diffing — out of proportion
with the operator workflow we actually want to support (use the CLI,
not file edits).

## R-010 — Conflict between two grants of the same user

**Question**: A superadmin tries to add two grants for `alice` that
overlap on `(client, port_range, protocol)`. Allowed or rejected?

**Decision**: **Allowed**. Overlapping grants are a no-op for
authorization (the union semantics are bounded by R-004's closed-set
rule, so overlap doesn't broaden capability). Operator may want to
hold two grants for distinct organizational reasons (one labeled
"prod", one labeled "staging"). The CLI / HTTP grant-add path does
NOT detect or warn on overlap.

**Rationale**: Cheaper to accept than to detect. The `Grant` entity
has an optional human-readable `note` field for the operator's own
bookkeeping.

**Alternatives considered**:

- **Reject overlap**: rejected. Forces operators to merge or split
  before adding, with no security gain (grants are additive
  capabilities, not exclusive).
- **Warn on overlap**: rejected. Operator-CLI warnings without an
  enforcement surface tend to be ignored.

## R-011 — Race: concurrent grant-revoke and rule-push

**Question**: Operator-A revokes a grant for `alice`; concurrently,
`alice` pushes a rule that depended on that grant. Who wins?

**Decision**: **Last-writer-wins on `identity.json`, with the
push-rule path re-checking authorization under the lock**. The
sequence:

1. `alice`'s push handler enters `rules.rs::push_rule`, takes the
   rules `RwLock` write lock.
2. Inside the lock, `rbac::enforce_push(&alice_identity, &rule)`
   calls `OperatorAuthenticator::grants_for(&alice)`, which hits
   the `FileOperatorStore`'s read lock. If operator-A's revoke
   already committed, the read lock will see the empty grant set
   and `enforce_push` returns `port_outside_grant` (or whichever
   reason fits). The push fails. Operator-A's revoke wins.
3. If operator-A's revoke is mid-write (holds the FileOperatorStore
   write lock), `alice`'s read blocks until the revoke commits,
   then sees the post-revoke state. Same outcome: operator-A wins.
4. If `alice`'s push commits before operator-A's revoke, the
   subsequent revoke's cascade scan (R-006) finds the new rule and
   removes it within the same revoke transaction. Operator-A still
   wins, just one step later.

**Rationale**: Tortoise-and-hare race resolution: whoever the
in-memory snapshot serializes second wins, and the revoke cascade
ensures consistency even under tight interleaving. No distributed
locking, no two-phase commit.

## R-012 — Test fixture cost: bootstrapping every e2e test

**Question**: Every e2e test now has to bootstrap a superadmin and
issue per-test users. Does this multiply test runtime?

**Decision**: **Negligible**. `bootstrap-superadmin` + `user-add`
+ `credential-issue` + `grant-add` together touch `identity.json` 4
times. At 10 ms per write, 40 ms per test. The full `forward-e2e`
suite has ~30 tests today; +1.2 s total wall-clock is acceptable.

A shared fixture (`spawn_server_with_bootstrapped_superadmin()`) in
`forward-e2e/tests/common/mod.rs` amortizes this for any test that
just needs "be a superadmin"; tests that exercise non-superadmin
behavior pay the per-user-add cost individually (each adds at most
one extra write).
