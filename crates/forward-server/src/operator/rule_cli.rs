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

/// Translate the HTTP API's frozen `error.code` strings into the frozen CLI
/// exit codes from `operator-api.md`. New v1.1 codes (`exceeds_cap`,
/// `range_invalid`, `mismatched_range`) reuse the existing exit-3 family
/// per the stability guarantee in `contracts/operator-api.md`.
fn code_to_exit(code: &str) -> u8 {
    match code {
        "client_already_exists" => 2,
        "invalid_name" | "invalid_protocol" | "invalid_target" | "exceeds_cap"
        | "range_invalid" | "range_inverted" | "mismatched_range" => 3,
        // 003-domain-name-forward: target_host validator codes share
        // the exit-3 family (input validation).
        "invalid_target_host"
        | "invalid_target_host_too_long"
        | "invalid_target_host_label_too_long"
        | "invalid_target_host_label_hyphen" => 3,
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

pub fn push(
    endpoint: &str,
    raw_client: &str,
    listen_spec: &str,
    target: &str,
    protocol: &str,
    ack_timeout_secs: u64,
    prefer_ipv6: bool,
) -> Result<(), u8> {
    let listen = parse_listen(listen_spec).map_err(|e| {
        eprintln!("error: {e}");
        e.exit_code()
    })?;
    let (target_host, target_range) = parse_target(target).map_err(|e| {
        eprintln!("error: {e}");
        e.exit_code()
    })?;
    // 003-domain-name-forward T021: validate the host before we open
    // the HTTP socket so the operator gets exit-3 immediately on
    // malformed input, instead of a round-trip and a server-side
    // 400. The HTTP path validates again as a backstop.
    if let Err(e) = Target::parse(&target_host) {
        let op_err: OperatorError = e.into();
        eprintln!("error: {op_err}");
        return Err(op_err.exit_code());
    }
    let url = format!("http://{endpoint}/v1/rules");
    let mut body = serde_json::json!({
        "client": raw_client,
        "listen_port": listen.start(),
        "target_host": target_host,
        "target_port": target_range.start(),
        "protocol": protocol,
        "ack_timeout_secs": ack_timeout_secs,
    });
    // Co-presence enforced by the server: send both `*_port_end` fields
    // together, only when the user actually requested a range.
    if listen.len() > 1 || target_range.len() > 1 {
        let obj = body.as_object_mut().expect("just built a json object");
        obj.insert("listen_port_end".into(), listen.end().into());
        obj.insert("target_port_end".into(), target_range.end().into());
    }
    // 003-domain-name-forward T041: only emit `prefer_ipv6` when the
    // operator explicitly opted in. Absence on the wire decodes to
    // default `false` server-side per `contracts/operator-api.md`,
    // so omitting keeps v0.2.0 byte-compatibility for the IP path.
    if prefer_ipv6 {
        let obj = body.as_object_mut().expect("just built a json object");
        obj.insert("prefer_ipv6".into(), true.into());
    }
    let resp = client()?.post(&url).json(&body).send().map_err(|e| {
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

pub fn remove(endpoint: &str, rule_id: u64) -> Result<(), u8> {
    let url = format!("http://{endpoint}/v1/rules/{rule_id}");
    let resp = client()?.delete(&url).send().map_err(|e| {
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
    let resp = client()?.get(&url).send().map_err(|e| {
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
    updated_at: DateTime<Utc>,
    /// Optional per-port detail; populated only when `?per_port=true`
    /// was requested AND the rule is a range rule with cached samples
    /// (002-port-range-forward, T046).
    #[serde(default)]
    per_port: Option<Vec<PerPortStat>>,
}

#[derive(Debug, Deserialize)]
struct PerPortStat {
    listen_port: u16,
    bytes_in: u64,
    bytes_out: u64,
    active_connections: u32,
}

pub fn stats(endpoint: &str, rule_id: u64, format: OutputFormat, per_port: bool) -> Result<(), u8> {
    let mut url = format!("http://{endpoint}/v1/rules/{rule_id}/stats");
    if per_port {
        url.push_str("?per_port=true");
    }
    let resp = client()?.get(&url).send().map_err(|e| {
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
            println!(
                "rule_id={} client={} bytes_in={} bytes_out={} active={} updated_at={}",
                body.rule_id,
                body.client_name,
                body.bytes_in,
                body.bytes_out,
                body.active_connections,
                body.updated_at.format("%Y-%m-%dT%H:%M:%SZ"),
            );
            if let Some(rows) = body.per_port.as_ref() {
                use std::fmt::Write as _;
                let mut buf = String::new();
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
        "bytes_in": body.bytes_in,
        "bytes_out": body.bytes_out,
        "active_connections": body.active_connections,
        "updated_at": body.updated_at,
    });
    if let Some(rows) = body.per_port.as_ref() {
        v["per_port"] = serde_json::Value::Array(
            rows.iter()
                .map(|r| {
                    serde_json::json!({
                        "listen_port": r.listen_port,
                        "bytes_in": r.bytes_in,
                        "bytes_out": r.bytes_out,
                        "active_connections": r.active_connections,
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
    let _ = writeln!(
        s,
        "{:<6} {:<20} {:<6} {:<32} {:<10}",
        "ID", "CLIENT", "PORT", "TARGET", "STATE"
    );
    for r in rules {
        let state = match &r.state {
            RuleState::Pending => "pending".to_string(),
            RuleState::Active => "active".to_string(),
            RuleState::Failed { reason } => format!("failed:{reason}"),
            RuleState::Removed => "removed".to_string(),
        };
        let _ = writeln!(
            s,
            "{:<6} {:<20} {:<6} {:<32} {:<10}",
            r.id.0,
            r.client_name,
            r.listen_port,
            format!("{}:{}", r.target_host, r.target_port),
            state,
        );
    }
    s
}
