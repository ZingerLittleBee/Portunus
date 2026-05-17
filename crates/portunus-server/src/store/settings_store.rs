//! Singleton `server_settings` row accessor (operator advertised-endpoint override).

use std::sync::Arc;

use rusqlite::OptionalExtension;

use crate::store::{Store, StoreError, map_rusqlite};

#[derive(Clone, Debug)]
pub struct SqliteSettingsStore {
    store: Arc<Store>,
}

impl SqliteSettingsStore {
    #[must_use]
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Read the operator override. `NULL`/empty → `None`.
    ///
    /// # Errors
    /// Propagates `StoreError` on a DB failure.
    pub fn get_advertised_endpoint(&self) -> Result<Option<String>, StoreError> {
        self.store.with_conn(|conn| {
            let raw: Option<String> = conn
                .query_row(
                    "SELECT advertised_endpoint FROM server_settings WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(map_rusqlite)?
                .flatten();
            Ok(raw.filter(|s| !s.is_empty()))
        })
    }

    /// Write (or clear with `None`/empty) the operator override.
    /// Validates authority grammar before persisting.
    ///
    /// # Errors
    /// `StoreError::Internal` on grammar rejection; `StoreError` on DB failure.
    pub fn set_advertised_endpoint(&self, value: Option<String>) -> Result<(), StoreError> {
        let normalized = value.filter(|s| !s.is_empty());
        if let Some(v) = &normalized {
            crate::advertised::grammar::validate_authority(v).map_err(|reason| {
                StoreError::Internal {
                    message: format!("invalid advertised_endpoint: {reason}"),
                }
            })?;
        }
        self.store.with_write_tx(|tx| {
            tx.execute(
                "UPDATE server_settings SET advertised_endpoint = ? WHERE id = 1",
                rusqlite::params![normalized],
            )
            .map_err(map_rusqlite)?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn store() -> Arc<Store> {
        let dir = tempdir().unwrap();
        Arc::new(Store::open(dir.path()).unwrap())
    }

    #[test]
    fn get_is_none_by_default() {
        let s = SqliteSettingsStore::new(store());
        assert_eq!(s.get_advertised_endpoint().unwrap(), None);
    }

    #[test]
    fn set_then_get_round_trips() {
        let s = SqliteSettingsStore::new(store());
        s.set_advertised_endpoint(Some("public.example:7443".into()))
            .unwrap();
        assert_eq!(
            s.get_advertised_endpoint().unwrap(),
            Some("public.example:7443".to_string())
        );
    }

    #[test]
    fn set_none_clears() {
        let s = SqliteSettingsStore::new(store());
        s.set_advertised_endpoint(Some("public.example:7443".into()))
            .unwrap();
        s.set_advertised_endpoint(None).unwrap();
        assert_eq!(s.get_advertised_endpoint().unwrap(), None);
    }

    #[test]
    fn set_rejects_malformed() {
        let s = SqliteSettingsStore::new(store());
        for bad in [
            "https://x:7443",
            "x/y:7443",
            "host-only",
            "x:bad",
            "x:0",
            "x:70000",
            "[::1]:7443",
        ] {
            assert!(
                s.set_advertised_endpoint(Some(bad.into())).is_err(),
                "expected reject for {bad}"
            );
        }
    }

    #[test]
    fn set_empty_string_clears() {
        let s = SqliteSettingsStore::new(store());
        s.set_advertised_endpoint(Some("public.example:7443".into()))
            .unwrap();
        s.set_advertised_endpoint(Some(String::new())).unwrap();
        assert_eq!(s.get_advertised_endpoint().unwrap(), None);
    }
}
