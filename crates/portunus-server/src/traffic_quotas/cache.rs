//! In-memory cache of active `traffic_quotas` rows. Refreshed from
//! SQLite on construction and on every write through quota CRUD. The
//! aggregator reads + writes through the cache (so StatsReport
//! accumulation does not block on SQLite each tick); the cache calls
//! into the store on mutating ops to keep both in sync.

use crate::store::{Store, StoreError};
use crate::traffic_quotas::{TrafficQuotaRow, store as quota_store};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::warn;

#[derive(Clone)]
pub struct TrafficQuotaCache {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    cache: RwLock<HashMap<(String, String), TrafficQuotaRow>>,
}

impl TrafficQuotaCache {
    /// Build a fresh cache by loading every persisted quota row.
    pub fn load(store: Store) -> Result<Self, StoreError> {
        let rows = quota_store::list_all(&store)?;
        let mut map = HashMap::with_capacity(rows.len());
        for row in rows {
            map.insert((row.user_id.clone(), row.client_name.clone()), row);
        }
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                cache: RwLock::new(map),
            }),
        })
    }

    #[must_use]
    pub fn get(&self, user_id: &str, client_name: &str) -> Option<TrafficQuotaRow> {
        self.inner
            .cache
            .read()
            .ok()
            .and_then(|m| {
                m.get(&(user_id.to_string(), client_name.to_string()))
                    .cloned()
            })
    }

    #[must_use]
    pub fn list_for_client(&self, client_name: &str) -> Vec<TrafficQuotaRow> {
        self.inner
            .cache
            .read()
            .ok()
            .map(|m| {
                m.values()
                    .filter(|r| r.client_name == client_name)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn list_for_user(&self, user_id: &str) -> Vec<TrafficQuotaRow> {
        self.inner
            .cache
            .read()
            .ok()
            .map(|m| {
                m.values()
                    .filter(|r| r.user_id == user_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn list_all(&self) -> Vec<TrafficQuotaRow> {
        self.inner
            .cache
            .read()
            .ok()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Upsert a quota row through both cache and store. Idempotent.
    pub fn upsert(&self, row: TrafficQuotaRow) -> Result<TrafficQuotaRow, StoreError> {
        quota_store::insert_or_replace(&self.inner.store, &row)?;
        if let Ok(mut m) = self.inner.cache.write() {
            m.insert((row.user_id.clone(), row.client_name.clone()), row.clone());
        }
        Ok(row)
    }

    pub fn delete(&self, user_id: &str, client_name: &str) -> Result<bool, StoreError> {
        let removed = quota_store::delete(&self.inner.store, user_id, client_name)?;
        if let Ok(mut m) = self.inner.cache.write() {
            m.remove(&(user_id.to_string(), client_name.to_string()));
        }
        Ok(removed)
    }

    /// Accumulate cumulative byte delta into the current period
    /// (write-through). Returns `(post_row, just_exhausted)` where
    /// `just_exhausted` is true iff THIS call transitioned the row from
    /// non-exhausted to exhausted. Compares pre vs post `exhausted_at`
    /// using the cached snapshot, so callers can detect the first
    /// crossing without relying on `exhausted_at == now_unix_sec`
    /// (which is ambiguous when multiple deltas land within the same
    /// second).
    pub fn accumulate(
        &self,
        user_id: &str,
        client_name: &str,
        delta: i64,
        now_unix_sec: i64,
    ) -> Result<Option<(TrafficQuotaRow, bool)>, StoreError> {
        let pre_exhausted = self
            .get(user_id, client_name)
            .map(|r| r.exhausted_at.is_some());
        let updated = quota_store::accumulate_bytes_used(
            &self.inner.store,
            user_id,
            client_name,
            delta,
            now_unix_sec,
        )?;
        if let Some(row) = updated {
            let post_exhausted = row.exhausted_at.is_some();
            let just_exhausted = post_exhausted && pre_exhausted == Some(false);
            if let Ok(mut m) = self.inner.cache.write() {
                m.insert((row.user_id.clone(), row.client_name.clone()), row.clone());
            }
            Ok(Some((row, just_exhausted)))
        } else {
            warn!(
                event = "traffic_quota.accumulate_missing",
                user_id, client_name, delta,
                "accumulate found no row; cache may be stale"
            );
            Ok(None)
        }
    }

    pub fn clear_period_usage(
        &self,
        user_id: &str,
        client_name: &str,
        now: i64,
    ) -> Result<Option<TrafficQuotaRow>, StoreError> {
        let updated = quota_store::clear_period_usage(&self.inner.store, user_id, client_name, now)?;
        if let Some(ref row) = updated
            && let Ok(mut m) = self.inner.cache.write()
        {
            m.insert((row.user_id.clone(), row.client_name.clone()), row.clone());
        }
        Ok(updated)
    }

    pub fn reset_period(
        &self,
        user_id: &str,
        client_name: &str,
        new_period_started_at: i64,
        now: i64,
    ) -> Result<Option<TrafficQuotaRow>, StoreError> {
        let updated = quota_store::reset_period(
            &self.inner.store,
            user_id,
            client_name,
            new_period_started_at,
            now,
        )?;
        if let Some(ref row) = updated
            && let Ok(mut m) = self.inner.cache.write()
        {
            m.insert((row.user_id.clone(), row.client_name.clone()), row.clone());
        }
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;
    use tempfile::tempdir;

    fn make_cache() -> (tempfile::TempDir, TrafficQuotaCache) {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).expect("open store");
        let cache = TrafficQuotaCache::load(store).expect("load cache");
        (dir, cache)
    }

    fn sample_row() -> TrafficQuotaRow {
        TrafficQuotaRow {
            user_id: "alice".into(),
            client_name: "edge-01".into(),
            monthly_bytes: 1_000,
            billing_anchor: 0,
            current_period_started_at: 0,
            current_period_bytes_used: 0,
            exhausted_at: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn empty_cache_load() {
        let (_d, cache) = make_cache();
        assert!(cache.list_all().is_empty());
    }

    #[test]
    fn upsert_then_get() {
        let (_d, cache) = make_cache();
        cache.upsert(sample_row()).unwrap();
        let got = cache.get("alice", "edge-01").unwrap();
        assert_eq!(got.monthly_bytes, 1_000);
    }

    #[test]
    fn delete_removes_from_cache_and_store() {
        let (_d, cache) = make_cache();
        cache.upsert(sample_row()).unwrap();
        assert!(cache.delete("alice", "edge-01").unwrap());
        assert!(cache.get("alice", "edge-01").is_none());
    }

    #[test]
    fn accumulate_increments_cache() {
        let (_d, cache) = make_cache();
        cache.upsert(sample_row()).unwrap();
        let (row, just) = cache.accumulate("alice", "edge-01", 100, 1).unwrap().unwrap();
        assert_eq!(row.current_period_bytes_used, 100);
        assert!(!just);
        let got = cache.get("alice", "edge-01").unwrap();
        assert_eq!(got.current_period_bytes_used, 100);
    }

    #[test]
    fn accumulate_flags_just_exhausted_once() {
        let (_d, cache) = make_cache();
        let mut r = sample_row();
        r.monthly_bytes = 100;
        cache.upsert(r).unwrap();
        // First crossing -> just_exhausted=true.
        let (_row, just) = cache.accumulate("alice", "edge-01", 200, 5).unwrap().unwrap();
        assert!(just);
        // Already exhausted -> just_exhausted=false even with the same `now`.
        let (_row, just2) = cache.accumulate("alice", "edge-01", 50, 5).unwrap().unwrap();
        assert!(!just2);
    }

    #[test]
    fn accumulate_missing_row_returns_none_without_panic() {
        let (_d, cache) = make_cache();
        let r = cache.accumulate("ghost", "edge-01", 100, 1).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn load_picks_up_existing_rows() {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path()).expect("open");
        quota_store::insert_or_replace(&store, &sample_row()).unwrap();
        let cache = TrafficQuotaCache::load(store).expect("reload");
        assert_eq!(cache.list_all().len(), 1);
    }
}
