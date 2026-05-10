//! 008-sqlite-storage T018 — `rusqlite::Error` → `StoreError` mapping.
//!
//! Per `specs/008-sqlite-storage/research.md` R-015:
//! - `SQLITE_BUSY` / `SQLITE_LOCKED` → `Transient` (caller retry; rare)
//! - constraint violations → `Conflict { detail }`
//! - corruption codes → `Corruption` (boot path only; runtime callers
//!   should never observe this — fail-fast in `Store::open`).
//! - everything else → `Internal { message }` (opaque to operator-API)

use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("transient: {message}")]
    Transient { message: String },

    #[error("conflict: {detail}")]
    Conflict { detail: String },

    #[error("corruption: {detail}")]
    Corruption { detail: String },

    #[error("internal: {message}")]
    Internal { message: String },
}

/// Map a `rusqlite::Error` into the project's store-error taxonomy.
pub fn map_rusqlite(e: rusqlite::Error) -> StoreError {
    use rusqlite::ErrorCode;
    match &e {
        rusqlite::Error::SqliteFailure(err, msg) => match err.code {
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => StoreError::Transient {
                message: msg.clone().unwrap_or_else(|| e.to_string()),
            },
            ErrorCode::ConstraintViolation => StoreError::Conflict {
                detail: msg.clone().unwrap_or_else(|| e.to_string()),
            },
            ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt => StoreError::Corruption {
                detail: msg.clone().unwrap_or_else(|| e.to_string()),
            },
            _ => StoreError::Internal {
                message: e.to_string(),
            },
        },
        rusqlite::Error::QueryReturnedNoRows => StoreError::Internal {
            message: "query returned no rows".into(),
        },
        _ => StoreError::Internal {
            message: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_maps_to_transient() {
        let err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DatabaseBusy,
                extended_code: 5,
            },
            Some("database is locked".into()),
        );
        assert!(matches!(map_rusqlite(err), StoreError::Transient { .. }));
    }

    #[test]
    fn unique_violation_maps_to_conflict() {
        let err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::ConstraintViolation,
                extended_code: 2067,
            },
            Some("UNIQUE constraint failed".into()),
        );
        assert!(matches!(map_rusqlite(err), StoreError::Conflict { .. }));
    }

    #[test]
    fn not_a_database_maps_to_corruption() {
        let err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::NotADatabase,
                extended_code: 26,
            },
            Some("file is not a database".into()),
        );
        assert!(matches!(map_rusqlite(err), StoreError::Corruption { .. }));
    }
}
