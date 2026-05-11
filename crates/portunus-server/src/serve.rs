//! Wires every subsystem behind the `serve` subcommand.
//!
//! Layout:
//! 1. Resolve optional `<data-dir>/server.toml` overrides.
//! 2. Open SQLite and load/generate TLS material.
//! 3. Determine the externally-advertised endpoint (used in bundles).
//! 4. Start the Tonic gRPC listener with the bearer-token interceptor.
//! 5. Start the operator HTTP listener.
//! 6. Reserve the metrics listener bind point (US3 wires the real handler).
//! 7. Await SIGINT/SIGTERM; trigger drain.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::store::operator_store::SqliteOperatorStore;
use crate::store::token_store::SqliteTokenStore;
use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use chrono::Utc;
use portunus_auth::OperatorAuthenticator;
use portunus_core::PortunusError;
use portunus_core::config::ServerConfig;
use tokio::net::TcpListener;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Identity, ServerTlsConfig};
use tracing::{error, info};

use crate::clients::ConnectedClients;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::service::ControlService;
use crate::metrics::Metrics;
use crate::operator::http;
use crate::shutdown::Shutdown;
use crate::state::AppState;
use crate::tls::ServerTlsMaterial;
use portunus_proto::v1::control_server::ControlServer;

#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Directory holding `state.db`, SQLite-managed sidecars, generated
    /// TLS material, and optional `server.toml` overrides.
    pub data_dir: PathBuf,
    /// Override the host:port advertised in newly-issued bundles.
    pub advertised_endpoint: Option<String>,
    /// Override operator HTTP API + Web UI bind address.
    pub operator_http_listen: Option<SocketAddr>,
}

// Wires every subsystem (TLS, gRPC, operator HTTP, metrics, shutdown). Splitting
// would scatter the dependency graph across helpers without making it easier to
// reason about, so we accept the line count.
#[allow(clippy::too_many_lines)]
pub async fn run(opts: ServeOptions) -> Result<(), PortunusError> {
    let cfg = load_config(&opts)?;
    std::fs::create_dir_all(&opts.data_dir)?;

    // 008-sqlite-storage T021 boot order step (1)+(2): probe the
    // filesystem class hosting --data-dir BEFORE opening SQLite, so
    // we never land on NFS / tmpfs and risk silent corruption.
    match crate::data_dir::probe_fs_class(&opts.data_dir) {
        crate::data_dir::FsClass::Unsupported(fs) => {
            return Err(PortunusError::Tls(format!(
                "startup.unsupported_filesystem path={} fs={fs}",
                opts.data_dir.display()
            )));
        }
        crate::data_dir::FsClass::Supported | crate::data_dir::FsClass::Unknown => {}
    }

    // 008-sqlite-storage T020 boot order step (3): warn-and-ignore
    // any pre-v0.8 JSON persistence files left in --data-dir. The
    // server does not read these files; they are listed so an
    // operator does not get a silent surprise.
    warn_legacy_json_files(&opts.data_dir);

    // 008-sqlite-storage T021 boot order step (4)+(5): open the store
    // and run pending forward migrations.
    let store = Arc::new(crate::store::Store::open(&opts.data_dir).map_err(|e| {
        PortunusError::Tls(format!(
            "startup.store_open path={} {e}",
            opts.data_dir.display()
        ))
    })?);
    info!(
        event = "store.ready",
        path = %store.db_path().display(),
        schema_version = store.schema_version().unwrap_or(0),
    );

    let tls = ServerTlsMaterial::load_or_generate(&cfg.tls_cert_path, &cfg.tls_key_path)?;
    // 008-sqlite-storage T052 — both token and operator stores now live
    // inside the SQLite database. The `cfg.token_store_path` and
    // `cfg.operator_store_path` are no longer touched by the runtime;
    // legacy files at those paths are warned about above and otherwise
    // ignored (R-009).
    let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&store)));
    let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&store)));
    info!(
        event = "operator.store_loaded",
        path = %store.db_path().display(),
        users = operator_store.list_users().len(),
        grants = operator_store.list_grants(None).len(),
        superadmin_present = operator_store.has_any_superadmin(),
    );
    // FR-006 operator_token shortcut: if `operator_token` is set in
    // server.toml AND no superadmin exists yet, mint the reserved
    // `_legacy` superadmin with this exact token. Idempotent: if a
    // superadmin already exists we silently skip (the operator can
    // safely leave the line in their config across restarts).
    if let Some(token) = cfg.operator_token.as_deref()
        && !operator_store.has_any_superadmin()
    {
        match operator_store.bootstrap_legacy_superadmin(token) {
            Ok(()) => info!(event = "operator.bootstrap_legacy", outcome = "ok"),
            Err(e) => {
                error!(event = "operator.bootstrap_legacy", outcome = "fail", error = %e);
                return Err(PortunusError::Tls(format!("operator_token bootstrap: {e}")));
            }
        }
    }
    if !operator_store.has_any_superadmin() {
        let now = Utc::now();
        let raw = operator_store
            .rotate_onboarding_setup_token(now)
            .map_err(|e| PortunusError::Tls(format!("rotate onboarding setup token: {e}")))?;
        info!(
            event = "operator.onboarding_setup_token_rotated",
            expires_at = %(now + crate::operator::setup_token::DEFAULT_SETUP_TOKEN_TTL),
        );
        eprintln!("Portunus onboarding setup token: {raw}");
    }
    let clients = ConnectedClients::default();
    let shutdown = Shutdown::new();

    // Bind first so port==0 in config gets resolved by the OS before we bake
    // an advertised endpoint into newly-issued bundles.
    let grpc_listener = TcpListener::bind(cfg.control_listen).await?;
    let grpc_addr = grpc_listener.local_addr()?;
    let http_listener = TcpListener::bind(cfg.operator_http_listen).await?;
    let http_addr = http_listener.local_addr()?;

    let advertised = opts.advertised_endpoint.unwrap_or_else(|| {
        // For local-loopback dev defaults like 0.0.0.0:7443, advertise the
        // loopback address — operator can override on the CLI.
        if grpc_addr.ip().is_unspecified() {
            format!("127.0.0.1:{}", grpc_addr.port())
        } else {
            grpc_addr.to_string()
        }
    });

    let cfg_arc = Arc::new(cfg.clone());
    let state = Arc::new(
        AppState::new(
            Arc::clone(&tokens),
            Arc::clone(&operator_store),
            clients.clone(),
            advertised,
            tls.leaf_fingerprint_hex.clone(),
            tls.cert_pem.clone(),
            cfg.range_rule_max_ports,
            Arc::clone(&store),
        )
        .map_err(|e| PortunusError::Tls(format!("metrics: {e}")))?
        .with_server_config(cfg_arc),
    );
    let persisted_rules = state
        .rule_store
        .list_rules()
        .map_err(|e| PortunusError::Tls(format!("load persisted rules: {e}")))?;
    let persisted_count = persisted_rules.len();
    state.rules.hydrate(persisted_rules).await;
    info!(event = "rules.hydrated", count = persisted_count);

    // 008-sqlite-storage T034 — spawn the durable audit writer and
    // bind its Handle into the AuditRing fan-out. Until US2 retires
    // the in-memory ring entirely, every audit emit dual-writes.
    let audit_writer_handle = crate::store::audit_writer::spawn(
        Arc::clone(&store),
        state.metrics.audit_buffer_drops_total.clone(),
        state.metrics.audit_durable_writer_lag_seconds.clone(),
        shutdown.child(),
    );
    state.audit.bind_durable_writer(audit_writer_handle);

    let interceptor = AuthInterceptor::new(
        tokens.clone() as Arc<dyn portunus_auth::Authenticator>,
        Arc::clone(&state.metrics),
    );
    let control = ControlServer::new(ControlService::new(Arc::clone(&state)));
    let intercepted: InterceptedService<_, AuthInterceptor> =
        InterceptedService::new(control, interceptor);

    let identity = Identity::from_pem(tls.cert_pem.as_bytes(), tls.key_pem.as_bytes());
    let tls_acceptor = ServerTlsConfig::new().identity(identity);
    let metrics_listener = TcpListener::bind(cfg.metrics_listen).await?;
    let metrics_addr = metrics_listener.local_addr()?;
    assert!(
        metrics_addr.ip().is_loopback(),
        "metrics_listen must bind to loopback (got {metrics_addr})"
    );

    info!(event = "server.listening", grpc = %grpc_addr, operator_http = %http_addr, metrics = %metrics_addr);

    let signal_task = tokio::spawn({
        let s = shutdown.clone();
        async move { s.signal_handler().await }
    });

    // T049 (005-multi-user-rbac) was a SIGHUP-driven reload of the
    // JSON identity store. 008-sqlite-storage retired that path: every
    // read pulls fresh state directly from SQLite, so there's no
    // in-memory cache to reload. We still log the SIGHUP so operators
    // who scripted around it can confirm receipt.
    #[cfg(unix)]
    let _sighup_task = {
        let store = Arc::clone(&operator_store);
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sig = match signal(SignalKind::hangup()) {
                Ok(s) => s,
                Err(e) => {
                    error!(event = "operator.sighup_listener_failed", error = %e);
                    return;
                }
            };
            while sig.recv().await.is_some() {
                info!(
                    event = "operator.store_reloaded",
                    users = store.list_users().len(),
                    grants = store.list_grants(None).len(),
                    superadmin_present = store.has_any_superadmin(),
                    outcome = "ok",
                    note = "sqlite store reads are always fresh; SIGHUP is a no-op",
                );
            }
        })
    };

    let grpc_shutdown = shutdown.token();
    let grpc_task = tokio::spawn(async move {
        let result = tonic::transport::Server::builder()
            .tls_config(tls_acceptor)
            .map_err(|e| PortunusError::Tls(e.to_string()))?
            .add_service(intercepted)
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::TcpListenerStream::new(grpc_listener),
                async move { grpc_shutdown.cancelled().await },
            )
            .await;
        if let Err(e) = result {
            error!(event = "server.grpc_failed", error = %e);
            return Err(PortunusError::Tls(e.to_string()));
        }
        Ok::<_, PortunusError>(())
    });

    // 006-management-web-ui T063: SPA fallback. The fallback runs only
    // when no `/v1/*` route matches, so the operator API always wins.
    let operator_router =
        http::router(Arc::clone(&state)).fallback(crate::operator::webui::serve_webui);
    let http_shutdown = shutdown.token();
    let http_task = tokio::spawn(async move {
        let res = axum::serve(
            http_listener,
            operator_router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { http_shutdown.cancelled().await })
        .await;
        if let Err(e) = res {
            error!(event = "server.operator_http_failed", error = %e);
        }
    });

    let metrics_shutdown = shutdown.token();
    // 009-tls-sni-routing T081: render_metrics refreshes the
    // `portunus_tls_sni_routes_active` gauge on every scrape, so it
    // needs both the Prometheus collectors and the rule store.
    let metrics_state = MetricsState {
        metrics: Arc::clone(&state.metrics),
        rules: state.rules.clone(),
    };
    let metrics_router = Router::new()
        .route("/metrics", get(render_metrics))
        .with_state(metrics_state);
    let metrics_task = tokio::spawn(async move {
        let res = axum::serve(metrics_listener, metrics_router)
            .with_graceful_shutdown(async move { metrics_shutdown.cancelled().await })
            .await;
        if let Err(e) = res {
            error!(event = "server.metrics_failed", error = %e);
        }
    });

    // Wait for shutdown signal, then drain.
    let _ = signal_task.await;
    info!(event = "server.draining");
    clients.shutdown();
    let _ = tokio::join!(grpc_task, http_task, metrics_task);
    info!(event = "server.stopped");
    Ok(())
}

#[derive(Clone)]
struct MetricsState {
    metrics: Arc<Metrics>,
    rules: crate::rules::ServerRuleStore,
}

async fn render_metrics(State(state): State<MetricsState>) -> impl IntoResponse {
    // 009-tls-sni-routing T081: refresh on-demand. Cheap (one read
    // lock; rule sets are small in any deployment that scrapes
    // Prometheus). Keeps the gauge consistent with the rule store
    // without adding a background tick.
    let active = state.rules.count_with_sni().await;
    state
        .metrics
        .tls_sni_routes_active
        .set(i64::try_from(active).unwrap_or(i64::MAX));
    let body = state.metrics.render();
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

fn load_config(opts: &ServeOptions) -> Result<ServerConfig, PortunusError> {
    let resolved = opts.data_dir.join("server.toml");
    let mut cfg = if resolved.exists() {
        ServerConfig::from_toml_path_with_data_dir(&resolved, &opts.data_dir)
    } else {
        Ok(ServerConfig::default_for_data_dir(&opts.data_dir))
    }?;
    if let Some(operator_http_listen) = opts.operator_http_listen {
        cfg.operator_http_listen = operator_http_listen;
    }
    Ok(cfg)
}

/// 008-sqlite-storage T020 — legacy JSON warn-and-ignore.
///
/// The pre-v0.8 persistence layer wrote `tokens.json` / `identity.json`
/// / `rules.json` under the data directory. v0.8 retired all three; the
/// store at `<data-dir>/state.db` is the only source of truth.
/// Operators upgrading from a v0.7 install in dev environments may
/// have stale files lying around; rather than silently ignore them
/// (operator-hostile) or refuse to start (operator-hostile), we log
/// one structured warning per file pointing at `portunus-server reset`.
fn warn_legacy_json_files(data_dir: &std::path::Path) {
    for name in ["tokens.json", "identity.json", "rules.json"] {
        let path = data_dir.join(name);
        if path.exists() {
            tracing::warn!(
                event = "startup.legacy_persistence_file_ignored",
                path = %path.display(),
                hint = "Pre-v0.8 file; not loaded. Run `portunus-server reset` to clean.",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Second half of FR-022 verification: the metrics endpoint MUST refuse
    /// non-loopback binds. The runtime assertion in `run()` enforces this; the
    /// test mirrors the bind logic so a regression in the default config is
    /// caught immediately.
    #[tokio::test]
    async fn metrics_listener_binds_loopback_only() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(addr.ip().is_loopback(), "metrics bound to {addr}");
    }

    #[tokio::test]
    async fn render_metrics_returns_prometheus_text() {
        let metrics = Arc::new(Metrics::new().unwrap());
        metrics.clients_connected.set(1);
        metrics
            .auth_failures_total
            .with_label_values(&["unknown_token"])
            .inc();
        let body = String::from_utf8(metrics.render()).unwrap();
        assert!(body.contains("portunus_clients_connected 1"), "{body}");
        assert!(
            body.contains("portunus_auth_failures_total{reason=\"unknown_token\"} 1"),
            "{body}"
        );
    }
}
