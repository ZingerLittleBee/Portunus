//! Operator-managed configuration files.
//!
//! Schemas mirror `data-model.md`'s `ServerConfig` and `ClientConfig`. Both
//! are TOML and loaded with `serde::Deserialize`; defaults are applied via
//! `#[serde(default = ...)]`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::ForwardError;

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
pub fn default_range_rule_max_ports() -> u32 {
    1024
}

impl ServerConfig {
    pub fn from_toml_path(path: &Path) -> Result<Self, ForwardError> {
        let raw = std::fs::read_to_string(path)?;
        let cfg: Self =
            toml::from_str(&raw).map_err(|e| ForwardError::ConfigInvalid(e.to_string()))?;
        if cfg.range_rule_max_ports == 0 {
            return Err(ForwardError::ConfigInvalid(
                "range_rule_max_ports must be >= 1 (a cap of 0 rejects every range push, almost certainly a misconfiguration)".into(),
            ));
        }
        Ok(cfg)
    }
}

impl ClientConfig {
    pub fn from_toml_path(path: &Path) -> Result<Self, ForwardError> {
        let raw = std::fs::read_to_string(path)?;
        toml::from_str(&raw).map_err(|e| ForwardError::ConfigInvalid(e.to_string()))
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
        let toml = r#"
            tls_cert_path = "/etc/forward/server.crt"
            tls_key_path = "/etc/forward/server.key"
            token_store_path = "/etc/forward/tokens.json"
        "#;
        let dir = write_tmp("server.toml", toml);
        let cfg = ServerConfig::from_toml_path(&dir.path().join("server.toml")).unwrap();
        assert_eq!(cfg.control_listen.port(), 7443);
        assert_eq!(cfg.operator_http_listen.port(), 7080);
        assert_eq!(cfg.metrics_listen.port(), 7081);
        assert_eq!(cfg.shutdown_drain_timeout_secs, 30);
        assert_eq!(cfg.log_format, LogFormat::Json);
        // T011: default cap is 1024.
        assert_eq!(cfg.range_rule_max_ports, 1024);
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
            ForwardError::ConfigInvalid(msg) => {
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
            ForwardError::ConfigInvalid(_) => {}
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    #[test]
    fn client_config_minimal_loads() {
        let toml = r#"bundle_path = "/etc/forward/edge-01.bundle.json""#;
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
}
