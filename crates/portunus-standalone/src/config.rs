//! TOML configuration schema, parsing, validation, and `RuleId` derivation
//! for `portunus-standalone`.
//!
//! § 4 of the standalone-forwarder spec.
//!
//! Some public struct fields are schema elements retained for future use or
//! TOML round-trip fidelity; suppress dead_code for the whole module.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;

use portunus_core::{PortRange, Protocol, RuleId, RuleTarget, Target};
use portunus_forwarder::{ClientRule, MultiTarget};

// ---------------------------------------------------------------------------
// Public error type
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("toml parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config must contain at least one [[rule]]")]
    NoRules,

    #[error("duplicate rule name: {0:?}")]
    DuplicateName(String),

    #[error("each rule must have exactly one of `listen_port` or `listen_ports`")]
    ListenExclusivity,

    #[error("each rule must have exactly one of `target` or `targets`")]
    TargetExclusivity,

    #[error("range size mismatch: listen has {listen_len} ports, target has {target_len}")]
    RangeSizeMismatch { listen_len: u32, target_len: u32 },

    #[error("port range error: {0}")]
    PortRange(#[from] portunus_core::PortRangeError),

    #[error("target parse error in rule {rule:?}: {msg}")]
    TargetParse { rule: String, msg: String },

    #[error("port range string invalid in rule {rule:?}: {msg}")]
    PortRangeParse { rule: String, msg: String },

    #[error("rule {0:?}: `targets` list must contain at least one entry")]
    EmptyTargets(String),

    #[error(
        "rule {0:?}: multi-target `targets` cannot be combined with a listen-port range; \
         use a single `listen_port`"
    )]
    RangeMultiTargetUnsupported(String),

    #[error("rule name collision: {a:?} and {b:?} derive the same RuleId; rename one")]
    RuleIdCollision { a: String, b: String },

    #[error("validation error: {msg}")]
    Validation { msg: String },
}

// ---------------------------------------------------------------------------
// Raw TOML schema (deny_unknown_fields)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatsConfig {
    #[serde(default = "default_stats_enabled")]
    pub enabled: bool,
    #[serde(default = "default_stats_socket_path")]
    pub socket_path: PathBuf,
    #[serde(default = "default_stats_refresh_ms")]
    pub refresh_ms: u64,
}

impl Default for StatsConfig {
    fn default() -> Self {
        Self {
            enabled: default_stats_enabled(),
            socket_path: default_stats_socket_path(),
            refresh_ms: default_stats_refresh_ms(),
        }
    }
}

fn default_stats_enabled() -> bool {
    true
}

fn default_stats_socket_path() -> PathBuf {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from("/run/portunus/standalone.sock")
    }
    #[cfg(target_os = "macos")]
    {
        let base = std::env::var_os("TMPDIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
        base.join("portunus-standalone.sock")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        PathBuf::from("portunus-standalone.sock")
    }
}

fn default_stats_refresh_ms() -> u64 {
    1000
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub global: GlobalConfig,

    #[serde(default)]
    pub defaults: DefaultsConfig,

    #[serde(default)]
    pub stats: StatsConfig,

    #[serde(default, rename = "rule")]
    pub rules: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalConfig {
    /// Optional human-readable label for this config file.
    #[serde(default)]
    pub label: Option<String>,
    /// EnvFilter directive ("info", "debug", "portunus=debug,info", …).
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Either "json" or "pretty".
    #[serde(default = "default_log_format")]
    pub log_format: String,
    /// Drain budget passed to each forwarder task on shutdown.
    #[serde(default = "default_shutdown_drain_secs")]
    pub shutdown_drain_secs: u64,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            label: None,
            log_level: default_log_level(),
            log_format: default_log_format(),
            shutdown_drain_secs: default_shutdown_drain_secs(),
        }
    }
}

fn default_log_level() -> String {
    "warn".into()
}
fn default_log_format() -> String {
    "json".into()
}
fn default_shutdown_drain_secs() -> u64 {
    30
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultsConfig {
    /// Default protocol if a rule omits `protocol`.
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default = "default_udp_max_flows")]
    pub udp_max_flows: u32,
    #[serde(default = "default_udp_flow_idle_secs")]
    pub udp_flow_idle_secs: u32,
    #[serde(default)]
    pub prefer_ipv6: bool,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            protocol: None,
            udp_max_flows: default_udp_max_flows(),
            udp_flow_idle_secs: default_udp_flow_idle_secs(),
            prefer_ipv6: false,
        }
    }
}

fn default_udp_max_flows() -> u32 {
    1024
}
fn default_udp_flow_idle_secs() -> u32 {
    60
}

/// Raw TOML representation of a single forwarding rule.
/// Both `listen_port` and `listen_ports` are optional here; XOR is
/// enforced by `Config::validate`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRule {
    pub name: String,

    /// `"tcp"` or `"udp"`. Falls back to `defaults.protocol` if absent.
    #[serde(default)]
    pub protocol: Option<String>,

    /// Single-port listen (mutually exclusive with `listen_ports`).
    #[serde(default)]
    pub listen_port: Option<u16>,

    /// Port range listen, e.g. `"8000-8009"` (mutually exclusive with `listen_port`).
    #[serde(default)]
    pub listen_ports: Option<String>,

    /// Single-target shorthand: `"host:port"` or `"host:lo-hi"`.
    /// Mutually exclusive with `targets`.
    #[serde(default)]
    pub target: Option<String>,

    /// Multi-target list (mutually exclusive with `target`).
    #[serde(default)]
    pub targets: Option<Vec<RawTarget>>,

    /// Per-rule override of `defaults.prefer_ipv6`.
    #[serde(default)]
    pub prefer_ipv6: Option<bool>,

    /// Per-rule override of `defaults.udp_max_flows` (UDP only).
    #[serde(default)]
    pub udp_max_flows: Option<u32>,

    /// Per-rule override of `defaults.udp_flow_idle_secs` (UDP only).
    #[serde(default)]
    pub udp_flow_idle_secs: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawTarget {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub priority: u32,
    /// `"v1"` or `"v2"`, optional.
    #[serde(default)]
    pub proxy_protocol: Option<String>,
}

// ---------------------------------------------------------------------------
// Parsed / validated representation
// ---------------------------------------------------------------------------

/// A fully validated rule ready for conversion to `ClientRule`.
#[derive(Debug)]
pub struct ParsedRule {
    pub rule_id: RuleId,
    pub name: String,
    pub protocol: Protocol,
    pub listen_range: PortRange,
    pub target_host: String,
    pub target: Target,
    pub target_range: PortRange,
    /// Non-empty only when the rule carries PROXY protocol on the single target,
    /// or the operator used the `targets` list form.
    pub targets: Vec<MultiTarget>,
    pub prefer_ipv6: bool,
    pub udp_max_flows: u32,
    pub udp_flow_idle_secs: u32,
}

impl ParsedRule {
    /// Convert into a `ClientRule` suitable for handing to `portunus_forwarder`.
    ///
    /// All rate-limit, quota, SNI, and observability fields are `None`/`0` —
    /// standalone mode does not use those features.
    #[must_use]
    pub fn into_client_rule(self) -> ClientRule {
        ClientRule {
            rule_id: self.rule_id,
            listen_range: self.listen_range,
            target_host: self.target_host,
            target: self.target,
            target_range: self.target_range,
            prefer_ipv6: self.prefer_ipv6,
            protocol: self.protocol,
            udp_max_flows: self.udp_max_flows,
            udp_flow_idle_secs: self.udp_flow_idle_secs,
            targets: self.targets,
            health_check_interval_secs: None,
            multi_target_obs: None,
            sni_pattern: None,
            rate_limit: None,
            rate_limit_stats: None,
            owner_rate_limit: None,
            owner_rate_limit_stats: None,
            quota: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Config impl
// ---------------------------------------------------------------------------

impl Config {
    /// Parse a TOML string into a `Config`.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::TomlParse` if the input is not valid TOML or
    /// contains unknown fields.
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        let cfg: Self = toml::from_str(s)?;
        Ok(cfg)
    }

    /// Load config from a file path.
    ///
    /// # Errors
    ///
    /// Returns `ConfigError::Io` on I/O failure, `ConfigError::TomlParse`
    /// on parse failure.
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path)?;
        Self::from_toml_str(&content)
    }

    /// Load config from the default path `./portunus.toml`.
    ///
    /// # Errors
    ///
    /// Same as `load_from`.
    pub fn load_default() -> Result<Self, ConfigError> {
        Self::load_from(Path::new("portunus.toml"))
    }

    /// Validate the parsed config.
    ///
    /// Checks performed:
    /// - At least one rule exists.
    /// - No duplicate rule names.
    /// - Each rule has exactly one of `listen_port` / `listen_ports`.
    /// - Each rule has exactly one of `target` / `targets`.
    /// - Range sizes match between listen and target when both are ranges.
    ///
    /// # Errors
    ///
    /// Returns the first validation error encountered.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.rules.is_empty() {
            return Err(ConfigError::NoRules);
        }

        // Duplicate name check.
        let mut seen = std::collections::HashSet::new();
        for rule in &self.rules {
            if !seen.insert(rule.name.as_str()) {
                return Err(ConfigError::DuplicateName(rule.name.clone()));
            }
        }

        for rule in &self.rules {
            // listen_port XOR listen_ports.
            match (rule.listen_port, rule.listen_ports.as_deref()) {
                (None, None) | (Some(_), Some(_)) => {
                    return Err(ConfigError::ListenExclusivity);
                }
                _ => {}
            }

            // target XOR targets.
            match (rule.target.as_deref(), rule.targets.as_deref()) {
                (None, None) | (Some(_), Some(_)) => {
                    return Err(ConfigError::TargetExclusivity);
                }
                _ => {}
            }

            // Listen-port count for this rule (1 for a single port, N for a range).
            let listen_len = if rule.listen_port.is_some() {
                1
            } else {
                let s = rule
                    .listen_ports
                    .as_deref()
                    .expect("listen XOR checked above");
                parse_port_range(s)
                    .map_err(|msg| ConfigError::PortRangeParse {
                        rule: rule.name.clone(),
                        msg,
                    })?
                    .len()
            };

            // Validate listen/target arity for EVERY shape, not just
            // `listen_ports` + string `target`. Two shapes previously slipped
            // through validation and produced silent mis-routing or dropped
            // packets downstream (and `into_iter_rules` did not catch them
            // either).
            if let Some(target_str) = rule.target.as_deref() {
                // String `target`: count its `host:port` / `host:lo-hi` ports.
                let target_len = parse_target_range_part(target_str)
                    .map_err(|msg| ConfigError::TargetParse {
                        rule: rule.name.clone(),
                        msg,
                    })?
                    .len();
                if listen_len != target_len {
                    return Err(ConfigError::RangeSizeMismatch {
                        listen_len,
                        target_len,
                    });
                }
            } else if let Some(targets) = rule.targets.as_deref() {
                // Multi-target `targets` list: must be non-empty, and each
                // entry is a single port. A multi-target rule is failover
                // across targets for one listen port; combining it with a
                // listen-port range has no per-port mapping, so reject it.
                if targets.is_empty() {
                    return Err(ConfigError::EmptyTargets(rule.name.clone()));
                }
                if listen_len != 1 {
                    return Err(ConfigError::RangeMultiTargetUnsupported(rule.name.clone()));
                }
            }
        }

        // Reject rule-name collisions on the derived RuleId. Names are unique
        // (checked above) but the 64-bit BLAKE3 prefix could in principle
        // collapse two distinct names into one id, which would silently fold
        // their stats/registry entries (last-writer-wins).
        let mut seen_ids = std::collections::HashMap::new();
        for rule in &self.rules {
            let id = derive_rule_id(&rule.name);
            if let Some(prev) = seen_ids.insert(id, rule.name.clone()) {
                return Err(ConfigError::RuleIdCollision {
                    a: prev,
                    b: rule.name.clone(),
                });
            }
        }

        if self.stats.enabled && !(250..=5000).contains(&self.stats.refresh_ms) {
            return Err(ConfigError::Validation {
                msg: format!(
                    "[stats] refresh_ms must be in 250..=5000 (got {})",
                    self.stats.refresh_ms
                ),
            });
        }

        Ok(())
    }

    /// Consume the config and produce an iterator of `ParsedRule`.
    ///
    /// Callers should call `validate` before this method; `.expect` calls
    /// inside document structural invariants that hold after validation.
    ///
    /// # Errors
    ///
    /// Returns a `ConfigError` if a rule cannot be parsed/converted
    /// (e.g. invalid host string, bad port range).
    pub fn into_iter_rules(self) -> Result<impl Iterator<Item = ParsedRule>, ConfigError> {
        let defaults_protocol = self.defaults.protocol;
        let defaults_prefer_ipv6 = self.defaults.prefer_ipv6;
        let defaults_udp_max_flows = self.defaults.udp_max_flows;
        let defaults_udp_flow_idle_secs = self.defaults.udp_flow_idle_secs;
        let mut parsed = Vec::with_capacity(self.rules.len());

        for raw in self.rules {
            let protocol = parse_protocol(
                raw.protocol
                    .as_deref()
                    .or(defaults_protocol.as_deref())
                    .unwrap_or("tcp"),
                &raw.name,
            )?;

            let listen_range = if let Some(port) = raw.listen_port {
                PortRange::single(port)
            } else {
                // listen_ports is Some — validated upstream.
                let s = raw.listen_ports.as_deref().expect("validated upstream");
                parse_port_range(s).map_err(|msg| ConfigError::PortRangeParse {
                    rule: raw.name.clone(),
                    msg,
                })?
            };

            let rule_id = derive_rule_id(&raw.name);

            if let Some(target_str) = raw.target {
                // Single-target string form.
                let (host, target_range) = parse_target_string(&target_str, &raw.name)?;
                let target = classify_target(&host, &raw.name)?;

                parsed.push(ParsedRule {
                    rule_id,
                    name: raw.name,
                    protocol,
                    listen_range,
                    target_host: host,
                    target,
                    target_range,
                    targets: Vec::new(),
                    prefer_ipv6: raw.prefer_ipv6.unwrap_or(defaults_prefer_ipv6),
                    udp_max_flows: raw.udp_max_flows.unwrap_or(defaults_udp_max_flows),
                    udp_flow_idle_secs: raw
                        .udp_flow_idle_secs
                        .unwrap_or(defaults_udp_flow_idle_secs),
                });
            } else {
                // Multi-target list form.
                let raw_targets = raw.targets.expect("validated upstream");
                if raw_targets.is_empty() {
                    return Err(ConfigError::EmptyTargets(raw.name.clone()));
                }

                // Use the first target for back-compat fields.
                let first = &raw_targets[0];
                let first_host = first.host.clone();
                let first_target = classify_target(&first_host, &raw.name)?;
                let first_range = PortRange::single(first.port);

                let mut multi_targets = Vec::with_capacity(raw_targets.len());
                for rt in &raw_targets {
                    let proxy_protocol =
                        parse_proxy_protocol(rt.proxy_protocol.as_deref(), &raw.name)?;
                    let spec = RuleTarget {
                        host: rt.host.clone(),
                        port: rt.port,
                        priority: rt.priority,
                        proxy_protocol,
                    };
                    let target = classify_target(&rt.host, &raw.name)?;
                    multi_targets.push(MultiTarget { spec, target });
                }

                parsed.push(ParsedRule {
                    rule_id,
                    name: raw.name,
                    protocol,
                    listen_range,
                    target_host: first_host,
                    target: first_target,
                    target_range: first_range,
                    targets: multi_targets,
                    prefer_ipv6: raw.prefer_ipv6.unwrap_or(defaults_prefer_ipv6),
                    udp_max_flows: raw.udp_max_flows.unwrap_or(defaults_udp_max_flows),
                    udp_flow_idle_secs: raw
                        .udp_flow_idle_secs
                        .unwrap_or(defaults_udp_flow_idle_secs),
                });
            }
        }

        Ok(parsed.into_iter())
    }
}

// ---------------------------------------------------------------------------
// RuleId derivation
// ---------------------------------------------------------------------------

/// Derive a deterministic `RuleId` from a rule name using the first 8 bytes
/// of its BLAKE3 hash (little-endian u64).
#[must_use]
pub fn derive_rule_id(name: &str) -> RuleId {
    let hash = blake3::hash(name.as_bytes());
    let bytes = hash.as_bytes();
    let n = u64::from_le_bytes(bytes[..8].try_into().expect("blake3 output is 32 bytes"));
    RuleId(n)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn parse_protocol(s: &str, rule_name: &str) -> Result<Protocol, ConfigError> {
    s.parse::<Protocol>().map_err(|e| ConfigError::TargetParse {
        rule: rule_name.to_string(),
        msg: e.to_string(),
    })
}

/// Parse `"lo-hi"` or `"port"` into a `PortRange`.
fn parse_port_range(s: &str) -> Result<PortRange, String> {
    if let Some((lo_s, hi_s)) = s.split_once('-') {
        let lo: u16 = lo_s
            .trim()
            .parse()
            .map_err(|_| format!("invalid port range start {lo_s:?}"))?;
        let hi: u16 = hi_s
            .trim()
            .parse()
            .map_err(|_| format!("invalid port range end {hi_s:?}"))?;
        PortRange::new(lo, hi).map_err(|e| e.to_string())
    } else {
        let port: u16 = s
            .trim()
            .parse()
            .map_err(|_| format!("invalid port {s:?}"))?;
        Ok(PortRange::single(port))
    }
}

/// Extract just the target-side `PortRange` from a `"host:port"` or
/// `"host:lo-hi"` string. Used during validation only.
fn parse_target_range_part(target_str: &str) -> Result<PortRange, String> {
    let port_part = extract_port_part(target_str)?;
    parse_port_range(port_part)
}

/// Split `"host:port"` or `"host:lo-hi"` into `(host, PortRange)`.
/// Handles bracketed IPv6 like `"[::1]:8080"`.
fn parse_target_string(
    target_str: &str,
    rule_name: &str,
) -> Result<(String, PortRange), ConfigError> {
    let port_part = extract_port_part(target_str).map_err(|msg| ConfigError::TargetParse {
        rule: rule_name.to_string(),
        msg,
    })?;

    // host is everything before the last colon (that's the port separator)
    let colon_pos = last_colon_pos(target_str).ok_or_else(|| ConfigError::TargetParse {
        rule: rule_name.to_string(),
        msg: format!("target {target_str:?} must be host:port or host:lo-hi"),
    })?;
    let host = target_str[..colon_pos].to_string();

    let range = parse_port_range(port_part).map_err(|msg| ConfigError::TargetParse {
        rule: rule_name.to_string(),
        msg,
    })?;

    Ok((host, range))
}

/// Return the port/range portion of a `host:port` or `host:lo-hi` string.
fn extract_port_part(s: &str) -> Result<&str, String> {
    let pos =
        last_colon_pos(s).ok_or_else(|| format!("target {s:?} must be host:port or host:lo-hi"))?;
    Ok(&s[pos + 1..])
}

/// Find the position of the colon that separates host from port.
/// For bracketed IPv6 (`[::1]:8080`) this is the colon after `]`.
/// For everything else it is the last colon in the string.
fn last_colon_pos(s: &str) -> Option<usize> {
    if s.starts_with('[') {
        // Bracketed IPv6: find ']' then expect ':' immediately after.
        let close = s.find(']')?;
        let colon = close + 1;
        if s.as_bytes().get(colon) == Some(&b':') {
            Some(colon)
        } else {
            None
        }
    } else {
        s.rfind(':')
    }
}

/// Parse a host string into a `Target` using `portunus_core::Target::parse`.
fn classify_target(host: &str, rule_name: &str) -> Result<Target, ConfigError> {
    Target::parse(host).map_err(|e| ConfigError::TargetParse {
        rule: rule_name.to_string(),
        msg: e.to_string(),
    })
}

/// Parse an optional `"v1"` / `"v2"` string into `ProxyProtocolVersion`.
fn parse_proxy_protocol(
    s: Option<&str>,
    rule_name: &str,
) -> Result<Option<portunus_core::ProxyProtocolVersion>, ConfigError> {
    match s {
        None => Ok(None),
        Some("v1") => Ok(Some(portunus_core::ProxyProtocolVersion::V1)),
        Some("v2") => Ok(Some(portunus_core::ProxyProtocolVersion::V2)),
        Some(other) => Err(ConfigError::TargetParse {
            rule: rule_name.to_string(),
            msg: format!("unknown proxy_protocol {other:?}; expected v1 or v2"),
        }),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_str: &str) -> Result<Config, ConfigError> {
        Config::from_toml_str(toml_str)
    }

    #[test]
    fn minimal_tcp_rule_parses() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "ssh"
            protocol = "tcp"
            listen_port = 2222
            target = "10.0.0.5:22"
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].name, "ssh");
    }

    #[test]
    fn duplicate_name_rejected() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 1
            target = "1.1.1.1:1"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 2
            target = "1.1.1.1:2"
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateName(ref n) if n == "a"));
    }

    #[test]
    fn empty_config_rejected() {
        let cfg = parse("").unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::NoRules));
    }

    #[test]
    fn unknown_field_rejected() {
        let err = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 1
            target = "1.1.1.1:1"
            bind_addr = "0.0.0.0"
        "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::TomlParse(_)));
    }

    #[test]
    fn target_and_targets_mutually_exclusive() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 1
            target = "1.1.1.1:1"
            targets = [{ host = "x", port = 1, priority = 0 }]
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::TargetExclusivity));
    }

    #[test]
    fn range_size_mismatch_rejected() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_ports = "8000-8009"
            target = "1.1.1.1:8000-8019"
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::RangeSizeMismatch { .. }));
    }

    #[test]
    fn single_listen_with_target_range_rejected() {
        // Shape previously unvalidated: single listen_port + target range.
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 8000
            target = "1.1.1.1:8000-8009"
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(
            matches!(
                err,
                ConfigError::RangeSizeMismatch {
                    listen_len: 1,
                    target_len: 10
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn listen_range_with_multi_targets_rejected() {
        // Shape previously unvalidated: listen_ports range + multi-target list.
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_ports = "8000-8009"
            targets = [{ host = "1.1.1.1", port = 9000, priority = 0 }]
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(
            matches!(err, ConfigError::RangeMultiTargetUnsupported(ref n) if n == "a"),
            "got {err:?}"
        );
    }

    #[test]
    fn single_listen_with_multi_targets_accepted() {
        // Valid HA shape: single listen_port + multi-target failover.
        let cfg = parse(
            r#"
            [[rule]]
            name = "ha"
            protocol = "tcp"
            listen_port = 443
            targets = [
                { host = "1.1.1.1", port = 443, priority = 0 },
                { host = "2.2.2.2", port = 443, priority = 1 },
            ]
        "#,
        )
        .unwrap();
        cfg.validate().expect("single-port multi-target is valid");
    }

    #[test]
    fn empty_targets_rejected_by_validate() {
        // Previously passed validate() but failed at startup (into_iter_rules),
        // so `--check` wrongly reported ok.
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 1
            targets = []
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(
            matches!(err, ConfigError::EmptyTargets(ref n) if n == "a"),
            "got {err:?}"
        );
    }

    #[test]
    fn rule_id_derived_via_blake3_prefix() {
        let id_a = derive_rule_id("ssh-tunnel");
        let id_b = derive_rule_id("ssh-tunnel");
        let id_c = derive_rule_id("game-udp");
        assert_eq!(id_a, id_b, "deterministic for same name");
        assert_ne!(id_a, id_c, "different names → different ids");
    }

    #[test]
    fn stats_default_enabled_with_platform_path() {
        let toml = r#"
[[rule]]
name = "x"
protocol = "tcp"
listen_port = 1
target = "1.1.1.1:1"
"#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert!(cfg.stats.enabled);
        assert_eq!(cfg.stats.refresh_ms, 1000);
        #[cfg(target_os = "linux")]
        assert_eq!(
            cfg.stats.socket_path.as_os_str(),
            std::ffi::OsStr::new("/run/portunus/standalone.sock"),
        );
        #[cfg(target_os = "macos")]
        {
            let p = cfg.stats.socket_path.display().to_string();
            assert!(p.ends_with("portunus-standalone.sock"));
        }
    }

    #[test]
    fn stats_refresh_ms_validation() {
        let toml = r#"
[stats]
refresh_ms = 100
[[rule]]
name = "x"
protocol = "tcp"
listen_port = 1
target = "1.1.1.1:1"
"#;
        let cfg: Result<Config, _> = toml::from_str(toml);
        if let Ok(c) = cfg {
            assert!(
                c.validate().is_err(),
                "refresh_ms=100 must be rejected by validate()"
            );
        }
    }

    // -----------------------------------------------------------------------
    // load_from / load_default (file I/O)
    // -----------------------------------------------------------------------

    #[test]
    fn load_from_reads_and_parses_file() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("portunus.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(
            br#"
[[rule]]
name = "ssh"
protocol = "tcp"
listen_port = 2222
target = "10.0.0.5:22"
"#,
        )
        .unwrap();
        drop(f);

        let cfg = Config::load_from(&path).unwrap();
        cfg.validate().unwrap();
        assert_eq!(cfg.rules.len(), 1);
        assert_eq!(cfg.rules[0].name, "ssh");
    }

    #[test]
    fn load_from_missing_file_is_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let err = Config::load_from(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Io(_)), "got {err:?}");
    }

    #[test]
    fn load_default_uses_cwd_path() {
        // `portunus.toml` almost certainly does not exist in the test's
        // working directory, so this exercises the default-path plumbing
        // and surfaces an I/O error rather than a parse error.
        // Missing file → I/O error; a stray `portunus.toml` → parse error.
        // Both are acceptable here; any OTHER variant would be a logic bug.
        if let Err(e) = Config::load_default() {
            assert!(
                matches!(e, ConfigError::Io(_) | ConfigError::TomlParse(_)),
                "unexpected error kind: {e:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // validate() remaining branches
    // -----------------------------------------------------------------------

    #[test]
    fn listen_exclusivity_neither_rejected() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            target = "1.1.1.1:1"
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::ListenExclusivity), "got {err:?}");
    }

    #[test]
    fn listen_exclusivity_both_rejected() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 1
            listen_ports = "1-2"
            target = "1.1.1.1:1"
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::ListenExclusivity), "got {err:?}");
    }

    #[test]
    fn target_exclusivity_neither_rejected() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 1
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(matches!(err, ConfigError::TargetExclusivity), "got {err:?}");
    }

    #[test]
    fn invalid_listen_ports_range_rejected_by_validate() {
        // Non-numeric range bound -> PortRangeParse error path.
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_ports = "abc-2"
            target = "1.1.1.1:1-2"
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(
            matches!(err, ConfigError::PortRangeParse { ref rule, .. } if rule == "a"),
            "got {err:?}"
        );
    }

    #[test]
    fn invalid_target_string_rejected_by_validate() {
        // `target` has no colon -> TargetParse error path during validate.
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 1
            target = "1.1.1.1"
        "#,
        )
        .unwrap();
        let err = cfg.validate().unwrap_err();
        assert!(
            matches!(err, ConfigError::TargetParse { ref rule, .. } if rule == "a"),
            "got {err:?}"
        );
    }

    #[test]
    fn stats_disabled_skips_refresh_validation() {
        // With stats disabled, an out-of-range refresh_ms is not validated.
        let cfg = parse(
            r#"
            [stats]
            enabled = false
            refresh_ms = 100
            [[rule]]
            name = "a"
            protocol = "tcp"
            listen_port = 1
            target = "1.1.1.1:1"
        "#,
        )
        .unwrap();
        cfg.validate().expect("disabled stats bypass refresh check");
    }

    // -----------------------------------------------------------------------
    // into_iter_rules: single-target shape
    // -----------------------------------------------------------------------

    #[test]
    fn into_iter_rules_single_tcp_target() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "ssh"
            protocol = "tcp"
            listen_port = 2222
            target = "10.0.0.5:22"
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let rules: Vec<ParsedRule> = cfg.into_iter_rules().unwrap().collect();
        assert_eq!(rules.len(), 1);
        let r = &rules[0];
        assert_eq!(r.name, "ssh");
        assert_eq!(r.protocol, Protocol::Tcp);
        assert_eq!(r.listen_range, PortRange::single(2222));
        assert_eq!(r.target_host, "10.0.0.5");
        assert_eq!(r.target_range, PortRange::single(22));
        assert_eq!(r.rule_id, derive_rule_id("ssh"));
        assert!(r.targets.is_empty(), "single-target carries no multi list");
        assert!(!r.prefer_ipv6);
        assert_eq!(r.udp_max_flows, default_udp_max_flows());
        assert_eq!(r.udp_flow_idle_secs, default_udp_flow_idle_secs());
    }

    #[test]
    fn into_iter_rules_defaults_protocol_applied() {
        // No per-rule protocol; pulls from `defaults.protocol`.
        let cfg = parse(
            r#"
            [defaults]
            protocol = "udp"
            [[rule]]
            name = "dns"
            listen_port = 53
            target = "8.8.8.8:53"
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let rules: Vec<ParsedRule> = cfg.into_iter_rules().unwrap().collect();
        assert_eq!(rules[0].protocol, Protocol::Udp);
    }

    #[test]
    fn into_iter_rules_protocol_falls_back_to_tcp() {
        // No per-rule and no defaults protocol -> "tcp".
        let cfg = parse(
            r#"
            [[rule]]
            name = "web"
            listen_port = 80
            target = "1.1.1.1:80"
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let rules: Vec<ParsedRule> = cfg.into_iter_rules().unwrap().collect();
        assert_eq!(rules[0].protocol, Protocol::Tcp);
    }

    #[test]
    fn into_iter_rules_listen_ports_range() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "range"
            protocol = "tcp"
            listen_ports = "8000-8009"
            target = "1.1.1.1:9000-9009"
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let rules: Vec<ParsedRule> = cfg.into_iter_rules().unwrap().collect();
        assert_eq!(rules[0].listen_range, PortRange::new(8000, 8009).unwrap());
        assert_eq!(rules[0].target_range, PortRange::new(9000, 9009).unwrap());
    }

    #[test]
    fn into_iter_rules_per_rule_overrides_applied() {
        // Per-rule overrides take precedence over `[defaults]`.
        let cfg = parse(
            r#"
            [defaults]
            udp_max_flows = 10
            udp_flow_idle_secs = 5
            prefer_ipv6 = false
            [[rule]]
            name = "u"
            protocol = "udp"
            listen_port = 5000
            target = "1.1.1.1:5000"
            udp_max_flows = 99
            udp_flow_idle_secs = 7
            prefer_ipv6 = true
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let rules: Vec<ParsedRule> = cfg.into_iter_rules().unwrap().collect();
        assert_eq!(rules[0].udp_max_flows, 99);
        assert_eq!(rules[0].udp_flow_idle_secs, 7);
        assert!(rules[0].prefer_ipv6);
    }

    #[test]
    fn into_iter_rules_inherits_defaults_when_unset() {
        // No per-rule overrides -> values inherited from `[defaults]`.
        let cfg = parse(
            r#"
            [defaults]
            udp_max_flows = 42
            udp_flow_idle_secs = 11
            prefer_ipv6 = true
            [[rule]]
            name = "u"
            protocol = "udp"
            listen_port = 5000
            target = "1.1.1.1:5000"
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let rules: Vec<ParsedRule> = cfg.into_iter_rules().unwrap().collect();
        assert_eq!(rules[0].udp_max_flows, 42);
        assert_eq!(rules[0].udp_flow_idle_secs, 11);
        assert!(rules[0].prefer_ipv6);
    }

    #[test]
    fn into_iter_rules_dns_target_classified() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "web"
            protocol = "tcp"
            listen_port = 80
            target = "example.com:80"
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let rules: Vec<ParsedRule> = cfg.into_iter_rules().unwrap().collect();
        assert_eq!(rules[0].target_host, "example.com");
        assert!(matches!(rules[0].target, Target::Dns(_)));
    }

    #[test]
    fn into_iter_rules_bracketed_ipv6_target() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "v6"
            protocol = "tcp"
            listen_port = 8080
            target = "[::1]:8080"
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let rules: Vec<ParsedRule> = cfg.into_iter_rules().unwrap().collect();
        assert_eq!(rules[0].target_host, "[::1]");
        assert_eq!(rules[0].target_range, PortRange::single(8080));
        assert!(matches!(rules[0].target, Target::Ip(_)));
    }

    #[test]
    fn into_iter_rules_bad_host_is_target_parse_error() {
        // Bare unbracketed IPv6 host -> classify_target error.
        let cfg = parse(
            r#"
            [[rule]]
            name = "bad"
            protocol = "tcp"
            listen_port = 1
            target = "::1:8080"
        "#,
        )
        .unwrap();
        // validate() splits on the LAST colon, leaving a parseable host/port
        // shape, so it passes; the host then fails to classify in
        // into_iter_rules.
        let res = cfg.into_iter_rules();
        let err = res.err().expect("bad host must fail to classify");
        assert!(
            matches!(err, ConfigError::TargetParse { ref rule, .. } if rule == "bad"),
            "got {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // into_iter_rules: multi-target shape
    // -----------------------------------------------------------------------

    #[test]
    fn into_iter_rules_multi_target_build() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "ha"
            protocol = "tcp"
            listen_port = 443
            targets = [
                { host = "1.1.1.1", port = 443, priority = 0, proxy_protocol = "v1" },
                { host = "2.2.2.2", port = 8443, priority = 1, proxy_protocol = "v2" },
                { host = "example.com", port = 443, priority = 2 },
            ]
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let rules: Vec<ParsedRule> = cfg.into_iter_rules().unwrap().collect();
        let r = &rules[0];
        // Back-compat fields are taken from the first target.
        assert_eq!(r.target_host, "1.1.1.1");
        assert_eq!(r.target_range, PortRange::single(443));
        assert!(matches!(r.target, Target::Ip(_)));
        // Multi-target list preserves order, ports, priorities, and proxy ver.
        assert_eq!(r.targets.len(), 3);
        assert_eq!(r.targets[0].spec.host, "1.1.1.1");
        assert_eq!(r.targets[0].spec.port, 443);
        assert_eq!(r.targets[0].spec.priority, 0);
        assert_eq!(
            r.targets[0].spec.proxy_protocol,
            Some(portunus_core::ProxyProtocolVersion::V1)
        );
        assert_eq!(
            r.targets[1].spec.proxy_protocol,
            Some(portunus_core::ProxyProtocolVersion::V2)
        );
        assert_eq!(r.targets[1].spec.port, 8443);
        assert_eq!(r.targets[2].spec.proxy_protocol, None);
        assert!(matches!(r.targets[2].target, Target::Dns(_)));
    }

    #[test]
    fn into_iter_rules_multi_target_bad_proxy_protocol() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "ha"
            protocol = "tcp"
            listen_port = 443
            targets = [
                { host = "1.1.1.1", port = 443, priority = 0, proxy_protocol = "v9" },
            ]
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let err = cfg.into_iter_rules().err().expect("v9 is invalid");
        assert!(
            matches!(err, ConfigError::TargetParse { ref rule, .. } if rule == "ha"),
            "got {err:?}"
        );
    }

    #[test]
    fn into_iter_rules_multi_target_bad_host() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "ha"
            protocol = "tcp"
            listen_port = 443
            targets = [
                { host = "not a host!", port = 443, priority = 0 },
            ]
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let err = cfg.into_iter_rules().err().expect("invalid host");
        assert!(
            matches!(err, ConfigError::TargetParse { ref rule, .. } if rule == "ha"),
            "got {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // into_client_rule
    // -----------------------------------------------------------------------

    #[test]
    fn parsed_rule_into_client_rule_maps_fields() {
        let cfg = parse(
            r#"
            [[rule]]
            name = "ssh"
            protocol = "tcp"
            listen_port = 2222
            target = "10.0.0.5:22"
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let parsed: Vec<ParsedRule> = cfg.into_iter_rules().unwrap().collect();
        let id = parsed[0].rule_id;
        let client_rule = parsed.into_iter().next().unwrap().into_client_rule();
        assert_eq!(client_rule.rule_id, id);
        assert_eq!(client_rule.listen_range, PortRange::single(2222));
        assert_eq!(client_rule.target_host, "10.0.0.5");
        assert_eq!(client_rule.protocol, Protocol::Tcp);
        // Standalone never sets the advanced server-only fields.
        assert!(client_rule.sni_pattern.is_none());
        assert!(client_rule.rate_limit.is_none());
        assert!(client_rule.quota.is_none());
        assert!(client_rule.health_check_interval_secs.is_none());
    }

    // -----------------------------------------------------------------------
    // RuleId collision and protocol parsing
    // -----------------------------------------------------------------------

    #[test]
    fn invalid_protocol_is_target_parse_error() {
        // Distinct rule names so validate() reaches into_iter_rules cleanly,
        // then the unknown protocol string fails to parse.
        let cfg = parse(
            r#"
            [[rule]]
            name = "a"
            protocol = "sctp"
            listen_port = 1
            target = "1.1.1.1:1"
        "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let err = cfg.into_iter_rules().err().expect("unknown protocol");
        assert!(
            matches!(err, ConfigError::TargetParse { ref rule, .. } if rule == "a"),
            "got {err:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Internal helper functions
    // -----------------------------------------------------------------------

    #[test]
    fn parse_port_range_single_and_range() {
        assert_eq!(parse_port_range("8080").unwrap(), PortRange::single(8080));
        assert_eq!(
            parse_port_range("8000-8009").unwrap(),
            PortRange::new(8000, 8009).unwrap()
        );
        // Whitespace is trimmed.
        assert_eq!(
            parse_port_range(" 8000 - 8009 ").unwrap(),
            PortRange::new(8000, 8009).unwrap()
        );
    }

    #[test]
    fn parse_port_range_invalid_inputs() {
        assert!(parse_port_range("abc").is_err());
        assert!(parse_port_range("xx-10").is_err());
        assert!(parse_port_range("10-yy").is_err());
        // Inverted range surfaces the PortRange validator's error.
        assert!(parse_port_range("20-10").is_err());
    }

    #[test]
    fn last_colon_pos_plain_and_ipv6() {
        // Plain host:port -> last colon.
        assert_eq!(last_colon_pos("host:8080"), Some(4));
        // No colon at all.
        assert_eq!(last_colon_pos("host"), None);
        // Bracketed IPv6 -> colon right after ']'.
        assert_eq!(last_colon_pos("[::1]:8080"), Some(5));
        // Bracketed IPv6 with no trailing port colon.
        assert_eq!(last_colon_pos("[::1]"), None);
        // Open bracket but no close bracket.
        assert_eq!(last_colon_pos("[::1"), None);
    }

    #[test]
    fn extract_port_part_works() {
        assert_eq!(extract_port_part("host:8080").unwrap(), "8080");
        assert_eq!(extract_port_part("[::1]:9000").unwrap(), "9000");
        assert!(extract_port_part("nocolon").is_err());
    }

    #[test]
    fn parse_target_string_plain_dns() {
        let (host, range) = parse_target_string("example.com:80", "r").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(range, PortRange::single(80));
    }

    #[test]
    fn parse_target_string_ipv6_with_range() {
        let (host, range) = parse_target_string("[::1]:9000-9001", "r").unwrap();
        assert_eq!(host, "[::1]");
        assert_eq!(range, PortRange::new(9000, 9001).unwrap());
    }

    #[test]
    fn parse_target_string_missing_colon_errors() {
        let err = parse_target_string("nohostport", "r").unwrap_err();
        assert!(
            matches!(err, ConfigError::TargetParse { ref rule, .. } if rule == "r"),
            "got {err:?}"
        );
    }

    #[test]
    fn parse_target_string_bad_port_errors() {
        let err = parse_target_string("host:notaport", "r").unwrap_err();
        assert!(
            matches!(err, ConfigError::TargetParse { ref rule, .. } if rule == "r"),
            "got {err:?}"
        );
    }

    #[test]
    fn parse_target_range_part_returns_range() {
        assert_eq!(
            parse_target_range_part("host:8000-8009").unwrap(),
            PortRange::new(8000, 8009).unwrap()
        );
        assert!(parse_target_range_part("nocolon").is_err());
    }

    #[test]
    fn classify_target_variants() {
        assert!(matches!(
            classify_target("1.1.1.1", "r").unwrap(),
            Target::Ip(_)
        ));
        assert!(matches!(
            classify_target("[::1]", "r").unwrap(),
            Target::Ip(_)
        ));
        assert!(matches!(
            classify_target("example.com", "r").unwrap(),
            Target::Dns(_)
        ));
        let err = classify_target("bad host!", "r").unwrap_err();
        assert!(matches!(err, ConfigError::TargetParse { ref rule, .. } if rule == "r"));
    }

    #[test]
    fn parse_protocol_ok_and_err() {
        assert_eq!(parse_protocol("tcp", "r").unwrap(), Protocol::Tcp);
        assert_eq!(parse_protocol("udp", "r").unwrap(), Protocol::Udp);
        let err = parse_protocol("nope", "r").unwrap_err();
        assert!(matches!(err, ConfigError::TargetParse { ref rule, .. } if rule == "r"));
    }

    #[test]
    fn parse_proxy_protocol_all_branches() {
        assert_eq!(parse_proxy_protocol(None, "r").unwrap(), None);
        assert_eq!(
            parse_proxy_protocol(Some("v1"), "r").unwrap(),
            Some(portunus_core::ProxyProtocolVersion::V1)
        );
        assert_eq!(
            parse_proxy_protocol(Some("v2"), "r").unwrap(),
            Some(portunus_core::ProxyProtocolVersion::V2)
        );
        let err = parse_proxy_protocol(Some("v3"), "r").unwrap_err();
        assert!(matches!(err, ConfigError::TargetParse { ref rule, .. } if rule == "r"));
    }

    // -----------------------------------------------------------------------
    // Display / Default impls
    // -----------------------------------------------------------------------

    #[test]
    fn config_error_display_messages() {
        assert_eq!(
            ConfigError::NoRules.to_string(),
            "config must contain at least one [[rule]]"
        );
        assert_eq!(
            ConfigError::ListenExclusivity.to_string(),
            "each rule must have exactly one of `listen_port` or `listen_ports`"
        );
        assert_eq!(
            ConfigError::TargetExclusivity.to_string(),
            "each rule must have exactly one of `target` or `targets`"
        );
        assert!(
            ConfigError::DuplicateName("dup".into())
                .to_string()
                .contains("dup")
        );
        assert!(
            ConfigError::EmptyTargets("e".into())
                .to_string()
                .contains('e')
        );
        let collision = ConfigError::RuleIdCollision {
            a: "x".into(),
            b: "y".into(),
        };
        assert!(collision.to_string().contains("collision"));
        let mismatch = ConfigError::RangeSizeMismatch {
            listen_len: 2,
            target_len: 3,
        };
        assert!(mismatch.to_string().contains('2') && mismatch.to_string().contains('3'));
        let validation = ConfigError::Validation { msg: "boom".into() };
        assert!(validation.to_string().contains("boom"));
        assert!(
            ConfigError::RangeMultiTargetUnsupported("m".into())
                .to_string()
                .contains("multi-target")
        );
    }

    #[test]
    fn defaults_impls_match_default_fns() {
        let g = GlobalConfig::default();
        assert_eq!(g.label, None);
        assert_eq!(g.log_level, default_log_level());
        assert_eq!(g.log_format, default_log_format());
        assert_eq!(g.shutdown_drain_secs, default_shutdown_drain_secs());

        let d = DefaultsConfig::default();
        assert_eq!(d.protocol, None);
        assert_eq!(d.udp_max_flows, default_udp_max_flows());
        assert_eq!(d.udp_flow_idle_secs, default_udp_flow_idle_secs());
        assert!(!d.prefer_ipv6);

        let s = StatsConfig::default();
        assert_eq!(s.enabled, default_stats_enabled());
        assert_eq!(s.refresh_ms, default_stats_refresh_ms());
        assert_eq!(s.socket_path, default_stats_socket_path());
    }
}
