//! T037 (005-multi-user-rbac, US2) — operator-facing CLI subcommand
//! HTTP wrappers for the user / grant surface.
//!
//! Symmetrical with [`crate::operator::rule_cli`]: each function takes
//! the operator's `--http-endpoint` and the request shape, attaches
//! `Authorization: Bearer <PORTUNUS_OPERATOR_TOKEN>`, and prints the
//! response in the operator's chosen [`crate::OutputFormat`].
//!
//! Exit codes match `contracts/operator-api.md` § "CLI Exit Codes":
//!   0 success | 2 conflict (already_exists, last_superadmin, cannot_remove_self)
//!   3 validation | 4 auth | 5 rbac denial | 6 bootstrap_required | 1 other.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::OutputFormat;

const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

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

fn bearer() -> Result<String, u8> {
    if let Some(s) = crate::operator::operator_token_from_env() {
        Ok(s)
    } else {
        eprintln!("{}", crate::operator::operator_token_missing_message());
        Err(4)
    }
}

fn code_to_exit(code: &str) -> u8 {
    match code {
        "user_already_exists" | "cannot_remove_self" | "last_superadmin" => 2,
        "invalid_user_id"
        | "invalid_display_name"
        | "reserved_user_id"
        | "invalid_port_range"
        | "empty_protocol_set"
        | "invalid_client" => 3,
        "unauthenticated" | "credential_invalid" | "user_disabled" => 4,
        "client_not_granted"
        | "port_outside_grant"
        | "protocol_not_granted"
        | "not_owner"
        | "role_required" => 5,
        "bootstrap_required" => 6,
        "user_not_found" | "credential_not_found" | "grant_not_found" => 8,
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

fn render(value: &Value, format: OutputFormat) -> Result<(), u8> {
    match format {
        OutputFormat::Json => {
            let s = serde_json::to_string_pretty(value).map_err(|_| 1u8)?;
            println!("{s}");
        }
        OutputFormat::Text => {
            // The operator surface is small enough that a single
            // pretty-printed JSON dump is the friendliest "text" output
            // (no shell quoting traps, valid input for `jq`). When more
            // structured rendering is needed, callers can `--format=text`
            // for the human variant we add here per command.
            let s = serde_json::to_string_pretty(value).map_err(|_| 1u8)?;
            println!("{s}");
        }
    }
    Ok(())
}

// ---------------- users ----------------

pub fn user_add(
    endpoint: &str,
    user_id: &str,
    display_name: &str,
    role: &str,
    format: OutputFormat,
) -> Result<(), u8> {
    let url = format!("http://{endpoint}/v1/users");
    let body = serde_json::json!({
        "user_id": user_id,
        "display_name": display_name,
        "role": role,
    });
    let resp = client()?
        .post(&url)
        .bearer_auth(bearer()?)
        .json(&body)
        .send()
        .map_err(|e| {
            eprintln!("error: http: {e}");
            1
        })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let v: Value = resp.json().map_err(|_| 1u8)?;
    render(&v, format)
}

pub fn user_list(endpoint: &str, format: OutputFormat) -> Result<(), u8> {
    let url = format!("http://{endpoint}/v1/users");
    let resp = client()?
        .get(&url)
        .bearer_auth(bearer()?)
        .send()
        .map_err(|e| {
            eprintln!("error: http: {e}");
            1
        })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let v: Value = resp.json().map_err(|_| 1u8)?;
    render(&v, format)
}

pub fn user_get(endpoint: &str, user_id: &str, format: OutputFormat) -> Result<(), u8> {
    let url = format!("http://{endpoint}/v1/users/{user_id}");
    let resp = client()?
        .get(&url)
        .bearer_auth(bearer()?)
        .send()
        .map_err(|e| {
            eprintln!("error: http: {e}");
            1
        })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let v: Value = resp.json().map_err(|_| 1u8)?;
    render(&v, format)
}

pub fn user_remove(endpoint: &str, user_id: &str, format: OutputFormat) -> Result<(), u8> {
    let url = format!("http://{endpoint}/v1/users/{user_id}");
    let resp = client()?
        .delete(&url)
        .bearer_auth(bearer()?)
        .send()
        .map_err(|e| {
            eprintln!("error: http: {e}");
            1
        })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let v: Value = resp.json().map_err(|_| 1u8)?;
    render(&v, format)
}

// ---------------- grants ----------------

#[allow(clippy::too_many_arguments)]
pub fn grant_add(
    endpoint: &str,
    user_id: &str,
    client_scope: &str,
    listen_port_start: u16,
    listen_port_end: u16,
    protocols: &[String],
    note: Option<&str>,
    format: OutputFormat,
) -> Result<(), u8> {
    let url = format!("http://{endpoint}/v1/grants");
    let mut body = serde_json::json!({
        "user_id": user_id,
        "client": client_scope,
        "listen_port_start": listen_port_start,
        "listen_port_end": listen_port_end,
        "protocols": protocols,
    });
    if let Some(n) = note {
        body["note"] = serde_json::Value::String(n.to_string());
    }
    let resp = client()?
        .post(&url)
        .bearer_auth(bearer()?)
        .json(&body)
        .send()
        .map_err(|e| {
            eprintln!("error: http: {e}");
            1
        })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let v: Value = resp.json().map_err(|_| 1u8)?;
    render(&v, format)
}

pub fn grant_list(
    endpoint: &str,
    user_filter: Option<&str>,
    format: OutputFormat,
) -> Result<(), u8> {
    let mut url = format!("http://{endpoint}/v1/grants");
    if let Some(u) = user_filter {
        url.push_str(&format!("?user_id={u}"));
    }
    let resp = client()?
        .get(&url)
        .bearer_auth(bearer()?)
        .send()
        .map_err(|e| {
            eprintln!("error: http: {e}");
            1
        })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let v: Value = resp.json().map_err(|_| 1u8)?;
    render(&v, format)
}

pub fn grant_revoke(endpoint: &str, grant_id: &str, format: OutputFormat) -> Result<(), u8> {
    let url = format!("http://{endpoint}/v1/grants/{grant_id}");
    let resp = client()?
        .delete(&url)
        .bearer_auth(bearer()?)
        .send()
        .map_err(|e| {
            eprintln!("error: http: {e}");
            1
        })?;
    if !resp.status().is_success() {
        return Err(extract_error(resp));
    }
    let v: Value = resp.json().map_err(|_| 1u8)?;
    render(&v, format)
}
