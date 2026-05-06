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

use forward_auth::file_store::FileTokenStore;
use forward_core::ForwardError;
use forward_core::config::ServerConfig;
use tokio::net::TcpListener;
use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Identity, ServerTlsConfig};
use tracing::{error, info};

use crate::clients::ConnectedClients;
use crate::grpc::interceptor::AuthInterceptor;
use crate::grpc::service::ControlService;
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

pub async fn run(opts: ServeOptions) -> Result<(), ForwardError> {
    let cfg = load_config(&opts)?;
    std::fs::create_dir_all(&opts.config_dir)?;

    let tls = ServerTlsMaterial::load_or_generate(&cfg.tls_cert_path, &cfg.tls_key_path)?;
    let tokens = Arc::new(
        FileTokenStore::open(&cfg.token_store_path)
            .map_err(|e| ForwardError::Tls(format!("token store: {e}")))?,
    );
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

    let state = Arc::new(AppState::new(
        Arc::clone(&tokens),
        clients.clone(),
        advertised,
        tls.leaf_fingerprint_hex.clone(),
        tls.cert_pem.clone(),
    ));

    let interceptor = AuthInterceptor::new(tokens.clone() as Arc<dyn forward_auth::Authenticator>);
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

    let operator_router = http::router(Arc::clone(&state));
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
    let metrics_task = tokio::spawn(async move {
        // Phase 3 placeholder: keep the bind point reserved per FR-022 until
        // T063 swaps in the real Prometheus handler. Listen, drop incoming.
        loop {
            tokio::select! {
                () = metrics_shutdown.cancelled() => break,
                accept = metrics_listener.accept() => {
                    match accept {
                        Ok((stream, _)) => drop(stream),
                        Err(_) => break,
                    }
                }
            }
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
    }
}
