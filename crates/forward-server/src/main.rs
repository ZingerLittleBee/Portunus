//! `forward-server` binary entry point.

mod bundle;
mod clients;
mod grpc;
mod operator;
mod rules;
mod serve;
mod shutdown;
mod state;
mod tls;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use forward_auth::file_store::FileTokenStore;

use crate::clients::ConnectedClients;
use crate::operator::cli::{self, OperatorError};
use crate::operator::rule_cli;
use crate::state::AppState;
use crate::tls::ServerTlsMaterial;

#[derive(Parser, Debug)]
#[command(name = "forward-server", version, about = "forward-rs control plane")]
struct Cli {
    /// Override the configuration directory.
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,

    /// Override the path to `server.toml`.
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Override the host:port advertised in newly-issued credential bundles.
    #[arg(long, global = true)]
    advertised_endpoint: Option<String>,

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
        target: String,
        #[arg(long, default_value = "tcp")]
        protocol: String,
        #[arg(long, default_value_t = 2)]
        ack_timeout: u64,
        /// Operator HTTP endpoint of the running server.
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    RemoveRule {
        rule_id: u64,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    ListRules {
        #[arg(long)]
        client: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
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

    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => ExitCode::from(code),
    }
}

fn run(cli: Cli) -> Result<(), u8> {
    let config_dir = resolve_config_dir(cli.config_dir.clone());

    match cli.cmd {
        Cmd::Serve => {
            let opts = serve::ServeOptions {
                config_dir,
                config_file: cli.config.clone(),
                advertised_endpoint: cli.advertised_endpoint.clone(),
            };
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|_| 1)?;
            // Install crypto provider for rustls (server side uses it for TLS).
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
            runtime.block_on(serve::run(opts)).map_err(|e| {
                eprintln!("serve failed: {e}");
                1
            })
        }
        Cmd::ProvisionClient { name, out } => {
            let state = build_offline_state(&config_dir, cli.advertised_endpoint.clone())?;
            match cli::provision_client(&state, &name, out) {
                Ok((path, _)) => {
                    println!("{}", path.display());
                    Ok(())
                }
                Err(e) => {
                    let code = e.exit_code();
                    eprintln!("error: {e}");
                    Err(code)
                }
            }
        }
        Cmd::Revoke { name } => {
            let state = build_offline_state(&config_dir, cli.advertised_endpoint.clone())?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| 1u8)?;
            runtime
                .block_on(cli::revoke(&state, &name))
                .map_err(|e: OperatorError| {
                    eprintln!("error: {e}");
                    e.exit_code()
                })
        }
        Cmd::ListClients { format } => {
            let state = build_offline_state(&config_dir, cli.advertised_endpoint.clone())?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|_| 1u8)?;
            let views = runtime.block_on(cli::list_clients(&state));
            match format {
                OutputFormat::Json => {
                    let s = serde_json::to_string_pretty(&views).map_err(|_| 1u8)?;
                    println!("{s}");
                }
                OutputFormat::Text => {
                    print!("{}", cli::render_client_view_text(&views));
                }
            }
            Ok(())
        }
        Cmd::PushRule {
            client,
            listen_port,
            target,
            protocol,
            ack_timeout,
            http_endpoint,
        } => rule_cli::push(
            &http_endpoint,
            &client,
            listen_port,
            &target,
            &protocol,
            ack_timeout,
        ),
        Cmd::RemoveRule {
            rule_id,
            http_endpoint,
        } => rule_cli::remove(&http_endpoint, rule_id),
        Cmd::ListRules {
            client,
            format,
            http_endpoint,
        } => rule_cli::list(&http_endpoint, client.as_deref(), format),
        Cmd::RuleStats { .. } => {
            eprintln!("rule-stats is implemented in US3 (Phase 5)");
            Err(1)
        }
    }
}

fn resolve_config_dir(override_: Option<PathBuf>) -> PathBuf {
    if let Some(p) = override_ {
        return p;
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("forward-rs");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config/forward-rs");
    }
    PathBuf::from("./forward-rs.config")
}

/// Build a state suitable for *offline* operator commands (provision-client,
/// revoke, list-clients). Loads (or generates) TLS material so that newly
/// issued bundles carry the correct fingerprint.
fn build_offline_state(
    config_dir: &std::path::Path,
    advertised_endpoint: Option<String>,
) -> Result<AppState, u8> {
    std::fs::create_dir_all(config_dir).map_err(|e| {
        eprintln!("config dir: {e}");
        1u8
    })?;
    let paths = cli::default_paths(config_dir);
    let tls = ServerTlsMaterial::load_or_generate(&paths.cert, &paths.key).map_err(|e| {
        eprintln!("tls: {e}");
        1u8
    })?;
    let tokens = Arc::new(FileTokenStore::open(&paths.tokens).map_err(|e| {
        eprintln!("token store: {e}");
        1u8
    })?);
    let endpoint = advertised_endpoint.unwrap_or_else(|| "127.0.0.1:7443".to_string());
    Ok(AppState::new(
        tokens,
        ConnectedClients::default(),
        endpoint,
        tls.leaf_fingerprint_hex,
        tls.cert_pem,
    ))
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
