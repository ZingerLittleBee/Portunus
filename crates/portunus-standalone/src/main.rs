use portunus_standalone::{config, runtime};

use std::collections::HashMap;
use std::process::ExitCode;

use clap::Parser;

use config::derive_rule_id;

#[derive(Parser, Debug)]
#[command(
    name = "portunus-standalone",
    version,
    about = "Standalone TCP/UDP forwarder"
)]
struct Cli {
    /// Path to standalone.toml. If omitted, the loader searches
    /// $PORTUNUS_STANDALONE_CONFIG, ./portunus.toml.
    #[arg(short, long, global = true)]
    config: Option<std::path::PathBuf>,

    /// Validate config and exit (0 = valid, 2 = invalid).
    #[arg(long)]
    check: bool,

    /// Override log level (e.g. debug, info, warn, error).
    #[arg(long, global = true)]
    log_level: Option<String>,

    /// Override log format: "json" (default) or "pretty".
    #[arg(long, global = true)]
    log_format: Option<String>,

    /// Disable the [stats] UDS server entirely (daemon mode only).
    #[arg(long)]
    no_stats: bool,

    /// Override [stats] socket_path (daemon mode only).
    #[arg(long)]
    stats_socket: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Option<Subcommand>,
}

#[derive(clap::Subcommand, Debug)]
enum Subcommand {
    /// Connect to a running daemon's stats UDS and render a TUI.
    Stats {
        /// Path to the stats UDS (overrides daemon default).
        #[arg(long)]
        socket: Option<std::path::PathBuf>,
        /// Print one snapshot as JSON and exit (no TUI).
        #[arg(long)]
        once: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Stats subcommand: connect to a running daemon and render / dump.
    if let Some(Subcommand::Stats { socket, once }) = &cli.command {
        return run_stats(socket.clone(), *once);
    }

    let mut cfg = match cli.config.as_deref() {
        Some(p) => match config::Config::load_from(p) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        },
        None => match config::Config::load_default() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        },
    };

    // Apply daemon-only CLI overrides before validation.
    if cli.no_stats {
        cfg.stats.enabled = false;
    }
    if let Some(ref p) = cli.stats_socket {
        cfg.stats.socket_path.clone_from(p);
    }

    if let Err(e) = cfg.validate() {
        eprintln!("error: {e}");
        return ExitCode::from(2);
    }

    // Build the rule registry (RuleId → name) before consuming cfg.
    let registry: HashMap<_, _> = cfg
        .rules
        .iter()
        .map(|r| (derive_rule_id(&r.name), r.name.clone()))
        .collect();

    if cli.check {
        println!("ok");
        return ExitCode::SUCCESS;
    }

    init_tracing(&cli, &cfg);

    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: tokio runtime: {e}");
            return ExitCode::from(1);
        }
    };
    rt.block_on(runtime::run(cfg, registry))
}

fn run_stats(socket: Option<std::path::PathBuf>, once: bool) -> ExitCode {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");

    rt.block_on(async move {
        let path = match socket {
            Some(p) => p,
            None => default_stats_socket_path_runtime(),
        };
        if once {
            match portunus_standalone::stats::client::once(&path).await {
                Ok(s) => {
                    println!("{s}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("error: stats --once: {e}");
                    ExitCode::from(2)
                }
            }
        } else {
            #[cfg(feature = "stats-tui")]
            {
                portunus_standalone::stats::tui::run(&path).await
            }
            #[cfg(not(feature = "stats-tui"))]
            {
                eprintln!(
                    "error: this build was compiled without --features stats-tui; only `stats --once` is available"
                );
                ExitCode::from(2)
            }
        }
    })
}

/// Default stats socket path for the client side (no config file loaded).
/// Mirrors `config::default_stats_socket_path`. Duplicated here because
/// `stats --socket` may be invoked without `--config`.
fn default_stats_socket_path_runtime() -> std::path::PathBuf {
    #[cfg(target_os = "linux")]
    {
        std::path::PathBuf::from("/run/portunus/standalone.sock")
    }
    #[cfg(target_os = "macos")]
    {
        let base = std::env::var_os("TMPDIR").map_or_else(
            || std::path::PathBuf::from("/tmp"),
            std::path::PathBuf::from,
        );
        base.join("portunus-standalone.sock")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        std::path::PathBuf::from("portunus-standalone.sock")
    }
}

fn init_tracing(cli: &Cli, cfg: &config::Config) {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    let level = cli.log_level.as_deref().unwrap_or(&cfg.global.log_level);
    let format = cli.log_format.as_deref().unwrap_or(&cfg.global.log_format);

    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("warn"));
    let reg = tracing_subscriber::registry().with(filter);
    match format {
        "pretty" => {
            let _ = reg
                .with(fmt::layer().pretty().with_writer(std::io::stderr))
                .try_init();
        }
        _ => {
            let _ = reg
                .with(fmt::layer().json().with_writer(std::io::stderr))
                .try_init();
        }
    }
}
