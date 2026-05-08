# Phase 1 Data Model — 008 Unified SQLite Store

The on-disk DDL semantics for `<data-dir>/state.db`. Tables map 1:1 to
the spec's "Key Entities" list. SQL fragments are illustrative — the
authoritative form lives in `crates/forward-server/src/store/migrations/V001__initial_schema.sql`.

All `*_at` columns are stored as ISO-8601 UTC strings (`TEXT`) to match
`chrono::DateTime<Utc>` round-trip behaviour already used by
`forward-auth`. ULIDs serialise as their canonical 26-char base32 form.

---

## ER overview

```
                                          ┌───────────────────┐
                                          │ schema_migrations │
                                          └───────────────────┘
                                                   (meta)

      ┌───────┐   1..N   ┌─────────────┐
      │ users │──────────│ credentials │
      └───┬───┘          └─────────────┘
          │ 1
          │
          │ 0..N         ┌─────────┐
          ├──────────────│ grants  │
          │              └─────────┘
          │
          │ 1            ┌────────┐  1..N   ┌──────────────┐
          └──────────────│ rules  │─────────│ rule_targets │
                         └────────┘         └──────────────┘
                              ▲ owner_user_id

      ┌───────────────┐                         ┌──────────┐
      │ client_tokens │  (independent of users) │  audit   │
      └───────────────┘                         └──────────┘
                                                  (append-only)
```

`audit` rows reference users by id (loose FK; on user delete the audit
row keeps a bare `user_id` string for forensic continuity — explicit
non-cascading semantic).

---

## Tables

### `schema_migrations`

```sql
CREATE TABLE schema_migrations (
    version       INTEGER PRIMARY KEY,
    name          TEXT    NOT NULL,
    applied_on    TEXT    NOT NULL,             -- RFC3339 UTC
    checksum      TEXT    NOT NULL              -- refinery payload digest
);
```

Owned by `refinery`. Read at startup to decide which forward
migrations to run; backup artefacts inherit it via the Online Backup
copy. FR-012 schema-version handshake reads
`SELECT MAX(version) FROM schema_migrations`.

---

### `users`

```sql
CREATE TABLE users (
    user_id        TEXT    PRIMARY KEY,         -- regex ^[a-z][a-z0-9_-]{0,31}$ enforced in app layer
    role           TEXT    NOT NULL,            -- 'superadmin' | 'operator'
    display_name   TEXT    NOT NULL,
    created_at     TEXT    NOT NULL
) STRICT;
```

Mirrors `forward_auth::User`. `STRICT` table mode is enabled (SQLite
≥ 3.37) to catch type drift early.

---

### `credentials`

```sql
CREATE TABLE credentials (
    credential_id  TEXT    PRIMARY KEY,         -- ULID
    user_id        TEXT    NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    hash           TEXT    NOT NULL,            -- blake3 hex(64)
    status         TEXT    NOT NULL,            -- 'active' | 'revoked'
    issued_at      TEXT    NOT NULL,
    revoked_at     TEXT,
    last_used_at   TEXT
) STRICT;

CREATE INDEX credentials_user_id_idx ON credentials(user_id);
CREATE INDEX credentials_status_idx  ON credentials(status);
```

Cascade delete on `user_id` realises FR-004 (atomic multi-entity
mutation: removing a user removes their credentials in the same
transaction).

---

### `grants`

```sql
CREATE TABLE grants (
    grant_id        TEXT   PRIMARY KEY,         -- ULID
    user_id         TEXT   NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    resource_kind   TEXT   NOT NULL,            -- 'rule' | 'rule_range' | etc.
    resource_value  TEXT   NOT NULL,            -- domain-specific selector
    created_at      TEXT   NOT NULL
) STRICT;

CREATE INDEX grants_user_id_idx       ON grants(user_id);
CREATE INDEX grants_resource_lookup   ON grants(resource_kind, resource_value);
```

The `resource_kind` / `resource_value` pair is opaque at the SQL layer
— `forward-auth` interprets it. v0.7 RBAC envelope unchanged.

---

### `rules`

```sql
CREATE TABLE rules (
    id                          INTEGER PRIMARY KEY AUTOINCREMENT,
    listen_port                 INTEGER NOT NULL CHECK (listen_port BETWEEN 1 AND 65535),
    listen_port_end             INTEGER          CHECK (listen_port_end IS NULL OR
                                                        listen_port_end BETWEEN listen_port AND 65535),
    target_host                 TEXT    NOT NULL,
    target_port                 INTEGER NOT NULL CHECK (target_port BETWEEN 1 AND 65535),
    target_port_end             INTEGER          CHECK (target_port_end IS NULL OR
                                                        target_port_end BETWEEN target_port AND 65535),
    protocol                    TEXT    NOT NULL DEFAULT 'tcp', -- 'tcp' | 'udp'
    owner_user_id               TEXT    NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    health_check_interval_secs  INTEGER          CHECK (health_check_interval_secs IS NULL OR
                                                        health_check_interval_secs BETWEEN 1 AND 3600),
    created_at                  TEXT    NOT NULL,
    updated_at                  TEXT    NOT NULL
) STRICT;

CREATE INDEX rules_owner_idx ON rules(owner_user_id);
CREATE INDEX rules_listen_idx ON rules(listen_port, listen_port_end);
```

Single-target rules use `target_host` / `target_port` directly and
keep `rule_targets` empty. Multi-target rules (v0.7) populate
`rule_targets` and treat `target_host` / `target_port` as ignored at
runtime — same shape `forward_server::rules::Rule` already uses. The
`!targets.is_empty()` discriminator stays at the application layer
(documented in v0.7 `data-model.md`).

---

### `rule_targets`

```sql
CREATE TABLE rule_targets (
    rule_id    INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
    idx        INTEGER NOT NULL,                -- 0..7
    host       TEXT    NOT NULL,
    port       INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    priority   INTEGER NOT NULL CHECK (priority BETWEEN 0 AND 99),
    PRIMARY KEY (rule_id, idx)
) STRICT;
```

Cascade on rule delete. The `(rule_id, idx)` composite PK preserves
the v0.7 ordered-list semantics.

---

### `client_tokens`

```sql
CREATE TABLE client_tokens (
    client_name   TEXT  PRIMARY KEY,            -- regex enforced in app layer
    token_hash    TEXT  NOT NULL UNIQUE,        -- blake3 hex(64)
    issued_at     TEXT  NOT NULL,
    revoked_at    TEXT
) STRICT;
```

`UNIQUE(token_hash)` defends against a (negligible-probability)
collision across re-provisions; an INSERT failing this constraint
maps to `ForwardError::Conflict`.

---

### `audit`

```sql
CREATE TABLE audit (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    ts              TEXT    NOT NULL,           -- RFC3339 UTC, nanosecond resolution
    user_id         TEXT,                       -- nullable: pre-auth deny events have no user
    outcome         TEXT    NOT NULL,           -- 'allow' | 'deny'
    action          TEXT    NOT NULL,           -- e.g., 'rules.push', 'users.delete'
    resource_kind   TEXT,                       -- nullable for actions without resource (e.g., login)
    resource_value  TEXT,
    correlation_id  TEXT    NOT NULL,           -- ties to /metrics & request log
    details_json    TEXT    NOT NULL DEFAULT '{}'
                                               -- bag of action-specific fields, kept JSON-encoded so
                                               -- the audit shape can grow without schema migrations
) STRICT;

CREATE INDEX audit_ts_idx           ON audit(ts DESC);
CREATE INDEX audit_outcome_ts_idx   ON audit(outcome, ts DESC);
CREATE INDEX audit_user_ts_idx      ON audit(user_id, ts DESC);
```

Append-only at the application level (no UPDATE / DELETE except
through `forward-server audit prune --before <ts>`). `seq` doubles as
the cursor pagination token (FR-007).

The `details_json` column carries forward-compatible extension
fields without schema migrations — the column shape we promise to
stay byte-stable while individual JSON keys may grow.

---

## Validation rules (application-layer; mirror v0.7)

- `users.user_id`: `^[a-z][a-z0-9_-]{0,31}$`, plus reserved-name
  blacklist (`__bootstrap__`, `__system__`, etc.).
- `credentials.credential_id`: ULID; sortable by issuance time.
- `rules.listen_port` < `listen_port_end` when both set; same for
  target ports; symmetric port-range cardinality.
- `rule_targets.idx` is contiguous 0..N-1; gap detection happens on
  insert.
- `audit.outcome` ∈ {'allow', 'deny'}; CHECK constraint enforces this.

---

## State transitions

The only entity with a state machine is `credentials`:

```
        ┌─────────┐  revoke   ┌──────────┐
init ───│ active  │──────────▶│ revoked  │── (terminal)
        └────┬────┘           └──────────┘
             │ rotate
             ▼
        ┌─────────────────────┐
        │ active (new id)     │  ← old credential row goes to revoked
        └─────────────────────┘
```

`credentials.last_used_at` updates lazily inside the audit
transaction on each verify-success path; this update writes through a
prepared `UPDATE credentials SET last_used_at = ? WHERE
credential_id = ?` with no `WHERE status = 'active'` filter (the
authentication seam already gate-keeps).

---

## Migration topology (forward-only)

`V001__initial_schema.sql` ships the entire schema above. v0.9+ adds
`V002__...sql`, etc. The `schema_migrations` table is the source of
truth for "what has been applied"; downgrades require backup/restore
(see plan + spec).

---

## Sizing back-of-envelope

Worst plausible v0.8 deployment in scope (per Technical Context):

| Table          | Row count budget | Per-row | Total |
|----------------|------------------|---------|-------|
| users          | 1 000            | ~120 B  | ~120 KB |
| credentials    | 10 000           | ~200 B  | ~2 MB |
| grants         | 10 000           | ~150 B  | ~1.5 MB |
| rules          | 10 000           | ~250 B  | ~2.5 MB |
| rule_targets   | 80 000           | ~80 B   | ~6.4 MB |
| client_tokens  | 1 000            | ~150 B  | ~150 KB |
| audit          | 10 000 000       | ~400 B  | ~4 GB |
| **total**      |                  |         | **~4 GB** |

`audit` dominates and is operator-prunable. Indexes add ~30 % overhead.
SC-005 mandates page query <2 s at 100 k entries — well inside the
prototyped index plan; the 10 M figure is an unbounded-retention worst
case the operator chose, not a v0.8 SLA.
