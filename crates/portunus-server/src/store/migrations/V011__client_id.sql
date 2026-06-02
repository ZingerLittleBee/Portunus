-- 015-client-stable-id: introduce a stable, opaque `client_id` (ULID) as the
-- canonical key for a client, and demote `client_name` to a free-form,
-- mutable, non-unique display field.
--
-- Strategy (see specs/015-client-stable-id/contracts/migration-v011.md):
--   1. Build a one-ULID-per-client mapping from the UNION of every
--      client_name that appears anywhere (client_tokens is the roster, but
--      traffic quota rows are billing artifacts that can outlive a token —
--      union-building avoids dropping them).
--   2. Rebuild `client_tokens` so `client_id` is the PRIMARY KEY and
--      `client_name` becomes an ordinary (non-unique) display column. This is
--      the only table whose PK was client_name-based AND has no inbound FKs,
--      so it can be rebuilt safely now.
--   3. Add a backfilled `client_id` column to the six dependent tables. Their
--      PRIMARY KEYs stay client_name-based in this migration; the PK flip to
--      client_id lands in a follow-up migration in lockstep with the store +
--      operator-route changes that address clients by id (keeps the tree from
--      sitting in a long broken state). The column + backfill are done now so
--      the one-time id assignment happens once.
--
-- Legacy rows get a freshly-minted random 128-bit ULID string: the first
-- Crockford char is forced into 0-7 so the value fits 128 bits (Ulid::
-- from_string rejects an overflowing 26-char string), and the remaining 25
-- chars come from uppercase hex (a valid Crockford subset). The timestamp
-- prefix of a backfilled ULID is not meaningful — these ids are opaque. New
-- clients minted after this migration use Rust ClientId::new() (time-ordered).

------------------------------------------------------------------------
-- 1. client_name -> client_id map (one fresh ULID per distinct name)
------------------------------------------------------------------------

CREATE TABLE _client_id_map (
    client_name TEXT PRIMARY KEY,
    client_id   TEXT NOT NULL UNIQUE
);

INSERT INTO _client_id_map (client_name, client_id)
SELECT name,
       substr('01234567', (random() & 7) + 1, 1) || substr(hex(randomblob(13)), 2)
FROM (
    SELECT client_name AS name FROM client_tokens
    UNION SELECT client_name FROM rules               WHERE client_name IS NOT NULL
    UNION SELECT client_name FROM rate_limit_owner
    UNION SELECT client_name FROM traffic_quotas
    UNION SELECT client_name FROM traffic_samples_1m
    UNION SELECT client_name FROM traffic_samples_1h
    UNION SELECT client_name FROM client_enrollments
);

------------------------------------------------------------------------
-- 2. client_tokens — rebuild: client_id becomes PK, client_name a
--    plain (now non-unique) display column.
------------------------------------------------------------------------

CREATE TABLE client_tokens_new (
    client_id      TEXT PRIMARY KEY,
    client_name    TEXT NOT NULL,
    token_hash     TEXT NOT NULL UNIQUE,
    issued_at      TEXT NOT NULL,
    revoked_at     TEXT,
    client_address TEXT
) STRICT;

INSERT INTO client_tokens_new (client_id, client_name, token_hash, issued_at, revoked_at, client_address)
SELECT m.client_id, t.client_name, t.token_hash, t.issued_at, t.revoked_at, t.client_address
FROM client_tokens t
JOIN _client_id_map m ON m.client_name = t.client_name;

DROP TABLE client_tokens;
ALTER TABLE client_tokens_new RENAME TO client_tokens;

------------------------------------------------------------------------
-- 3. Dependent tables — add a backfilled client_id column (PK flip deferred).
------------------------------------------------------------------------

-- 3a. rules: client_name is a plain nullable column; add client_id and swap
--      the (client_name, listen_port) index for (client_id, listen_port).
ALTER TABLE rules ADD COLUMN client_id TEXT;
UPDATE rules
SET client_id = (SELECT m.client_id FROM _client_id_map m WHERE m.client_name = rules.client_name)
WHERE client_name IS NOT NULL;
DROP INDEX IF EXISTS rules_client_idx;
CREATE INDEX rules_client_idx ON rules (client_id, listen_port);

-- 3b. rate_limit_owner
ALTER TABLE rate_limit_owner ADD COLUMN client_id TEXT;
UPDATE rate_limit_owner
SET client_id = (SELECT m.client_id FROM _client_id_map m WHERE m.client_name = rate_limit_owner.client_name);

-- 3c. traffic_quotas
ALTER TABLE traffic_quotas ADD COLUMN client_id TEXT;
UPDATE traffic_quotas
SET client_id = (SELECT m.client_id FROM _client_id_map m WHERE m.client_name = traffic_quotas.client_name);

-- 3d. traffic_samples_1m
ALTER TABLE traffic_samples_1m ADD COLUMN client_id TEXT;
UPDATE traffic_samples_1m
SET client_id = (SELECT m.client_id FROM _client_id_map m WHERE m.client_name = traffic_samples_1m.client_name);

-- 3e. traffic_samples_1h
ALTER TABLE traffic_samples_1h ADD COLUMN client_id TEXT;
UPDATE traffic_samples_1h
SET client_id = (SELECT m.client_id FROM _client_id_map m WHERE m.client_name = traffic_samples_1h.client_name);

-- 3f. client_enrollments
ALTER TABLE client_enrollments ADD COLUMN client_id TEXT;
UPDATE client_enrollments
SET client_id = (SELECT m.client_id FROM _client_id_map m WHERE m.client_name = client_enrollments.client_name);

------------------------------------------------------------------------
-- 4. Drop the scratch map.
------------------------------------------------------------------------

DROP TABLE _client_id_map;
