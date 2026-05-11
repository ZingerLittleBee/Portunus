-- v1.1.0 local password auth.

ALTER TABLE users ADD COLUMN password_hash TEXT;
ALTER TABLE users ADD COLUMN password_change_required INTEGER NOT NULL DEFAULT 0 CHECK (password_change_required IN (0, 1));

CREATE TABLE web_sessions (
    session_hash         TEXT    PRIMARY KEY,
    user_id              TEXT    NOT NULL REFERENCES users(user_id) ON DELETE CASCADE,
    created_at           TEXT    NOT NULL,
    last_seen_at         TEXT    NOT NULL,
    absolute_expires_at  TEXT    NOT NULL,
    revoked_at           TEXT,
    remote_addr          TEXT,
    user_agent           TEXT
) STRICT;

CREATE INDEX web_sessions_user_idx ON web_sessions(user_id);
CREATE INDEX web_sessions_expiry_idx ON web_sessions(absolute_expires_at, revoked_at);

CREATE TABLE login_attempts (
    subject         TEXT    NOT NULL,
    remote_addr     TEXT    NOT NULL,
    action          TEXT    NOT NULL CHECK (action IN ('login', 'onboarding', 'password_reset')),
    failures        INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0),
    first_failed_at TEXT,
    last_failed_at  TEXT,
    locked_until    TEXT,
    PRIMARY KEY (subject, remote_addr, action)
) STRICT;

CREATE TABLE onboarding_setup (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    token_hash  TEXT    NOT NULL,
    issued_at   TEXT    NOT NULL,
    expires_at  TEXT    NOT NULL
) STRICT;
