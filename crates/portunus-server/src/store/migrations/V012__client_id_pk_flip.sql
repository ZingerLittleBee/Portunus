-- 015-client-stable-id (T013/T016): flip the PRIMARY KEY of the four
-- client_name-keyed per-client tables onto the stable `client_id` that
-- V011 added and backfilled. `client_name` is dropped from these tables
-- entirely — the canonical display name lives in `client_tokens` and is
-- resolved by id. This is the "follow-up migration in lockstep with the
-- store + operator-route changes" that V011 deferred.
--
-- rules and client_enrollments keep their surrogate keys (autoincrement
-- id) and are addressed by their backfilled `client_id` column, so they
-- need no rebuild here (V011 already swapped rules_client_idx onto
-- client_id).
--
-- Every row in these four tables received a client_id from V011's
-- union-built map, so the copy is loss-free; the defensive
-- `WHERE client_id IS NOT NULL` drops any row that somehow escaped the
-- backfill rather than carrying a NULL key into the rebuilt table.

------------------------------------------------------------------------
-- 1. rate_limit_owner — PK (client_name, owner_id) -> (client_id, owner_id)
------------------------------------------------------------------------

CREATE TABLE rate_limit_owner_new (
    client_id                   TEXT    NOT NULL,
    owner_id                    TEXT    NOT NULL,
    rl_bandwidth_in_bps         INTEGER CHECK (rl_bandwidth_in_bps         IS NULL OR rl_bandwidth_in_bps         > 0),
    rl_bandwidth_out_bps        INTEGER CHECK (rl_bandwidth_out_bps        IS NULL OR rl_bandwidth_out_bps        > 0),
    rl_new_connections_per_sec  INTEGER CHECK (rl_new_connections_per_sec  IS NULL OR rl_new_connections_per_sec  > 0),
    rl_concurrent_connections   INTEGER CHECK (rl_concurrent_connections   IS NULL OR rl_concurrent_connections   > 0),
    rl_bandwidth_in_burst       INTEGER CHECK (rl_bandwidth_in_burst       IS NULL OR rl_bandwidth_in_burst       > 0),
    rl_bandwidth_out_burst      INTEGER CHECK (rl_bandwidth_out_burst      IS NULL OR rl_bandwidth_out_burst      > 0),
    rl_new_connections_burst    INTEGER CHECK (rl_new_connections_burst    IS NULL OR rl_new_connections_burst    > 0),
    updated_at_unix_ms          INTEGER NOT NULL,
    PRIMARY KEY (client_id, owner_id)
);

INSERT INTO rate_limit_owner_new (
    client_id, owner_id,
    rl_bandwidth_in_bps, rl_bandwidth_out_bps,
    rl_new_connections_per_sec, rl_concurrent_connections,
    rl_bandwidth_in_burst, rl_bandwidth_out_burst,
    rl_new_connections_burst, updated_at_unix_ms
)
SELECT client_id, owner_id,
       rl_bandwidth_in_bps, rl_bandwidth_out_bps,
       rl_new_connections_per_sec, rl_concurrent_connections,
       rl_bandwidth_in_burst, rl_bandwidth_out_burst,
       rl_new_connections_burst, updated_at_unix_ms
FROM rate_limit_owner
WHERE client_id IS NOT NULL;

DROP TABLE rate_limit_owner;
ALTER TABLE rate_limit_owner_new RENAME TO rate_limit_owner;

------------------------------------------------------------------------
-- 2. traffic_quotas — PK (user_id, client_name) -> (user_id, client_id)
------------------------------------------------------------------------

-- Unlike rate_limit_owner, the traffic tables KEEP client_name as a
-- plain display column: the TrafficQuotaUpdate wire frame still carries
-- the human name (legacy clients scope by it), and the value at set-time
-- is the right one to echo. Only the PK flips to client_id so accounting
-- stays attached to the stable identity across a rename.
CREATE TABLE traffic_quotas_new (
    user_id                       TEXT    NOT NULL,
    client_id                     TEXT    NOT NULL,
    client_name                   TEXT    NOT NULL,
    monthly_bytes                 INTEGER NOT NULL
                                    CHECK (monthly_bytes >= 0
                                       AND monthly_bytes <= 9223372036854775807),
    billing_anchor                INTEGER NOT NULL,
    current_period_started_at     INTEGER NOT NULL,
    current_period_bytes_used     INTEGER NOT NULL DEFAULT 0
                                    CHECK (current_period_bytes_used >= 0),
    exhausted_at                  INTEGER,
    created_at                    INTEGER NOT NULL,
    updated_at                    INTEGER NOT NULL,
    PRIMARY KEY (user_id, client_id)
);

INSERT INTO traffic_quotas_new (
    user_id, client_id, client_name, monthly_bytes, billing_anchor,
    current_period_started_at, current_period_bytes_used,
    exhausted_at, created_at, updated_at
)
SELECT user_id, client_id, client_name, monthly_bytes, billing_anchor,
       current_period_started_at, current_period_bytes_used,
       exhausted_at, created_at, updated_at
FROM traffic_quotas
WHERE client_id IS NOT NULL;

DROP TABLE traffic_quotas;
ALTER TABLE traffic_quotas_new RENAME TO traffic_quotas;
CREATE INDEX idx_traffic_quotas_client ON traffic_quotas(client_id);

------------------------------------------------------------------------
-- 3. traffic_samples_1m — PK (user_id, client_name, ts_minute)
--                          -> (user_id, client_id, ts_minute)
------------------------------------------------------------------------

CREATE TABLE traffic_samples_1m_new (
    user_id      TEXT    NOT NULL,
    client_id    TEXT    NOT NULL,
    client_name  TEXT    NOT NULL,
    ts_minute    INTEGER NOT NULL,
    bytes_in     INTEGER NOT NULL CHECK (bytes_in  >= 0),
    bytes_out    INTEGER NOT NULL CHECK (bytes_out >= 0),
    PRIMARY KEY (user_id, client_id, ts_minute)
);

INSERT INTO traffic_samples_1m_new (user_id, client_id, client_name, ts_minute, bytes_in, bytes_out)
SELECT user_id, client_id, client_name, ts_minute, bytes_in, bytes_out
FROM traffic_samples_1m
WHERE client_id IS NOT NULL;

DROP TABLE traffic_samples_1m;
ALTER TABLE traffic_samples_1m_new RENAME TO traffic_samples_1m;
CREATE INDEX idx_traffic_samples_1m_ts ON traffic_samples_1m(ts_minute);

------------------------------------------------------------------------
-- 4. traffic_samples_1h — PK (user_id, client_name, ts_hour)
--                          -> (user_id, client_id, ts_hour)
------------------------------------------------------------------------

CREATE TABLE traffic_samples_1h_new (
    user_id      TEXT    NOT NULL,
    client_id    TEXT    NOT NULL,
    client_name  TEXT    NOT NULL,
    ts_hour      INTEGER NOT NULL,
    bytes_in     INTEGER NOT NULL CHECK (bytes_in  >= 0),
    bytes_out    INTEGER NOT NULL CHECK (bytes_out >= 0),
    PRIMARY KEY (user_id, client_id, ts_hour)
);

INSERT INTO traffic_samples_1h_new (user_id, client_id, client_name, ts_hour, bytes_in, bytes_out)
SELECT user_id, client_id, client_name, ts_hour, bytes_in, bytes_out
FROM traffic_samples_1h
WHERE client_id IS NOT NULL;

DROP TABLE traffic_samples_1h;
ALTER TABLE traffic_samples_1h_new RENAME TO traffic_samples_1h;
CREATE INDEX idx_traffic_samples_1h_ts ON traffic_samples_1h(ts_hour);
