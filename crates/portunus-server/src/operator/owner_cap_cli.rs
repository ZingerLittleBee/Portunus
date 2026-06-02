//! 011-rate-limiting-qos T028: blocking HTTP wrappers for the
//! `owner-cap` subcommand. Mirrors `rule_cli.rs`'s pattern — talks
//! to the loopback operator HTTP API instead of executing in-process.
//!
//! Subcommands:
//! - `owner-cap list <client>` → `GET /v1/clients/{id}/owners`
//! - `owner-cap get <client> <owner>` → `GET /v1/clients/{id}/owners/{owner_id}/rate-limit`
//! - `owner-cap set <client> <owner> [--cap …]` → `PUT /v1/clients/{id}/owners/{owner_id}/rate-limit`
//! - `owner-cap delete <client> <owner>` → `DELETE /v1/clients/{id}/owners/{owner_id}/rate-limit`

use std::str::FromStr;
use std::time::Duration;

use serde::Deserialize;

use crate::OutputFormat;
use crate::operator::rule_cli::RateLimitArgs;

const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct ApiErrorEnvelope {
    error: ApiErrorBody,
}

#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    code: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct OwnerListEntry {
    owner_id: String,
    has_rate_limit: bool,
    rule_count: usize,
}

#[derive(Debug, Deserialize)]
struct OwnerRateLimitView {
    client_name: String,
    owner_id: String,
    rate_limit: RateLimitJson,
    updated_at_unix_ms: u64,
}

#[derive(Debug, Default, Deserialize)]
struct RateLimitJson {
    #[serde(default)]
    bandwidth_in_bps: Option<u64>,
    #[serde(default)]
    bandwidth_out_bps: Option<u64>,
    #[serde(default)]
    new_connections_per_sec: Option<u32>,
    #[serde(default)]
    concurrent_connections: Option<u32>,
    #[serde(default)]
    bandwidth_in_burst: Option<u64>,
    #[serde(default)]
    bandwidth_out_burst: Option<u64>,
    #[serde(default)]
    new_connections_burst: Option<u32>,
}

fn http_client() -> Result<reqwest::blocking::Client, u8> {
    reqwest::blocking::Client::builder()
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .timeout(HTTP_REQUEST_TIMEOUT)
        .build()
        .map_err(|e| {
            eprintln!("error: build http client: {e}");
            1
        })
}

fn bearer_token_from_env() -> Option<String> {
    crate::operator::operator_token_from_env()
}

fn apply_auth(req: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
    if let Some(t) = bearer_token_from_env() {
        req.bearer_auth(t)
    } else {
        req
    }
}

/// Translate frozen `error.code` strings into CLI exit codes.
/// Reuses the existing exit-3 family for input-validation errors;
/// `owner_rate_limit_not_found` lands on exit 8 (rule_not_found
/// family) because both indicate "the resource you asked for is
/// absent."
fn code_to_exit(code: &str) -> u8 {
    match code {
        "owner_rate_limit_not_found" => 8,
        "rate_limit_unsupported_by_client" => 3,
        c if c.starts_with("validation.") => 3,
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
struct ClientSummary {
    client_id: String,
    client_name: String,
}

/// 015-client-stable-id (T020/T021): the operator HTTP surface addresses
/// clients by their stable `client_id` (ULID). For ergonomics the CLI
/// still accepts a display name and resolves it here against
/// `GET /v1/clients`. A bare ULID is used verbatim (no lookup). A name
/// matching zero clients is "not found" (exit 8); a name matching more
/// than one is ambiguous (exit 3) — the operator must pass the
/// `client_id` to disambiguate (display names are non-unique, FR-009).
fn resolve_client_id(endpoint: &str, client: &str) -> Result<String, u8> {
    if portunus_core::ClientId::from_str(client).is_ok() {
        return Ok(client.to_string());
    }
    let url = format!("http://{endpoint}/v1/clients");
    let resp = apply_auth(http_client()?.get(&url)).send().map_err(|e| {
        eprintln!("error: http: {e}");
        1
    })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let clients: Vec<ClientSummary> = resp.json().map_err(|e| {
        eprintln!("error: parse response: {e}");
        1
    })?;
    let mut matched = clients.into_iter().filter(|c| c.client_name == client);
    match matched.next() {
        None => {
            eprintln!("error: client_not_found (no client named `{client}`)");
            Err(8)
        }
        Some(c) if matched.next().is_none() => Ok(c.client_id),
        Some(_) => {
            eprintln!(
                "error: ambiguous_client_name (`{client}` matches multiple clients; pass the client_id instead)"
            );
            Err(3)
        }
    }
}

pub fn list(endpoint: &str, client_name: &str, format: OutputFormat) -> Result<(), u8> {
    let client_id = resolve_client_id(endpoint, client_name)?;
    let url = format!("http://{endpoint}/v1/clients/{client_id}/owners");
    let resp = apply_auth(http_client()?.get(&url)).send().map_err(|e| {
        eprintln!("error: http: {e}");
        1
    })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let entries: Vec<OwnerListEntry> = resp.json().map_err(|e| {
        eprintln!("error: parse response: {e}");
        1
    })?;
    match format {
        OutputFormat::Json => {
            // Re-emit the parsed shape so the JSON is canonical
            // (server-side `skip_serializing_if` preserved).
            let json = serde_json::json!({
                "owners": entries.iter().map(|e| serde_json::json!({
                    "owner_id": e.owner_id,
                    "has_rate_limit": e.has_rate_limit,
                    "rule_count": e.rule_count,
                })).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&json).unwrap());
        }
        OutputFormat::Text => {
            println!("{:<24} {:<8} {:<10}", "OWNER", "RULES", "CAP STATUS");
            for e in &entries {
                let cap = if e.has_rate_limit {
                    "capped"
                } else {
                    "uncapped"
                };
                println!("{:<24} {:<8} {:<10}", e.owner_id, e.rule_count, cap);
            }
        }
    }
    Ok(())
}

pub fn get(
    endpoint: &str,
    client_name: &str,
    owner_id: &str,
    format: OutputFormat,
) -> Result<(), u8> {
    let client_id = resolve_client_id(endpoint, client_name)?;
    let url = format!("http://{endpoint}/v1/clients/{client_id}/owners/{owner_id}/rate-limit");
    let resp = apply_auth(http_client()?.get(&url)).send().map_err(|e| {
        eprintln!("error: http: {e}");
        1
    })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let view: OwnerRateLimitView = resp.json().map_err(|e| {
        eprintln!("error: parse response: {e}");
        1
    })?;
    match format {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "client_name": view.client_name,
                    "owner_id": view.owner_id,
                    "rate_limit": serde_json::to_value(&view.rate_limit).unwrap(),
                    "updated_at_unix_ms": view.updated_at_unix_ms,
                }))
                .unwrap()
            );
        }
        OutputFormat::Text => {
            println!("client     {}", view.client_name);
            println!("owner      {}", view.owner_id);
            println!("updated_ms {}", view.updated_at_unix_ms);
            print_cap_line("ingress_bps", view.rate_limit.bandwidth_in_bps);
            print_cap_line("egress_bps", view.rate_limit.bandwidth_out_bps);
            print_cap_line(
                "new_conn_per_sec",
                view.rate_limit.new_connections_per_sec.map(u64::from),
            );
            print_cap_line(
                "concurrent_max",
                view.rate_limit.concurrent_connections.map(u64::from),
            );
            print_cap_line("ingress_burst", view.rate_limit.bandwidth_in_burst);
            print_cap_line("egress_burst", view.rate_limit.bandwidth_out_burst);
            print_cap_line(
                "new_conn_burst",
                view.rate_limit.new_connections_burst.map(u64::from),
            );
        }
    }
    Ok(())
}

fn print_cap_line(label: &str, value: Option<u64>) {
    match value {
        Some(v) => println!("{label:<18} {v}"),
        None => println!("{label:<18} -"),
    }
}

impl serde::Serialize for RateLimitJson {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(None)?;
        if let Some(v) = self.bandwidth_in_bps {
            map.serialize_entry("bandwidth_in_bps", &v)?;
        }
        if let Some(v) = self.bandwidth_out_bps {
            map.serialize_entry("bandwidth_out_bps", &v)?;
        }
        if let Some(v) = self.new_connections_per_sec {
            map.serialize_entry("new_connections_per_sec", &v)?;
        }
        if let Some(v) = self.concurrent_connections {
            map.serialize_entry("concurrent_connections", &v)?;
        }
        if let Some(v) = self.bandwidth_in_burst {
            map.serialize_entry("bandwidth_in_burst", &v)?;
        }
        if let Some(v) = self.bandwidth_out_burst {
            map.serialize_entry("bandwidth_out_burst", &v)?;
        }
        if let Some(v) = self.new_connections_burst {
            map.serialize_entry("new_connections_burst", &v)?;
        }
        map.end()
    }
}

pub fn set(
    endpoint: &str,
    client_name: &str,
    owner_id: &str,
    caps: RateLimitArgs,
) -> Result<(), u8> {
    if caps.is_empty() {
        eprintln!(
            "error: validation.rate_limit_no_caps_provided (set at least one --bandwidth-in-bps, --bandwidth-out-bps, --new-connections-per-sec, or --concurrent-connections)"
        );
        return Err(3);
    }
    let client_id = resolve_client_id(endpoint, client_name)?;
    let url = format!("http://{endpoint}/v1/clients/{client_id}/owners/{owner_id}/rate-limit");
    let resp = apply_auth(http_client()?.put(&url).json(&caps.to_json()))
        .send()
        .map_err(|e| {
            eprintln!("error: http: {e}");
            1
        })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let view: OwnerRateLimitView = resp.json().map_err(|e| {
        eprintln!("error: parse response: {e}");
        1
    })?;
    println!(
        "owner-cap set client={} owner={} updated_at_unix_ms={}",
        view.client_name, view.owner_id, view.updated_at_unix_ms,
    );
    Ok(())
}

pub fn delete(endpoint: &str, client_name: &str, owner_id: &str) -> Result<(), u8> {
    let client_id = resolve_client_id(endpoint, client_name)?;
    let url = format!("http://{endpoint}/v1/clients/{client_id}/owners/{owner_id}/rate-limit");
    let resp = apply_auth(http_client()?.delete(&url))
        .send()
        .map_err(|e| {
            eprintln!("error: http: {e}");
            1
        })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    println!("owner-cap deleted client={client_name} owner={owner_id}");
    Ok(())
}
