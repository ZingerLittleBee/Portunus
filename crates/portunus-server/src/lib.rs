//! `portunus-server` library surface.
//!
//! The crate is primarily delivered as the `portunus-server` binary
//! (see `src/main.rs`), but a thin library facade is exposed so
//! integration tests under `tests/` can build an in-process router
//! and exercise the operator HTTP API without spawning a subprocess.
//!
//! The OutputFormat / CLI-arg types intentionally stay in `main.rs`;
//! everything tests need to assemble an `AppState` and call into the
//! axum router is re-exported here.

pub mod advertised;
pub mod clients;
pub mod data_dir;
pub mod grpc;
pub mod metrics;
pub mod operator;
pub mod owner;
pub mod rules;
pub mod serve;
pub mod shutdown;
pub mod state;
pub mod store;
pub mod tls;
pub mod traffic_quotas;

/// Output format for CLI subcommands. Used by `rule_cli` (and now,
/// transitively, by integration tests that exercise the operator HTTP
/// API in-process).
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
}
