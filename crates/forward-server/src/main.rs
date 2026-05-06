//! `forward-server` binary entry point.
//!
//! Subcommands are stubbed in this skeleton; real implementations land in
//! Phase 3 (US1) onward.

mod shutdown;
mod tls;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "forward-server", version, about = "forward-rs control plane")]
struct Cli {
    /// Override the configuration directory.
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,

    /// Override the path to `server.toml`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the long-lived server (gRPC + operator HTTP + metrics).
    Serve,
    /// Provision a new client and write its credential bundle.
    ProvisionClient {
        name: String,
        /// Output path. Defaults to `<cwd>/<name>.bundle.json`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Revoke a previously-provisioned client.
    Revoke { name: String },
    /// List provisioned + connected clients.
    ListClients {
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Push a forwarding rule to a connected client.
    PushRule {
        client: String,
        listen_port: u16,
        /// `host:port` target spec.
        target: String,
        #[arg(long, default_value = "tcp")]
        protocol: String,
        #[arg(long, default_value_t = 2)]
        ack_timeout: u64,
    },
    /// Remove a previously-pushed rule (any state).
    RemoveRule { rule_id: u64 },
    /// List rules with their state.
    ListRules {
        #[arg(long)]
        client: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Show per-rule stats from the server-side cache.
    RuleStats {
        rule_id: u64,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum OutputFormat {
    Text,
    Json,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing();

    match cli.cmd {
        Cmd::Serve => unimplemented!("serve — implemented in T035 (US1 wiring)"),
        Cmd::ProvisionClient { .. } => {
            unimplemented!("provision-client — implemented in T028 (US1)")
        }
        Cmd::Revoke { .. } => unimplemented!("revoke — implemented in T029 (US1)"),
        Cmd::ListClients { .. } => unimplemented!("list-clients — implemented in T030 (US1)"),
        Cmd::PushRule { .. } => unimplemented!("push-rule — implemented in T045 (US2)"),
        Cmd::RemoveRule { .. } => unimplemented!("remove-rule — implemented in T045 (US2)"),
        Cmd::ListRules { .. } => unimplemented!("list-rules — implemented in T045 (US2)"),
        Cmd::RuleStats { .. } => unimplemented!("rule-stats — implemented in T061 (US3)"),
    }
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
