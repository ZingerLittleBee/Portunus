//! 011-rate-limiting-qos T028 — REST handlers for the per-owner cap
//! envelope (`/v1/clients/{client_id}/owners/{owner_id}/rate-limit`)
//! plus the owners listing under a client.
//!
//! Mirrors the per-rule cap surface in shape: `RateLimit`-typed body,
//! the same four validation subcategories (cap_zero, burst_without_rate,
//! burst_range, burst_unsupported), and the same v0.11 capability gate
//! (`rate_limit_unsupported_by_client` → 422).

use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use portunus_core::ClientName;
use portunus_proto::v1 as proto;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::operator::cli::OperatorError;
use crate::operator::http::ApiError;
use crate::owner::OwnerCapError;
use crate::state::AppState;
use crate::store::owner_cap_store::OwnerRateLimitRow;

// ----------------------------------------------------------------------
// Wire shapes (same field set as the per-rule body, but standalone so
// we can evolve them independently of `RateLimitBody`).
// ----------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct OwnerRateLimitPutBody {
    #[serde(default)]
    pub bandwidth_in_bps: Option<u64>,
    #[serde(default)]
    pub bandwidth_out_bps: Option<u64>,
    #[serde(default)]
    pub new_connections_per_sec: Option<u32>,
    #[serde(default)]
    pub concurrent_connections: Option<u32>,
    #[serde(default)]
    pub bandwidth_in_burst: Option<u64>,
    #[serde(default)]
    pub bandwidth_out_burst: Option<u64>,
    #[serde(default)]
    pub new_connections_burst: Option<u32>,
    /// Reserved per data-model.md §1.1; rejected with the stable code
    /// `validation.rate_limit_burst_unsupported`.
    #[serde(default)]
    pub concurrent_connections_burst: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct OwnerRateLimitView {
    pub client_name: String,
    pub owner_id: String,
    pub rate_limit: RateLimitView,
    pub updated_at_unix_ms: u64,
}

#[derive(Debug, Serialize, Default)]
pub struct RateLimitView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandwidth_in_bps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandwidth_out_bps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_connections_per_sec: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrent_connections: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandwidth_in_burst: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandwidth_out_burst: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_connections_burst: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct OwnerListEntry {
    pub owner_id: String,
    pub has_rate_limit: bool,
    pub rule_count: usize,
}

// ----------------------------------------------------------------------
// Handlers
// ----------------------------------------------------------------------

/// `GET /v1/clients/{client_id}/owners/{owner_id}/rate-limit`
pub async fn get_owner_rate_limit(
    State(state): State<Arc<AppState>>,
    Path((client_id, owner_id)): Path<(String, String)>,
) -> Result<Json<OwnerRateLimitView>, ApiError> {
    let client_name = parse_client_name(&client_id)?;
    let row = state
        .owner_caps
        .get(&client_name, &owner_id)
        .await
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "owner_rate_limit_not_found",
                format!("no rate-limit envelope for client={client_id} owner={owner_id}"),
            )
        })?;
    Ok(Json(envelope_to_view(&row)))
}

/// `PUT /v1/clients/{client_id}/owners/{owner_id}/rate-limit`
pub async fn put_owner_rate_limit(
    State(state): State<Arc<AppState>>,
    Path((client_id, owner_id)): Path<(String, String)>,
    Json(body): Json<OwnerRateLimitPutBody>,
) -> Result<Json<OwnerRateLimitView>, ApiError> {
    let client_name = parse_client_name(&client_id)?;
    // Reserved-burst rejection mirrors the per-rule path so operators
    // get the same stable subcategory across both surfaces.
    if body.concurrent_connections_burst.is_some() {
        return Err(ApiError::from(OperatorError::RateLimitValidation {
            code: "validation.rate_limit_burst_unsupported",
            message: "concurrent_connections_burst is reserved; concurrent caps are a hard ceiling and cannot be bursted".into(),
        }));
    }
    // Capability gate: refuse when the target client's reported
    // version is below 0.11. An unknown / disconnected client gates
    // conservatively (mirrors the per-rule path).
    let Some(client_version) = state.clients.client_version_by_name(&client_name).await else {
        return Err(ApiError::from(
            OperatorError::RateLimitUnsupportedByClient {
                client_name: client_name.clone(),
                client_version: "unknown".into(),
            },
        ));
    };
    if !crate::operator::http::version_at_least_0_11(&client_version) {
        return Err(ApiError::from(
            OperatorError::RateLimitUnsupportedByClient {
                client_name: client_name.clone(),
                client_version,
            },
        ));
    }
    let envelope = portunus_core::RateLimit {
        bandwidth_in_bps: body.bandwidth_in_bps,
        bandwidth_out_bps: body.bandwidth_out_bps,
        new_connections_per_sec: body.new_connections_per_sec,
        concurrent_connections: body.concurrent_connections,
        bandwidth_in_burst: body.bandwidth_in_burst,
        bandwidth_out_burst: body.bandwidth_out_burst,
        new_connections_burst: body.new_connections_burst,
    };
    let row = state
        .owner_caps
        .upsert(&client_name, &owner_id, envelope)
        .await
        .map_err(map_cap_error)?;
    // Push OwnerRateLimitUpdate{SET} to the connected client. Wire
    // delivery is best-effort: an unreachable client gets the cap on
    // its next reconnect via the welcome-replay path (T029). REST
    // success only requires the SQLite commit.
    if let Some((outbound, _waiters)) = state.clients.handles_by_name(&client_name).await {
        let push = proto::ServerMessage {
            payload: Some(proto::server_message::Payload::OwnerRateLimitUpdate(
                proto::OwnerRateLimitUpdate {
                    client_name: client_name.as_str().to_string(),
                    owner_id: owner_id.clone(),
                    rate_limit: Some(rate_limit_to_proto(&row.rate_limit)),
                    action: proto::OwnerRateLimitAction::Set as i32,
                    // TODO(T020): populate once routes address clients by id.
                    client_id: String::new(),
                },
            )),
        };
        if outbound.send(Ok(push)).await.is_err() {
            warn!(
                event = "owner_cap.push_failed",
                client_name = %client_name,
                owner_id = %owner_id,
                action = "set",
            );
        }
    }
    Ok(Json(envelope_to_view(&row)))
}

/// `DELETE /v1/clients/{client_id}/owners/{owner_id}/rate-limit`
pub async fn delete_owner_rate_limit(
    State(state): State<Arc<AppState>>,
    Path((client_id, owner_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let client_name = parse_client_name(&client_id)?;
    let removed = state
        .owner_caps
        .delete(&client_name, &owner_id)
        .await
        .map_err(map_cap_error)?;
    // Push OwnerRateLimitUpdate{REMOVE}. Idempotent on the wire; a
    // best-effort send is sufficient (welcome-replay restores the
    // post-DELETE state on reconnect — i.e. the absence of an entry).
    if let Some((outbound, _waiters)) = state.clients.handles_by_name(&client_name).await {
        let push = proto::ServerMessage {
            payload: Some(proto::server_message::Payload::OwnerRateLimitUpdate(
                proto::OwnerRateLimitUpdate {
                    client_name: client_name.as_str().to_string(),
                    owner_id: owner_id.clone(),
                    rate_limit: None,
                    action: proto::OwnerRateLimitAction::Remove as i32,
                    // TODO(T020): populate once routes address clients by id.
                    client_id: String::new(),
                },
            )),
        };
        if outbound.send(Ok(push)).await.is_err() {
            warn!(
                event = "owner_cap.push_failed",
                client_name = %client_name,
                owner_id = %owner_id,
                action = "remove",
            );
        }
    }
    // 204 on both first and idempotent-replay so callers can DELETE
    // without checking prior existence.
    let _ = removed;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/clients/{client_id}/owners`
///
/// Lists every owner who has any rule on the client OR any cap envelope
/// on the client (so a recently-orphaned cap awaiting GC still surfaces).
/// `has_rate_limit` is `true` iff a cap row exists; `rule_count` is the
/// number of `Active`/`Pending`/`Failed` rules under that owner.
pub async fn get_owners_under_client(
    State(state): State<Arc<AppState>>,
    Path(client_id): Path<String>,
) -> Result<Json<Vec<OwnerListEntry>>, ApiError> {
    let client_name = parse_client_name(&client_id)?;
    // Build the owner -> rule_count map from the in-memory rule store.
    use std::collections::BTreeMap;
    let rules = state.rules.list(Some(&client_name)).await;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for rule in rules {
        *counts.entry(rule.owner_user_id.to_string()).or_insert(0) += 1;
    }
    // Fold the cap rows so an owner with a cap but no rules still
    // appears (e.g. immediately after the last rule was removed and
    // before the GC sweep ran).
    let cap_rows = state.owner_caps.list_for_client(&client_name).await;
    let mut capped: std::collections::HashSet<String> = std::collections::HashSet::new();
    for row in &cap_rows {
        capped.insert(row.owner_id.clone());
        counts.entry(row.owner_id.clone()).or_insert(0);
    }
    let mut out: Vec<OwnerListEntry> = counts
        .into_iter()
        .map(|(owner_id, rule_count)| OwnerListEntry {
            has_rate_limit: capped.contains(&owner_id),
            owner_id,
            rule_count,
        })
        .collect();
    // BTreeMap iteration is already alphabetical; keep the contract
    // explicit so a future map-impl swap doesn't break operators.
    out.sort_by(|a, b| a.owner_id.cmp(&b.owner_id));
    Ok(Json(out))
}

// ----------------------------------------------------------------------
// Helpers
// ----------------------------------------------------------------------

fn parse_client_name(raw: &str) -> Result<ClientName, ApiError> {
    ClientName::new(raw).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "validation.client_name_invalid",
            format!("client_name `{raw}`: {e}"),
        )
    })
}

fn map_cap_error(e: OwnerCapError) -> ApiError {
    match e {
        OwnerCapError::InvalidEnvelope(inner) => match inner {
            portunus_core::rate_limit::RateLimitError::CapZero { .. } => {
                ApiError::from(OperatorError::RateLimitValidation {
                    code: "validation.rate_limit_cap_zero",
                    message: inner.to_string(),
                })
            }
            portunus_core::rate_limit::RateLimitError::BurstWithoutRate { .. } => {
                ApiError::from(OperatorError::RateLimitValidation {
                    code: "validation.rate_limit_burst_without_rate",
                    message: inner.to_string(),
                })
            }
            portunus_core::rate_limit::RateLimitError::BurstRange { .. } => {
                ApiError::from(OperatorError::RateLimitValidation {
                    code: "validation.rate_limit_burst_range",
                    message: inner.to_string(),
                })
            }
        },
        OwnerCapError::UnsupportedByClient => ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "rate_limit_unsupported_by_client",
            "client version below 0.11.0",
        ),
        OwnerCapError::Store(e) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        ),
    }
}

fn rate_limit_to_proto(rl: &portunus_core::RateLimit) -> proto::RateLimit {
    proto::RateLimit {
        bandwidth_in_bps: rl.bandwidth_in_bps,
        bandwidth_out_bps: rl.bandwidth_out_bps,
        new_connections_per_sec: rl.new_connections_per_sec,
        concurrent_connections: rl.concurrent_connections,
        bandwidth_in_burst: rl.bandwidth_in_burst,
        bandwidth_out_burst: rl.bandwidth_out_burst,
        new_connections_burst: rl.new_connections_burst,
    }
}

fn envelope_to_view(row: &OwnerRateLimitRow) -> OwnerRateLimitView {
    OwnerRateLimitView {
        client_name: row.client_name.as_str().to_string(),
        owner_id: row.owner_id.clone(),
        rate_limit: RateLimitView {
            bandwidth_in_bps: row.rate_limit.bandwidth_in_bps,
            bandwidth_out_bps: row.rate_limit.bandwidth_out_bps,
            new_connections_per_sec: row.rate_limit.new_connections_per_sec,
            concurrent_connections: row.rate_limit.concurrent_connections,
            bandwidth_in_burst: row.rate_limit.bandwidth_in_burst,
            bandwidth_out_burst: row.rate_limit.bandwidth_out_burst,
            new_connections_burst: row.rate_limit.new_connections_burst,
        },
        updated_at_unix_ms: row.updated_at_unix_ms,
    }
}
