-- 013-traffic-quotas: per-(user, client) monthly quota + two-tier rollup
-- history. Quota survives rule deletion (billing artifact, see design
-- spec §7.1). All timestamps are unix seconds UTC; monthly_bytes uses
-- i64 range (matches Rust AtomicI64 + proto int64 — see design spec §3.1).

CREATE TABLE traffic_quotas (
    user_id                       TEXT    NOT NULL,
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
    PRIMARY KEY (user_id, client_name)
);

CREATE INDEX idx_traffic_quotas_client ON traffic_quotas(client_name);

CREATE TABLE traffic_samples_1m (
    user_id      TEXT    NOT NULL,
    client_name  TEXT    NOT NULL,
    ts_minute    INTEGER NOT NULL,
    bytes_in     INTEGER NOT NULL CHECK (bytes_in  >= 0),
    bytes_out    INTEGER NOT NULL CHECK (bytes_out >= 0),
    PRIMARY KEY (user_id, client_name, ts_minute)
);

CREATE INDEX idx_traffic_samples_1m_ts ON traffic_samples_1m(ts_minute);

CREATE TABLE traffic_samples_1h (
    user_id      TEXT    NOT NULL,
    client_name  TEXT    NOT NULL,
    ts_hour      INTEGER NOT NULL,
    bytes_in     INTEGER NOT NULL CHECK (bytes_in  >= 0),
    bytes_out    INTEGER NOT NULL CHECK (bytes_out >= 0),
    PRIMARY KEY (user_id, client_name, ts_hour)
);

CREATE INDEX idx_traffic_samples_1h_ts ON traffic_samples_1h(ts_hour);

CREATE TABLE traffic_rollup_state (
    id                   INTEGER PRIMARY KEY CHECK (id = 1),
    last_rolled_up_hour  INTEGER NOT NULL DEFAULT 0
);

INSERT INTO traffic_rollup_state(id, last_rolled_up_hour) VALUES (1, 0);
