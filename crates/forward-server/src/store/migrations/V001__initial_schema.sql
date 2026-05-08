-- 008-sqlite-storage T014 — initial schema.
--
-- Authoritative DDL for the v0.8 SQLite store. Mirrors
-- `specs/008-sqlite-storage/data-model.md` table-for-table.
--
-- Conventions:
--   * STRICT mode (SQLite ≥ 3.37) so type drift is caught at INSERT.
--   * RFC3339 UTC timestamps as TEXT for chrono::DateTime<Utc> parity
--     with v0.7 wire types.
--   * Cascade deletes on user removal so a multi-entity mutation is
--     a single DELETE inside one transaction (FR-004 / SC-003).

------------------------------------------------------------------------
-- Operator users / credentials / grants (RBAC, retired identity.json)
------------------------------------------------------------------------

CREATE TABLE users (
    user_id        TEXT    PRIMARY KEY,
    role           TEXT    NOT NULL CHECK (role IN ('superadmin', 'user')),
    display_name   TEXT    NOT NULL,
    disabled       INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0, 1)),
    created_at     TEXT    NOT NULL
) STRICT;

CREATE TABLE credentials (
    credential_id  TEXT    PRIMARY KEY,
    user_id        TEXT    NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    hash           TEXT    NOT NULL UNIQUE,
    label          TEXT,
    status         TEXT    NOT NULL CHECK (status IN ('active', 'revoked')),
    issued_at      TEXT    NOT NULL,
    revoked_at     TEXT,
    last_used_at   TEXT
) STRICT;

CREATE INDEX credentials_user_id_idx ON credentials(user_id);
CREATE INDEX credentials_status_idx  ON credentials(status);

CREATE TABLE grants (
    grant_id            TEXT    PRIMARY KEY,
    user_id             TEXT    NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    -- ClientScope serialization: '*' for Any, ClientName string for Named.
    client              TEXT    NOT NULL,
    listen_port_start   INTEGER NOT NULL CHECK (listen_port_start BETWEEN 1 AND 65535),
    listen_port_end     INTEGER NOT NULL CHECK (listen_port_end BETWEEN listen_port_start AND 65535),
    -- ProtocolSet packed bits: TCP=1, UDP=2, both=3. Non-zero invariant.
    protocols           INTEGER NOT NULL CHECK (protocols BETWEEN 1 AND 3),
    note                TEXT,
    created_at          TEXT    NOT NULL
) STRICT;

CREATE INDEX grants_user_id_idx ON grants(user_id);

------------------------------------------------------------------------
-- Forwarding rules (retired rules.json) + v0.7 multi-target subtable
------------------------------------------------------------------------

CREATE TABLE rules (
    id                          INTEGER PRIMARY KEY AUTOINCREMENT,
    listen_port                 INTEGER NOT NULL CHECK (listen_port BETWEEN 1 AND 65535),
    listen_port_end             INTEGER          CHECK (listen_port_end IS NULL OR
                                                        listen_port_end BETWEEN listen_port AND 65535),
    target_host                 TEXT    NOT NULL,
    target_port                 INTEGER NOT NULL CHECK (target_port BETWEEN 1 AND 65535),
    target_port_end             INTEGER          CHECK (target_port_end IS NULL OR
                                                        target_port_end BETWEEN target_port AND 65535),
    protocol                    TEXT    NOT NULL DEFAULT 'tcp' CHECK (protocol IN ('tcp', 'udp')),
    owner_user_id               TEXT    NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    health_check_interval_secs  INTEGER          CHECK (health_check_interval_secs IS NULL OR
                                                        health_check_interval_secs BETWEEN 1 AND 3600),
    created_at                  TEXT    NOT NULL,
    updated_at                  TEXT    NOT NULL
) STRICT;

CREATE INDEX rules_owner_idx  ON rules(owner_user_id);
CREATE INDEX rules_listen_idx ON rules(listen_port, listen_port_end);

CREATE TABLE rule_targets (
    rule_id    INTEGER NOT NULL REFERENCES rules(id) ON DELETE CASCADE,
    idx        INTEGER NOT NULL CHECK (idx BETWEEN 0 AND 7),
    host       TEXT    NOT NULL,
    port       INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    priority   INTEGER NOT NULL CHECK (priority BETWEEN 0 AND 99),
    PRIMARY KEY (rule_id, idx)
) STRICT;

------------------------------------------------------------------------
-- Client tokens (data-plane auth, retired tokens.json)
------------------------------------------------------------------------

CREATE TABLE client_tokens (
    client_name   TEXT  PRIMARY KEY,
    token_hash    TEXT  NOT NULL UNIQUE,
    issued_at     TEXT  NOT NULL,
    revoked_at    TEXT
) STRICT;

------------------------------------------------------------------------
-- Audit log (replaces in-memory ring buffer)
------------------------------------------------------------------------

CREATE TABLE audit (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    ts              TEXT    NOT NULL,
    user_id         TEXT,
    outcome         TEXT    NOT NULL CHECK (outcome IN ('allow', 'deny')),
    action          TEXT    NOT NULL,
    resource_kind   TEXT,
    resource_value  TEXT,
    correlation_id  TEXT    NOT NULL,
    details_json    TEXT    NOT NULL DEFAULT '{}'
) STRICT;

CREATE INDEX audit_ts_idx         ON audit(ts DESC);
CREATE INDEX audit_outcome_ts_idx ON audit(outcome, ts DESC);
CREATE INDEX audit_user_ts_idx    ON audit(user_id, ts DESC);
