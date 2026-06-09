//! `portunus-client` binary entry point.

mod bundle;
mod control;
mod enroll;
mod port_groups;
mod tls;
mod wire;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tracing::{error, info};

use crate::bundle::{CredentialBundle, resolve_bundle_path};
use crate::control::ReconnectConfig;
use portunus_forwarder::shutdown::Shutdown;

#[derive(Parser, Debug)]
#[command(name = "portunus-client", version, about = "Portunus edge client")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Path to the `.bundle.json` produced by `portunus-client enroll`.
    /// When omitted, the resolver searches `$PORTUNUS_CLIENT_BUNDLE`,
    /// `$XDG_CONFIG_HOME/portunus/client.bundle.json`,
    /// `$HOME/.config/portunus/client.bundle.json`, and
    /// `./client.bundle.json` (in that order).
    #[arg(long)]
    bundle: Option<PathBuf>,

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

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Redeem a one-time enrollment URI and write a client bundle.
    Enroll {
        uri: String,
        /// Output path. Defaults to the normal client bundle location.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

/// Decide whether to self-enroll on startup. Returns the URI to redeem
/// only when no bundle is present yet AND a non-empty `PORTUNUS_ENROLL_URI`
/// was supplied. A bundle that already exists always wins — the one-time
/// enrollment code is spent after first use, so a present bundle is loaded
/// as-is. Used by the Docker image to onboard on first boot.
fn self_enroll_uri(bundle_present: bool, env_uri: Option<String>) -> Option<String> {
    if bundle_present {
        return None;
    }
    let uri = env_uri?;
    let trimmed = uri.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing();

    // Install rustls crypto provider for TLS dialing.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    if let Some(Cmd::Enroll { uri, out }) = cli.cmd {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                error!(event = "client.runtime_failed", error = %e);
                return ExitCode::from(1);
            }
        };
        return match runtime.block_on(enroll::enroll(&uri, out)) {
            Ok(path) => {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                error!(event = "client.enrollment_failed", error = %e);
                ExitCode::from(1)
            }
        };
    }

    let bundle_path = match resolve_bundle_path(cli.bundle.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            // Surface every attempted candidate so operators can fix
            // their environment without strace-ing the client.
            eprintln!("error: {e}");
            error!(event = "client.bundle_search_failed", attempted = ?e.attempted);
            return ExitCode::from(1);
        }
    };
    // Self-bootstrap (Docker first boot): when the resolved bundle is
    // absent but PORTUNUS_ENROLL_URI is set, redeem it once into the
    // resolved path before loading. The Docker image always passes
    // `--bundle /etc/portunus/client.bundle.json`, so the target path is
    // known; on subsequent boots the persisted bundle wins.
    if let Some(uri) = self_enroll_uri(
        bundle_path.is_file(),
        std::env::var("PORTUNUS_ENROLL_URI").ok(),
    ) {
        info!(event = "client.self_bootstrap", path = %bundle_path.display());
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                error!(event = "client.runtime_failed", error = %e);
                return ExitCode::from(1);
            }
        };
        if let Err(e) = rt.block_on(enroll::enroll(&uri, Some(bundle_path.clone()))) {
            eprintln!("error: {e}");
            error!(
                event = "client.self_bootstrap_failed",
                error = %e,
                path = %bundle_path.display()
            );
            return ExitCode::from(1);
        }
    }
    let bundle = match CredentialBundle::read_from(&bundle_path) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            error!(event = "client.bundle_load_failed", error = %e, path = %bundle_path.display());
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
    use portunus_core::log_redact::RedactionLayer;
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

#[cfg(test)]
mod tests {
    use super::self_enroll_uri;

    #[test]
    fn skips_when_bundle_present() {
        assert_eq!(self_enroll_uri(true, Some("portunus://x".into())), None);
    }

    #[test]
    fn returns_uri_when_absent_and_set() {
        assert_eq!(
            self_enroll_uri(false, Some("portunus://x".into())),
            Some("portunus://x".to_string())
        );
    }

    #[test]
    fn none_when_absent_and_unset() {
        assert_eq!(self_enroll_uri(false, None), None);
    }

    #[test]
    fn trims_and_rejects_blank() {
        assert_eq!(self_enroll_uri(false, Some("   ".into())), None);
        assert_eq!(
            self_enroll_uri(false, Some("  portunus://x  ".into())),
            Some("portunus://x".to_string())
        );
    }
}
