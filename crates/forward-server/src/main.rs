//! `forward-server` binary entry point.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use forward_server::OutputFormat;
use forward_server::clients::ConnectedClients;
use forward_server::operator::bootstrap;
use forward_server::operator::cli::{self, OperatorError};
use forward_server::operator::identity_cli;
use forward_server::operator::owner_cap_cli;
use forward_server::operator::rule_cli;
use forward_server::serve;
use forward_server::state::AppState;
use forward_server::tls::ServerTlsMaterial;

#[derive(Parser, Debug)]
#[command(name = "forward-server", version, about = "Portunus control plane")]
struct Cli {
    /// Override the configuration directory (admin-edited config + TLS material).
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,

    /// Override the data directory (daemon-managed `state.db` and SQLite
    /// sidecars). Independent from `--config-dir`. When omitted, resolved
    /// in order: $STATE_DIRECTORY → $XDG_STATE_HOME/portunus →
    /// $HOME/.local/state/portunus → ./portunus.state. See
    /// specs/008-sqlite-storage/ FR-019.
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

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
        /// `host:port` or `host:start-end`. Legacy single-target form.
        /// Omit when supplying `--target` or `--targets-json` (007).
        target: Option<String>,
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
        /// 007-multi-target-failover: repeatable target spec
        /// `host:port[@priority]`. When provided two or more times,
        /// the rule activates with priority-ordered failover.
        /// Mutually exclusive with the positional `target`.
        #[arg(long = "target", conflicts_with_all = ["targets_json"])]
        target_specs: Vec<String>,
        /// 007-multi-target-failover: JSON array of targets:
        /// `[{"host":"...","port":N,"priority":N?}, ...]`. Mutually
        /// exclusive with `--target` and the positional `target`.
        #[arg(long, conflicts_with_all = ["target_specs"])]
        targets_json: Option<String>,
        /// 007-multi-target-failover: per-rule active TCP-connect
        /// probe interval, in seconds (1..=3600). Omit to keep
        /// passive-only failover detection (FR-015).
        #[arg(long)]
        health_check_interval_secs: Option<u32>,
        /// 009-tls-sni-routing: optional Server Name Indication
        /// selector. Accepts an exact host (`api.example.com`) or
        /// single-label wildcard (`*.example.com`). TCP single-port
        /// rules only — UDP and port-range rules are rejected with
        /// `validation.sni_on_unsupported_rule`. Omit (or pass an
        /// empty string) for the legacy / fallback shape. The server
        /// lowercases and grammar-validates the value (R-001).
        #[arg(long = "sni")]
        sni_pattern: Option<String>,
        /// 011-rate-limiting-qos: ingress bandwidth cap (bytes / sec).
        /// Server validates `> 0`; absent leaves ingress uncapped.
        #[arg(long)]
        bandwidth_in_bps: Option<u64>,
        /// 011-rate-limiting-qos: egress bandwidth cap (bytes / sec).
        /// Server validates `> 0`; absent leaves egress uncapped.
        #[arg(long)]
        bandwidth_out_bps: Option<u64>,
        /// 011-rate-limiting-qos: new TCP connections / new UDP flows
        /// per second. Surplus accepts get RST (TCP) or are dropped
        /// before NAT bind (UDP).
        #[arg(long)]
        new_connections_per_sec: Option<u32>,
        /// 011-rate-limiting-qos: ceiling on simultaneously-active
        /// TCP connections + UDP flows. Surplus accepts get RST.
        #[arg(long)]
        concurrent_connections: Option<u32>,
        /// 011-rate-limiting-qos: optional ingress burst override
        /// (bytes). Defaults to `1 × bandwidth_in_bps`. Server
        /// validates `[rate/100, rate*60]`.
        #[arg(long)]
        bandwidth_in_burst: Option<u64>,
        /// 011-rate-limiting-qos: optional egress burst override
        /// (bytes). Defaults to `1 × bandwidth_out_bps`.
        #[arg(long)]
        bandwidth_out_burst: Option<u64>,
        /// 011-rate-limiting-qos: optional new-connection burst
        /// override. Defaults to `1 × new_connections_per_sec`.
        #[arg(long)]
        new_connections_burst: Option<u32>,
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
    /// Take a snapshot of the SQLite store (008-sqlite-storage T062).
    /// Refuses to overwrite an existing file. If `--out` points at a
    /// directory, writes `forward-state-<RFC3339>.db` inside it.
    Backup {
        #[arg(long)]
        out: PathBuf,
    },
    /// Restore from a backup artefact (008-sqlite-storage T063).
    /// Refuses to clobber a non-empty data-dir without `--force`.
    Restore {
        #[arg(long)]
        r#in: PathBuf,
        #[arg(long)]
        force: bool,
    },
    /// Wipe the SQLite store (008-sqlite-storage T064). Verifies the
    /// target file looks like a SQLite database first so a typo'd
    /// `--data-dir` cannot delete arbitrary files.
    Reset {
        /// Required to actually proceed; without it, `reset` is a
        /// dry-run that prints the path it would remove.
        #[arg(long)]
        confirm: bool,
    },
    /// Audit-table maintenance subcommands (008-sqlite-storage T076).
    #[command(subcommand)]
    Audit(AuditCmd),
    /// 011-rate-limiting-qos T028: per-owner rate-limit envelope
    /// CRUD. Wraps `/v1/clients/{id}/owners/{owner_id}/rate-limit`
    /// for operators who don't want to hand-craft curl invocations.
    /// Per-owner ceilings bind before per-rule caps (FR-013) and
    /// apply to every rule the user pushes to the named client.
    #[command(subcommand)]
    OwnerCap(OwnerCapCmd),
}

#[derive(Subcommand, Debug)]
enum AuditCmd {
    /// Delete audit rows older than `--before <RFC3339>`. Pass
    /// `--dry-run` to print the count without mutating the store.
    Prune {
        #[arg(long)]
        before: String,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand, Debug)]
enum OwnerCapCmd {
    /// `owner-cap list <client>` — list every owner pushing rules
    /// to this client and whether each currently carries a cap
    /// envelope.
    List {
        client: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    /// `owner-cap get <client> <owner>` — fetch the current cap
    /// envelope. Exits with `rule_not_found`-family code 8 when the
    /// owner has no envelope (uncapped is the default state).
    Get {
        client: String,
        owner: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    /// `owner-cap set <client> <owner> [--bandwidth-in-bps ...]` —
    /// upsert the envelope. Idempotent; later calls overwrite
    /// earlier values. At least one cap must be provided.
    Set {
        client: String,
        owner: String,
        /// Aggregate ingress bytes/sec across all the owner's
        /// rules on this client. Must be `> 0`; absent leaves
        /// ingress uncapped.
        #[arg(long)]
        bandwidth_in_bps: Option<u64>,
        /// Aggregate egress bytes/sec. Same shape as
        /// `--bandwidth-in-bps`.
        #[arg(long)]
        bandwidth_out_bps: Option<u64>,
        /// Aggregate new TCP connections / new UDP flows per
        /// second across the owner's rules.
        #[arg(long)]
        new_connections_per_sec: Option<u32>,
        /// Aggregate ceiling on simultaneously-active connections
        /// + UDP flows.
        #[arg(long)]
        concurrent_connections: Option<u32>,
        /// Optional burst override for `--bandwidth-in-bps`;
        /// defaults to `1 × rate`.
        #[arg(long)]
        bandwidth_in_burst: Option<u64>,
        /// Optional burst override for `--bandwidth-out-bps`.
        #[arg(long)]
        bandwidth_out_burst: Option<u64>,
        /// Optional burst override for
        /// `--new-connections-per-sec`.
        #[arg(long)]
        new_connections_burst: Option<u32>,
        #[arg(long, default_value = "127.0.0.1:7080")]
        http_endpoint: String,
    },
    /// `owner-cap delete <client> <owner>` — remove the envelope.
    /// Idempotent; absent envelope returns success.
    Delete {
        client: String,
        owner: String,
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
    let data_dir = forward_server::data_dir::resolve(cli.data_dir.clone());

    match cli.cmd {
        Cmd::Serve => {
            let opts = serve::ServeOptions {
                config_dir,
                data_dir: data_dir.clone(),
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
            let state =
                build_offline_state(&config_dir, &data_dir, cli.advertised_endpoint.clone())?;
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
            let state =
                build_offline_state(&config_dir, &data_dir, cli.advertised_endpoint.clone())?;
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
            let state =
                build_offline_state(&config_dir, &data_dir, cli.advertised_endpoint.clone())?;
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
            target_specs,
            targets_json,
            health_check_interval_secs,
            sni_pattern,
            bandwidth_in_bps,
            bandwidth_out_bps,
            new_connections_per_sec,
            concurrent_connections,
            bandwidth_in_burst,
            bandwidth_out_burst,
            new_connections_burst,
            http_endpoint,
        } => rule_cli::push(
            &http_endpoint,
            &client,
            &listen,
            target.as_deref(),
            &protocol,
            ack_timeout,
            prefer_ipv6,
            &target_specs,
            targets_json.as_deref(),
            health_check_interval_secs,
            sni_pattern.as_deref(),
            rule_cli::RateLimitArgs {
                bandwidth_in_bps,
                bandwidth_out_bps,
                new_connections_per_sec,
                concurrent_connections,
                bandwidth_in_burst,
                bandwidth_out_burst,
                new_connections_burst,
            },
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
            std::fs::create_dir_all(&data_dir).map_err(|e| {
                eprintln!("data dir: {e}");
                1u8
            })?;
            let code = bootstrap::bootstrap_superadmin(&data_dir, &name);
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
        Cmd::Backup { out } => match forward_server::store::backup::run_backup(&data_dir, &out) {
            Ok(written) => {
                println!("backup={}", written.display());
                Ok(())
            }
            Err(e) => {
                eprintln!("error: {e}");
                Err(e.exit_code())
            }
        },
        Cmd::Restore { r#in, force } => {
            std::fs::create_dir_all(&data_dir).map_err(|e| {
                eprintln!("data dir: {e}");
                1u8
            })?;
            match forward_server::store::backup::run_restore(&r#in, &data_dir, force) {
                Ok(()) => {
                    println!("restore=ok data_dir={}", data_dir.display());
                    Ok(())
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    Err(e.exit_code())
                }
            }
        }
        Cmd::Audit(AuditCmd::Prune { before, dry_run }) => {
            let cutoff = match chrono::DateTime::parse_from_rfc3339(&before) {
                Ok(dt) => dt.with_timezone(&chrono::Utc),
                Err(e) => {
                    eprintln!("error: --before must be RFC3339: {e}");
                    return Err(3);
                }
            };
            let store = match forward_server::store::Store::open(&data_dir) {
                Ok(s) => std::sync::Arc::new(s),
                Err(e) => {
                    eprintln!("error: open store: {e:?}");
                    return Err(1);
                }
            };
            if dry_run {
                match store.audit_prune_count(cutoff) {
                    Ok(n) => {
                        println!("audit_prune dry_run=true would_delete={n}");
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        Err(1)
                    }
                }
            } else {
                match store.audit_prune_apply(cutoff) {
                    Ok(n) => {
                        println!("audit_prune deleted={n}");
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        Err(1)
                    }
                }
            }
        }
        Cmd::Reset { confirm } => {
            if !confirm {
                println!("would remove: {}", data_dir.join("state.db").display());
                println!("dry-run: pass --confirm to proceed");
                return Ok(());
            }
            match forward_server::store::backup::run_reset(&data_dir) {
                Ok(()) => Ok(()),
                Err(e) => {
                    eprintln!("error: {e}");
                    Err(e.exit_code())
                }
            }
        }
        Cmd::OwnerCap(OwnerCapCmd::List {
            client,
            format,
            http_endpoint,
        }) => owner_cap_cli::list(&http_endpoint, &client, format),
        Cmd::OwnerCap(OwnerCapCmd::Get {
            client,
            owner,
            format,
            http_endpoint,
        }) => owner_cap_cli::get(&http_endpoint, &client, &owner, format),
        Cmd::OwnerCap(OwnerCapCmd::Set {
            client,
            owner,
            bandwidth_in_bps,
            bandwidth_out_bps,
            new_connections_per_sec,
            concurrent_connections,
            bandwidth_in_burst,
            bandwidth_out_burst,
            new_connections_burst,
            http_endpoint,
        }) => owner_cap_cli::set(
            &http_endpoint,
            &client,
            &owner,
            rule_cli::RateLimitArgs {
                bandwidth_in_bps,
                bandwidth_out_bps,
                new_connections_per_sec,
                concurrent_connections,
                bandwidth_in_burst,
                bandwidth_out_burst,
                new_connections_burst,
            },
        ),
        Cmd::OwnerCap(OwnerCapCmd::Delete {
            client,
            owner,
            http_endpoint,
        }) => owner_cap_cli::delete(&http_endpoint, &client, &owner),
    }
}

fn resolve_config_dir(override_: Option<PathBuf>) -> PathBuf {
    if let Some(p) = override_ {
        return p;
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("portunus");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config/portunus");
    }
    PathBuf::from("./portunus.config")
}

/// Build a state suitable for *offline* operator commands (provision-client,
/// revoke, list-clients). Loads (or generates) TLS material so that newly
/// issued bundles carry the correct fingerprint.
fn build_offline_state(
    config_dir: &std::path::Path,
    data_dir: &std::path::Path,
    advertised_endpoint: Option<String>,
) -> Result<AppState, u8> {
    std::fs::create_dir_all(config_dir).map_err(|e| {
        eprintln!("config dir: {e}");
        1u8
    })?;
    std::fs::create_dir_all(data_dir).map_err(|e| {
        eprintln!("data dir: {e}");
        1u8
    })?;
    let paths = cli::default_paths(config_dir);
    let tls = ServerTlsMaterial::load_or_generate(&paths.cert, &paths.key).map_err(|e| {
        eprintln!("tls: {e}");
        1u8
    })?;
    // 008-sqlite-storage T052 — offline path opens SQLite first, then
    // wraps it with both Authenticator surfaces. The legacy file paths
    // (`paths.tokens`, `identity.json`) are no longer touched.
    let _ = paths.tokens; // silence "unused" if the field is unread now
    let store = Arc::new(forward_server::store::Store::open(data_dir).map_err(|e| {
        eprintln!("store: {e}");
        1u8
    })?);
    let tokens = Arc::new(forward_server::store::token_store::SqliteTokenStore::new(
        Arc::clone(&store),
    ));
    let operator_store = Arc::new(
        forward_server::store::operator_store::SqliteOperatorStore::new(Arc::clone(&store)),
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
        store,
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
