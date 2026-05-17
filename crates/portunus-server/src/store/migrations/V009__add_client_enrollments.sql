CREATE TABLE client_enrollments (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    client_name    TEXT    NOT NULL,
    client_address TEXT,
    code_hash      TEXT    NOT NULL UNIQUE,
    issued_at      TEXT    NOT NULL,
    expires_at     TEXT    NOT NULL,
    consumed_at    TEXT
) STRICT;

CREATE INDEX idx_client_enrollments_expires_at
    ON client_enrollments(expires_at);
