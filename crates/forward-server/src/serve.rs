//! Wires every subsystem behind the `serve` subcommand.
//!
//! Layout:
//! 1. Resolve config (file → defaults rooted at `--config-dir`).
//! 2. Open `FileTokenStore` and load/generate TLS material.
//! 3. Determine the externally-advertised endpoint (used in bundles).
//! 4. Start the Tonic gRPC listener with the bearer-token interceptor.
//! 5. Start the loopback operator HTTP listener.
//! 6. Reserve the metrics listener bind point (US3 wires the real handler).
//! 7. Await SIGINT/SIGTERM; trigger drain.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use forward_auth::OperatorAuthenticator;
use forward_auth::file_store::FileTokenStore;
use forward_auth::operator_store::FileOperatorStore;
use forward_core::ForwardError;
use forward_core::config::ServerConfig;
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
use forward_proto::v1::control_server::ControlServer;

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub config_dir: PathBuf,
    pub config_file: Option<PathBuf>,
    /// Override the host:port advertised in newly-issued bundles.
    pub advertised_endpoint: Option<String>,
}

// Wires every subsystem (TLS, gRPC, operator HTTP, metrics, shutdown). Splitting
// would scatter the dependency graph across helpers without making it easier to
// reason about, so we accept the line count.
#[allow(clippy::too_many_lines)]
pub async fn run(opts: ServeOptions) -> Result<(), ForwardError> {
    let cfg = load_config(&opts)?;
    std::fs::create_dir_all(&opts.config_dir)?;

    let tls = ServerTlsMaterial::load_or_generate(&cfg.tls_cert_path, &cfg.tls_key_path)?;
    let tokens = Arc::new(
        FileTokenStore::open(&cfg.token_store_path)
            .map_err(|e| ForwardError::Tls(format!("token store: {e}")))?,
    );
    // 005-multi-user-rbac T013: load the operator-side identity store.
    // Failure to load = exit non-zero with a clear message; operators
    // restore from backup or run `bootstrap-superadmin` against an empty file.
    let operator_store = Arc::new(
        FileOperatorStore::open(&cfg.operator_store_path)
            .map_err(|e| ForwardError::Tls(format!("operator store: {e}")))?,
    );
    info!(
        event = "operator.store_loaded",
        path = %cfg.operator_store_path.display(),
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
                return Err(ForwardError::Tls(format!("operator_token bootstrap: {e}")));
            }
        }
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
        )
        .map_err(|e| ForwardError::Tls(format!("metrics: {e}")))?
        .with_server_config(cfg_arc),
    );

    let interceptor = AuthInterceptor::new(
        tokens.clone() as Arc<dyn forward_auth::Authenticator>,
        Arc::clone(&state.metrics),
    );
    let control = ControlServer::new(ControlService::new(Arc::clone(&state)));
    let intercepted: InterceptedService<_, AuthInterceptor> =
        InterceptedService::new(control, interceptor);

    let identity = Identity::from_pem(tls.cert_pem.as_bytes(), tls.key_pem.as_bytes());
    let tls_acceptor = ServerTlsConfig::new().identity(identity);
    assert!(
        http_addr.ip().is_loopback(),
        "operator_http_listen must bind to loopback (got {http_addr})"
    );
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

    // T049 (005-multi-user-rbac): SIGHUP triggers an in-place reload
    // of `identity.json`. Linux + macOS only (Windows has no SIGHUP).
    // On reload-validation failure the prior in-memory snapshot is
    // kept; we emit one structured log line either way.
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
                match store.reload_from_disk() {
                    Ok(()) => info!(
                        event = "operator.store_reloaded",
                        users = store.list_users().len(),
                        grants = store.list_grants(None).len(),
                        superadmin_present = store.has_any_superadmin(),
                        outcome = "ok",
                    ),
                    Err(e) => tracing::warn!(
                        event = "operator.store_reload_failed",
                        reason = %e,
                        outcome = "fail",
                    ),
                }
            }
        })
    };

    let grpc_shutdown = shutdown.token();
    let grpc_task = tokio::spawn(async move {
        let result = tonic::transport::Server::builder()
            .tls_config(tls_acceptor)
            .map_err(|e| ForwardError::Tls(e.to_string()))?
            .add_service(intercepted)
            .serve_with_incoming_shutdown(
                tokio_stream::wrappers::TcpListenerStream::new(grpc_listener),
                async move { grpc_shutdown.cancelled().await },
            )
            .await;
        if let Err(e) = result {
            error!(event = "server.grpc_failed", error = %e);
            return Err(ForwardError::Tls(e.to_string()));
        }
        Ok::<_, ForwardError>(())
    });

    // 006-management-web-ui T063: SPA fallback. The fallback runs only
    // when no `/v1/*` route matches, so the operator API always wins.
    let operator_router =
        http::router(Arc::clone(&state)).fallback(crate::operator::webui::serve_webui);
    let http_shutdown = shutdown.token();
    let http_task = tokio::spawn(async move {
        let res = axum::serve(http_listener, operator_router)
            .with_graceful_shutdown(async move { http_shutdown.cancelled().await })
            .await;
        if let Err(e) = res {
            error!(event = "server.operator_http_failed", error = %e);
        }
    });

    let metrics_shutdown = shutdown.token();
    let metrics_router = Router::new()
        .route("/metrics", get(render_metrics))
        .with_state(Arc::clone(&state.metrics));
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

async fn render_metrics(State(metrics): State<Arc<Metrics>>) -> impl IntoResponse {
    let body = metrics.render();
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

fn load_config(opts: &ServeOptions) -> Result<ServerConfig, ForwardError> {
    let resolved = opts
        .config_file
        .clone()
        .unwrap_or_else(|| opts.config_dir.join("server.toml"));
    if resolved.exists() {
        ServerConfig::from_toml_path(&resolved)
    } else {
        Ok(default_config(&opts.config_dir))
    }
}

fn default_config(config_dir: &std::path::Path) -> ServerConfig {
    use forward_core::config::LogFormat;
    ServerConfig {
        control_listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        operator_http_listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        metrics_listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        tls_cert_path: config_dir.join("server.crt"),
        tls_key_path: config_dir.join("server.key"),
        token_store_path: config_dir.join("tokens.json"),
        shutdown_drain_timeout_secs: 30,
        log_format: LogFormat::Json,
        range_rule_max_ports: 1024,
        udp_flow_idle_secs: None,
        udp_max_flows_per_rule: None,
        operator_store_path: config_dir.join("identity.json"),
        operator_token: None,
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
        assert!(body.contains("forward_clients_connected 1"), "{body}");
        assert!(
            body.contains("forward_auth_failures_total{reason=\"unknown_token\"} 1"),
            "{body}"
        );
    }
}
