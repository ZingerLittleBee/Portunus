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
    #[arg(short, long)]
    config: Option<std::path::PathBuf>,

    /// Validate config and exit (0 = valid, 2 = invalid).
    #[arg(long)]
    check: bool,

    /// Override log level (e.g. debug, info, warn, error).
    #[arg(long)]
    log_level: Option<String>,

    /// Override log format: "json" (default) or "pretty".
    #[arg(long)]
    log_format: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let cfg = match cli.config.as_deref() {
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

fn init_tracing(cli: &Cli, cfg: &config::Config) {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};

    let level = cli.log_level.as_deref().unwrap_or(&cfg.global.log_level);
    let format = cli.log_format.as_deref().unwrap_or(&cfg.global.log_format);

    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
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
