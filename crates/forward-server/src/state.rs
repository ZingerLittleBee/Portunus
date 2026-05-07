//! Shared state injected into operator handlers and the gRPC service.

use std::sync::Arc;

use forward_auth::file_store::FileTokenStore;
use forward_core::config::ServerConfig;

use crate::clients::ConnectedClients;
use crate::metrics::{Metrics, RuleStatsCache};
use crate::operator::per_port_stats::PerPortStatsCache;
use crate::rules::ServerRuleStore;

#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<FileTokenStore>,
    pub clients: ConnectedClients,
    pub rules: ServerRuleStore,
    /// `host:port` advertised in newly-issued credential bundles.
    pub server_endpoint: String,
    /// Lowercase 64-char hex SHA-256 of the server leaf cert DER.
    pub server_cert_sha256: String,
    /// PEM-encoded server leaf certificate (carried in bundles so the
    /// client can trust exactly this cert without a CA chain).
    pub server_cert_pem: String,
    /// Process-wide Prometheus collectors. Cheap to clone (`Arc` internal).
    pub metrics: Arc<Metrics>,
    /// Latest per-rule `StatsReport` snapshots, fed by US3 stream and read by
    /// the operator `rule-stats` view.
    pub stats_cache: RuleStatsCache,
    /// Per-port detail cache for range rules (FR-011 / 002-port-range-forward).
    /// Fed by `StatsReport.per_port`; read on demand when an operator
    /// passes `--per-port`. Never re-exported as Prometheus series.
    pub per_port_stats: PerPortStatsCache,
    /// Maximum ports any single range rule may span (FR-008). Loaded
    /// from `server.toml`'s `range_rule_max_ports`.
    pub range_rule_max_ports: u32,
    /// Full loaded `ServerConfig` if the server was started from a
    /// TOML file. The gRPC handler reads `udp_flow_idle_secs()` and
    /// `udp_max_flows_per_rule()` from this to populate `Welcome`
    /// (T013, 004-udp-forward). `None` when the server was started
    /// from a CLI-only path that pre-dates the v0.4.0 tunables.
    pub server_config: Option<Arc<ServerConfig>>,
}

impl AppState {
    /// # Errors
    ///
    /// Propagates `prometheus::Error` from collector registration — only fails
    /// on duplicate metric names, which would be a programming bug.
    pub fn new(
        tokens: Arc<FileTokenStore>,
        clients: ConnectedClients,
        server_endpoint: impl Into<String>,
        server_cert_sha256: impl Into<String>,
        server_cert_pem: impl Into<String>,
        range_rule_max_ports: u32,
    ) -> Result<Self, prometheus::Error> {
        Ok(Self {
            tokens,
            clients,
            rules: ServerRuleStore::new(),
            server_endpoint: server_endpoint.into(),
            server_cert_sha256: server_cert_sha256.into(),
            server_cert_pem: server_cert_pem.into(),
            metrics: Arc::new(Metrics::new()?),
            stats_cache: RuleStatsCache::new(),
            per_port_stats: PerPortStatsCache::new(),
            range_rule_max_ports,
            server_config: None,
        })
    }

    /// Attach a loaded `ServerConfig` so the handler can plumb UDP
    /// tunables into Welcome (T013, 004-udp-forward).
    #[must_use]
    pub fn with_server_config(mut self, cfg: Arc<ServerConfig>) -> Self {
        // Keep `range_rule_max_ports` in sync — when AppState is built
        // before the config is attached, the original value is what
        // `main.rs` saw on the CLI; passing the same value through here
        // is harmless but makes intent explicit.
        self.range_rule_max_ports = cfg.range_rule_max_ports;
        self.server_config = Some(cfg);
        self
    }
}
