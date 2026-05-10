//! Shared state injected into operator handlers and the gRPC service.

use std::sync::Arc;

use portunus_auth::OperatorAuthenticator;
use portunus_core::config::ServerConfig;

use crate::clients::ConnectedClients;
use crate::metrics::{Metrics, RuleStatsCache};
use crate::operator::audit::AuditRing;
use crate::operator::per_port_stats::PerPortStatsCache;
use crate::owner::OwnerCapService;
use crate::rules::ServerRuleStore;
use crate::store::Store;
use crate::store::operator_store::SqliteOperatorStore;
use crate::store::rule_store::SqliteRuleStore;
use crate::store::token_store::SqliteTokenStore;

#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<SqliteTokenStore>,
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
    /// Operator-side identity store (005-multi-user-rbac). Always
    /// present after `serve.rs` startup; used by the auth_layer
    /// middleware (T019) to verify operator bearer tokens.
    pub operator_store: Arc<SqliteOperatorStore>,
    /// Same store, exposed via the `OperatorAuthenticator` trait so
    /// the auth_layer can take an abstraction (Constitution I single
    /// seam). In v0.5.0 this is just `Arc::clone`-cast of the store
    /// above; future impls (e.g., OIDC) can swap it.
    pub operator_auth: Arc<dyn OperatorAuthenticator>,
    /// 006-management-web-ui T009: in-memory audit ring buffer fed by
    /// the auth_layer's allow/deny emit sites and read by
    /// `GET /v1/audit`. Capacity 1000; ≈ 200 KB resident.
    ///
    /// 008-sqlite-storage US1 retires this in favour of `store` once
    /// the audit_writer is wired (T032). For Phase 2 + early Phase 3
    /// the ring buffer stays as the primary read path while the
    /// audit_writer fans the same entries into the durable table; the
    /// retirement flip happens in T032 / T033.
    pub audit: Arc<AuditRing>,
    /// 008-sqlite-storage T019 — persistent SQLite store. Owns the
    /// connection pool, schema migrations, and (after US1's wiring)
    /// the audit-write durable sink. `Arc<Store>` is cheap to clone.
    pub store: Arc<Store>,
    /// Persisted rule definitions / runtime state.
    pub rule_store: Arc<SqliteRuleStore>,
    /// 011-rate-limiting-qos T027: per-owner cap service. Wraps the
    /// SQLite `rate_limit_owner` table with validation, in-memory
    /// cache, and GC-on-rule-removal sweep. REST handlers (T028) and
    /// the gRPC `OwnerRateLimitUpdate` push path (T029) read from this.
    pub owner_caps: OwnerCapService,
}

impl AppState {
    /// # Errors
    ///
    /// Propagates `prometheus::Error` from collector registration — only fails
    /// on duplicate metric names, which would be a programming bug.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tokens: Arc<SqliteTokenStore>,
        operator_store: Arc<SqliteOperatorStore>,
        clients: ConnectedClients,
        server_endpoint: impl Into<String>,
        server_cert_sha256: impl Into<String>,
        server_cert_pem: impl Into<String>,
        range_rule_max_ports: u32,
        store: Arc<Store>,
    ) -> Result<Self, prometheus::Error> {
        let operator_auth: Arc<dyn OperatorAuthenticator> = operator_store.clone();
        let audit = Arc::new(AuditRing::new());
        let metrics = Arc::new(Metrics::new()?);
        let rule_store = Arc::new(SqliteRuleStore::new(Arc::clone(&store)));
        // 011-rate-limiting-qos T027: hydrate the owner-cap service from
        // SQLite. Boot path failures degrade to an empty cache so the
        // server still comes up — the rate-limit subsystem treats a
        // missing envelope as "uncapped" anyway, and a corrupted cap row
        // would have failed the migration's CHECK constraint at write
        // time.
        let owner_caps = match OwnerCapService::open(Arc::clone(&store)) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    event = "owner_cap.hydrate_failed",
                    error = %e,
                );
                return Err(prometheus::Error::Msg(format!(
                    "owner_cap_service_open: {e}"
                )));
            }
        };
        // T009: stitch the audit ring's drop counter into Prometheus so
        // an oversaturated buffer becomes visible without grepping logs.
        audit.bind_drops_metric(metrics.audit_buffer_drops_total.clone());
        Ok(Self {
            tokens,
            clients,
            rules: ServerRuleStore::new(),
            server_endpoint: server_endpoint.into(),
            server_cert_sha256: server_cert_sha256.into(),
            server_cert_pem: server_cert_pem.into(),
            metrics,
            stats_cache: RuleStatsCache::new(),
            per_port_stats: PerPortStatsCache::new(),
            range_rule_max_ports,
            server_config: None,
            operator_store,
            operator_auth,
            audit,
            store,
            rule_store,
            owner_caps,
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
