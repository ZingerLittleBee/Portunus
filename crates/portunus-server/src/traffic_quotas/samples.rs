//! SQLite I/O for `traffic_samples_1m` and `traffic_samples_1h`, plus
//! the query helpers used by `/v1/users/{u}/traffic` and
//! `/v1/clients/{c}/traffic`.

use crate::store::{Store, StoreError, map_rusqlite};
use rusqlite::params;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SampleBucket {
    /// 1-minute granularity, 7 day retention.
    #[serde(rename = "1m")]
    M1,
    /// 1-hour granularity, 90 day retention.
    #[serde(rename = "1h")]
    H1,
}

impl SampleBucket {
    #[must_use]
    pub const fn retention_seconds(self) -> i64 {
        match self {
            SampleBucket::M1 => 7 * 24 * 3600,
            SampleBucket::H1 => 90 * 24 * 3600,
        }
    }

    #[must_use]
    pub const fn align(self, ts_unix_sec: i64) -> i64 {
        match self {
            SampleBucket::M1 => ts_unix_sec - (ts_unix_sec % 60),
            SampleBucket::H1 => ts_unix_sec - (ts_unix_sec % 3600),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TrafficSample {
    pub ts: i64,
    pub bytes_in: i64,
    pub bytes_out: i64,
}

/// UPSERT delta into the 1m bucket for the given minute. Caller should
/// pre-align `ts_minute` to a minute boundary.
pub fn upsert_1m_delta(
    store: &Store,
    user_id: &str,
    client_id: &str,
    client_name: &str,
    ts_minute: i64,
    delta_in: i64,
    delta_out: i64,
) -> Result<(), StoreError> {
    store.with_conn(|c| {
        c.execute(
            "INSERT INTO traffic_samples_1m (user_id, client_id, client_name, ts_minute, bytes_in, bytes_out)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(user_id, client_id, ts_minute) DO UPDATE
               SET bytes_in  = bytes_in  + excluded.bytes_in,
                   bytes_out = bytes_out + excluded.bytes_out",
            params![user_id, client_id, client_name, ts_minute, delta_in, delta_out],
        )
        .map_err(map_rusqlite)?;
        Ok(())
    })
}

/// Roll up all 1m rows in `[ts_hour, ts_hour + 3600)` into one 1h row
/// per (user, client). Idempotent (uses ON CONFLICT REPLACE on the
/// destination).
pub fn rollup_hour(store: &Store, ts_hour: i64) -> Result<(), StoreError> {
    store.with_conn(|c| {
        c.execute(
            "INSERT INTO traffic_samples_1h (user_id, client_id, client_name, ts_hour, bytes_in, bytes_out)
             SELECT user_id, client_id, MIN(client_name) AS client_name, ?1 AS ts_hour,
                    COALESCE(SUM(bytes_in), 0)  AS bytes_in,
                    COALESCE(SUM(bytes_out), 0) AS bytes_out
             FROM traffic_samples_1m
             WHERE ts_minute >= ?1 AND ts_minute < ?2
             GROUP BY user_id, client_id
             ON CONFLICT(user_id, client_id, ts_hour) DO UPDATE
               SET bytes_in  = excluded.bytes_in,
                   bytes_out = excluded.bytes_out",
            params![ts_hour, ts_hour + 3600],
        )
        .map_err(map_rusqlite)?;
        Ok(())
    })
}

pub fn delete_1m_older_than(store: &Store, threshold_unix_sec: i64) -> Result<usize, StoreError> {
    store.with_conn(|c| {
        let n = c
            .execute(
                "DELETE FROM traffic_samples_1m WHERE ts_minute < ?1",
                params![threshold_unix_sec],
            )
            .map_err(map_rusqlite)?;
        Ok(n)
    })
}

pub fn delete_1h_older_than(store: &Store, threshold_unix_sec: i64) -> Result<usize, StoreError> {
    store.with_conn(|c| {
        let n = c
            .execute(
                "DELETE FROM traffic_samples_1h WHERE ts_hour < ?1",
                params![threshold_unix_sec],
            )
            .map_err(map_rusqlite)?;
        Ok(n)
    })
}

pub fn get_last_rolled_up_hour(store: &Store) -> Result<i64, StoreError> {
    store.with_conn(|c| {
        let v: i64 = c
            .query_row(
                "SELECT last_rolled_up_hour FROM traffic_rollup_state WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .map_err(map_rusqlite)?;
        Ok(v)
    })
}

pub fn set_last_rolled_up_hour(store: &Store, ts_hour: i64) -> Result<(), StoreError> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE traffic_rollup_state SET last_rolled_up_hour = ?1 WHERE id = 1",
            params![ts_hour],
        )
        .map_err(map_rusqlite)?;
        Ok(())
    })
}

/// Query the chosen bucket, optionally filtered by user / client.
/// `from_unix_sec` and `to_unix_sec` are inclusive lower, exclusive
/// upper. Aggregates across rows when a filter is partial.
pub fn query_samples(
    store: &Store,
    bucket: SampleBucket,
    user_id: Option<&str>,
    client_id: Option<&str>,
    from_unix_sec: i64,
    to_unix_sec: i64,
) -> Result<Vec<TrafficSample>, StoreError> {
    let (table, ts_col) = match bucket {
        SampleBucket::M1 => ("traffic_samples_1m", "ts_minute"),
        SampleBucket::H1 => ("traffic_samples_1h", "ts_hour"),
    };
    let mut sql = format!(
        "SELECT {ts_col} AS ts, SUM(bytes_in) AS bin, SUM(bytes_out) AS bout
           FROM {table}
          WHERE {ts_col} >= ?1 AND {ts_col} < ?2"
    );
    let next_param_index = |i: &mut usize| -> String {
        *i += 1;
        format!("?{i}")
    };
    let mut idx = 2;
    if user_id.is_some() {
        sql.push_str(&format!(" AND user_id = {}", next_param_index(&mut idx)));
    }
    if client_id.is_some() {
        sql.push_str(&format!(" AND client_id = {}", next_param_index(&mut idx)));
    }
    sql.push_str(&format!(" GROUP BY {ts_col} ORDER BY {ts_col} ASC"));

    let user_id = user_id.map(str::to_string);
    let client_id = client_id.map(str::to_string);
    store.with_conn(move |c| {
        let mut stmt = c.prepare(&sql).map_err(map_rusqlite)?;
        let map_row = |r: &rusqlite::Row| -> rusqlite::Result<TrafficSample> {
            Ok(TrafficSample {
                ts: r.get(0)?,
                bytes_in: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                bytes_out: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            })
        };
        let rows: Vec<TrafficSample> = match (user_id.as_deref(), client_id.as_deref()) {
            (None, None) => stmt
                .query_map(params![from_unix_sec, to_unix_sec], map_row)
                .map_err(map_rusqlite)?
                .collect::<Result<_, _>>()
                .map_err(map_rusqlite)?,
            (Some(u), None) => stmt
                .query_map(params![from_unix_sec, to_unix_sec, u], map_row)
                .map_err(map_rusqlite)?
                .collect::<Result<_, _>>()
                .map_err(map_rusqlite)?,
            (None, Some(cn)) => stmt
                .query_map(params![from_unix_sec, to_unix_sec, cn], map_row)
                .map_err(map_rusqlite)?
                .collect::<Result<_, _>>()
                .map_err(map_rusqlite)?,
            (Some(u), Some(cn)) => stmt
                .query_map(params![from_unix_sec, to_unix_sec, u, cn], map_row)
                .map_err(map_rusqlite)?
                .collect::<Result<_, _>>()
                .map_err(map_rusqlite)?,
        };
        Ok(rows)
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

    #[test]
    fn bucket_align_minute() {
        // 123 mod 60 = 3 -> floor to 120
        assert_eq!(SampleBucket::M1.align(123), 120);
        assert_eq!(SampleBucket::M1.align(60), 60);
        assert_eq!(SampleBucket::M1.align(59), 0);
        assert_eq!(SampleBucket::M1.align(0), 0);
    }

    #[test]
    fn bucket_align_hour() {
        assert_eq!(SampleBucket::H1.align(3700), 3600);
        assert_eq!(SampleBucket::H1.align(3600), 3600);
        assert_eq!(SampleBucket::H1.align(0), 0);
    }

    #[test]
    fn retention_seconds_constants() {
        assert_eq!(SampleBucket::M1.retention_seconds(), 604_800);
        assert_eq!(SampleBucket::H1.retention_seconds(), 7_776_000);
    }

    #[test]
    fn upsert_1m_delta_is_additive() {
        let (_d, store) = make_store();
        upsert_1m_delta(&store, "u", "c", "c", 120, 100, 200).unwrap();
        upsert_1m_delta(&store, "u", "c", "c", 120, 50, 75).unwrap();
        let rows = query_samples(&store, SampleBucket::M1, Some("u"), Some("c"), 0, 180).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].bytes_in, 150);
        assert_eq!(rows[0].bytes_out, 275);
    }

    #[test]
    fn rollup_hour_aggregates_minutes() {
        let (_d, store) = make_store();
        for m in 0..60 {
            upsert_1m_delta(&store, "u", "c", "c", m * 60, 10, 20).unwrap();
        }
        rollup_hour(&store, 0).unwrap();
        let rows = query_samples(&store, SampleBucket::H1, Some("u"), Some("c"), 0, 3600).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].bytes_in, 600);
        assert_eq!(rows[0].bytes_out, 1200);
    }

    #[test]
    fn rollup_hour_is_idempotent_on_rerun() {
        let (_d, store) = make_store();
        upsert_1m_delta(&store, "u", "c", "c", 0, 10, 20).unwrap();
        rollup_hour(&store, 0).unwrap();
        rollup_hour(&store, 0).unwrap();
        let rows = query_samples(&store, SampleBucket::H1, Some("u"), Some("c"), 0, 3600).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].bytes_in, 10);
        assert_eq!(rows[0].bytes_out, 20);
    }

    #[test]
    fn delete_old_removes_only_old_rows() {
        let (_d, store) = make_store();
        upsert_1m_delta(&store, "u", "c", "c", 60, 1, 1).unwrap();
        upsert_1m_delta(&store, "u", "c", "c", 1000, 2, 2).unwrap();
        let removed = delete_1m_older_than(&store, 100).unwrap();
        assert_eq!(removed, 1);
        let rows = query_samples(&store, SampleBucket::M1, None, None, 0, 2000).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts, 1000);
    }

    #[test]
    fn query_aggregates_across_users_when_filter_omits_user() {
        let (_d, store) = make_store();
        upsert_1m_delta(&store, "alice", "edge-01", "edge-01", 60, 100, 200).unwrap();
        upsert_1m_delta(&store, "bob", "edge-01", "edge-01", 60, 50, 75).unwrap();
        // No user filter, single client filter -> rows sum across users.
        let rows = query_samples(&store, SampleBucket::M1, None, Some("edge-01"), 0, 120).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].bytes_in, 150);
        assert_eq!(rows[0].bytes_out, 275);
    }

    #[test]
    fn rollup_state_get_and_set() {
        let (_d, store) = make_store();
        assert_eq!(get_last_rolled_up_hour(&store).unwrap(), 0);
        set_last_rolled_up_hour(&store, 3600).unwrap();
        assert_eq!(get_last_rolled_up_hour(&store).unwrap(), 3600);
    }
}
