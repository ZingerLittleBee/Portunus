//! `forward-server` binary entry point.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use forward_auth::file_store::FileTokenStore;

use forward_server::OutputFormat;
use forward_server::clients::ConnectedClients;
use forward_server::operator::bootstrap;
use forward_server::operator::cli::{self, OperatorError};
use forward_server::operator::identity_cli;
use forward_server::operator::rule_cli;
use forward_server::serve;
use forward_server::state::AppState;
use forward_server::tls::ServerTlsMaterial;

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
    /// Push a forwarding rule to a connected client. `<listen>` and the
    /// port portion of `<target>` accept either a single port (`18080`)
    /// or a contiguous range (`30000-30050`); when one side is a range
    /// the other side MUST be a same-length range (002-port-range-forward).
    PushRule {
        client: String,
        /// Listen port or `start-end` range.
        listen: String,
        /// `host:port` or `host:start-end`.
        target: String,
        #[arg(long, default_value = "tcp")]
        protocol: String,
        #[arg(long, default_value_t = 2)]
        ack_timeout: u64,
        /// Prefer IPv6 (AAAA) addresses over IPv4 when the resolver
        /// returns both families for the target hostname (FR-007 /
        /// 003-domain-name-forward US3). Falls back to IPv4 if no
        /// AAAA is available — "prefer" is not "only".
        #[arg(long, default_value_t = false)]
        prefer_ipv6: bool,
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
        /// Include per-port detail for range rules
        /// (002-port-range-forward, US3). Adds `?per_port=true` to the
        /// HTTP request and renders the per-port table in text mode.
        /// No-op for single-port rules.
        #[arg(long, default_value_t = false)]
        per_port: bool,
        /// 007-multi-target-failover (US3): include per-target detail
        /// for multi-target rules. Adds `?per_target=true` to the HTTP
        /// request and renders the per-target table in text mode.
        /// Single-target rules print a `(single-target rule, no
        /// per-target state)` note and exit 0.
        #[arg(long, default_value_t = false)]
        per_target: bool,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    /// 005-multi-user-rbac: seed an empty operator store with the
    /// canonical `_superadmin` user + an Active credential. Prints the
    /// raw bearer token to stdout EXACTLY ONCE — capture it now.
    BootstrapSuperadmin {
        /// Display name for the new superadmin.
        #[arg(long, default_value = "ops")]
        name: String,
    },
    /// 005-multi-user-rbac: print a fresh URL-safe-base64 token to
    /// stdout. Useful for seeding `operator_token` in `server.toml`
    /// out-of-band before first start.
    GenToken,
    /// Add a new operator user (superadmin-only).
    UserAdd {
        user_id: String,
        #[arg(long)]
        display_name: String,
        #[arg(long, default_value = "user")]
        role: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    UserList {
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    UserGet {
        user_id: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    UserRemove {
        user_id: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    /// Issue a fresh credential for a user. Prints raw token in JSON exactly once.
    CredentialIssue {
        user_id: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    CredentialList {
        user_id: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    CredentialRevoke {
        user_id: String,
        credential_id: String,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    CredentialRotate {
        user_id: String,
        credential_id: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    /// Add a grant (superadmin-only). `--client` is either a `ClientName`
    /// or `*` for wildcard. `--protocols` is a comma-separated list
    /// (e.g. `tcp` or `tcp,udp`).
    GrantAdd {
        #[arg(long)]
        user_id: String,
        #[arg(long)]
        client: String,
        #[arg(long)]
        listen_port_start: u16,
        #[arg(long)]
        listen_port_end: u16,
        #[arg(long, value_delimiter = ',', default_value = "tcp")]
        protocols: Vec<String>,
        #[arg(long)]
        note: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    GrantList {
        #[arg(long)]
        user_id: Option<String>,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    GrantRevoke {
        grant_id: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
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
            listen,
            target,
            protocol,
            ack_timeout,
            prefer_ipv6,
            http_endpoint,
        } => rule_cli::push(
            &http_endpoint,
            &client,
            &listen,
            &target,
            &protocol,
            ack_timeout,
            prefer_ipv6,
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
        Cmd::RuleStats {
            rule_id,
            format,
            per_port,
            per_target,
            http_endpoint,
        } => rule_cli::stats(&http_endpoint, rule_id, format, per_port, per_target),
        Cmd::BootstrapSuperadmin { name } => {
            std::fs::create_dir_all(&config_dir).map_err(|e| {
                eprintln!("config dir: {e}");
                1u8
            })?;
            let identity_path = config_dir.join("identity.json");
            let code = bootstrap::bootstrap_superadmin(&identity_path, &name);
            if code == 0 { Ok(()) } else { Err(code) }
        }
        Cmd::GenToken => {
            let code = bootstrap::gen_token();
            if code == 0 { Ok(()) } else { Err(code) }
        }
        Cmd::UserAdd {
            user_id,
            display_name,
            role,
            format,
            http_endpoint,
        } => identity_cli::user_add(&http_endpoint, &user_id, &display_name, &role, format),
        Cmd::UserList {
            format,
            http_endpoint,
        } => identity_cli::user_list(&http_endpoint, format),
        Cmd::UserGet {
            user_id,
            format,
            http_endpoint,
        } => identity_cli::user_get(&http_endpoint, &user_id, format),
        Cmd::UserRemove {
            user_id,
            format,
            http_endpoint,
        } => identity_cli::user_remove(&http_endpoint, &user_id, format),
        Cmd::CredentialIssue {
            user_id,
            label,
            format,
            http_endpoint,
        } => identity_cli::credential_issue(&http_endpoint, &user_id, label.as_deref(), format),
        Cmd::CredentialList {
            user_id,
            format,
            http_endpoint,
        } => identity_cli::credential_list(&http_endpoint, &user_id, format),
        Cmd::CredentialRevoke {
            user_id,
            credential_id,
            http_endpoint,
        } => identity_cli::credential_revoke(&http_endpoint, &user_id, &credential_id),
        Cmd::CredentialRotate {
            user_id,
            credential_id,
            label,
            format,
            http_endpoint,
        } => identity_cli::credential_rotate(
            &http_endpoint,
            &user_id,
            &credential_id,
            label.as_deref(),
            format,
        ),
        Cmd::GrantAdd {
            user_id,
            client,
            listen_port_start,
            listen_port_end,
            protocols,
            note,
            format,
            http_endpoint,
        } => identity_cli::grant_add(
            &http_endpoint,
            &user_id,
            &client,
            listen_port_start,
            listen_port_end,
            &protocols,
            note.as_deref(),
            format,
        ),
        Cmd::GrantList {
            user_id,
            format,
            http_endpoint,
        } => identity_cli::grant_list(&http_endpoint, user_id.as_deref(), format),
        Cmd::GrantRevoke {
            grant_id,
            format,
            http_endpoint,
        } => identity_cli::grant_revoke(&http_endpoint, &grant_id, format),
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
    let operator_store = Arc::new(
        forward_auth::operator_store::FileOperatorStore::open(config_dir.join("identity.json"))
            .map_err(|e| {
                eprintln!("operator store: {e}");
                1u8
            })?,
    );
    let endpoint = advertised_endpoint.unwrap_or_else(|| "127.0.0.1:7443".to_string());
    AppState::new(
        tokens,
        operator_store,
        ConnectedClients::default(),
        endpoint,
        tls.leaf_fingerprint_hex,
        tls.cert_pem,
        // Offline operator commands (provision-client, revoke,
        // list-clients) never push range rules, so the cap is
        // effectively unused. Use the default to stay close to the
        // serve-path config.
        forward_core::config::default_range_rule_max_ports(),
    )
    .map_err(|e| {
        eprintln!("metrics: {e}");
        1u8
    })
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
