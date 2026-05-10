//! Operator-managed configuration files.
//!
//! Schemas mirror `data-model.md`'s `ServerConfig` and `ClientConfig`. Both
//! are TOML and loaded with `serde::Deserialize`; defaults are applied via
//! `#[serde(default = ...)]`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::PortunusError;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_control_listen")]
    pub control_listen: SocketAddr,
    #[serde(default = "default_operator_http_listen")]
    pub operator_http_listen: SocketAddr,
    #[serde(default = "default_metrics_listen")]
    pub metrics_listen: SocketAddr,
    pub tls_cert_path: PathBuf,
    pub tls_key_path: PathBuf,
    pub token_store_path: PathBuf,
    #[serde(default = "default_drain_secs")]
    pub shutdown_drain_timeout_secs: u64,
    #[serde(default)]
    pub log_format: LogFormat,
    /// Maximum ports any single range rule may span (FR-008,
    /// 002-port-range-forward). Default `1024` matches the Linux
    /// default soft `RLIMIT_NOFILE`. Operators on hosts with raised
    /// `LimitNOFILE` may raise this; on stricter hosts they should
    /// lower it. A value of `0` is rejected at load time.
    #[serde(default = "default_range_rule_max_ports")]
    pub range_rule_max_ports: u32,

    /// Idle window for UDP flows before the per-rule reaper retires
    /// them (004-udp-forward, FR-014). Optional in TOML so v0.3.0
    /// configs continue to load; absent → 60 s default. Validated
    /// range `30..=300`. Surfaced to the client over the Welcome
    /// message; the client falls back to the same default when the
    /// value is 0 (i.e. v0.3.0 server).
    #[serde(default)]
    pub udp_flow_idle_secs: Option<u32>,

    /// Per-rule cap on simultaneous live UDP flows (004-udp-forward,
    /// FR-014). Optional in TOML; absent → 1024 default. Validated
    /// range `1..=65535`. Surfaced to the client over the Welcome
    /// message; the client falls back to the default on 0.
    #[serde(default)]
    pub udp_max_flows_per_rule: Option<u32>,

    /// Path to the operator-side identity store (`identity.json`,
    /// 005-multi-user-rbac, FR-004). The loader defaults this to
    /// `<data_dir>/identity.json` when absent.
    #[serde(default = "default_operator_store_path")]
    pub operator_store_path: PathBuf,

    /// Optional bootstrap shortcut (005-multi-user-rbac, FR-006). When
    /// set on a deployment with no existing superadmin in
    /// `identity.json`, mints a built-in `_superadmin` user backed by
    /// this token on first start. After first start, removing this key
    /// does NOT revoke the token (it has been hashed and persisted to
    /// `identity.json`). Validated to be a 43-char URL-safe-base64
    /// string at load time; rejected otherwise.
    #[serde(default)]
    pub operator_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerConfigToml {
    control_listen: Option<SocketAddr>,
    operator_http_listen: Option<SocketAddr>,
    metrics_listen: Option<SocketAddr>,
    tls_cert_path: Option<PathBuf>,
    tls_key_path: Option<PathBuf>,
    token_store_path: Option<PathBuf>,
    shutdown_drain_timeout_secs: Option<u64>,
    log_format: Option<LogFormat>,
    range_rule_max_ports: Option<u32>,
    udp_flow_idle_secs: Option<u32>,
    udp_max_flows_per_rule: Option<u32>,
    operator_store_path: Option<PathBuf>,
    operator_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub bundle_path: PathBuf,
    #[serde(default = "default_reconnect_initial_delay_ms")]
    pub reconnect_initial_delay_ms: u64,
    #[serde(default = "default_reconnect_max_delay_secs")]
    pub reconnect_max_delay_secs: u64,
    #[serde(default = "default_drain_secs")]
    pub shutdown_drain_timeout_secs: u64,
    #[serde(default)]
    pub log_format: LogFormat,
    #[serde(default = "default_stats_report_interval_secs")]
    pub stats_report_interval_secs: u64,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    #[default]
    Json,
    Compact,
}

fn default_control_listen() -> SocketAddr {
    "0.0.0.0:7443".parse().expect("static addr")
}
fn default_operator_http_listen() -> SocketAddr {
    "127.0.0.1:7080".parse().expect("static addr")
}
fn default_metrics_listen() -> SocketAddr {
    "127.0.0.1:7081".parse().expect("static addr")
}
fn default_drain_secs() -> u64 {
    30
}
fn default_reconnect_initial_delay_ms() -> u64 {
    500
}
fn default_reconnect_max_delay_secs() -> u64 {
    30
}
fn default_stats_report_interval_secs() -> u64 {
    5
}
/// Default cap for `range_rule_max_ports` (1024). Public so the offline
/// CLI paths can stay aligned with the served config without needing to
/// instantiate a `ServerConfig`.
#[must_use]
fn default_operator_store_path() -> PathBuf {
    // `from_toml_path_with_data_dir` installs the real
    // `<data_dir>/identity.json` default. The bare relative fallback is
    // only used by direct deserializations of `ServerConfig`.
    PathBuf::from("identity.json")
}

#[must_use]
pub fn default_range_rule_max_ports() -> u32 {
    1024
}

/// Default idle window for UDP flows (60 s, 004-udp-forward FR-014).
/// Exposed for the client compile-time fallback when the server's
/// Welcome carries 0 (i.e. a v0.3.0 server).
#[must_use]
pub fn default_udp_flow_idle_secs() -> u32 {
    60
}

/// Default per-rule UDP flow cap (1024, 004-udp-forward FR-014).
#[must_use]
pub fn default_udp_max_flows_per_rule() -> u32 {
    1024
}

impl ServerConfig {
    #[must_use]
    pub fn default_for_data_dir(data_dir: &Path) -> Self {
        Self {
            control_listen: default_control_listen(),
            operator_http_listen: default_operator_http_listen(),
            metrics_listen: default_metrics_listen(),
            tls_cert_path: data_dir.join("server.crt"),
            tls_key_path: data_dir.join("server.key"),
            token_store_path: data_dir.join("tokens.json"),
            shutdown_drain_timeout_secs: default_drain_secs(),
            log_format: LogFormat::Json,
            range_rule_max_ports: default_range_rule_max_ports(),
            udp_flow_idle_secs: None,
            udp_max_flows_per_rule: None,
            operator_store_path: data_dir.join("identity.json"),
            operator_token: None,
        }
    }

    pub fn from_toml_path(path: &Path) -> Result<Self, PortunusError> {
        let data_dir = path.parent().unwrap_or_else(|| Path::new("."));
        Self::from_toml_path_with_data_dir(path, data_dir)
    }

    pub fn from_toml_path_with_data_dir(
        path: &Path,
        data_dir: &Path,
    ) -> Result<Self, PortunusError> {
        let raw = std::fs::read_to_string(path)?;
        let overrides: ServerConfigToml =
            toml::from_str(&raw).map_err(|e| PortunusError::ConfigInvalid(e.to_string()))?;
        let mut cfg = Self::default_for_data_dir(data_dir);

        if let Some(v) = overrides.control_listen {
            cfg.control_listen = v;
        }
        if let Some(v) = overrides.operator_http_listen {
            cfg.operator_http_listen = v;
        }
        if let Some(v) = overrides.metrics_listen {
            cfg.metrics_listen = v;
        }
        if let Some(v) = overrides.tls_cert_path {
            cfg.tls_cert_path = v;
        }
        if let Some(v) = overrides.tls_key_path {
            cfg.tls_key_path = v;
        }
        if let Some(v) = overrides.token_store_path {
            cfg.token_store_path = v;
        }
        if let Some(v) = overrides.shutdown_drain_timeout_secs {
            cfg.shutdown_drain_timeout_secs = v;
        }
        if let Some(v) = overrides.log_format {
            cfg.log_format = v;
        }
        if let Some(v) = overrides.range_rule_max_ports {
            cfg.range_rule_max_ports = v;
        }
        if let Some(v) = overrides.udp_flow_idle_secs {
            cfg.udp_flow_idle_secs = Some(v);
        }
        if let Some(v) = overrides.udp_max_flows_per_rule {
            cfg.udp_max_flows_per_rule = Some(v);
        }
        if let Some(v) = overrides.operator_store_path {
            cfg.operator_store_path = v;
        }
        if let Some(v) = overrides.operator_token {
            cfg.operator_token = Some(v);
        }

        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), PortunusError> {
        if self.range_rule_max_ports == 0 {
            return Err(PortunusError::ConfigInvalid(
                "range_rule_max_ports must be >= 1 (a cap of 0 rejects every range push, almost certainly a misconfiguration)".into(),
            ));
        }
        if let Some(v) = self.udp_flow_idle_secs
            && !(30..=300).contains(&v)
        {
            return Err(PortunusError::ConfigInvalid(format!(
                "udp_flow_idle_secs out of range (got {v}, expected 30..=300)"
            )));
        }
        if let Some(v) = self.udp_max_flows_per_rule
            && !(1..=65535).contains(&v)
        {
            return Err(PortunusError::ConfigInvalid(format!(
                "udp_max_flows_per_rule out of range (got {v}, expected 1..=65535)"
            )));
        }
        Ok(())
    }

    /// Resolved UDP idle-flow window in seconds (default-applied).
    #[must_use]
    pub fn udp_flow_idle_secs(&self) -> u32 {
        self.udp_flow_idle_secs
            .unwrap_or_else(default_udp_flow_idle_secs)
    }

    /// Resolved per-rule UDP flow cap (default-applied).
    #[must_use]
    pub fn udp_max_flows_per_rule(&self) -> u32 {
        self.udp_max_flows_per_rule
            .unwrap_or_else(default_udp_max_flows_per_rule)
    }
}

impl ClientConfig {
    pub fn from_toml_path(path: &Path) -> Result<Self, PortunusError> {
        let raw = std::fs::read_to_string(path)?;
        toml::from_str(&raw).map_err(|e| PortunusError::ConfigInvalid(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(name: &str, body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join(name)).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        dir
    }

    #[test]
    fn server_config_minimal_loads() {
        let toml = "";
        let dir = write_tmp("server.toml", toml);
        let cfg = ServerConfig::from_toml_path(&dir.path().join("server.toml")).unwrap();
        assert_eq!(cfg.control_listen.port(), 7443);
        assert_eq!(cfg.operator_http_listen.port(), 7080);
        assert_eq!(cfg.metrics_listen.port(), 7081);
        assert_eq!(cfg.shutdown_drain_timeout_secs, 30);
        assert_eq!(cfg.log_format, LogFormat::Json);
        // T011: default cap is 1024.
        assert_eq!(cfg.range_rule_max_ports, 1024);
        assert_eq!(cfg.tls_cert_path, dir.path().join("server.crt"));
        assert_eq!(cfg.tls_key_path, dir.path().join("server.key"));
        assert_eq!(cfg.token_store_path, dir.path().join("tokens.json"));
        assert_eq!(cfg.operator_store_path, dir.path().join("identity.json"));
    }

    #[test]
    fn server_config_partial_file_overrides_defaults() {
        let toml = r#"
            control_listen = "127.0.0.1:9443"
        "#;
        let dir = write_tmp("server.toml", toml);
        let cfg = ServerConfig::from_toml_path(&dir.path().join("server.toml")).unwrap();
        assert_eq!(cfg.control_listen.port(), 9443);
        assert_eq!(cfg.operator_http_listen.port(), 7080);
        assert_eq!(cfg.metrics_listen.port(), 7081);
        assert_eq!(cfg.tls_cert_path, dir.path().join("server.crt"));
        assert_eq!(cfg.tls_key_path, dir.path().join("server.key"));
    }

    #[test]
    fn server_config_range_cap_override() {
        let toml = r#"
            tls_cert_path = "/a"
            tls_key_path = "/a"
            token_store_path = "/a"
            range_rule_max_ports = 256
        "#;
        let dir = write_tmp("server.toml", toml);
        let cfg = ServerConfig::from_toml_path(&dir.path().join("server.toml")).unwrap();
        assert_eq!(cfg.range_rule_max_ports, 256);
    }

    #[test]
    fn server_config_range_cap_zero_rejected() {
        let toml = r#"
            tls_cert_path = "/a"
            tls_key_path = "/a"
            token_store_path = "/a"
            range_rule_max_ports = 0
        "#;
        let dir = write_tmp("server.toml", toml);
        let err = ServerConfig::from_toml_path(&dir.path().join("server.toml")).unwrap_err();
        match err {
            PortunusError::ConfigInvalid(msg) => {
                assert!(msg.contains("range_rule_max_ports"), "msg: {msg}");
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    #[test]
    fn server_config_overrides_apply() {
        let toml = r#"
            control_listen = "127.0.0.1:9443"
            operator_http_listen = "127.0.0.1:9080"
            metrics_listen = "127.0.0.1:9081"
            tls_cert_path = "a.crt"
            tls_key_path = "a.key"
            token_store_path = "tokens.json"
            shutdown_drain_timeout_secs = 5
            log_format = "compact"
        "#;
        let dir = write_tmp("server.toml", toml);
        let cfg = ServerConfig::from_toml_path(&dir.path().join("server.toml")).unwrap();
        assert_eq!(cfg.control_listen.port(), 9443);
        assert_eq!(cfg.shutdown_drain_timeout_secs, 5);
        assert_eq!(cfg.log_format, LogFormat::Compact);
    }

    #[test]
    fn server_config_rejects_unknown_keys() {
        let toml = r#"
            tls_cert_path = "/a"
            tls_key_path = "/a"
            token_store_path = "/a"
            wat = 1
        "#;
        let dir = write_tmp("server.toml", toml);
        let err = ServerConfig::from_toml_path(&dir.path().join("server.toml")).unwrap_err();
        match err {
            PortunusError::ConfigInvalid(_) => {}
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    #[test]
    fn client_config_minimal_loads() {
        let toml = r#"bundle_path = "/etc/portunus/edge-01.bundle.json""#;
        let dir = write_tmp("client.toml", toml);
        let cfg = ClientConfig::from_toml_path(&dir.path().join("client.toml")).unwrap();
        assert_eq!(cfg.reconnect_initial_delay_ms, 500);
        assert_eq!(cfg.reconnect_max_delay_secs, 30);
        assert_eq!(cfg.stats_report_interval_secs, 5);
    }

    #[test]
    fn client_config_rejects_malformed() {
        let toml = "this is not toml = =";
        let dir = write_tmp("client.toml", toml);
        assert!(ClientConfig::from_toml_path(&dir.path().join("client.toml")).is_err());
    }

    // ----- 004-udp-forward T012 -----

    #[test]
    fn udp_tunables_default_when_absent() {
        let toml = r#"
            tls_cert_path = "/a"
            tls_key_path = "/a"
            token_store_path = "/a"
        "#;
        let dir = write_tmp("server.toml", toml);
        let cfg = ServerConfig::from_toml_path(&dir.path().join("server.toml")).unwrap();
        assert_eq!(cfg.udp_flow_idle_secs, None);
        assert_eq!(cfg.udp_max_flows_per_rule, None);
        assert_eq!(cfg.udp_flow_idle_secs(), 60);
        assert_eq!(cfg.udp_max_flows_per_rule(), 1024);
    }

    #[test]
    fn udp_tunables_edges_accepted() {
        for v in [30_u32, 300] {
            let toml = format!(
                r#"
                tls_cert_path = "/a"
                tls_key_path = "/a"
                token_store_path = "/a"
                udp_flow_idle_secs = {v}
            "#
            );
            let dir = write_tmp("server.toml", &toml);
            let cfg = ServerConfig::from_toml_path(&dir.path().join("server.toml"))
                .expect("edge value must be accepted");
            assert_eq!(cfg.udp_flow_idle_secs(), v);
        }
        for v in [1_u32, 65535] {
            let toml = format!(
                r#"
                tls_cert_path = "/a"
                tls_key_path = "/a"
                token_store_path = "/a"
                udp_max_flows_per_rule = {v}
            "#
            );
            let dir = write_tmp("server.toml", &toml);
            let cfg = ServerConfig::from_toml_path(&dir.path().join("server.toml"))
                .expect("edge value must be accepted");
            assert_eq!(cfg.udp_max_flows_per_rule(), v);
        }
    }

    #[test]
    fn udp_tunables_out_of_range_rejected() {
        for v in [0_u32, 29, 301] {
            let toml = format!(
                r#"
                tls_cert_path = "/a"
                tls_key_path = "/a"
                token_store_path = "/a"
                udp_flow_idle_secs = {v}
            "#
            );
            let dir = write_tmp("server.toml", &toml);
            let err = ServerConfig::from_toml_path(&dir.path().join("server.toml")).unwrap_err();
            match err {
                PortunusError::ConfigInvalid(msg) => {
                    assert!(msg.contains("udp_flow_idle_secs"), "msg: {msg}");
                }
                other => panic!("expected ConfigInvalid for v={v}, got {other:?}"),
            }
        }
        for v in [0_u32, 65536] {
            let toml = format!(
                r#"
                tls_cert_path = "/a"
                tls_key_path = "/a"
                token_store_path = "/a"
                udp_max_flows_per_rule = {v}
            "#
            );
            let dir = write_tmp("server.toml", &toml);
            let err = ServerConfig::from_toml_path(&dir.path().join("server.toml")).unwrap_err();
            match err {
                PortunusError::ConfigInvalid(msg) => {
                    assert!(msg.contains("udp_max_flows_per_rule"), "msg: {msg}");
                }
                other => panic!("expected ConfigInvalid for v={v}, got {other:?}"),
            }
        }
    }
}
