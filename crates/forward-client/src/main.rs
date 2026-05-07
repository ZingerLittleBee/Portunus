//! `forward-client` binary entry point.

mod bundle;
mod control;
mod forwarder;
mod resolver;
mod shutdown;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use tracing::{error, info};

use crate::bundle::CredentialBundle;
use crate::control::ReconnectConfig;
use crate::shutdown::Shutdown;

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
    let cli = Cli::parse();
    init_tracing();

    // Install rustls crypto provider for TLS dialing.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let bundle = match CredentialBundle::read_from(&cli.bundle) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            error!(event = "client.bundle_load_failed", error = %e, path = %cli.bundle.display());
            return ExitCode::from(1);
        }
    };
    info!(
        event = "client.bundle_loaded",
        client_name = %bundle.client_name,
        endpoint = %bundle.server_endpoint,
    );

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            error!(event = "client.runtime_failed", error = %e);
            return ExitCode::from(1);
        }
    };

    let shutdown = Shutdown::new();
    runtime.block_on(async {
        let signal_task = tokio::spawn({
            let s = shutdown.clone();
            async move { s.signal_handler().await }
        });
        let cancel = shutdown.token();
        let reconnect = ReconnectConfig {
            initial_delay_ms: cli.reconnect_initial_delay_ms,
            max_delay_secs: cli.reconnect_max_delay_secs,
            drain_timeout: Duration::from_secs(cli.shutdown_drain_timeout_secs),
            stats_report_interval: Duration::from_secs(cli.stats_report_interval_secs),
        };
        control::run_with_reconnect(bundle, reconnect, cancel).await;
        let _ = signal_task.await;
    });

    info!(event = "client.stopped");
    ExitCode::SUCCESS
}

fn init_tracing() {
    use forward_core::log_redact::RedactionLayer;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let json_layer = fmt::layer().json().with_writer(std::io::stderr);
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(json_layer)
        .with(RedactionLayer::new())
        .try_init();
}
