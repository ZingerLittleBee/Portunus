//! Synchronous HTTP wrappers used by the rule subcommands.
//!
//! Rule operations require a live gRPC channel to the target client, which
//! only the running server holds. The CLI therefore talks to the server's
//! loopback HTTP API rather than executing in-process. Exit codes follow
//! `operator-api.md` (frozen for v1).

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use forward_core::Target;

use crate::OutputFormat;
use crate::operator::cli::{OperatorError, parse_listen, parse_target};
use crate::rules::{Rule, RuleState};

const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
// Rule pushes can wait on the client's ack — pad the HTTP timeout above the
// server-side --ack-timeout to avoid the HTTP layer firing first.
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorBody,
}

#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    code: String,
    #[allow(dead_code)]
    message: String,
}

fn client() -> Result<reqwest::blocking::Client, u8> {
    reqwest::blocking::Client::builder()
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .timeout(HTTP_REQUEST_TIMEOUT)
        .build()
        .map_err(|e| {
            eprintln!("error: build http client: {e}");
            1
        })
}

/// 005-multi-user-rbac T025: every operator HTTP request now requires
/// `Authorization: Bearer <token>`. The CLI reads the token from the
/// `FORWARD_OPERATOR_TOKEN` env var (set by the operator's shell, by
/// e2e tests, or by `bootstrap-superadmin --print-export`).
fn bearer_token_from_env() -> Option<String> {
    match std::env::var("FORWARD_OPERATOR_TOKEN") {
        Ok(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

fn apply_auth(req: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
    if let Some(t) = bearer_token_from_env() {
        req.bearer_auth(t)
    } else {
        req
    }
}

/// Translate the HTTP API's frozen `error.code` strings into the frozen CLI
/// exit codes from `operator-api.md`. New v1.1 codes (`exceeds_cap`,
/// `range_invalid`, `mismatched_range`) reuse the existing exit-3 family
/// per the stability guarantee in `contracts/operator-api.md`.
fn code_to_exit(code: &str) -> u8 {
    match code {
        "client_already_exists" => 2,
        // 003-domain-name-forward: target_host validator codes
        // (`invalid_target_host*`) share the exit-3 family (input
        // validation) with v0.2.0 codes per the stability guarantee
        // in `contracts/operator-api.md`.
        "invalid_name"
        | "invalid_protocol"
        | "invalid_target"
        | "exceeds_cap"
        | "range_invalid"
        | "range_inverted"
        | "mismatched_range"
        | "invalid_target_host"
        | "invalid_target_host_too_long"
        | "invalid_target_host_label_too_long"
        | "invalid_target_host_label_hyphen"
        // 004-udp-forward T018/T034: capability mismatch reuses exit 3
        // (input-validation family) per operator-api.md stability rules.
        | "unsupported_protocol"
        | "proxy_protocol_unsupported_by_client"
        | "validation.proxy_protocol_invalid"
        | "validation.proxy_protocol_on_unsupported_rule" => 3,
        "client_not_connected" => 4,
        "port_in_use" => 5,
        "activation_failed" => 6,
        "ack_timeout" => 7,
        "rule_not_found" => 8,
        _ => 1,
    }
}

fn extract_error(resp: reqwest::blocking::Response) -> u8 {
    let status = resp.status();
    if let Ok(env) = resp.json::<ApiErrorEnvelope>() {
        eprintln!("error: {} ({})", env.error.code, env.error.message);
        code_to_exit(&env.error.code)
    } else {
        eprintln!("error: server returned {status}");
        1
    }
}

#[derive(Debug, Deserialize)]
struct PushResponse {
    rule_id: u64,
}

#[allow(clippy::too_many_arguments)]
pub fn push(
    endpoint: &str,
    raw_client: &str,
    listen_spec: &str,
    target: Option<&str>,
    protocol: &str,
    ack_timeout_secs: u64,
    prefer_ipv6: bool,
    target_specs: &[String],
    targets_json: Option<&str>,
    health_check_interval_secs: Option<u32>,
    sni_pattern: Option<&str>,
) -> Result<(), u8> {
    let listen = parse_listen(listen_spec).map_err(|e| {
        eprintln!("error: {e}");
        e.exit_code()
    })?;

    // 007-multi-target-failover T043: shape detection — exactly one
    // of {positional target, --target, --targets-json} must be set.
    // Clap already enforces conflict between --target and
    // --targets-json; we still need to reject "neither" and "all
    // three" early so the operator gets exit-3 instead of a network
    // round-trip + 400.
    // 007-multi-target-failover T041: enforce the health-check interval
    // bound (1..=3600 per `data-model.md` V-R5) client-side so the
    // operator gets exit 3 immediately instead of a network round-trip
    // + 400. Server enforces the same bound for defence-in-depth.
    if let Some(hci) = health_check_interval_secs
        && !(1..=3600).contains(&hci)
    {
        eprintln!("error: health_check_interval_out_of_range: {hci} (must be 1..=3600)");
        return Err(3);
    }

    // 009-tls-sni-routing T044: client-side rejection per contracts/cli.md.
    // Operators get exit 2 immediately for shape errors that the server
    // would also reject (a network round-trip is wasteful when the
    // problem is in the local invocation).
    let sni_normalised: Option<String> = match sni_pattern {
        None => None,
        Some(s) if s.trim().is_empty() => None,
        Some(s) => {
            if !protocol.eq_ignore_ascii_case("tcp") {
                eprintln!(
                    "error: validation.sni_on_unsupported_rule: --sni is only valid on tcp single-port rules"
                );
                return Err(2);
            }
            if listen.len() > 1 {
                eprintln!(
                    "error: validation.sni_on_unsupported_rule: --sni is only valid on single-port rules, not ranges"
                );
                return Err(2);
            }
            Some(s.trim().to_ascii_lowercase())
        }
    };

    let multi_form = !target_specs.is_empty() || targets_json.is_some();
    let legacy_form = target.is_some();
    if multi_form && legacy_form {
        eprintln!(
            "error: rule_shape_conflict (mix of positional target + --target/--targets-json)"
        );
        return Err(3);
    }
    if !multi_form && !legacy_form {
        eprintln!(
            "error: rule_shape_missing (provide a positional target, --target, or --targets-json)"
        );
        return Err(3);
    }

    let url = format!("http://{endpoint}/v1/rules");
    let mut body = serde_json::json!({
        "client": raw_client,
        "listen_port": listen.start(),
        "protocol": protocol,
        "ack_timeout_secs": ack_timeout_secs,
    });

    if multi_form {
        let targets = if let Some(json) = targets_json {
            // Parse the JSON array verbatim — server validates V-T1..V-T4.
            serde_json::from_str::<serde_json::Value>(json).map_err(|e| {
                eprintln!("error: invalid_targets_json: {e}");
                3u8
            })?
        } else {
            let mut arr: Vec<serde_json::Value> = Vec::with_capacity(target_specs.len());
            for spec in target_specs {
                let (host, port, priority) = parse_target_spec(spec).map_err(|e| {
                    eprintln!("error: invalid_target_spec --target {spec:?}: {e}");
                    3u8
                })?;
                let mut entry = serde_json::json!({"host": host, "port": port});
                if let Some(p) = priority {
                    entry["priority"] = serde_json::Value::Number(p.into());
                }
                arr.push(entry);
            }
            serde_json::Value::Array(arr)
        };
        let obj = body.as_object_mut().expect("just built a json object");
        obj.insert("targets".into(), targets);
        if let Some(hci) = health_check_interval_secs {
            obj.insert(
                "health_check_interval_secs".into(),
                serde_json::Value::Number(hci.into()),
            );
        }
    } else {
        let target_str = target.expect("legacy_form true");
        let (target_host, target_range) = parse_target(target_str).map_err(|e| {
            eprintln!("error: {e}");
            e.exit_code()
        })?;
        if let Err(e) = Target::parse(&target_host) {
            let op_err: OperatorError = e.into();
            eprintln!("error: {op_err}");
            return Err(op_err.exit_code());
        }
        let obj = body.as_object_mut().expect("just built a json object");
        obj.insert("target_host".into(), target_host.into());
        obj.insert("target_port".into(), target_range.start().into());
        if listen.len() > 1 || target_range.len() > 1 {
            obj.insert("listen_port_end".into(), listen.end().into());
            obj.insert("target_port_end".into(), target_range.end().into());
        }
    }

    // 003-domain-name-forward T041: only emit `prefer_ipv6` when the
    // operator explicitly opted in. Absence on the wire decodes to
    // default `false` server-side per `contracts/operator-api.md`,
    // so omitting keeps v0.2.0 byte-compatibility for the IP path.
    if prefer_ipv6 {
        let obj = body.as_object_mut().expect("just built a json object");
        obj.insert("prefer_ipv6".into(), true.into());
    }
    if let Some(sni) = sni_normalised {
        let obj = body.as_object_mut().expect("just built a json object");
        obj.insert("sni_pattern".into(), sni.into());
    }
    let resp = apply_auth(client()?.post(&url).json(&body))
        .send()
        .map_err(|e| {
            eprintln!("error: http: {e}");
            1
        })?;
    if resp.status().is_success() {
        let parsed: PushResponse = resp.json().map_err(|e| {
            eprintln!("error: parse response: {e}");
            1
        })?;
        println!("{}", parsed.rule_id);
        Ok(())
    } else {
        Err(extract_error(resp))
    }
}

/// 007-multi-target-failover T043: parse `host:port[@priority]`.
/// Returns the components for `--target` CLI assembly.
fn parse_target_spec(spec: &str) -> Result<(String, u16, Option<u32>), String> {
    let (rest, priority) = if let Some((before, after)) = spec.rsplit_once('@') {
        let p: u32 = after
            .parse()
            .map_err(|_| format!("invalid priority {after:?} (must be u32)"))?;
        (before, Some(p))
    } else {
        (spec, None)
    };
    let (host, port_str) = rest
        .rsplit_once(':')
        .ok_or_else(|| "expected host:port[@priority]".to_string())?;
    if host.is_empty() {
        return Err("host must be non-empty".to_string());
    }
    let port: u16 = port_str
        .parse()
        .map_err(|_| format!("invalid port {port_str:?}"))?;
    if port == 0 {
        return Err("port must be 1..=65535".to_string());
    }
    Ok((host.to_string(), port, priority))
}

pub fn remove(endpoint: &str, rule_id: u64) -> Result<(), u8> {
    let url = format!("http://{endpoint}/v1/rules/{rule_id}");
    let resp = apply_auth(client()?.delete(&url)).send().map_err(|e| {
        eprintln!("error: http: {e}");
        1
    })?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(extract_error(resp))
    }
}

pub fn list(endpoint: &str, client_filter: Option<&str>, format: OutputFormat) -> Result<(), u8> {
    use std::fmt::Write as _;
    let mut url = format!("http://{endpoint}/v1/rules");
    if let Some(c) = client_filter {
        let _ = write!(url, "?client={c}");
    }
    let resp = apply_auth(client()?.get(&url)).send().map_err(|e| {
        eprintln!("error: http: {e}");
        1
    })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let rules: Vec<Rule> = resp.json().map_err(|e| {
        eprintln!("error: parse: {e}");
        1
    })?;
    match format {
        OutputFormat::Json => {
            let s = serde_json::to_string_pretty(&rules).map_err(|_| 1u8)?;
            println!("{s}");
        }
        OutputFormat::Text => {
            print!("{}", render_rules_text(&rules));
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct StatsResponse {
    rule_id: u64,
    client_name: String,
    bytes_in: u64,
    bytes_out: u64,
    active_connections: u32,
    /// 003-domain-name-forward T052: per-rule DNS-failure counter.
    /// Always present in the body per `contracts/operator-api.md`;
    /// 0 for IP-target rules.
    #[serde(default)]
    dns_failures: u64,
    /// 004-udp-forward T040: protocol discriminator from the store.
    /// Defaults to `"tcp"` for v0.3 backward-compat (a v0.3 server
    /// won't include the field; the cli falls back to TCP rendering).
    #[serde(default = "default_protocol_str")]
    protocol: String,
    /// 004-udp-forward T040: UDP-specific counters. Always present in
    /// the JSON body for v0.4 servers; default-zero for TCP rules.
    #[serde(default)]
    datagrams_in: u64,
    #[serde(default)]
    datagrams_out: u64,
    #[serde(default)]
    active_flows: u32,
    #[serde(default)]
    flows_dropped_overflow: u64,
    updated_at: DateTime<Utc>,
    /// Optional per-port detail; populated only when `?per_port=true`
    /// was requested AND the rule is a range rule with cached samples
    /// (002-port-range-forward, T046).
    #[serde(default)]
    per_port: Option<Vec<PerPortStat>>,
    /// 007-multi-target-failover T039: lifetime count of target
    /// Healthy↔Failed transitions. Always present in v0.7+ server
    /// responses; default-zero for single-target rules (I-3).
    #[serde(default)]
    target_failovers_total: u64,
    /// 007-multi-target-failover T039: per-target detail; populated
    /// only when `?per_target=true` AND the rule has targets.
    #[serde(default)]
    per_target: Option<Vec<PerTargetStat>>,
}

#[derive(Debug, Deserialize)]
struct PerTargetStat {
    index: u32,
    host: String,
    port: u32,
    priority: u32,
    health: u32,
    consecutive_failures: u32,
    last_failure_at_unix_ms: u64,
    last_success_at_unix_ms: u64,
    bytes_in: u64,
    bytes_out: u64,
    connections_accepted: u64,
}

fn default_protocol_str() -> String {
    "tcp".to_string()
}

#[derive(Debug, Deserialize)]
struct PerPortStat {
    listen_port: u16,
    bytes_in: u64,
    bytes_out: u64,
    active_connections: u32,
    /// 004-udp-forward T053: per-port UDP datagram counters. Default-
    /// zero for TCP per-port entries.
    #[serde(default)]
    datagrams_in: u64,
    #[serde(default)]
    datagrams_out: u64,
}

pub fn stats(
    endpoint: &str,
    rule_id: u64,
    format: OutputFormat,
    per_port: bool,
    per_target: bool,
) -> Result<(), u8> {
    let mut url = format!("http://{endpoint}/v1/rules/{rule_id}/stats");
    let mut query: Vec<&str> = Vec::new();
    if per_port {
        query.push("per_port=true");
    }
    if per_target {
        query.push("per_target=true");
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(&query.join("&"));
    }
    let resp = apply_auth(client()?.get(&url)).send().map_err(|e| {
        eprintln!("error: http: {e}");
        1
    })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let body: StatsResponse = resp.json().map_err(|e| {
        eprintln!("error: parse: {e}");
        1
    })?;
    match format {
        OutputFormat::Json => {
            let s = serde_json::to_string_pretty(&body_as_json(&body)).map_err(|_| 1u8)?;
            println!("{s}");
        }
        OutputFormat::Text => {
            // 004-udp-forward T040: protocol-aware text rendering. UDP
            // rules drop `active_connections` (always 0) in favour of
            // `active_flows / datagrams_*`. The JSON shape (consumed by
            // generic operator tooling) keeps every field unconditional.
            if body.protocol == "udp" {
                println!(
                    "rule_id={} client={} protocol=udp bytes_in={} bytes_out={} active_flows={} datagrams_in={} datagrams_out={} flows_dropped_overflow={} dns_failures={} updated_at={}",
                    body.rule_id,
                    body.client_name,
                    body.bytes_in,
                    body.bytes_out,
                    body.active_flows,
                    body.datagrams_in,
                    body.datagrams_out,
                    body.flows_dropped_overflow,
                    body.dns_failures,
                    body.updated_at.format("%Y-%m-%dT%H:%M:%SZ"),
                );
            } else {
                println!(
                    "rule_id={} client={} protocol={} bytes_in={} bytes_out={} active={} dns_failures={} target_failovers_total={} updated_at={}",
                    body.rule_id,
                    body.client_name,
                    body.protocol,
                    body.bytes_in,
                    body.bytes_out,
                    body.active_connections,
                    body.dns_failures,
                    body.target_failovers_total,
                    body.updated_at.format("%Y-%m-%dT%H:%M:%SZ"),
                );
            }
            if let Some(rows) = body.per_target.as_ref() {
                use std::fmt::Write as _;
                if rows.is_empty() {
                    println!("(single-target rule, no per-target state)");
                } else {
                    let mut buf = String::new();
                    let _ = writeln!(
                        buf,
                        "{:<3} {:<24} {:<6} {:<4} {:<8} {:<5} {:<14} {:<14} {:<6}",
                        "IDX",
                        "HOST",
                        "PORT",
                        "PRIO",
                        "HEALTH",
                        "FAILS",
                        "BYTES_IN",
                        "BYTES_OUT",
                        "CONNS",
                    );
                    for r in rows {
                        let h = if r.health == 0 { "Healthy" } else { "Failed" };
                        let _ = writeln!(
                            buf,
                            "{:<3} {:<24} {:<6} {:<4} {:<8} {:<5} {:<14} {:<14} {:<6}",
                            r.index,
                            r.host,
                            r.port,
                            r.priority,
                            h,
                            r.consecutive_failures,
                            r.bytes_in,
                            r.bytes_out,
                            r.connections_accepted,
                        );
                    }
                    print!("{buf}");
                }
            }
            if let Some(rows) = body.per_port.as_ref() {
                use std::fmt::Write as _;
                let mut buf = String::new();
                if body.protocol == "udp" {
                    let _ = writeln!(
                        buf,
                        "{:<8} {:<14} {:<14} {:<14} {:<14}",
                        "PORT", "BYTES_IN", "BYTES_OUT", "DATAGRAMS_IN", "DATAGRAMS_OUT"
                    );
                    for r in rows {
                        let _ = writeln!(
                            buf,
                            "{:<8} {:<14} {:<14} {:<14} {:<14}",
                            r.listen_port, r.bytes_in, r.bytes_out, r.datagrams_in, r.datagrams_out
                        );
                    }
                } else {
                    let _ = writeln!(
                        buf,
                        "{:<8} {:<14} {:<14} {:<8}",
                        "PORT", "BYTES_IN", "BYTES_OUT", "ACTIVE"
                    );
                    for r in rows {
                        let _ = writeln!(
                            buf,
                            "{:<8} {:<14} {:<14} {:<8}",
                            r.listen_port, r.bytes_in, r.bytes_out, r.active_connections
                        );
                    }
                }
                print!("{buf}");
            }
        }
    }
    Ok(())
}

fn body_as_json(body: &StatsResponse) -> serde_json::Value {
    let mut v = serde_json::json!({
        "rule_id": body.rule_id,
        "client_name": body.client_name,
        "protocol": body.protocol,
        "bytes_in": body.bytes_in,
        "bytes_out": body.bytes_out,
        "active_connections": body.active_connections,
        "dns_failures": body.dns_failures,
        // 004-udp-forward T040: UDP fields always present in JSON shape
        // (default-zero for TCP) so generic operator tooling doesn't
        // need protocol-conditional parsing.
        "datagrams_in": body.datagrams_in,
        "datagrams_out": body.datagrams_out,
        "active_flows": body.active_flows,
        "flows_dropped_overflow": body.flows_dropped_overflow,
        // 007-multi-target-failover T039: present-but-zero for legacy
        // single-target rules (I-3) so generic tooling can read it
        // without a conditional.
        "target_failovers_total": body.target_failovers_total,
        "updated_at": body.updated_at,
    });
    if let Some(rows) = body.per_target.as_ref() {
        v["per_target"] = serde_json::Value::Array(
            rows.iter()
                .map(|r| {
                    serde_json::json!({
                        "index": r.index,
                        "host": r.host,
                        "port": r.port,
                        "priority": r.priority,
                        "health": if r.health == 0 { "Healthy" } else { "Failed" },
                        "consecutive_failures": r.consecutive_failures,
                        "last_failure_at_unix_ms": r.last_failure_at_unix_ms,
                        "last_success_at_unix_ms": r.last_success_at_unix_ms,
                        "bytes_in": r.bytes_in,
                        "bytes_out": r.bytes_out,
                        "connections_accepted": r.connections_accepted,
                    })
                })
                .collect(),
        );
    }
    if let Some(rows) = body.per_port.as_ref() {
        v["per_port"] = serde_json::Value::Array(
            rows.iter()
                .map(|r| {
                    serde_json::json!({
                        "listen_port": r.listen_port,
                        "bytes_in": r.bytes_in,
                        "bytes_out": r.bytes_out,
                        "active_connections": r.active_connections,
                        "datagrams_in": r.datagrams_in,
                        "datagrams_out": r.datagrams_out,
                    })
                })
                .collect(),
        );
    }
    v
}

fn render_rules_text(rules: &[Rule]) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    // 009-tls-sni-routing T085: SNI column. Rules without an SNI
    // selector render `-` so the column width stays stable.
    let _ = writeln!(
        s,
        "{:<6} {:<20} {:<6} {:<32} {:<24} {:<10}",
        "ID", "CLIENT", "PORT", "TARGET", "SNI/PROXY", "STATE"
    );
    for r in rules {
        let state = match &r.state {
            RuleState::Pending => "pending".to_string(),
            RuleState::Active => "active".to_string(),
            RuleState::Failed { reason } => format!("failed:{reason}"),
            RuleState::Removed => "removed".to_string(),
        };
        let proxy = r
            .targets_view()
            .iter()
            .filter_map(|target| {
                target.proxy_protocol.map(|mode| {
                    let mode = match mode {
                        forward_core::ProxyProtocolVersion::V1 => "v1",
                        forward_core::ProxyProtocolVersion::V2 => "v2",
                    };
                    format!("{}:{}={mode}", target.host, target.port)
                })
            })
            .collect::<Vec<_>>()
            .join(",");
        let selector = match (r.sni_pattern.as_deref(), proxy.is_empty()) {
            (Some(sni), false) => format!("{sni} | {proxy}"),
            (Some(sni), true) => sni.to_string(),
            (None, false) => proxy,
            (None, true) => "-".to_string(),
        };
        let _ = writeln!(
            s,
            "{:<6} {:<20} {:<6} {:<32} {:<24} {:<10}",
            r.id.0,
            r.client_name,
            r.listen_port,
            format!("{}:{}", r.target_host, r.target_port),
            selector,
            state,
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_protocol_errors_use_input_validation_exit_code() {
        for code in [
            "proxy_protocol_unsupported_by_client",
            "validation.proxy_protocol_invalid",
            "validation.proxy_protocol_on_unsupported_rule",
        ] {
            assert_eq!(code_to_exit(code), 3, "{code}");
        }
    }
}
