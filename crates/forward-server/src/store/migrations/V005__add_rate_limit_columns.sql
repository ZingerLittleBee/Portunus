-- 011-rate-limiting-qos: per-rule and per-owner cap envelopes.
--
-- Additive only. NULL on every cap column means "uncapped on this
-- dimension"; the data plane preserves v0.10 behaviour byte-for-byte
-- when every cap is NULL. The capability gate
-- (rate_limit_unsupported_by_client) prevents pushing rules with any
-- non-NULL cap field to a forward-client whose self-reported
-- client_version is < 0.11.0.

ALTER TABLE rules ADD COLUMN rl_bandwidth_in_bps        INTEGER
    CHECK (rl_bandwidth_in_bps        IS NULL OR rl_bandwidth_in_bps        > 0);
ALTER TABLE rules ADD COLUMN rl_bandwidth_out_bps       INTEGER
    CHECK (rl_bandwidth_out_bps       IS NULL OR rl_bandwidth_out_bps       > 0);
ALTER TABLE rules ADD COLUMN rl_new_connections_per_sec INTEGER
    CHECK (rl_new_connections_per_sec IS NULL OR rl_new_connections_per_sec > 0);
ALTER TABLE rules ADD COLUMN rl_concurrent_connections  INTEGER
    CHECK (rl_concurrent_connections  IS NULL OR rl_concurrent_connections  > 0);
ALTER TABLE rules ADD COLUMN rl_bandwidth_in_burst      INTEGER
    CHECK (rl_bandwidth_in_burst      IS NULL OR rl_bandwidth_in_burst      > 0);
ALTER TABLE rules ADD COLUMN rl_bandwidth_out_burst     INTEGER
    CHECK (rl_bandwidth_out_burst     IS NULL OR rl_bandwidth_out_burst     > 0);
ALTER TABLE rules ADD COLUMN rl_new_connections_burst   INTEGER
    CHECK (rl_new_connections_burst   IS NULL OR rl_new_connections_burst   > 0);

-- Per-owner cap envelope. Keyed by (client, owner) — see Q1
-- (per-RBAC-owner is the tenant boundary, not per-node). GC happens
-- in forward-server when the owner's last rule on this client is
-- removed.
CREATE TABLE rate_limit_owner (
    client_name                 TEXT    NOT NULL,
    owner_id                    TEXT    NOT NULL,
    rl_bandwidth_in_bps         INTEGER CHECK (rl_bandwidth_in_bps         IS NULL OR rl_bandwidth_in_bps         > 0),
    rl_bandwidth_out_bps        INTEGER CHECK (rl_bandwidth_out_bps        IS NULL OR rl_bandwidth_out_bps        > 0),
    rl_new_connections_per_sec  INTEGER CHECK (rl_new_connections_per_sec  IS NULL OR rl_new_connections_per_sec  > 0),
    rl_concurrent_connections   INTEGER CHECK (rl_concurrent_connections   IS NULL OR rl_concurrent_connections   > 0),
    rl_bandwidth_in_burst       INTEGER CHECK (rl_bandwidth_in_burst       IS NULL OR rl_bandwidth_in_burst       > 0),
    rl_bandwidth_out_burst      INTEGER CHECK (rl_bandwidth_out_burst      IS NULL OR rl_bandwidth_out_burst      > 0),
    rl_new_connections_burst    INTEGER CHECK (rl_new_connections_burst    IS NULL OR rl_new_connections_burst    > 0),
    updated_at_unix_ms          INTEGER NOT NULL,
    PRIMARY KEY (client_name, owner_id)
);
