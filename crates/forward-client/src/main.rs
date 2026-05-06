//! `forward-client` binary entry point.
//!
//! Real connect/forward logic lands in Phase 3+. This skeleton just parses
//! flags and exits with `unimplemented!` so other crates can depend on the
//! workspace member.

mod pinned_verifier;
mod shutdown;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "forward-client", version, about = "forward-rs edge client")]
struct Cli {
    /// Path to the `.bundle.json` produced by `forward-server provision-client`.
    #[arg(long)]
    bundle: PathBuf,

    /// Initial reconnect delay in milliseconds (full-jitter exponential backoff base).
    #[arg(long, default_value_t = 500)]
    reconnect_initial_delay_ms: u64,

    /// Maximum reconnect delay in seconds (backoff cap).
    #[arg(long, default_value_t = 30)]
    reconnect_max_delay_secs: u64,

    /// Drain timeout for in-flight forwarded connections on shutdown.
    #[arg(long, default_value_t = 30)]
    shutdown_drain_timeout_secs: u64,

    /// Stats reporting interval (seconds).
    #[arg(long, default_value_t = 5)]
    stats_report_interval_secs: u64,
}

fn main() -> ExitCode {
    let _cli = Cli::parse();
    init_tracing();
    unimplemented!("client main loop — implemented in T038 (US1 wiring)");
}

fn init_tracing() {
    use tracing_subscriber::{EnvFilter, fmt};
    let _ = fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .try_init();
}
