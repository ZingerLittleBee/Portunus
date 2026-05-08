//! 008-sqlite-storage T033 / T075 — audit table read path.
//!
//! `Store::query_audit_recent` is the v0.7-shape read used by
//! `GET /v1/audit?limit=&outcome=` (no envelope). Returns newest-first
//! `AuditEntry` rows reconstructed from the durable table.
//!
//! T075 lands the envelope variant (`since` / `until` / `cursor`
//! pagination) on top of this same module.

use chrono::{DateTime, Utc};

use crate::operator::audit::{AuditEntry, AuditOutcome};
use crate::store::{Store, StoreError, map_rusqlite};

impl Store {
    /// Newest-first slice of the audit table. Mirrors the v0.6 ring
    /// buffer's snapshot signature 1:1 so the v0.7-shape handler in
    /// `audit_http.rs` can swap to the durable read with a one-line
    /// change.
    pub fn query_audit_recent(
        &self,
        limit: usize,
        outcome_filter: Option<AuditOutcome>,
    ) -> Result<Vec<AuditEntry>, StoreError> {
        self.with_conn(|c| {
            let limit_i: i64 = limit.try_into().unwrap_or(i64::MAX);
            let mut rows = Vec::with_capacity(limit.min(1024));
            // Use the `audit_outcome_ts_idx` when filtering, otherwise
            // `audit_ts_idx`. Both indexes are DESC so SQLite walks
            // them without an ORDER BY sort.
            let sql = match outcome_filter {
                Some(_) => {
                    "SELECT ts, user_id, outcome, action, details_json \
                     FROM audit WHERE outcome = ? \
                     ORDER BY ts DESC, seq DESC LIMIT ?"
                }
                None => {
                    "SELECT ts, user_id, outcome, action, details_json \
                     FROM audit \
                     ORDER BY ts DESC, seq DESC LIMIT ?"
                }
            };
            let mut stmt = c.prepare(sql).map_err(map_rusqlite)?;
            let mapper = |r: &rusqlite::Row<'_>| -> rusqlite::Result<AuditEntry> {
                let ts: String = r.get(0)?;
                let user_id: Option<String> = r.get(1)?;
                let outcome_str: String = r.get(2)?;
                let action: String = r.get(3)?;
                let details_json: String = r.get(4)?;

                let timestamp = DateTime::parse_from_rfc3339(&ts)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let outcome = AuditOutcome::parse(&outcome_str).unwrap_or(AuditOutcome::Allow);
                let (method, path) = split_action(&action);
                let (role, reason) = parse_details(&details_json);
                Ok(AuditEntry {
                    timestamp,
                    actor: user_id.unwrap_or_default(),
                    role,
                    method,
                    path,
                    outcome,
                    reason,
                })
            };
            let iter = if let Some(o) = outcome_filter {
                stmt.query_map(rusqlite::params![o.as_str(), limit_i], mapper)
                    .map_err(map_rusqlite)?
                    .collect::<Vec<_>>()
            } else {
                stmt.query_map(rusqlite::params![limit_i], mapper)
                    .map_err(map_rusqlite)?
                    .collect::<Vec<_>>()
            };
            for r in iter {
                rows.push(r.map_err(map_rusqlite)?);
            }
            Ok(rows)
        })
    }
}

fn split_action(action: &str) -> (String, String) {
    if let Some((method, path)) = action.split_once(' ') {
        (method.to_string(), path.to_string())
    } else {
        (String::new(), action.to_string())
    }
}

fn parse_details(s: &str) -> (Option<forward_auth::OperatorRole>, Option<String>) {
    let v: serde_json::Value = serde_json::from_str(s).unwrap_or(serde_json::Value::Null);
    let role = v
        .get("role")
        .and_then(|r| serde_json::from_value(r.clone()).ok());
    let reason = v
        .get("reason")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string());
    (role, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::audit::AuditOutcome;
    use crate::store::Store;
    use chrono::Utc;
    use tempfile::tempdir;

    fn insert(store: &Store, when: chrono::DateTime<Utc>, outcome: AuditOutcome, actor: &str) {
        store
            .with_write_tx(|tx| {
                tx.execute(
                    "INSERT INTO audit \
                     (ts, user_id, outcome, action, resource_kind, resource_value, correlation_id, details_json) \
                     VALUES (?, ?, ?, ?, NULL, NULL, '', '{}')",
                    rusqlite::params![when.to_rfc3339(), actor, outcome.as_str(), "GET /v1/audit"],
                )
                .map_err(map_rusqlite)?;
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn newest_first_no_filter() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let t0 = Utc::now() - chrono::Duration::seconds(30);
        let t1 = Utc::now() - chrono::Duration::seconds(20);
        let t2 = Utc::now() - chrono::Duration::seconds(10);
        insert(&store, t0, AuditOutcome::Allow, "alice");
        insert(&store, t1, AuditOutcome::Deny, "bob");
        insert(&store, t2, AuditOutcome::Allow, "carol");

        let rows = store.query_audit_recent(10, None).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].actor, "carol");
        assert_eq!(rows[1].actor, "bob");
        assert_eq!(rows[2].actor, "alice");
    }

    #[test]
    fn outcome_filter_works() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let now = Utc::now();
        insert(&store, now, AuditOutcome::Allow, "alice");
        insert(&store, now, AuditOutcome::Deny, "bob");
        insert(&store, now, AuditOutcome::Deny, "eve");

        let denies = store
            .query_audit_recent(10, Some(AuditOutcome::Deny))
            .unwrap();
        assert_eq!(denies.len(), 2);
        assert!(denies.iter().all(|e| e.outcome == AuditOutcome::Deny));

        let allows = store
            .query_audit_recent(10, Some(AuditOutcome::Allow))
            .unwrap();
        assert_eq!(allows.len(), 1);
    }

    #[test]
    fn limit_is_respected() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let base = Utc::now() - chrono::Duration::seconds(60);
        for i in 0..20 {
            insert(
                &store,
                base + chrono::Duration::seconds(i),
                AuditOutcome::Allow,
                &format!("u-{i}"),
            );
        }
        let rows = store.query_audit_recent(5, None).unwrap();
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn empty_table_returns_empty_vec() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        assert!(store.query_audit_recent(10, None).unwrap().is_empty());
    }
}
