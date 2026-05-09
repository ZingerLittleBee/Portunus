use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use forward_auth::UserId;
use forward_core::{ClientName, RuleId, RuleTarget};
use rusqlite::params;

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
            tx.execute(
                "INSERT INTO rules (
                    id, client_name, listen_port, listen_port_end, target_host, target_port,
                    target_port_end, prefer_ipv6, protocol, state_kind, state_reason,
                    owner_user_id, health_check_interval_secs, created_at, updated_at, sni_pattern
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                    sni_pattern = excluded.sni_pattern",
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
                            forward_core::ProxyProtocolVersion::V1 => "v1",
                            forward_core::ProxyProtocolVersion::V2 => "v2",
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
            let mut stmt = conn
                .prepare(
                    "SELECT id, client_name, listen_port, listen_port_end, target_host, target_port,
                            target_port_end, prefer_ipv6, protocol, state_kind, state_reason,
                            owner_user_id, health_check_interval_secs, created_at, updated_at, sni_pattern
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
                Some("v2") => Some(forward_core::ProxyProtocolVersion::V2),
                Some("v1") => Some(forward_core::ProxyProtocolVersion::V1),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
                    proxy_protocol: Some(forward_core::ProxyProtocolVersion::V2),
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
        };

        rule_store.upsert_rule(&rule).unwrap();
        let loaded = rule_store.get_rule(rule.id).unwrap().expect("rule exists");
        assert_eq!(loaded.client_name, rule.client_name);
        assert_eq!(loaded.prefer_ipv6, Some(true));
        assert_eq!(loaded.sni_pattern.as_deref(), Some("api.example.com"));
        assert_eq!(loaded.targets.len(), 2);
        assert_eq!(
            loaded.targets[0].proxy_protocol,
            Some(forward_core::ProxyProtocolVersion::V2)
        );
        assert_eq!(loaded.targets[1].proxy_protocol, None);
    }
}
