//! SQLite CRUD for `traffic_quotas`. Pure data access — period math
//! and aggregation logic live in the parent module / sibling modules.

use crate::store::{Store, StoreError, map_rusqlite};
use crate::traffic_quotas::TrafficQuotaRow;
use rusqlite::{OptionalExtension, params};

pub fn insert_or_replace(store: &Store, row: &TrafficQuotaRow) -> Result<(), StoreError> {
    store.with_conn(|c| {
        c.execute(
            "INSERT OR REPLACE INTO traffic_quotas (
                user_id, client_name, monthly_bytes, billing_anchor,
                current_period_started_at, current_period_bytes_used,
                exhausted_at, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.user_id,
                row.client_name,
                row.monthly_bytes,
                row.billing_anchor,
                row.current_period_started_at,
                row.current_period_bytes_used,
                row.exhausted_at,
                row.created_at,
                row.updated_at,
            ],
        )
        .map_err(map_rusqlite)?;
        Ok(())
    })
}

pub fn get(
    store: &Store,
    user_id: &str,
    client_name: &str,
) -> Result<Option<TrafficQuotaRow>, StoreError> {
    store.with_conn(|c| {
        let row = c
            .query_row(
                "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                        current_period_started_at, current_period_bytes_used,
                        exhausted_at, created_at, updated_at
                 FROM traffic_quotas
                 WHERE user_id = ?1 AND client_name = ?2",
                params![user_id, client_name],
                row_to_quota,
            )
            .optional()
            .map_err(map_rusqlite)?;
        Ok(row)
    })
}

pub fn delete(store: &Store, user_id: &str, client_name: &str) -> Result<bool, StoreError> {
    store.with_conn(|c| {
        let n = c
            .execute(
                "DELETE FROM traffic_quotas WHERE user_id = ?1 AND client_name = ?2",
                params![user_id, client_name],
            )
            .map_err(map_rusqlite)?;
        Ok(n > 0)
    })
}

pub fn list_for_user(store: &Store, user_id: &str) -> Result<Vec<TrafficQuotaRow>, StoreError> {
    store.with_conn(|c| {
        let mut stmt = c
            .prepare(
                "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                        current_period_started_at, current_period_bytes_used,
                        exhausted_at, created_at, updated_at
                 FROM traffic_quotas WHERE user_id = ?1",
            )
            .map_err(map_rusqlite)?;
        let rows: Vec<TrafficQuotaRow> = stmt
            .query_map(params![user_id], row_to_quota)
            .map_err(map_rusqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_rusqlite)?;
        Ok(rows)
    })
}

pub fn list_for_client(
    store: &Store,
    client_name: &str,
) -> Result<Vec<TrafficQuotaRow>, StoreError> {
    store.with_conn(|c| {
        let mut stmt = c
            .prepare(
                "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                        current_period_started_at, current_period_bytes_used,
                        exhausted_at, created_at, updated_at
                 FROM traffic_quotas WHERE client_name = ?1",
            )
            .map_err(map_rusqlite)?;
        let rows: Vec<TrafficQuotaRow> = stmt
            .query_map(params![client_name], row_to_quota)
            .map_err(map_rusqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_rusqlite)?;
        Ok(rows)
    })
}

pub fn list_all(store: &Store) -> Result<Vec<TrafficQuotaRow>, StoreError> {
    store.with_conn(|c| {
        let mut stmt = c
            .prepare(
                "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                        current_period_started_at, current_period_bytes_used,
                        exhausted_at, created_at, updated_at FROM traffic_quotas",
            )
            .map_err(map_rusqlite)?;
        let rows: Vec<TrafficQuotaRow> = stmt
            .query_map([], row_to_quota)
            .map_err(map_rusqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_rusqlite)?;
        Ok(rows)
    })
}

/// Atomically accumulate `delta_bytes` into `current_period_bytes_used`.
/// Sets `exhausted_at = now_unix_sec` on the first cumulative crossing
/// of `monthly_bytes` (subsequent crossings preserve the original
/// timestamp). Returns the post-update row if the pair existed.
pub fn accumulate_bytes_used(
    store: &Store,
    user_id: &str,
    client_name: &str,
    delta_bytes: i64,
    now_unix_sec: i64,
) -> Result<Option<TrafficQuotaRow>, StoreError> {
    store.with_conn(|c| {
        let updated = c
            .execute(
                "UPDATE traffic_quotas
                    SET current_period_bytes_used = current_period_bytes_used + ?3,
                        exhausted_at = CASE
                            WHEN exhausted_at IS NOT NULL THEN exhausted_at
                            WHEN (current_period_bytes_used + ?3) >= monthly_bytes THEN ?4
                            ELSE NULL
                        END,
                        updated_at = ?4
                  WHERE user_id = ?1 AND client_name = ?2",
                params![user_id, client_name, delta_bytes, now_unix_sec],
            )
            .map_err(map_rusqlite)?;
        if updated == 0 {
            return Ok(None);
        }
        let row = c
            .query_row(
                "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                        current_period_started_at, current_period_bytes_used,
                        exhausted_at, created_at, updated_at
                 FROM traffic_quotas
                 WHERE user_id = ?1 AND client_name = ?2",
                params![user_id, client_name],
                row_to_quota,
            )
            .optional()
            .map_err(map_rusqlite)?;
        Ok(row)
    })
}

/// Advance the period boundary (used by the rollover tick).
pub fn reset_period(
    store: &Store,
    user_id: &str,
    client_name: &str,
    new_period_started_at: i64,
    now_unix_sec: i64,
) -> Result<Option<TrafficQuotaRow>, StoreError> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE traffic_quotas
                SET current_period_started_at = ?3,
                    current_period_bytes_used = 0,
                    exhausted_at = NULL,
                    updated_at = ?4
              WHERE user_id = ?1 AND client_name = ?2",
            params![user_id, client_name, new_period_started_at, now_unix_sec],
        )
        .map_err(map_rusqlite)?;
        let row = c
            .query_row(
                "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                        current_period_started_at, current_period_bytes_used,
                        exhausted_at, created_at, updated_at
                 FROM traffic_quotas
                 WHERE user_id = ?1 AND client_name = ?2",
                params![user_id, client_name],
                row_to_quota,
            )
            .optional()
            .map_err(map_rusqlite)?;
        Ok(row)
    })
}

/// Zero out `current_period_bytes_used` without changing period boundaries
/// (admin "clear usage" action).
pub fn clear_period_usage(
    store: &Store,
    user_id: &str,
    client_name: &str,
    now_unix_sec: i64,
) -> Result<Option<TrafficQuotaRow>, StoreError> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE traffic_quotas
                SET current_period_bytes_used = 0,
                    exhausted_at = NULL,
                    updated_at = ?3
              WHERE user_id = ?1 AND client_name = ?2",
            params![user_id, client_name, now_unix_sec],
        )
        .map_err(map_rusqlite)?;
        let row = c
            .query_row(
                "SELECT user_id, client_name, monthly_bytes, billing_anchor,
                        current_period_started_at, current_period_bytes_used,
                        exhausted_at, created_at, updated_at
                 FROM traffic_quotas
                 WHERE user_id = ?1 AND client_name = ?2",
                params![user_id, client_name],
                row_to_quota,
            )
            .optional()
            .map_err(map_rusqlite)?;
        Ok(row)
    })
}

fn row_to_quota(r: &rusqlite::Row) -> rusqlite::Result<TrafficQuotaRow> {
    Ok(TrafficQuotaRow {
        user_id: r.get(0)?,
        client_name: r.get(1)?,
        monthly_bytes: r.get(2)?,
        billing_anchor: r.get(3)?,
        current_period_started_at: r.get(4)?,
        current_period_bytes_used: r.get(5)?,
        exhausted_at: r.get(6)?,
        created_at: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use tempfile::tempdir;

    fn make_store() -> (tempfile::TempDir, Store) {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).expect("open store");
        (dir, store)
    }

    fn sample_row() -> TrafficQuotaRow {
        TrafficQuotaRow {
            user_id: "alice".into(),
            client_name: "edge-01".into(),
            monthly_bytes: 1_000_000,
            billing_anchor: 1_704_067_200,
            current_period_started_at: 1_704_067_200,
            current_period_bytes_used: 0,
            exhausted_at: None,
            created_at: 1_704_067_200,
            updated_at: 1_704_067_200,
        }
    }

    #[test]
    fn insert_and_get_roundtrip() {
        let (_d, store) = make_store();
        let row = sample_row();
        insert_or_replace(&store, &row).unwrap();
        let got = get(&store, "alice", "edge-01").unwrap().unwrap();
        assert_eq!(got, row);
    }

    #[test]
    fn get_returns_none_for_missing() {
        let (_d, store) = make_store();
        assert!(get(&store, "alice", "edge-01").unwrap().is_none());
    }

    #[test]
    fn delete_returns_true_when_existed_then_false() {
        let (_d, store) = make_store();
        insert_or_replace(&store, &sample_row()).unwrap();
        assert!(delete(&store, "alice", "edge-01").unwrap());
        assert!(!delete(&store, "alice", "edge-01").unwrap());
    }

    #[test]
    fn accumulate_advances_bytes_used() {
        let (_d, store) = make_store();
        insert_or_replace(&store, &sample_row()).unwrap();
        let after = accumulate_bytes_used(&store, "alice", "edge-01", 100, 1_704_067_500)
            .unwrap()
            .unwrap();
        assert_eq!(after.current_period_bytes_used, 100);
        assert!(after.exhausted_at.is_none());
    }

    #[test]
    fn accumulate_sets_exhausted_at_on_first_crossing() {
        let (_d, store) = make_store();
        let mut row = sample_row();
        row.monthly_bytes = 100;
        insert_or_replace(&store, &row).unwrap();
        // First crossing: 0 + 200 = 200 >= 100 -> set exhausted_at
        let after = accumulate_bytes_used(&store, "alice", "edge-01", 200, 999)
            .unwrap()
            .unwrap();
        assert_eq!(after.exhausted_at, Some(999));
        // Second crossing: 200 + 50 = 250 >= 100 -> preserves original timestamp
        let after2 = accumulate_bytes_used(&store, "alice", "edge-01", 50, 1000)
            .unwrap()
            .unwrap();
        assert_eq!(after2.exhausted_at, Some(999));
    }

    #[test]
    fn accumulate_returns_none_for_missing_row() {
        let (_d, store) = make_store();
        assert!(
            accumulate_bytes_used(&store, "ghost", "edge-01", 100, 0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn reset_period_clears_used_and_exhausted_and_advances_start() {
        let (_d, store) = make_store();
        let mut row = sample_row();
        row.monthly_bytes = 100;
        row.current_period_bytes_used = 200;
        row.exhausted_at = Some(500);
        insert_or_replace(&store, &row).unwrap();
        let after = reset_period(&store, "alice", "edge-01", 2_000_000, 2_000_010)
            .unwrap()
            .unwrap();
        assert_eq!(after.current_period_started_at, 2_000_000);
        assert_eq!(after.current_period_bytes_used, 0);
        assert!(after.exhausted_at.is_none());
        assert_eq!(after.updated_at, 2_000_010);
    }

    #[test]
    fn clear_period_usage_does_not_change_period_start() {
        let (_d, store) = make_store();
        let mut row = sample_row();
        row.current_period_bytes_used = 999;
        row.exhausted_at = Some(123);
        let original_start = row.current_period_started_at;
        insert_or_replace(&store, &row).unwrap();
        let after = clear_period_usage(&store, "alice", "edge-01", 3000)
            .unwrap()
            .unwrap();
        assert_eq!(after.current_period_bytes_used, 0);
        assert!(after.exhausted_at.is_none());
        assert_eq!(after.current_period_started_at, original_start);
    }

    #[test]
    fn list_for_user_and_client_filter_correctly() {
        let (_d, store) = make_store();
        let mut a = sample_row();
        a.user_id = "alice".into();
        a.client_name = "edge-01".into();
        let mut b = sample_row();
        b.user_id = "alice".into();
        b.client_name = "edge-02".into();
        let mut c = sample_row();
        c.user_id = "bob".into();
        c.client_name = "edge-01".into();
        insert_or_replace(&store, &a).unwrap();
        insert_or_replace(&store, &b).unwrap();
        insert_or_replace(&store, &c).unwrap();

        let alice_rows = list_for_user(&store, "alice").unwrap();
        assert_eq!(alice_rows.len(), 2);

        let edge01_rows = list_for_client(&store, "edge-01").unwrap();
        assert_eq!(edge01_rows.len(), 2);

        let all = list_all(&store).unwrap();
        assert_eq!(all.len(), 3);
    }
}
