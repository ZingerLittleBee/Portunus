use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use portunus_auth::UserId;
use portunus_core::{ClientName, RateLimit, RuleId, RuleTarget};
use rusqlite::params;
use tracing::warn;

use crate::rules::{Protocol, Rule, RuleState};
use crate::store::{Store, StoreError, map_rusqlite};

#[derive(Clone, Debug)]
pub struct SqliteRuleStore {
    store: Arc<Store>,
}

impl SqliteRuleStore {
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    pub fn upsert_rule(&self, rule: &Rule) -> Result<(), StoreError> {
        self.store.with_write_tx(|tx| {
            let (state_kind, state_reason) = match &rule.state {
                RuleState::Pending => ("pending", None),
                RuleState::Active => ("active", None),
                RuleState::Failed { reason } => ("failed", Some(reason.as_str())),
                RuleState::Removed => ("removed", None),
            };
            // 011-rate-limiting-qos T015: rate_limit lands as seven
            // independently-nullable INTEGER columns. Absent envelope
            // ⇒ all seven NULL ⇒ rule load returns rate_limit=None,
            // preserving v0.10 behaviour byte-for-byte.
            let rl = rule.rate_limit.as_ref();
            tx.execute(
                "INSERT INTO rules (
                    id, client_name, listen_port, listen_port_end, target_host, target_port,
                    target_port_end, prefer_ipv6, protocol, state_kind, state_reason,
                    owner_user_id, health_check_interval_secs, created_at, updated_at, sni_pattern,
                    rl_bandwidth_in_bps, rl_bandwidth_out_bps, rl_new_connections_per_sec,
                    rl_concurrent_connections, rl_bandwidth_in_burst, rl_bandwidth_out_burst,
                    rl_new_connections_burst
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                    client_name = excluded.client_name,
                    listen_port = excluded.listen_port,
                    listen_port_end = excluded.listen_port_end,
                    target_host = excluded.target_host,
                    target_port = excluded.target_port,
                    target_port_end = excluded.target_port_end,
                    prefer_ipv6 = excluded.prefer_ipv6,
                    protocol = excluded.protocol,
                    state_kind = excluded.state_kind,
                    state_reason = excluded.state_reason,
                    owner_user_id = excluded.owner_user_id,
                    health_check_interval_secs = excluded.health_check_interval_secs,
                    updated_at = excluded.updated_at,
                    sni_pattern = excluded.sni_pattern,
                    rl_bandwidth_in_bps = excluded.rl_bandwidth_in_bps,
                    rl_bandwidth_out_bps = excluded.rl_bandwidth_out_bps,
                    rl_new_connections_per_sec = excluded.rl_new_connections_per_sec,
                    rl_concurrent_connections = excluded.rl_concurrent_connections,
                    rl_bandwidth_in_burst = excluded.rl_bandwidth_in_burst,
                    rl_bandwidth_out_burst = excluded.rl_bandwidth_out_burst,
                    rl_new_connections_burst = excluded.rl_new_connections_burst",
                params![
                    rule.id.0,
                    rule.client_name.as_str(),
                    rule.listen_port,
                    rule.listen_port_end,
                    rule.target_host,
                    rule.target_port,
                    rule.target_port_end,
                    rule.prefer_ipv6.map(i32::from),
                    rule.protocol.as_str(),
                    state_kind,
                    state_reason,
                    rule.owner_user_id.as_str(),
                    rule.health_check_interval_secs,
                    rule.created_at.to_rfc3339(),
                    rule.last_state_change_at.to_rfc3339(),
                    rule.sni_pattern,
                    rl.and_then(|r| r.bandwidth_in_bps),
                    rl.and_then(|r| r.bandwidth_out_bps),
                    rl.and_then(|r| r.new_connections_per_sec),
                    rl.and_then(|r| r.concurrent_connections),
                    rl.and_then(|r| r.bandwidth_in_burst),
                    rl.and_then(|r| r.bandwidth_out_burst),
                    rl.and_then(|r| r.new_connections_burst),
                ],
            )
            .map_err(map_rusqlite)?;

            tx.execute(
                "DELETE FROM rule_targets WHERE rule_id = ?",
                params![rule.id.0],
            )
            .map_err(map_rusqlite)?;
            for (idx, target) in rule.targets.iter().enumerate() {
                tx.execute(
                    "INSERT INTO rule_targets (rule_id, idx, host, port, priority, proxy_protocol)
                     VALUES (?, ?, ?, ?, ?, ?)",
                    params![
                        rule.id.0,
                        i64::try_from(idx).unwrap_or(i64::MAX),
                        target.host,
                        target.port,
                        target.priority,
                        target.proxy_protocol.map(|mode| match mode {
                            portunus_core::ProxyProtocolVersion::V1 => "v1",
                            portunus_core::ProxyProtocolVersion::V2 => "v2",
                        }),
                    ],
                )
                .map_err(map_rusqlite)?;
            }
            Ok(())
        })
    }

    pub fn delete_rule(&self, rule_id: RuleId) -> Result<(), StoreError> {
        self.store.with_write_tx(|tx| {
            tx.execute("DELETE FROM rules WHERE id = ?", params![rule_id.0])
                .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    pub fn delete_rules_owned_by(&self, owner: &UserId) -> Result<(), StoreError> {
        self.store.with_write_tx(|tx| {
            tx.execute(
                "DELETE FROM rules WHERE owner_user_id = ?",
                params![owner.as_str()],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }

    pub fn list_rules(&self) -> Result<Vec<Rule>, StoreError> {
        self.store.with_conn(|conn| {
            let skipped_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM rules WHERE client_name IS NULL", [], |row| {
                    row.get(0)
                })
                .map_err(map_rusqlite)?;
            if skipped_count > 0 {
                warn!(
                    skipped_count,
                    "skipping persisted rules with missing client_name"
                );
            }

            let mut stmt = conn
                .prepare(
                    "SELECT id, client_name, listen_port, listen_port_end, target_host, target_port,
                            target_port_end, prefer_ipv6, protocol, state_kind, state_reason,
                            owner_user_id, health_check_interval_secs, created_at, updated_at, sni_pattern,
                            rl_bandwidth_in_bps, rl_bandwidth_out_bps, rl_new_connections_per_sec,
                            rl_concurrent_connections, rl_bandwidth_in_burst, rl_bandwidth_out_burst,
                            rl_new_connections_burst
                     FROM rules
                     WHERE client_name IS NOT NULL
                     ORDER BY id ASC",
                )
                .map_err(map_rusqlite)?;
            let rows = stmt
                .query_map([], |row| {
                    let id = RuleId(row.get(0)?);
                    let client_name = ClientName::new(row.get::<_, String>(1)?).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                    let protocol = match row.get::<_, String>(8)?.as_str() {
                        "udp" => Protocol::Udp,
                        _ => Protocol::Tcp,
                    };
                    let state_kind: String = row.get(9)?;
                    let state_reason: Option<String> = row.get(10)?;
                    let state = match state_kind.as_str() {
                        "active" => RuleState::Active,
                        "failed" => RuleState::Failed {
                            reason: state_reason.unwrap_or_else(|| "unspecified".into()),
                        },
                        "removed" => RuleState::Removed,
                        _ => RuleState::Pending,
                    };
                    let owner_raw: String = row.get(11)?;
                    let owner_user_id = if owner_raw.starts_with('_') {
                        UserId::reserved(owner_raw)
                    } else {
                        UserId::from_str(&owner_raw).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                11,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?
                    };
                    let created_at = parse_ts(row.get::<_, String>(13)?)?;
                    let updated_at = parse_ts(row.get::<_, String>(14)?)?;
                    // 011-rate-limiting-qos T015: rebuild RateLimit
                    // from columns 16..=22. Returns None when every
                    // column is NULL (= uncapped — preserves v0.10
                    // shape). Any non-NULL column promotes the
                    // envelope to Some(..) with the other dimensions
                    // staying None.
                    let rate_limit = build_rate_limit_from_row(row)?;
                    Ok(Rule {
                        id,
                        client_name,
                        listen_port: row.get(2)?,
                        listen_port_end: row.get(3)?,
                        target_host: row.get(4)?,
                        target_port: row.get(5)?,
                        target_port_end: row.get(6)?,
                        prefer_ipv6: row.get::<_, Option<i32>>(7)?.map(|v| v != 0),
                        protocol,
                        state,
                        created_at,
                        last_state_change_at: updated_at,
                        owner_user_id,
                        targets: Vec::new(),
                        health_check_interval_secs: row.get(12)?,
                        sni_pattern: row.get(15)?,
                        rate_limit,
                    })
                })
                .map_err(map_rusqlite)?;

            let mut out = Vec::new();
            for row in rows {
                let mut rule = row.map_err(map_rusqlite)?;
                rule.targets = load_targets(conn, rule.id)?;
                out.push(rule);
            }
            Ok(out)
        })
    }

    pub fn get_rule(&self, rule_id: RuleId) -> Result<Option<Rule>, StoreError> {
        let rules = self.list_rules()?;
        Ok(rules.into_iter().find(|rule| rule.id == rule_id))
    }
}

fn load_targets(
    conn: &rusqlite::Connection,
    rule_id: RuleId,
) -> Result<Vec<RuleTarget>, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT host, port, priority, proxy_protocol
             FROM rule_targets
             WHERE rule_id = ?
             ORDER BY idx ASC",
        )
        .map_err(map_rusqlite)?;
    let rows = stmt
        .query_map(params![rule_id.0], |row| {
            let proxy = match row.get::<_, Option<String>>(3)?.as_deref() {
                Some("v2") => Some(portunus_core::ProxyProtocolVersion::V2),
                Some("v1") => Some(portunus_core::ProxyProtocolVersion::V1),
                Some(_) | None => None,
            };
            Ok(RuleTarget {
                host: row.get(0)?,
                port: row.get(1)?,
                priority: row.get(2)?,
                proxy_protocol: proxy,
            })
        })
        .map_err(map_rusqlite)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(map_rusqlite)?);
    }
    Ok(out)
}

fn parse_ts(raw: String) -> Result<DateTime<Utc>, rusqlite::Error> {
    DateTime::parse_from_rfc3339(&raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
}

/// 011-rate-limiting-qos T015: hydrate `Rule.rate_limit` from columns
/// 16..=22 of the `list_rules` SELECT. Returns `None` when every cap
/// column is NULL — that's the v0.10-shape "uncapped" rule the
/// loader has always produced. Any non-NULL cap promotes the envelope
/// to `Some(..)`; SQLite-side CHECK constraints already guard against
/// `NOT NULL OR > 0`, so the per-column `> 0` invariant holds even
/// across hand-edited databases.
fn build_rate_limit_from_row(
    row: &rusqlite::Row<'_>,
) -> Result<Option<RateLimit>, rusqlite::Error> {
    let bandwidth_in_bps: Option<u64> = row.get(16)?;
    let bandwidth_out_bps: Option<u64> = row.get(17)?;
    let new_connections_per_sec: Option<u32> = row.get(18)?;
    let concurrent_connections: Option<u32> = row.get(19)?;
    let bandwidth_in_burst: Option<u64> = row.get(20)?;
    let bandwidth_out_burst: Option<u64> = row.get(21)?;
    let new_connections_burst: Option<u32> = row.get(22)?;
    if bandwidth_in_bps.is_none()
        && bandwidth_out_bps.is_none()
        && new_connections_per_sec.is_none()
        && concurrent_connections.is_none()
        && bandwidth_in_burst.is_none()
        && bandwidth_out_burst.is_none()
        && new_connections_burst.is_none()
    {
        return Ok(None);
    }
    Ok(Some(RateLimit {
        bandwidth_in_bps,
        bandwidth_out_bps,
        new_connections_per_sec,
        concurrent_connections,
        bandwidth_in_burst,
        bandwidth_out_burst,
        new_connections_burst,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::sync::{Mutex, MutexGuard};
    use tempfile::tempdir;
    use tracing_subscriber::{fmt, layer::SubscriberExt};

    #[derive(Clone, Default)]
    struct SharedBuf {
        inner: Arc<Mutex<Vec<u8>>>,
    }

    impl SharedBuf {
        fn snapshot(&self) -> String {
            let guard: MutexGuard<'_, Vec<u8>> = self.inner.lock().unwrap();
            String::from_utf8_lossy(&guard).into_owned()
        }
    }

    impl io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn roundtrip_rule_with_proxy_protocol_target() {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        crate::store::operator_store::SqliteOperatorStore::new(Arc::clone(&store))
            .bootstrap_legacy_superadmin("test-token")
            .unwrap();
        let rule_store = SqliteRuleStore::new(store);
        let rule = Rule {
            id: RuleId(42),
            client_name: ClientName::new("edge-01".to_string()).unwrap(),
            listen_port: 443,
            listen_port_end: None,
            target_host: "10.0.0.1".into(),
            target_port: 8443,
            target_port_end: None,
            prefer_ipv6: Some(true),
            protocol: Protocol::Tcp,
            state: RuleState::Active,
            created_at: Utc::now(),
            last_state_change_at: Utc::now(),
            owner_user_id: UserId::reserved("_legacy"),
            targets: vec![
                RuleTarget {
                    host: "10.0.0.1".into(),
                    port: 8443,
                    priority: 0,
                    proxy_protocol: Some(portunus_core::ProxyProtocolVersion::V2),
                },
                RuleTarget {
                    host: "10.0.0.2".into(),
                    port: 9443,
                    priority: 1,
                    proxy_protocol: None,
                },
            ],
            health_check_interval_secs: Some(30),
            sni_pattern: Some("api.example.com".into()),
            rate_limit: None,
        };

        rule_store.upsert_rule(&rule).unwrap();
        let loaded = rule_store.get_rule(rule.id).unwrap().expect("rule exists");
        assert_eq!(loaded.client_name, rule.client_name);
        assert_eq!(loaded.prefer_ipv6, Some(true));
        assert_eq!(loaded.sni_pattern.as_deref(), Some("api.example.com"));
        assert_eq!(loaded.targets.len(), 2);
        assert_eq!(
            loaded.targets[0].proxy_protocol,
            Some(portunus_core::ProxyProtocolVersion::V2)
        );
        assert_eq!(loaded.targets[1].proxy_protocol, None);
    }

    #[test]
    fn roundtrip_rule_with_rate_limit_envelope() {
        // 011-rate-limiting-qos T015: a rule with every cap dimension
        // set must round-trip through SQLite verbatim. Burst overrides
        // round-trip alongside their companion rates.
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        crate::store::operator_store::SqliteOperatorStore::new(Arc::clone(&store))
            .bootstrap_legacy_superadmin("test-token")
            .unwrap();
        let rule_store = SqliteRuleStore::new(store);
        let rule = Rule {
            id: RuleId(101),
            client_name: ClientName::new("edge-02".to_string()).unwrap(),
            listen_port: 443,
            listen_port_end: None,
            target_host: "10.0.0.5".into(),
            target_port: 8443,
            target_port_end: None,
            prefer_ipv6: None,
            protocol: Protocol::Tcp,
            state: RuleState::Active,
            created_at: Utc::now(),
            last_state_change_at: Utc::now(),
            owner_user_id: UserId::reserved("_legacy"),
            targets: vec![RuleTarget {
                host: "10.0.0.5".into(),
                port: 8443,
                priority: 0,
                proxy_protocol: None,
            }],
            health_check_interval_secs: None,
            sni_pattern: None,
            rate_limit: Some(RateLimit {
                bandwidth_in_bps: Some(1_048_576),
                bandwidth_out_bps: Some(2_097_152),
                new_connections_per_sec: Some(50),
                concurrent_connections: Some(200),
                bandwidth_in_burst: Some(2_097_152),
                bandwidth_out_burst: Some(4_194_304),
                new_connections_burst: Some(100),
            }),
        };
        rule_store.upsert_rule(&rule).unwrap();
        let loaded = rule_store.get_rule(rule.id).unwrap().expect("rule exists");
        assert_eq!(loaded.rate_limit, rule.rate_limit);
    }

    #[test]
    fn rule_without_caps_loads_with_rate_limit_none() {
        // The byte-stability invariant on the storage side: a rule
        // pushed without any cap field must come back as
        // `rate_limit: None` (not `Some(empty envelope)`). This is
        // what keeps the v0.10 hot path active for legacy traffic.
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        crate::store::operator_store::SqliteOperatorStore::new(Arc::clone(&store))
            .bootstrap_legacy_superadmin("test-token")
            .unwrap();
        let rule_store = SqliteRuleStore::new(store);
        let rule = Rule {
            id: RuleId(102),
            client_name: ClientName::new("edge-03".to_string()).unwrap(),
            listen_port: 80,
            listen_port_end: None,
            target_host: "10.0.0.7".into(),
            target_port: 8080,
            target_port_end: None,
            prefer_ipv6: None,
            protocol: Protocol::Tcp,
            state: RuleState::Active,
            created_at: Utc::now(),
            last_state_change_at: Utc::now(),
            owner_user_id: UserId::reserved("_legacy"),
            targets: vec![RuleTarget {
                host: "10.0.0.7".into(),
                port: 8080,
                priority: 0,
                proxy_protocol: None,
            }],
            health_check_interval_secs: None,
            sni_pattern: None,
            rate_limit: None,
        };
        rule_store.upsert_rule(&rule).unwrap();
        let loaded = rule_store.get_rule(rule.id).unwrap().expect("rule exists");
        assert!(loaded.rate_limit.is_none());
    }

    #[test]
    fn roundtrip_rule_with_partial_rate_limit_envelope() {
        // Only one cap dimension set — the loader must still promote
        // to Some(..), with the other six dimensions remaining None.
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        crate::store::operator_store::SqliteOperatorStore::new(Arc::clone(&store))
            .bootstrap_legacy_superadmin("test-token")
            .unwrap();
        let rule_store = SqliteRuleStore::new(store);
        let rule = Rule {
            id: RuleId(103),
            client_name: ClientName::new("edge-04".to_string()).unwrap(),
            listen_port: 443,
            listen_port_end: None,
            target_host: "10.0.0.9".into(),
            target_port: 8443,
            target_port_end: None,
            prefer_ipv6: None,
            protocol: Protocol::Tcp,
            state: RuleState::Active,
            created_at: Utc::now(),
            last_state_change_at: Utc::now(),
            owner_user_id: UserId::reserved("_legacy"),
            targets: vec![RuleTarget {
                host: "10.0.0.9".into(),
                port: 8443,
                priority: 0,
                proxy_protocol: None,
            }],
            health_check_interval_secs: None,
            sni_pattern: None,
            rate_limit: Some(RateLimit {
                concurrent_connections: Some(64),
                ..RateLimit::default()
            }),
        };
        rule_store.upsert_rule(&rule).unwrap();
        let loaded = rule_store.get_rule(rule.id).unwrap().expect("rule exists");
        let rl = loaded.rate_limit.expect("partial envelope persists");
        assert_eq!(rl.concurrent_connections, Some(64));
        assert!(rl.bandwidth_in_bps.is_none());
        assert!(rl.new_connections_per_sec.is_none());
    }

    #[test]
    fn list_rules_warns_when_missing_client_name_rows_are_skipped() {
        let buf = SharedBuf::default();
        let buf_for_writer = buf.clone();
        let subscriber = tracing_subscriber::registry().with(
            fmt::layer()
                .json()
                .with_writer(move || buf_for_writer.clone()),
        );
        let _guard = tracing::subscriber::set_default(subscriber);

        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        crate::store::operator_store::SqliteOperatorStore::new(Arc::clone(&store))
            .bootstrap_legacy_superadmin("test-token")
            .unwrap();
        let now = Utc::now().to_rfc3339();
        store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO rules (
                        id, listen_port, target_host, target_port, protocol,
                        owner_user_id, created_at, updated_at
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    params![77, 443, "127.0.0.1", 8443, "tcp", "_legacy", now, now],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .unwrap();

        let rule_store = SqliteRuleStore::new(store);
        assert!(rule_store.list_rules().unwrap().is_empty());

        let logs = buf.snapshot();
        assert!(logs.contains("skipping persisted rules with missing client_name"));
        assert!(logs.contains("\"skipped_count\":1"));
    }
}
