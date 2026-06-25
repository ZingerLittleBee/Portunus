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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::ConnectedClients;
    use crate::store::Store;
    use crate::store::error::StoreError;
    use crate::store::operator_store::SqliteOperatorStore;
    use crate::store::token_store::SqliteTokenStore;
    use axum::response::IntoResponse;
    use portunus_core::{ClientName, RateLimit};
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    // ----------------------------------------------------------------
    // Fixtures — mirror the AppState builder used by the gRPC enrollment
    // tests so the handlers can run in-process against a temp SQLite db.
    // ----------------------------------------------------------------

    /// Build a fully-wired `AppState` over a fresh temp SQLite store. The
    /// `TempDir` is leaked so the db survives for the test's lifetime
    /// (matches the owner-cap service tests' `mem::forget` idiom).
    fn test_state() -> Arc<AppState> {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&store)));
        let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&store)));
        operator_store
            .bootstrap_legacy_superadmin("test-token")
            .unwrap();
        std::mem::forget(dir);
        Arc::new(
            AppState::new(
                tokens,
                operator_store,
                ConnectedClients::default(),
                None,
                7443,
                "deadbeef",
                include_str!("../advertised/testdata/san_fixture.pem"),
                16,
                store,
            )
            .unwrap(),
        )
    }

    /// Issue a client into `client_tokens` and return its stable id so
    /// `resolve_client_name` (via `tokens.get_by_id`) resolves.
    fn issue_client(state: &AppState, name: &str) -> ClientId {
        let cn = ClientName::new(name).unwrap();
        state.tokens.issue_with_address(cn, None).unwrap();
        // The id is minted internally; recover it from the roster.
        state
            .tokens
            .list()
            .unwrap()
            .into_iter()
            .find(|c| c.client_name.as_str() == name)
            .unwrap()
            .client_id
    }

    fn superadmin() -> OperatorIdentity {
        OperatorIdentity {
            user_id: portunus_auth::UserId::superadmin(),
            role: OperatorRole::Superadmin,
        }
    }

    fn tenant() -> OperatorIdentity {
        OperatorIdentity {
            user_id: portunus_auth::UserId::from_str("alice").unwrap(),
            role: OperatorRole::User,
        }
    }

    fn full_envelope() -> RateLimit {
        RateLimit {
            bandwidth_in_bps: Some(1_048_576),
            bandwidth_out_bps: Some(2_097_152),
            new_connections_per_sec: Some(50),
            concurrent_connections: Some(10),
            bandwidth_in_burst: None,
            bandwidth_out_burst: None,
            new_connections_burst: None,
        }
    }

    fn put_body() -> OwnerRateLimitPutBody {
        OwnerRateLimitPutBody {
            bandwidth_in_bps: Some(1_048_576),
            bandwidth_out_bps: None,
            new_connections_per_sec: None,
            concurrent_connections: None,
            bandwidth_in_burst: None,
            bandwidth_out_burst: None,
            new_connections_burst: None,
            concurrent_connections_burst: None,
        }
    }

    /// Register a connected client carrying `version`, returning the
    /// receiver so the test controls the push channel's liveness.
    async fn register_connected(
        state: &AppState,
        cid: ClientId,
        name: &str,
        version: &str,
    ) -> mpsc::Receiver<Result<proto::ServerMessage, tonic::Status>> {
        let (tx, rx) = mpsc::channel(4);
        let session = state
            .clients
            .register(
                cid,
                ClientName::new(name).unwrap(),
                None,
                CancellationToken::new(),
                tx,
                std::sync::Arc::default(),
            )
            .await;
        state
            .clients
            .set_client_version(&cid, session, version.to_string())
            .await;
        rx
    }

    // ----------------------------------------------------------------
    // GET handler
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn get_owner_rate_limit_returns_envelope_for_existing_cap() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        state
            .owner_caps
            .upsert(&cid, "alice", full_envelope())
            .await
            .unwrap();
        let resp = get_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((cid.to_string(), "alice".to_string())),
        )
        .await
        .expect("ok");
        let view = resp.0;
        assert_eq!(view.client_name, "edge-01");
        assert_eq!(view.owner_id, "alice");
        assert_eq!(view.rate_limit.bandwidth_in_bps, Some(1_048_576));
    }

    #[tokio::test]
    async fn get_owner_rate_limit_404_when_no_envelope() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        let err = get_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((cid.to_string(), "ghost".to_string())),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_owner_rate_limit_rejects_tenant_403() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        let err = get_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(tenant()),
            Path((cid.to_string(), "alice".to_string())),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_owner_rate_limit_404_for_unknown_client() {
        let state = test_state();
        let unknown = ClientId::new();
        let err = get_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((unknown.to_string(), "alice".to_string())),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::NOT_FOUND);
    }

    // ----------------------------------------------------------------
    // PUT handler
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn put_owner_rate_limit_persists_and_pushes_to_connected_client() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        // Keep the rx alive so the best-effort push succeeds (covers the
        // wire-push branch through `Ok(Json(..))`).
        let mut rx = register_connected(&state, cid, "edge-01", "0.11.0").await;
        let resp = put_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((cid.to_string(), "alice".to_string())),
            Json(put_body()),
        )
        .await
        .expect("ok");
        let view = resp.0;
        assert_eq!(view.client_name, "edge-01");
        assert_eq!(view.rate_limit.bandwidth_in_bps, Some(1_048_576));
        // The envelope is durable.
        assert!(state.owner_caps.get(&cid, "alice").await.is_some());
        // A SET push was delivered to the connected client.
        let msg = rx.try_recv().expect("push delivered");
        let pushed = msg.expect("ok server message");
        match pushed.payload {
            Some(proto::server_message::Payload::OwnerRateLimitUpdate(u)) => {
                assert_eq!(u.owner_id, "alice");
                assert_eq!(u.client_id, cid.to_string());
                assert_eq!(u.action, proto::OwnerRateLimitAction::Set as i32);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[tokio::test]
    async fn put_owner_rate_limit_rejects_reserved_concurrent_burst_400() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        let _rx = register_connected(&state, cid, "edge-01", "0.11.0").await;
        let mut body = put_body();
        body.concurrent_connections_burst = Some(5);
        let err = put_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((cid.to_string(), "alice".to_string())),
            Json(body),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn put_owner_rate_limit_gates_disconnected_client_422() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        // No connected session -> client_version_of is None -> 422.
        let err = put_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((cid.to_string(), "alice".to_string())),
            Json(put_body()),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.into_response().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn put_owner_rate_limit_gates_legacy_version_422() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        let _rx = register_connected(&state, cid, "edge-01", "0.10.0").await;
        let err = put_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((cid.to_string(), "alice".to_string())),
            Json(put_body()),
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.into_response().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[tokio::test]
    async fn put_owner_rate_limit_invalid_envelope_400() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        let _rx = register_connected(&state, cid, "edge-01", "0.11.0").await;
        // bandwidth_in_bps == 0 fails `validate` -> map_cap_error -> 400.
        let mut body = put_body();
        body.bandwidth_in_bps = Some(0);
        let err = put_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((cid.to_string(), "alice".to_string())),
            Json(body),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn put_owner_rate_limit_rejects_tenant_403() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        let err = put_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(tenant()),
            Path((cid.to_string(), "alice".to_string())),
            Json(put_body()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::FORBIDDEN);
    }

    // ----------------------------------------------------------------
    // DELETE handler
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn delete_owner_rate_limit_removes_and_pushes_remove() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        state
            .owner_caps
            .upsert(&cid, "alice", full_envelope())
            .await
            .unwrap();
        let mut rx = register_connected(&state, cid, "edge-01", "0.11.0").await;
        let status = delete_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((cid.to_string(), "alice".to_string())),
        )
        .await
        .expect("ok");
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(state.owner_caps.get(&cid, "alice").await.is_none());
        let msg = rx.try_recv().expect("push delivered");
        let pushed = msg.expect("ok server message");
        match pushed.payload {
            Some(proto::server_message::Payload::OwnerRateLimitUpdate(u)) => {
                assert_eq!(u.action, proto::OwnerRateLimitAction::Remove as i32);
                assert!(u.rate_limit.is_none());
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[tokio::test]
    async fn delete_owner_rate_limit_idempotent_no_content_when_absent() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        // No connected session and no cap row: still 204 (idempotent),
        // and `resolve_client_name` resolves the live client.
        let status = delete_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((cid.to_string(), "ghost".to_string())),
        )
        .await
        .expect("ok");
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_owner_rate_limit_falls_back_to_id_when_client_gone() {
        let state = test_state();
        // A client that was never issued: `resolve_client_name` fails and
        // the handler falls back to the id string for the wire push.
        let unknown = ClientId::new();
        let status = delete_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((unknown.to_string(), "alice".to_string())),
        )
        .await
        .expect("ok");
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn delete_owner_rate_limit_warns_on_failed_push_but_succeeds() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        state
            .owner_caps
            .upsert(&cid, "alice", full_envelope())
            .await
            .unwrap();
        // Register then drop the receiver so the best-effort push errors,
        // exercising the warn-on-send-failure branch.
        let rx = register_connected(&state, cid, "edge-01", "0.11.0").await;
        drop(rx);
        let status = delete_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path((cid.to_string(), "alice".to_string())),
        )
        .await
        .expect("ok");
        assert_eq!(status, StatusCode::NO_CONTENT);
        assert!(state.owner_caps.get(&cid, "alice").await.is_none());
    }

    #[tokio::test]
    async fn delete_owner_rate_limit_rejects_tenant_403() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        let err = delete_owner_rate_limit(
            State(Arc::clone(&state)),
            Extension(tenant()),
            Path((cid.to_string(), "alice".to_string())),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::FORBIDDEN);
    }

    // ----------------------------------------------------------------
    // owners listing
    // ----------------------------------------------------------------

    #[tokio::test]
    async fn get_owners_under_client_folds_rules_and_caps_sorted() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        let cn = ClientName::new("edge-01").unwrap();
        // Superadmin owns the rule (push helper stamps superadmin).
        state
            .rules
            .push(
                cid,
                cn,
                18080,
                "127.0.0.1".to_string(),
                9090,
                portunus_core::Protocol::Tcp,
                None,
            )
            .await
            .unwrap();
        // A cap row for an owner with no rules surfaces too.
        state
            .owner_caps
            .upsert(&cid, "zeta", full_envelope())
            .await
            .unwrap();
        let resp = get_owners_under_client(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path(cid.to_string()),
        )
        .await
        .expect("ok");
        let entries = resp.0;
        assert_eq!(entries.len(), 2);
        // Alphabetical by owner_id: superadmin id sorts before "zeta".
        assert!(entries[0].owner_id < entries[1].owner_id);
        // The rule-bearing owner has rule_count == 1 and no cap.
        let rule_owner = entries.iter().find(|e| e.rule_count == 1).unwrap();
        assert!(!rule_owner.has_rate_limit);
        // The cap-only owner has a cap and zero rules.
        let cap_owner = entries.iter().find(|e| e.owner_id == "zeta").unwrap();
        assert!(cap_owner.has_rate_limit);
        assert_eq!(cap_owner.rule_count, 0);
    }

    #[tokio::test]
    async fn get_owners_under_client_404_for_unknown_id() {
        let state = test_state();
        let unknown = ClientId::new();
        let err = get_owners_under_client(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path(unknown.to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_owners_under_client_404_for_malformed_id() {
        let state = test_state();
        let err = get_owners_under_client(
            State(Arc::clone(&state)),
            Extension(superadmin()),
            Path("not-a-ulid".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_owners_under_client_rejects_tenant_403() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        let err = get_owners_under_client(
            State(Arc::clone(&state)),
            Extension(tenant()),
            Path(cid.to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::FORBIDDEN);
    }

    // ----------------------------------------------------------------
    // pure helpers
    // ----------------------------------------------------------------

    #[test]
    fn parse_client_id_rejects_malformed_with_404() {
        let err = parse_client_id("not-a-ulid").unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn parse_client_id_accepts_valid_ulid() {
        let id = ClientId::new();
        let parsed = parse_client_id(&id.to_string()).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn resolve_client_name_404_for_unknown_id() {
        let state = test_state();
        let err = resolve_client_name(&state, ClientId::new()).unwrap_err();
        assert_eq!(err.into_response().status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn resolve_client_name_returns_display_name() {
        let state = test_state();
        let cid = issue_client(&state, "edge-01");
        let name = resolve_client_name(&state, cid).unwrap();
        assert_eq!(name, "edge-01");
    }

    #[test]
    fn map_cap_error_cap_zero_is_400() {
        let inner = portunus_core::rate_limit::RateLimitError::CapZero {
            field: "bandwidth_in_bps",
        };
        let err = map_cap_error(OwnerCapError::InvalidEnvelope(inner));
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn map_cap_error_burst_without_rate_is_400() {
        let inner = portunus_core::rate_limit::RateLimitError::BurstWithoutRate {
            field: "bandwidth_in_burst",
        };
        let err = map_cap_error(OwnerCapError::InvalidEnvelope(inner));
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn map_cap_error_burst_range_is_400() {
        let inner = portunus_core::rate_limit::RateLimitError::BurstRange {
            field: "bandwidth_in_burst",
            rate: 1_000,
            burst: 999_999,
            lo: 10,
            hi: 60_000,
        };
        let err = map_cap_error(OwnerCapError::InvalidEnvelope(inner));
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn map_cap_error_unsupported_by_client_is_422() {
        let err = map_cap_error(OwnerCapError::UnsupportedByClient);
        assert_eq!(
            err.into_response().status(),
            StatusCode::UNPROCESSABLE_ENTITY
        );
    }

    #[test]
    fn map_cap_error_store_is_500() {
        let err = map_cap_error(OwnerCapError::Store(StoreError::Internal {
            message: "boom".into(),
        }));
        assert_eq!(
            err.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn rate_limit_to_proto_round_trips_fields() {
        let rl = full_envelope();
        let p = rate_limit_to_proto(&rl);
        assert_eq!(p.bandwidth_in_bps, rl.bandwidth_in_bps);
        assert_eq!(p.bandwidth_out_bps, rl.bandwidth_out_bps);
        assert_eq!(p.new_connections_per_sec, rl.new_connections_per_sec);
        assert_eq!(p.concurrent_connections, rl.concurrent_connections);
        assert_eq!(p.bandwidth_in_burst, rl.bandwidth_in_burst);
        assert_eq!(p.bandwidth_out_burst, rl.bandwidth_out_burst);
        assert_eq!(p.new_connections_burst, rl.new_connections_burst);
    }

    #[test]
    fn envelope_to_view_copies_all_fields() {
        let row = OwnerRateLimitRow {
            client_id: ClientId::new(),
            owner_id: "alice".to_string(),
            rate_limit: full_envelope(),
            updated_at_unix_ms: 42,
        };
        let view = envelope_to_view(&row, "display");
        assert_eq!(view.client_name, "display");
        assert_eq!(view.owner_id, "alice");
        assert_eq!(view.updated_at_unix_ms, 42);
        assert_eq!(view.rate_limit.bandwidth_in_bps, Some(1_048_576));
        assert_eq!(view.rate_limit.concurrent_connections, Some(10));
    }
}
