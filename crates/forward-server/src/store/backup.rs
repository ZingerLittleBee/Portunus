//! 008-sqlite-storage T005 / T060..T061 — Online Backup API wrapper.
//!
//! See `specs/008-sqlite-storage/research.md` R-007:
//! uses `rusqlite::backup::Backup::run(-1)` to produce a clean
//! single-file artefact regardless of WAL state. Restore is the
//! reverse: copy the artefact to the data-dir, then run the regular
//! schema-version handshake.
//!
//! Implementation lands in T060..T061. Stub exists so `mod store;`
//! resolves before that task runs.
