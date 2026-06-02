//! 015-client-stable-id T010 — V011 migration: re-key clients to a stable
//! `client_id` (ULID), backfilling every dependent table consistently.
//!
//! Backfill correctness is validated by applying the migration SQL directly
//! (V001..V010 to build the prior schema, seed legacy name-keyed rows, then
//! V011) so we can inject pre-migration data. Idempotency / crash-safety is
//! validated separately through the real `Store::open` path, which is what
//! refinery's version gate actually guards in production.

use portunus_core::ClientId;
use rusqlite::Connection;

const V001: &str = include_str!("../src/store/migrations/V001__initial_schema.sql");
const V002: &str = include_str!("../src/store/migrations/V002__add_sni_pattern.sql");
const V003: &str = include_str!("../src/store/migrations/V003__add_rule_target_proxy_protocol.sql");
const V004: &str = include_str!("../src/store/migrations/V004__add_rule_runtime_columns.sql");
const V005: &str = include_str!("../src/store/migrations/V005__add_rate_limit_columns.sql");
const V006: &str = include_str!("../src/store/migrations/V006__add_local_password_auth.sql");
const V007: &str = include_str!("../src/store/migrations/V007__add_client_address.sql");
const V008: &str = include_str!("../src/store/migrations/V008__add_traffic_quotas.sql");
const V009: &str = include_str!("../src/store/migrations/V009__add_client_enrollments.sql");
const V010: &str = include_str!("../src/store/migrations/V010__add_server_settings.sql");
const V011: &str = include_str!("../src/store/migrations/V011__client_id.sql");

fn apply_through_v010(conn: &Connection) {
    for (label, sql) in [
        ("V001", V001),
        ("V002", V002),
        ("V003", V003),
        ("V004", V004),
        ("V005", V005),
        ("V006", V006),
        ("V007", V007),
        ("V008", V008),
        ("V009", V009),
        ("V010", V010),
    ] {
        conn.execute_batch(sql)
            .unwrap_or_else(|e| panic!("{label} failed: {e}"));
    }
}

/// Seed two clients, each with a token row plus one row in every dependent
/// table — including a traffic_quota for a *third* client_name that has NO
/// token row (a billing artifact that outlived its client). The union-built
/// id map must still mint an id for it (no orphan drop).
fn seed_legacy(conn: &Connection) {
    conn.execute_batch(
        r#"
        INSERT INTO users (user_id, role, display_name, disabled, created_at)
        VALUES ('alice', 'user', 'Alice', 0, '2026-01-01T00:00:00Z');

        INSERT INTO client_tokens (client_name, token_hash, issued_at, revoked_at, client_address)
        VALUES ('edge-01', 'hash01', '2026-01-01T00:00:00Z', NULL, 'edge01.example:7443'),
               ('edge-02', 'hash02', '2026-01-01T00:00:00Z', NULL, NULL);

        INSERT INTO rules (listen_port, target_host, target_port, protocol, owner_user_id,
                           created_at, updated_at, client_name, state_kind)
        VALUES (8080, '10.0.0.1', 80, 'tcp', 'alice',
                '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'edge-01', 'active');

        INSERT INTO rate_limit_owner (client_name, owner_id, rl_bandwidth_in_bps, updated_at_unix_ms)
        VALUES ('edge-01', 'alice', 1048576, 1700000000000);

        INSERT INTO traffic_quotas (user_id, client_name, monthly_bytes, billing_anchor,
                                    current_period_started_at, current_period_bytes_used,
                                    created_at, updated_at)
        VALUES ('alice', 'edge-01', 1000000, 1, 1700000000, 0, 1700000000, 1700000000),
               ('alice', 'ghost-client', 500000, 1, 1700000000, 0, 1700000000, 1700000000);

        INSERT INTO traffic_samples_1m (user_id, client_name, ts_minute, bytes_in, bytes_out)
        VALUES ('alice', 'edge-01', 28333333, 100, 200);

        INSERT INTO traffic_samples_1h (user_id, client_name, ts_hour, bytes_in, bytes_out)
        VALUES ('alice', 'edge-01', 472222, 1000, 2000);

        INSERT INTO client_enrollments (client_name, code_hash, issued_at, expires_at)
        VALUES ('edge-02', 'codehash02', '2026-01-01T00:00:00Z', '2026-01-01T00:05:00Z');
        "#,
    )
    .expect("seed legacy rows");
}

#[test]
fn v011_backfills_every_table_with_consistent_client_ids() {
    let conn = Connection::open_in_memory().unwrap();
    apply_through_v010(&conn);
    seed_legacy(&conn);

    conn.execute_batch(V011).expect("V011 applies");

    // 1. Every client_tokens row has a valid ULID client_id.
    let id_edge01: String = conn
        .query_row(
            "SELECT client_id FROM client_tokens WHERE client_name = 'edge-01'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(id_edge01.len(), 26, "ULID canonical form");
    id_edge01
        .parse::<ClientId>()
        .expect("backfilled id is a parseable ULID");

    // 2. Dependent rows for edge-01 share the SAME client_id (consistent join).
    let rule_cid: String = conn
        .query_row("SELECT client_id FROM rules WHERE client_name = 'edge-01'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rule_cid, id_edge01, "rules backfill matches token id");

    let rlo_cid: String = conn
        .query_row(
            "SELECT client_id FROM rate_limit_owner WHERE client_name = 'edge-01'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rlo_cid, id_edge01, "rate_limit_owner backfill matches");

    let tq_cid: String = conn
        .query_row(
            "SELECT client_id FROM traffic_quotas WHERE client_name = 'edge-01'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tq_cid, id_edge01, "traffic_quotas backfill matches");

    let s1m_cid: String = conn
        .query_row("SELECT client_id FROM traffic_samples_1m WHERE client_name = 'edge-01'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(s1m_cid, id_edge01);
    let s1h_cid: String = conn
        .query_row("SELECT client_id FROM traffic_samples_1h WHERE client_name = 'edge-01'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(s1h_cid, id_edge01);

    // 3. Distinct clients get distinct ids.
    let id_edge02: String = conn
        .query_row("SELECT client_id FROM client_tokens WHERE client_name = 'edge-02'", [], |r| r.get(0))
        .unwrap();
    assert_ne!(id_edge01, id_edge02);

    let enr_cid: String = conn
        .query_row("SELECT client_id FROM client_enrollments WHERE client_name = 'edge-02'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(enr_cid, id_edge02, "enrollment backfill matches its client");

    // 4. Zero orphans: every dependent row got a non-NULL client_id, including
    //    the tokenless 'ghost-client' billing-artifact quota row.
    for (table, _) in [
        ("rules", ""),
        ("rate_limit_owner", ""),
        ("traffic_quotas", ""),
        ("traffic_samples_1m", ""),
        ("traffic_samples_1h", ""),
        ("client_enrollments", ""),
    ] {
        let nulls: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE client_id IS NULL"),
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(nulls, 0, "{table} has rows with NULL client_id");
    }
    let ghost_cid: Option<String> = conn
        .query_row(
            "SELECT client_id FROM traffic_quotas WHERE client_name = 'ghost-client'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(ghost_cid.is_some(), "tokenless billing-artifact row kept + backfilled");

    // 5. client_name is no longer a PRIMARY KEY / UNIQUE on client_tokens —
    //    duplicate display names are now allowed (FR-013).
    conn.execute(
        "INSERT INTO client_tokens (client_id, client_name, token_hash, issued_at)
         VALUES (?, 'edge-01', 'hash03', '2026-01-02T00:00:00Z')",
        rusqlite::params![ClientId::new().to_string()],
    )
    .expect("duplicate display name is allowed after V011");
}

/// Idempotency / crash-safety: opening the store twice runs migrations only
/// once (refinery version gate). The second open must be a clean no-op.
#[test]
fn store_open_is_idempotent_across_restart() {
    use portunus_server::store::Store;

    let dir = tempfile::tempdir().unwrap();
    {
        let _store = Store::open(dir.path()).expect("first open runs V001..V011");
    }
    // Second open: all migrations already recorded — no re-run, no error.
    let _store = Store::open(dir.path()).expect("second open is a no-op");
}
