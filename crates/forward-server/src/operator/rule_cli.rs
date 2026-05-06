//! Synchronous HTTP wrappers used by the rule subcommands.
//!
//! Rule operations require a live gRPC channel to the target client, which
//! only the running server holds. The CLI therefore talks to the server's
//! loopback HTTP API rather than executing in-process. Exit codes follow
//! `operator-api.md` (frozen for v1).

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::OutputFormat;
use crate::operator::cli::parse_target;
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
/// exit codes from `operator-api.md`.
fn code_to_exit(code: &str) -> u8 {
    match code {
        "client_already_exists" => 2,
        "invalid_name" | "invalid_protocol" | "invalid_target" => 3,
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
    listen_port: u16,
    target: &str,
    protocol: &str,
    ack_timeout_secs: u64,
) -> Result<(), u8> {
    let (target_host, target_port) = parse_target(target).map_err(|e| {
        eprintln!("error: {e}");
        e.exit_code()
    })?;
    let url = format!("http://{endpoint}/v1/rules");
    let body = serde_json::json!({
        "client": raw_client,
        "listen_port": listen_port,
        "target_host": target_host,
        "target_port": target_port,
        "protocol": protocol,
        "ack_timeout_secs": ack_timeout_secs,
    });
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
}

pub fn stats(endpoint: &str, rule_id: u64, format: OutputFormat) -> Result<(), u8> {
    let url = format!("http://{endpoint}/v1/rules/{rule_id}/stats");
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
            let s = serde_json::to_string_pretty(&serde_json::json!({
                "rule_id": body.rule_id,
                "client_name": body.client_name,
                "bytes_in": body.bytes_in,
                "bytes_out": body.bytes_out,
                "active_connections": body.active_connections,
                "updated_at": body.updated_at,
            }))
            .map_err(|_| 1u8)?;
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
        }
    }
    Ok(())
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
