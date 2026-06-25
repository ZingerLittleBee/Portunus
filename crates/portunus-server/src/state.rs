//! Shared state injected into operator handlers and the gRPC service.

use std::path::Path;
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
use crate::traffic_quotas::aggregator::TrafficAggregator;
use crate::traffic_quotas::cache::TrafficQuotaCache;

#[derive(Clone)]
pub struct AppState {
    pub tokens: Arc<SqliteTokenStore>,
    pub clients: ConnectedClients,
    pub rules: ServerRuleStore,
    /// Tier-2 seed: CLI `--advertised-endpoint` / `PORTUNUS_ADVERTISED_ENDPOINT`.
    pub advertised_seed: Option<String>,
    /// Server's resolved control-plane port (tiers 3 & 4).
    pub control_port: u16,
    /// Parsed leaf-cert SAN set (coverage gate for the resolver).
    pub cert_san: std::sync::Arc<crate::advertised::CertSanSet>,
    /// Operator advertised-endpoint override accessor.
    pub settings: std::sync::Arc<crate::store::settings_store::SqliteSettingsStore>,
    /// Lowercase 64-char hex SHA-256 of the server leaf cert DER.
    pub server_cert_sha256: String,
    /// PEM-encoded server leaf certificate. Used at startup to derive the SAN
    /// set and the `server_cert_sha256` pin; the certificate itself is never
    /// sent to clients — they trust only the pin.
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
    /// Explicit public operator origin used for CSRF Origin checks when
    /// the UI is fronted by a reverse proxy or public hostname. `None`
    /// triggers same-origin fallback (Origin vs `Host` header), which is
    /// the zero-config default for `localhost` / loopback / LAN access.
    pub operator_http_public_origin: Option<String>,
    /// Whether operator cookies should be marked `Secure`.
    pub operator_http_cookie_secure: bool,
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
    /// 013-traffic-quotas: in-memory mirror of `traffic_quotas` rows.
    /// HTTP CRUD writes through it; the stats aggregator (B2) reads +
    /// writes through it; the gRPC push task (C5) reads from it on
    /// reconnect replay. Cheap to clone (Arc internal).
    pub traffic_quotas: TrafficQuotaCache,
    /// 013-traffic-quotas B2: per-(rule, owner) stats aggregator hooked
    /// into the gRPC StatsReport path. Owns its own prev map for
    /// delta computation and emits a `QuotaExhaustedEvent` on first
    /// crossing (consumed by the gRPC push task spawned in serve.rs).
    pub traffic_aggregator: TrafficAggregator,
    /// 013-traffic-quotas B2 → C5: receiver paired with the aggregator's
    /// exhaust sender. `serve.rs` takes ownership once at boot to spawn
    /// the consumer that turns `QuotaExhaustedEvent` into a
    /// `TrafficQuotaUpdate{SET}` push (exhausted=true) toward the
    /// affected client. Wrapped in `Mutex<Option<…>>` so AppState
    /// itself stays Clone.
    pub traffic_quota_exhaust_rx: Arc<
        std::sync::Mutex<
            Option<
                tokio::sync::mpsc::Receiver<crate::traffic_quotas::aggregator::QuotaExhaustedEvent>,
            >,
        >,
    >,
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
        advertised_seed: Option<String>,
        control_port: u16,
        server_cert_sha256: impl Into<String>,
        server_cert_pem: impl Into<String>,
        range_rule_max_ports: u32,
        store: Arc<Store>,
    ) -> Result<Self, prometheus::Error> {
        let server_cert_pem: String = server_cert_pem.into();
        let server_cert_pem_ref: &str = &server_cert_pem;
        let operator_auth: Arc<dyn OperatorAuthenticator> = operator_store.clone();
        let audit = Arc::new(AuditRing::new());
        let metrics = Arc::new(Metrics::new()?);
        let rule_store = Arc::new(SqliteRuleStore::new(Arc::clone(&store)));
        let default_server_config = ServerConfig::default_for_data_dir(Path::new("."));
        // 011-rate-limiting-qos T027: hydrate the owner-cap service from
        // SQLite. Boot path failures degrade to an empty cache so the
        // server still comes up — the rate-limit subsystem treats a
        // missing envelope as "uncapped" anyway, and a corrupted cap row
        // would have failed the migration's CHECK constraint at write
        // time.
        // 013-traffic-quotas: hydrate the quota cache from the store at
        // boot. A failed load falls through to an empty cache (the
        // server still functions; unauthorized clients just don't see
        // quotas until the next successful CRUD write).
        let traffic_quotas = match TrafficQuotaCache::load((*store).clone()) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    event = "traffic_quota.cache_hydrate_failed",
                    error = %e,
                );
                return Err(prometheus::Error::Msg(format!(
                    "traffic_quota_cache_load: {e}"
                )));
            }
        };
        let (traffic_quota_exhaust_tx, traffic_quota_exhaust_rx_owned) = tokio::sync::mpsc::channel::<
            crate::traffic_quotas::aggregator::QuotaExhaustedEvent,
        >(64);
        let traffic_aggregator = TrafficAggregator::with_metrics(
            (*store).clone(),
            traffic_quotas.clone(),
            traffic_quota_exhaust_tx,
            Arc::clone(&metrics),
        );
        let traffic_quota_exhaust_rx =
            Arc::new(std::sync::Mutex::new(Some(traffic_quota_exhaust_rx_owned)));
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
        let cert_san = match crate::advertised::CertSanSet::from_pem(server_cert_pem_ref) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    event = "advertised.cert_san_parse_failed",
                    error = %e,
                );
                crate::advertised::CertSanSet::default()
            }
        };
        Ok(Self {
            tokens,
            clients,
            rules: ServerRuleStore::new(),
            advertised_seed,
            control_port,
            cert_san: std::sync::Arc::new(cert_san),
            settings: std::sync::Arc::new(crate::store::settings_store::SqliteSettingsStore::new(
                std::sync::Arc::clone(&store),
            )),
            server_cert_sha256: server_cert_sha256.into(),
            server_cert_pem,
            metrics,
            stats_cache: RuleStatsCache::new(),
            per_port_stats: PerPortStatsCache::new(),
            range_rule_max_ports,
            server_config: None,
            operator_http_public_origin: default_server_config
                .operator_http_origin_for_csrf()
                .map(str::to_owned),
            operator_http_cookie_secure: default_server_config.operator_http_cookie_secure(),
            operator_store,
            operator_auth,
            audit,
            store,
            rule_store,
            owner_caps,
            traffic_quotas,
            traffic_aggregator,
            traffic_quota_exhaust_rx,
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
        self.operator_http_public_origin = cfg.operator_http_origin_for_csrf().map(str::to_owned);
        self.operator_http_cookie_secure = cfg.operator_http_cookie_secure();
        self.server_config = Some(cfg);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::ConnectedClients;
    use crate::store::error::map_rusqlite;
    use tempfile::tempdir;

    /// Valid leaf-cert PEM whose SAN set covers `public.example` and
    /// `localhost` — same fixture the `advertised` tests use.
    const FIXTURE_PEM: &str = include_str!("advertised/testdata/san_fixture.pem");

    /// Build an `AppState` from a freshly-opened temp store, mirroring the
    /// helper used by the gRPC service / enrollment tests. The returned
    /// `tempdir` guard must be kept alive for the store's lifetime.
    fn build_state(cert_pem: &str) -> (AppState, tempfile::TempDir) {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&store)));
        let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&store)));
        operator_store
            .bootstrap_legacy_superadmin("test-token")
            .unwrap();
        let state = AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            None,
            7443,
            "deadbeef",
            cert_pem.to_string(),
            16,
            store,
        )
        .expect("AppState::new should succeed for a fresh store");
        (state, dir)
    }

    #[test]
    fn new_with_valid_cert_pem_parses_san_set() {
        // Happy path: a valid PEM yields a SAN set that covers the
        // fixture's declared names, proving `from_pem` took the `Ok`
        // branch rather than falling back to the empty default.
        let (state, _dir) = build_state(FIXTURE_PEM);
        assert!(state.cert_san.covers("public.example"));
        assert!(state.cert_san.covers("localhost"));
        // Defaults wired through `default_for_data_dir(".")`: no explicit
        // public origin, so CSRF falls back to same-origin and cookies
        // stay non-Secure.
        assert_eq!(state.operator_http_public_origin, None);
        assert!(!state.operator_http_cookie_secure);
        assert_eq!(state.control_port, 7443);
        assert_eq!(state.range_rule_max_ports, 16);
        assert_eq!(state.server_cert_sha256, "deadbeef");
        assert!(state.server_config.is_none());
    }

    #[test]
    fn new_with_invalid_cert_pem_falls_back_to_empty_san_set() {
        // An unparseable PEM must NOT abort construction — `from_pem`
        // errors are logged and swallowed in favour of the empty default
        // SAN set (covers nothing). This exercises the `Err(e)` arm.
        let (state, _dir) = build_state("not a pem at all");
        assert!(!state.cert_san.covers("public.example"));
        assert!(!state.cert_san.covers("localhost"));
        // The rest of the state is still fully constructed.
        assert_eq!(state.control_port, 7443);
    }

    #[test]
    fn with_server_config_overrides_csrf_and_cookie_and_caps() {
        let (state, _dir) = build_state(FIXTURE_PEM);
        let mut cfg = ServerConfig::default_for_data_dir(Path::new("."));
        cfg.range_rule_max_ports = 4096;
        cfg.operator_http_public_origin = Some("https://ops.example.com".to_string());
        let state = state.with_server_config(Arc::new(cfg));

        // Range cap is taken from the attached config.
        assert_eq!(state.range_rule_max_ports, 4096);
        // An https public origin flips both the CSRF origin and the
        // Secure-cookie flag.
        assert_eq!(
            state.operator_http_public_origin.as_deref(),
            Some("https://ops.example.com")
        );
        assert!(state.operator_http_cookie_secure);
        // The config is now attached.
        assert!(state.server_config.is_some());
    }

    #[test]
    fn with_server_config_http_origin_keeps_cookie_insecure() {
        // A plain-http public origin sets the CSRF origin but leaves the
        // Secure-cookie flag off (browsers would drop a Secure cookie on
        // plain HTTP).
        let (state, _dir) = build_state(FIXTURE_PEM);
        let mut cfg = ServerConfig::default_for_data_dir(Path::new("."));
        cfg.operator_http_public_origin = Some("http://ops.example.com".to_string());
        let state = state.with_server_config(Arc::new(cfg));

        assert_eq!(
            state.operator_http_public_origin.as_deref(),
            Some("http://ops.example.com")
        );
        assert!(!state.operator_http_cookie_secure);
    }

    #[test]
    fn new_propagates_traffic_quota_cache_load_failure() {
        // Drop the `traffic_quotas` table so the boot-time cache hydrate
        // (`TrafficQuotaCache::load`) fails. `new()` must surface this as
        // a `prometheus::Error::Msg` rather than panic — the `Err(e)` arm
        // at line 157.
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        store
            .with_write_tx(|tx| {
                tx.execute_batch("DROP TABLE traffic_quotas")
                    .map_err(map_rusqlite)
            })
            .unwrap();
        let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&store)));
        let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&store)));

        // `AppState` holds `Arc<dyn ...>` collaborators and does not derive
        // Debug, so destructure the Result rather than calling `expect_err`.
        let Err(err) = AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            None,
            0,
            "deadbeef",
            FIXTURE_PEM.to_string(),
            16,
            store,
        ) else {
            panic!("missing traffic_quotas table must fail construction");
        };
        let msg = err.to_string();
        assert!(
            msg.contains("traffic_quota_cache_load"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn new_propagates_owner_cap_open_failure() {
        // Drop only the `rate_limit_owner` table. The traffic-quota cache
        // still hydrates, so construction reaches `OwnerCapService::open`,
        // whose `list_all` then fails — exercising the `Err(e)` arm at
        // line 180.
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        store
            .with_write_tx(|tx| {
                tx.execute_batch("DROP TABLE rate_limit_owner")
                    .map_err(map_rusqlite)
            })
            .unwrap();
        let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&store)));
        let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&store)));

        let Err(err) = AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            None,
            0,
            "deadbeef",
            FIXTURE_PEM.to_string(),
            16,
            store,
        ) else {
            panic!("missing rate_limit_owner table must fail construction");
        };
        let msg = err.to_string();
        assert!(
            msg.contains("owner_cap_service_open"),
            "unexpected error message: {msg}"
        );
    }
}
