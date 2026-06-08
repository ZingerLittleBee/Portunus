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
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use std::str::FromStr;

use portunus_auth::{OperatorIdentity, OperatorRole};
use portunus_core::ClientId;
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
    Extension(identity): Extension<OperatorIdentity>,
    Path((client_id, owner_id)): Path<(String, String)>,
) -> Result<Json<OwnerRateLimitView>, ApiError> {
    crate::operator::rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let cid = parse_client_id(&client_id)?;
    let display_name = resolve_client_name(&state, cid)?;
    let row = state.owner_caps.get(&cid, &owner_id).await.ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "owner_rate_limit_not_found",
            format!("no rate-limit envelope for client={client_id} owner={owner_id}"),
        )
    })?;
    Ok(Json(envelope_to_view(&row, &display_name)))
}

/// `PUT /v1/clients/{client_id}/owners/{owner_id}/rate-limit`
pub async fn put_owner_rate_limit(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path((client_id, owner_id)): Path<(String, String)>,
    Json(body): Json<OwnerRateLimitPutBody>,
) -> Result<Json<OwnerRateLimitView>, ApiError> {
    crate::operator::rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let cid = parse_client_id(&client_id)?;
    let display_name = resolve_client_name(&state, cid)?;
    // The name came from `client_tokens`, so it always satisfies the
    // relaxed `ClientName` contract; surface a 500 if it somehow does not.
    let client_name = portunus_core::ClientName::new(display_name.clone()).map_err(|e| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            format!("stored client_name invalid: {e}"),
        )
    })?;
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
    let Some(client_version) = state.clients.client_version_of(&cid).await else {
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
        .upsert(&cid, &owner_id, envelope)
        .await
        .map_err(map_cap_error)?;
    // Push OwnerRateLimitUpdate{SET} to the connected client. Wire
    // delivery is best-effort: an unreachable client gets the cap on
    // its next reconnect via the welcome-replay path (T029). REST
    // success only requires the SQLite commit.
    if let Some((outbound, _waiters)) = state.clients.handles(&cid).await {
        let push = proto::ServerMessage {
            payload: Some(proto::server_message::Payload::OwnerRateLimitUpdate(
                proto::OwnerRateLimitUpdate {
                    client_name: display_name.clone(),
                    owner_id: owner_id.clone(),
                    rate_limit: Some(rate_limit_to_proto(&row.rate_limit)),
                    action: proto::OwnerRateLimitAction::Set as i32,
                    client_id: cid.to_string(),
                },
            )),
        };
        if outbound.send(Ok(push)).await.is_err() {
            warn!(
                event = "owner_cap.push_failed",
                client_id = %cid,
                owner_id = %owner_id,
                action = "set",
            );
        }
    }
    Ok(Json(envelope_to_view(&row, &display_name)))
}

/// `DELETE /v1/clients/{client_id}/owners/{owner_id}/rate-limit`
pub async fn delete_owner_rate_limit(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path((client_id, owner_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    crate::operator::rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let cid = parse_client_id(&client_id)?;
    // Resolve the current display name for the wire push; tolerate a
    // missing client (already deleted) by falling back to the id string.
    let display_name = resolve_client_name(&state, cid).unwrap_or_else(|_| cid.to_string());
    let removed = state
        .owner_caps
        .delete(&cid, &owner_id)
        .await
        .map_err(map_cap_error)?;
    // Push OwnerRateLimitUpdate{REMOVE}. Idempotent on the wire; a
    // best-effort send is sufficient (welcome-replay restores the
    // post-DELETE state on reconnect — i.e. the absence of an entry).
    if let Some((outbound, _waiters)) = state.clients.handles(&cid).await {
        let push = proto::ServerMessage {
            payload: Some(proto::server_message::Payload::OwnerRateLimitUpdate(
                proto::OwnerRateLimitUpdate {
                    client_name: display_name,
                    owner_id: owner_id.clone(),
                    rate_limit: None,
                    action: proto::OwnerRateLimitAction::Remove as i32,
                    client_id: cid.to_string(),
                },
            )),
        };
        if outbound.send(Ok(push)).await.is_err() {
            warn!(
                event = "owner_cap.push_failed",
                client_id = %cid,
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
    Extension(identity): Extension<OperatorIdentity>,
    Path(client_id): Path<String>,
) -> Result<Json<Vec<OwnerListEntry>>, ApiError> {
    crate::operator::rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let cid = parse_client_id(&client_id)?;
    // 015-client-stable-id (T037): 404 for an unknown id rather than an
    // empty 200 that would imply the client exists. Never leaks whether
    // a colliding display name exists (Constitution V).
    resolve_client_name(&state, cid)?;
    // Build the owner -> rule_count map from the in-memory rule store.
    use std::collections::BTreeMap;
    let rules = state.rules.list(Some(&cid)).await;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for rule in rules {
        *counts.entry(rule.owner_user_id.to_string()).or_insert(0) += 1;
    }
    // Fold the cap rows so an owner with a cap but no rules still
    // appears (e.g. immediately after the last rule was removed and
    // before the GC sweep ran).
    let cap_rows = state.owner_caps.list_for_client(&cid).await;
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

/// 015-client-stable-id (T020): client-scoped owner-cap routes address
/// the client by its stable `client_id`. A malformed id is a 404 (the
/// same surface an unknown id gets — never leak whether a colliding name
/// exists, Constitution V / FR-012).
fn parse_client_id(raw: &str) -> Result<ClientId, ApiError> {
    ClientId::from_str(raw).map_err(|_| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "client_not_found",
            format!("client `{raw}` not found"),
        )
    })
}

/// Resolve a client's current display name for wire / view rendering.
/// An unknown id is a 404.
fn resolve_client_name(state: &AppState, client_id: ClientId) -> Result<String, ApiError> {
    match state.tokens.get_by_id(client_id) {
        Ok(Some(client)) => Ok(client.client_name.as_str().to_string()),
        Ok(None) => Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "client_not_found",
            format!("client `{client_id}` not found"),
        )),
        Err(e) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            e.to_string(),
        )),
    }
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

fn envelope_to_view(row: &OwnerRateLimitRow, client_name: &str) -> OwnerRateLimitView {
    OwnerRateLimitView {
        client_name: client_name.to_string(),
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
