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

    #[test]
    fn database_corrupt_maps_to_corruption() {
        let err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DatabaseCorrupt,
                extended_code: 11,
            },
            Some("database disk image is malformed".into()),
        );
        match map_rusqlite(err) {
            StoreError::Corruption { detail } => {
                assert_eq!(detail, "database disk image is malformed");
            }
            other => panic!("expected Corruption, got {other:?}"),
        }
    }

    #[test]
    fn database_locked_maps_to_transient() {
        let err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DatabaseLocked,
                extended_code: 6,
            },
            Some("database table is locked".into()),
        );
        match map_rusqlite(err) {
            StoreError::Transient { message } => {
                assert_eq!(message, "database table is locked");
            }
            other => panic!("expected Transient, got {other:?}"),
        }
    }

    // When `SqliteFailure` carries no message string, each arm falls back to
    // `e.to_string()` via `unwrap_or_else`. Exercise that fallback for the
    // Transient, Conflict, and Corruption arms.
    #[test]
    fn busy_without_message_falls_back_to_error_display() {
        let raw = rusqlite::ffi::Error {
            code: rusqlite::ErrorCode::DatabaseBusy,
            extended_code: 5,
        };
        let expected = rusqlite::Error::SqliteFailure(raw, None).to_string();
        let err = rusqlite::Error::SqliteFailure(raw, None);
        match map_rusqlite(err) {
            StoreError::Transient { message } => {
                assert_eq!(message, expected);
                assert!(!message.is_empty());
            }
            other => panic!("expected Transient, got {other:?}"),
        }
    }

    #[test]
    fn constraint_without_message_falls_back_to_error_display() {
        let raw = rusqlite::ffi::Error {
            code: rusqlite::ErrorCode::ConstraintViolation,
            extended_code: 2067,
        };
        let expected = rusqlite::Error::SqliteFailure(raw, None).to_string();
        let err = rusqlite::Error::SqliteFailure(raw, None);
        match map_rusqlite(err) {
            StoreError::Conflict { detail } => {
                assert_eq!(detail, expected);
                assert!(!detail.is_empty());
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn corruption_without_message_falls_back_to_error_display() {
        let raw = rusqlite::ffi::Error {
            code: rusqlite::ErrorCode::NotADatabase,
            extended_code: 26,
        };
        let expected = rusqlite::Error::SqliteFailure(raw, None).to_string();
        let err = rusqlite::Error::SqliteFailure(raw, None);
        match map_rusqlite(err) {
            StoreError::Corruption { detail } => {
                assert_eq!(detail, expected);
                assert!(!detail.is_empty());
            }
            other => panic!("expected Corruption, got {other:?}"),
        }
    }

    // An unrecognized `SqliteFailure` code lands in the catch-all inner arm,
    // mapping to `Internal` carrying the full error display.
    #[test]
    fn unknown_sqlite_code_maps_to_internal() {
        let raw = rusqlite::ffi::Error {
            code: rusqlite::ErrorCode::PermissionDenied,
            extended_code: 3,
        };
        let expected =
            rusqlite::Error::SqliteFailure(raw, Some("access denied".into())).to_string();
        let err = rusqlite::Error::SqliteFailure(raw, Some("access denied".into()));
        match map_rusqlite(err) {
            StoreError::Internal { message } => {
                assert_eq!(message, expected);
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    // `QueryReturnedNoRows` has its own dedicated arm with a fixed message.
    #[test]
    fn query_returned_no_rows_maps_to_internal() {
        let err = rusqlite::Error::QueryReturnedNoRows;
        match map_rusqlite(err) {
            StoreError::Internal { message } => {
                assert_eq!(message, "query returned no rows");
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    // Any other `rusqlite::Error` variant falls through to the outer catch-all.
    #[test]
    fn other_error_maps_to_internal() {
        let err = rusqlite::Error::InvalidColumnIndex(7);
        let expected = rusqlite::Error::InvalidColumnIndex(7).to_string();
        match map_rusqlite(err) {
            StoreError::Internal { message } => {
                assert_eq!(message, expected);
            }
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    // The `#[error(...)]` Display impls render each variant's template.
    #[test]
    fn display_renders_each_variant() {
        assert_eq!(
            StoreError::Transient {
                message: "busy".into()
            }
            .to_string(),
            "transient: busy"
        );
        assert_eq!(
            StoreError::Conflict {
                detail: "dup".into()
            }
            .to_string(),
            "conflict: dup"
        );
        assert_eq!(
            StoreError::Corruption {
                detail: "bad".into()
            }
            .to_string(),
            "corruption: bad"
        );
        assert_eq!(
            StoreError::Internal {
                message: "boom".into()
            }
            .to_string(),
            "internal: boom"
        );
    }
}
