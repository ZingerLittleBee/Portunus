-- 010-proxy-protocol-and-peek-histogram + rule persistence wiring:
-- add the remaining runtime columns required to hydrate rules from SQLite.

ALTER TABLE rules ADD COLUMN client_name TEXT;
ALTER TABLE rules ADD COLUMN prefer_ipv6 INTEGER CHECK (prefer_ipv6 IS NULL OR prefer_ipv6 IN (0, 1));
ALTER TABLE rules ADD COLUMN state_kind TEXT NOT NULL DEFAULT 'pending'
    CHECK (state_kind IN ('pending', 'active', 'failed', 'removed'));
ALTER TABLE rules ADD COLUMN state_reason TEXT;

CREATE INDEX rules_client_idx ON rules (client_name, listen_port);
