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
                    "SELECT ts, user_id, outcome, action, resource_kind, resource_value, details_json \
                     FROM audit WHERE outcome = ? \
                     ORDER BY ts DESC, seq DESC LIMIT ?"
                }
                None => {
                    "SELECT ts, user_id, outcome, action, resource_kind, resource_value, details_json \
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
                let resource_kind: Option<String> = r.get(4)?;
                let resource_value: Option<String> = r.get(5)?;
                let details_json: String = r.get(6)?;

                let timestamp = DateTime::parse_from_rfc3339(&ts)
                    .map_or_else(|_| Utc::now(), |dt| dt.with_timezone(&Utc));
                let outcome = AuditOutcome::parse(&outcome_str).unwrap_or(AuditOutcome::Allow);
                let (method, path) = split_action(&action);
                let (role, reason, details) = parse_details(&details_json);
                let action_field = (!action.contains(' ')).then_some(action);
                Ok(AuditEntry {
                    timestamp,
                    actor: user_id.unwrap_or_default(),
                    role,
                    method,
                    path,
                    outcome,
                    reason,
                    action: action_field,
                    resource_kind,
                    resource_value,
                    details,
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

/// 008-sqlite-storage T075 — envelope read.
///
/// Returns the matching audit rows newest-first plus an opaque
/// `next_cursor` if more rows match the filter beyond `limit`. Cursor
/// encoding is base64(`seq` integer), opaque to callers — they MUST
/// pass it back unchanged on the next page.
#[derive(Debug, Clone)]
pub struct AuditPage {
    pub rows: Vec<AuditEntry>,
    pub next_cursor: Option<String>,
    pub last_seq: Option<i64>,
}

#[derive(Debug, Default, Clone)]
pub struct AuditQuery {
    pub limit: usize,
    pub outcome: Option<AuditOutcome>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Decoded `seq` from the operator-provided cursor. Pages walk
    /// strictly older than this seq.
    pub before_seq: Option<i64>,
}

impl Store {
    pub fn query_audit_envelope(&self, q: &AuditQuery) -> Result<AuditPage, StoreError> {
        let limit = q.limit.max(1);
        let limit_plus_one: i64 = (limit as i64).saturating_add(1);

        self.with_conn(|c| {
            // Build the WHERE clause + bound params dynamically. Uses the
            // (outcome, ts) and (ts, seq) indexes for the common cases.
            let mut where_clauses: Vec<&'static str> = Vec::new();
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
            if let Some(o) = q.outcome {
                where_clauses.push("outcome = ?");
                params.push(Box::new(o.as_str().to_string()));
            }
            if let Some(s) = q.since {
                where_clauses.push("ts >= ?");
                params.push(Box::new(s.to_rfc3339()));
            }
            if let Some(u) = q.until {
                where_clauses.push("ts <= ?");
                params.push(Box::new(u.to_rfc3339()));
            }
            if let Some(seq) = q.before_seq {
                where_clauses.push("seq < ?");
                params.push(Box::new(seq));
            }
            let where_sql = if where_clauses.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", where_clauses.join(" AND "))
            };
            let sql = format!(
                "SELECT seq, ts, user_id, outcome, action, resource_kind, resource_value, details_json \
                 FROM audit{where_sql} \
                 ORDER BY ts DESC, seq DESC LIMIT ?"
            );
            params.push(Box::new(limit_plus_one));

            let mut stmt = c.prepare(&sql).map_err(map_rusqlite)?;
            let mapper = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(i64, AuditEntry)> {
                let seq: i64 = r.get(0)?;
                let ts: String = r.get(1)?;
                let user_id: Option<String> = r.get(2)?;
                let outcome_str: String = r.get(3)?;
                let action: String = r.get(4)?;
                let resource_kind: Option<String> = r.get(5)?;
                let resource_value: Option<String> = r.get(6)?;
                let details_json: String = r.get(7)?;

                let timestamp = DateTime::parse_from_rfc3339(&ts)
                    .map_or_else(|_| Utc::now(), |dt| dt.with_timezone(&Utc));
                let outcome = AuditOutcome::parse(&outcome_str).unwrap_or(AuditOutcome::Allow);
                let (method, path) = split_action(&action);
                let (role, reason, details) = parse_details(&details_json);
                let action_field = (!action.contains(' ')).then_some(action);
                Ok((
                    seq,
                    AuditEntry {
                        timestamp,
                        actor: user_id.unwrap_or_default(),
                        role,
                        method,
                        path,
                        outcome,
                        reason,
                        action: action_field,
                        resource_kind,
                        resource_value,
                        details,
                    },
                ))
            };
            let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(AsRef::as_ref).collect();
            let iter = stmt
                .query_map(rusqlite::params_from_iter(params_refs), mapper)
                .map_err(map_rusqlite)?;
            let mut pairs = Vec::with_capacity(limit + 1);
            for r in iter {
                pairs.push(r.map_err(map_rusqlite)?);
            }
            // The +1 sentinel tells us whether there's a next page.
            let has_more = pairs.len() > limit;
            if has_more {
                pairs.truncate(limit);
            }
            let last_seq = pairs.last().map(|(seq, _)| *seq);
            let next_cursor = if has_more {
                last_seq.map(encode_cursor)
            } else {
                None
            };
            let rows = pairs.into_iter().map(|(_, e)| e).collect();
            Ok(AuditPage {
                rows,
                next_cursor,
                last_seq,
            })
        })
    }
}

impl Store {
    /// 008-sqlite-storage T076 — `audit prune --before <RFC3339>`.
    /// Returns the number of rows deleted (or that would be deleted in
    /// `--dry-run`). Caller wraps in BEGIN IMMEDIATE.
    pub fn audit_prune_count(&self, before: DateTime<Utc>) -> Result<u64, StoreError> {
        self.with_conn(|c| {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM audit WHERE ts < ?",
                    rusqlite::params![before.to_rfc3339()],
                    |r| r.get(0),
                )
                .map_err(map_rusqlite)?;
            Ok(n.max(0) as u64)
        })
    }

    pub fn audit_prune_apply(&self, before: DateTime<Utc>) -> Result<u64, StoreError> {
        self.with_write_tx(|tx| {
            let n = tx
                .execute(
                    "DELETE FROM audit WHERE ts < ?",
                    rusqlite::params![before.to_rfc3339()],
                )
                .map_err(map_rusqlite)?;
            Ok(n as u64)
        })
    }
}

/// Base64-url(no-pad) of the seq integer's decimal string. Opaque to
/// callers; deliberately not JWT / not signed — operators see it only
/// through the `next_cursor` field.
#[must_use]
pub fn encode_cursor(seq: i64) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(seq.to_string().as_bytes())
}

/// Inverse of [`encode_cursor`]. Returns `None` for malformed input —
/// callers MUST surface that as `invalid_cursor` HTTP 400.
#[must_use]
pub fn decode_cursor(s: &str) -> Option<i64> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()?;
    let txt = std::str::from_utf8(&bytes).ok()?;
    txt.parse::<i64>().ok()
}

fn split_action(action: &str) -> (String, String) {
    if let Some((method, path)) = action.split_once(' ') {
        (method.to_string(), path.to_string())
    } else {
        (String::new(), action.to_string())
    }
}

fn parse_details(
    s: &str,
) -> (
    Option<portunus_auth::OperatorRole>,
    Option<String>,
    Option<serde_json::Value>,
) {
    let mut v: serde_json::Value = serde_json::from_str(s).unwrap_or(serde_json::Value::Null);
    let role = v
        .get("role")
        .and_then(|r| serde_json::from_value(r.clone()).ok());
    let reason = v
        .get("reason")
        .and_then(|r| r.as_str())
        .map(ToString::to_string);
    if let Some(obj) = v.as_object_mut() {
        obj.remove("role");
        obj.remove("reason");
        if obj.is_empty() {
            return (role, reason, None);
        }
    }
    (role, reason, Some(v))
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
