# Data Model: Multi-User RBAC for the Forward Server

**Feature**: 005-multi-user-rbac
**Phase**: 1 (design)
**Date**: 2026-05-07

This document defines the entities introduced by feature 005, their
in-memory representation in `forward-auth`, their on-disk shape in
`identity.json` (see `contracts/persistence.md` for the wire-level
JSON schema), and the state transitions an operator can drive.

The data model intentionally mirrors `forward-auth::file_store`'s
existing `TokenRecord` shape so the new code feels like a sibling, not
a rewrite.

## Entities

### User

Represents an operator-side identity that can hold credentials and
grants.

```rust,ignore
pub struct User {
    pub id: UserId,            // newtype around String, e.g. "alice", "_superadmin"
    pub display_name: String,  // human-readable label, free text, ≤ 64 chars
    pub role: OperatorRole,
    pub created_at: DateTime<Utc>,
    pub disabled: bool,        // soft-disable; auth fails with reason="user_disabled"
}

pub enum OperatorRole {
    Superadmin,                // unrestricted; bypass all grant checks
    User,                      // restricted to their grants
    // Future roles (e.g., ReadOnly) extend this enum without breaking
    // existing role assignments. Match arms in rbac.rs use exhaustive
    // matching to fail-loudly on unhandled variants.
}
```

**Validation rules** (enforced at the `user-add` boundary):

- `id` matches the regex `^[a-z][a-z0-9_-]{0,31}$` (lowercase ASCII,
  starts with a letter, ≤ 32 chars). Reserved IDs starting with `_`
  are rejected for user-issued additions; they are reserved for
  built-in identities (`_superadmin`, `_superadmin_legacy`).
- `display_name` is UTF-8, ≤ 64 chars, no leading/trailing whitespace,
  no control characters.
- `role` defaults to `User` when omitted.

**State transitions**:

- `add` → User exists, `disabled = false`, no credentials, no grants.
- `disable` → `disabled = true`. Authentication for this user's
  credentials fails with `user_disabled`. Existing rules continue
  running until a superadmin removes them or the user is fully
  removed. *(Out of scope for v0.5.0; we expose only `add` /
  `remove` in the CLI but the field exists in the schema for
  future use.)*
- `remove` → cascade per R-006: revoke all credentials, remove all
  grants, remove all rules owned by this user. The cascade is
  committed to `identity.json` before the rule removals run.

**Relationships**:

- 1 → N `Credential` (each credential belongs to exactly one user)
- 1 → N `Grant` (each grant belongs to exactly one user)
- 1 → N `Rule` (each rule has exactly one owner)

### Credential

A bearer token that authenticates a user.

```rust,ignore
pub struct Credential {
    pub id: CredentialId,       // newtype around UUID v4, opaque to operators
    pub user_id: UserId,
    pub token_hash: [u8; 32],   // blake3 of the raw token; never reversed
    pub label: Option<String>,  // free-text annotation, ≤ 64 chars
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub status: CredentialStatus,
}

pub enum CredentialStatus {
    Active,
    Revoked { revoked_at: DateTime<Utc> },
}
```

**Wire format of the raw token**: 256-bit `OsRng` random,
URL-safe base64, no padding, 43 ASCII chars. **Identical** to the
existing client-token format from `forward-auth::token`. The same
`generate_token` / `hash_token` functions are reused — no new
cryptographic primitive.

**Validation rules**:

- `label` is optional UTF-8, ≤ 64 chars.
- `last_used_at` updates on each successful authentication. The update
  is best-effort (no fsync per request); persists piggy-backed on the
  next user/grant write or on a periodic flush (1 minute interval).
  Acceptable lossiness because `last_used_at` is an audit hint, not a
  security control.

**State transitions**:

- `issue` → Active, `last_used_at = None`.
- `verify` (on each request) → updates `last_used_at`. No state change.
- `rotate` → atomic transaction: insert new Active credential for the
  same user, mark old credential Revoked. Both writes commit in one
  `identity.json` snapshot.
- `revoke` → status transitions to Revoked. Verification of the token
  fails with `credential_invalid`. Revoked records are kept in the
  store for audit (mirrors `FileTokenStore`'s revoked-record
  retention).

**Relationships**: N → 1 `User`.

**Storage invariant**: the raw token value is never persisted. Only
the blake3 hash is stored. Verification = blake3 the presented token,
constant-time compare against `token_hash`.

### Grant

A capability that the superadmin attaches to a user, bounding what
that user can push.

```rust,ignore
pub struct Grant {
    pub id: GrantId,                  // UUID v4
    pub user_id: UserId,
    pub client: ClientScope,
    pub listen_ports: PortRange,      // start..=end, start ≤ end, both u16
    pub protocols: ProtocolSet,       // non-empty subset of {TCP, UDP}
    pub note: Option<String>,         // operator's bookkeeping label, ≤ 128 chars
    pub created_at: DateTime<Utc>,
}

pub enum ClientScope {
    Any,                              // matches every client name
    Named(ClientName),                // matches one specific client
}

pub struct PortRange {
    pub start: u16,                   // inclusive
    pub end: u16,                     // inclusive; start ≤ end
}

#[bitflags]
pub enum ProtocolSet { TCP = 1, UDP = 2 }   // non-empty enforced at construct
```

**Validation rules** (enforced at `grant-add`):

- `listen_ports.start ≤ listen_ports.end`, both in `1..=65535`. Port
  `0` is rejected (kernel reserves it for ephemeral assignment).
- `protocols` MUST be non-empty. An empty set is rejected with
  `empty_protocol_set` (this is stricter than the spec's edge-case
  note about `protocols: []` being a "no-op grant" — closing the door
  is simpler than documenting the no-op).
- `client.Named(name)` does NOT have to refer to a currently-connected
  client (per FR-011). Names are validated against the same regex as
  `ClientName` elsewhere in the codebase.

**State transitions**:

- `add` → Grant exists.
- `revoke` → Grant removed from the store; cascade per R-006: for each
  rule owned by `user_id`, recompute coverage; remove any rule that
  no longer has a covering grant. The cascade is synchronous; the
  revoke HTTP response returns after the cascade completes.
- (no `update`) — to change a grant, revoke and add. This avoids
  partial-update races and keeps the audit log clean (one `grant_add`
  + one `grant_revoke` is easier to reason about than one `grant_update`
  with a diff).

**Authorization predicate** (used by `rbac::enforce_push`):

```text
A grant G covers a push request P iff:
  G.client matches P.client (Any matches everything; Named(n) matches iff n == P.client)
  AND G.listen_ports.start ≤ P.listen_port_start
  AND P.listen_port_end ≤ G.listen_ports.end
  AND P.protocol ∈ G.protocols

A user U is authorized to push P iff:
  U.role == Superadmin
  OR ∃ G ∈ grants_of(U) such that G covers P.
```

This is the closed-set semantic from R-004: the entire range
`P.listen_port_start..=P.listen_port_end` must fit inside ONE grant's
`listen_ports`. Union-cover is explicitly NOT used.

**Relationships**: N → 1 `User`.

### Rule (extension of v0.4.0 entity)

The existing `Rule` struct in `crates/forward-server/src/rules.rs`
gains one field:

```rust,ignore
pub struct Rule {
    // ... existing v0.4.0 fields (id, client_name, listen_port,
    // listen_port_end, target_host, target_port, target_port_end,
    // protocol, prefer_ipv6, state, created_at, ...) ...

    /// Owner user id. Stamped at rule creation. Required (no Option)
    /// because every rule has an owner — superadmin-pushed rules
    /// are stamped with `UserId("_superadmin")`.
    pub owner_user_id: UserId,
}
```

**Lifetime invariant**: a Rule is created via `push_rule`. The owner
is taken from the `OperatorIdentity` extracted by the auth middleware
(see `contracts/operator-api.md` § Auth). It is never mutated
afterward. On rule removal (operator-initiated, grant-revoke cascade,
or user-remove cascade), the rule is dropped wholesale; the owner
field disappears with it.

**Persistence**: in-memory only, per R-003. Restart loses both the
rule and the owner stamp.

### AuthDecision (audit-log payload, transient)

Not a persisted entity — the shape of one structured log line emitted
by the auth middleware on every operator request. Defined here so the
test suite can pin the shape.

```rust,ignore
struct AuthDecision<'a> {
    pub event: &'static str,             // "operator.allow" | "operator.deny"
    pub ts: DateTime<Utc>,
    pub actor: &'a UserId,                // or "_anonymous"
    pub role: OperatorRole,               // or sentinel for _anonymous
    pub action: &'static str,             // see R-008 for the closed set
    pub resource: Option<String>,         // rule_id / user_id / grant_id / None
    pub outcome: &'static str,            // "allow" | "deny"
    pub reason: Option<&'static str>,     // FR-008 enumerated codes; None on allow
}
```

**Wire format**: serialized by `tracing_subscriber::fmt::json()` (the
existing JSON layer). The keys above become top-level JSON fields.

## Storage layout (identity.json)

Single document, atomic-write. Schema-versioned for forward-compat.

```json
{
  "version": 1,
  "users": [
    { "id": "_superadmin", "display_name": "Built-in superadmin",
      "role": "superadmin", "created_at": "2026-05-07T10:00:00Z",
      "disabled": false }
  ],
  "credentials": [
    { "id": "f7c6...", "user_id": "_superadmin",
      "token_hash": "<64 hex chars>", "label": "bootstrap",
      "created_at": "2026-05-07T10:00:00Z",
      "last_used_at": "2026-05-07T10:05:32Z",
      "status": "active" }
  ],
  "grants": []
}
```

A revoked credential is represented with:
`"status": { "revoked": { "revoked_at": "..." } }` (untagged-enum
serialization mirrors `RuleState` from v0.4 for consistency).

**Schema migration**: a future feature that bumps `version` reads the
old shape, transforms in-memory, writes back at the new version. The
loader refuses unknown versions with `unsupported_schema_version`
(forward-compat default). v0.5.0 ships at version 1.

**Atomic-write protocol** (verbatim from `forward-auth::file_store`):

1. Serialize the in-memory snapshot to bytes.
2. Open `identity.json.tmp` with `O_CREAT | O_TRUNC | O_WRONLY` in
   the same directory as the target.
3. Write all bytes; `fsync()` the file.
4. `rename()` `identity.json.tmp` → `identity.json`.
5. `fsync()` the parent directory.

Any failure prior to step 4 leaves `identity.json` untouched.
A failure between step 4 and step 5 is recoverable by the next write
(the rename is durable on most filesystems even without the parent
fsync; the parent fsync is the belt-and-suspenders against power-loss
on ext4 + data=writeback).

## Mapping to existing v0.4.0 types

| New type | Reuses |
|---|---|
| `UserId` | newtype `String` — same shape as `forward_core::ClientName` |
| `Credential.token_hash` | `forward_auth::token::hash_token` |
| `Credential` raw token | `forward_auth::token::generate_token` |
| `Grant.client` | `forward_core::ClientName` (when `Named`) |
| `Grant.listen_ports` | `forward_core::PortRange` (existing) |
| `OperatorRole` enum | new — no equivalent in v0.4 |
| `Rule.owner_user_id` | new field on existing struct |
| `FileOperatorStore` write protocol | mirrors `forward_auth::file_store::FileTokenStore` |

The existing `FileTokenStore` (client→server tokens) remains
**untouched**. The new `FileOperatorStore` (operator→server tokens
+ users + grants) is a sibling. Two separate files, two separate
locks, zero shared state. This is intentional: the two concerns have
different access patterns (client store is read-only after
provisioning; operator store sees frequent grant churn) and
different blast radii (a corrupted operator store doesn't break
forwarding).

## Cardinality bounds

| Entity | Expected count | Hard ceiling |
|---|---|---|
| User | O(10) | 1 000 (above this, persistence-write latency starts to matter) |
| Credential per user | O(1–3) | 100 (intentional, supports rotation history) |
| Grant per user | O(1–10) | 1 000 |
| Rule per user (existing v0.4 ceiling) | O(100) | unbounded by this feature |

Reaching the hard ceilings makes `identity.json` ~1 MB, which is fine.
Beyond that, the JSON-file pattern starts to hit fsync-latency
ceilings and a future feature should revisit R-001.
