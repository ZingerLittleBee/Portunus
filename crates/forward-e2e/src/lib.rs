//! Workspace-level end-to-end / contract tests for forward-rs.
//!
//! This crate's `tests/` directory holds tests that span the binaries
//! (`forward-server` + `forward-client`). Helper code that needs to be
//! shared across multiple test files lives in `tests/common/`.
//!
//! The crate ships an empty library so that `cargo` keeps it as a
//! workspace member; no production code lives here.
