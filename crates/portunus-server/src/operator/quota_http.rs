//! 013-traffic-quotas C1 — HTTP handlers for per-(user, client) traffic
//! quotas and historical traffic queries.
//!
//! Routes (mounted under the auth_middleware layer in `operator/http.rs`):
//!   - GET    /v1/users/{user_id}/quotas
//!   - PUT    /v1/users/{user_id}/quotas/{client_id}
//!   - PATCH  /v1/users/{user_id}/quotas/{client_id}
//!   - DELETE /v1/users/{user_id}/quotas/{client_id}
//!   - GET    /v1/users/{user_id}/quotas/{client_id}/status
//!   - GET    /v1/users/{user_id}/traffic
//!   - GET    /v1/clients/{client_id}/quotas
//!   - GET    /v1/clients/{client_id}/traffic
//!   - GET    /v1/traffic/global (superadmin-only)
//!
//! 015-client-stable-id (T016/T020): clients are addressed by their
//! stable, opaque `client_id` (ULID), never the mutable display name.
//! Accounting rows (`traffic_quotas`, `traffic_samples_*`) are keyed by
//! `client_id`; `client_name` is carried alongside for display and
//! echoed on the wire frame so a renamed client keeps its history.
//!
//! CRUD writes go through the in-memory `TrafficQuotaCache`, which
//! fans them into SQLite. PUT/PATCH/DELETE also best-effort push a
//! `TrafficQuotaUpdate{SET|REMOVE}` to the connected client; the
//! offline-client path picks the quota up via reconnect replay (C5).

use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::{TimeZone, Utc};
use portunus_auth::{ClientScope, OperatorIdentity, OperatorRole, UserId};
use portunus_core::{ClientId, ClientName};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::warn;

use crate::grpc::service::version_at_least;
use crate::operator::http::ApiError;
use crate::operator::rbac;
use crate::state::AppState;
use crate::traffic_quotas::samples::{self, SampleBucket, TrafficSample};
use crate::traffic_quotas::{TrafficQuotaRow, compute_period_end, period_start_at};

// ---------------------------------------------------------------------------
// Wire shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct QuotaView {
    pub user_id: String,
    pub client_id: String,
    pub client_name: String,
    pub monthly_bytes: i64,
    pub billing_anchor: i64,
    pub current_period_started_at: i64,
    pub current_period_ends_at: i64,
    pub current_period_bytes_used: i64,
    pub budget_remaining_bytes: i64,
    pub exhausted_at: Option<i64>,
    pub exhausted: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl QuotaView {
    fn from_row(r: TrafficQuotaRow) -> Self {
        let ends_at = compute_period_end(r.billing_anchor, r.current_period_started_at);
        let budget_remaining = r.budget_remaining();
        let exhausted = r.is_exhausted();
        Self {
            user_id: r.user_id,
            client_id: r.client_id,
            client_name: r.client_name,
            monthly_bytes: r.monthly_bytes,
            billing_anchor: r.billing_anchor,
            current_period_started_at: r.current_period_started_at,
            current_period_ends_at: ends_at,
            current_period_bytes_used: r.current_period_bytes_used,
            budget_remaining_bytes: budget_remaining,
            exhausted_at: r.exhausted_at,
            exhausted,
            created_at: r.created_at,
            updated_at: r.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PutQuotaBody {
    pub monthly_bytes: i64,
    pub billing_anchor: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct PatchQuotaBody {
    pub monthly_bytes: Option<i64>,
    pub clear_period_usage: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct TrafficQuery {
    /// Optional client filter, addressed by stable `client_id` (ULID).
    pub client_id: Option<String>,
    pub user_id: Option<String>,
    pub from: i64,
    pub to: i64,
    pub bucket: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TrafficResponse {
    pub bucket: SampleBucket,
    pub samples: Vec<TrafficSample>,
    pub total_bytes_in: i64,
    pub total_bytes_out: i64,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

pub async fn put_quota(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path((user_id, client_id)): Path<(String, String)>,
    Json(body): Json<PutQuotaBody>,
) -> Result<Json<QuotaView>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    if body.monthly_bytes < 0 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_quota_size",
            "monthly_bytes must be >= 0",
        ));
    }
    let cid = parse_client_id(&client_id)?;
    let client = resolve_client_name(&state, cid)?;
    require_grant(&state, &user_id, &client)?;
    require_client_supports_quota(&state, cid, &client).await?;

    let now = now_unix_sec();
    let anchor = body.billing_anchor.unwrap_or(now);
    let anchor_dt = Utc.timestamp_opt(anchor, 0).single().ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_billing_anchor",
            "billing_anchor out of range",
        )
    })?;
    let started = period_start_at(anchor_dt, 0).timestamp();
    let row = TrafficQuotaRow {
        user_id: user_id.clone(),
        client_id: cid.to_string(),
        client_name: client.as_str().to_string(),
        monthly_bytes: body.monthly_bytes,
        billing_anchor: anchor,
        current_period_started_at: started,
        current_period_bytes_used: 0,
        exhausted_at: None,
        created_at: now,
        updated_at: now,
    };
    let saved = state.traffic_quotas.upsert(row).map_err(map_store_error)?;
    push_quota_set(&state, cid, &saved).await;
    Ok(Json(QuotaView::from_row(saved)))
}

pub async fn patch_quota(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path((user_id, client_id)): Path<(String, String)>,
    Json(body): Json<PatchQuotaBody>,
) -> Result<Json<QuotaView>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let cid = parse_client_id(&client_id)?;
    let mut row = state
        .traffic_quotas
        .get(&user_id, &cid.to_string())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "quota_not_found",
                "no quota for (user, client)",
            )
        })?;
    let now = now_unix_sec();
    if let Some(mb) = body.monthly_bytes {
        if mb < 0 {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_quota_size",
                "monthly_bytes must be >= 0",
            ));
        }
        row.monthly_bytes = mb;
        row.updated_at = now;
        row = state.traffic_quotas.upsert(row).map_err(map_store_error)?;
    }
    if body.clear_period_usage.unwrap_or(false)
        && let Some(updated) = state
            .traffic_quotas
            .clear_period_usage(&user_id, &cid.to_string(), now)
            .map_err(map_store_error)?
    {
        row = updated;
    }
    push_quota_set(&state, cid, &row).await;
    Ok(Json(QuotaView::from_row(row)))
}

pub async fn delete_quota(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path((user_id, client_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let cid = parse_client_id(&client_id)?;
    // Resolve the current display name for the wire push; tolerate a
    // missing client (already deleted) by falling back to the id string.
    let display_name = resolve_client_name(&state, cid)
        .map_or_else(|_| cid.to_string(), |c| c.as_str().to_string());
    let removed = state
        .traffic_quotas
        .delete(&user_id, &cid.to_string())
        .map_err(map_store_error)?;
    if !removed {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "quota_not_found",
            "no quota for (user, client)",
        ));
    }
    push_quota_remove(&state, &user_id, cid, &display_name).await;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_quota_status(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path((user_id, client_id)): Path<(String, String)>,
) -> Result<Json<QuotaView>, ApiError> {
    require_self_or_superadmin(&identity, &user_id)?;
    let cid = parse_client_id(&client_id)?;
    state
        .traffic_quotas
        .get(&user_id, &cid.to_string())
        .map(|r| Json(QuotaView::from_row(r)))
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "quota_not_found",
                "no quota for (user, client)",
            )
        })
}

pub async fn list_user_quotas(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(user_id): Path<String>,
) -> Result<Json<Vec<QuotaView>>, ApiError> {
    require_self_or_superadmin(&identity, &user_id)?;
    let rows = state.traffic_quotas.list_for_user(&user_id);
    Ok(Json(rows.into_iter().map(QuotaView::from_row).collect()))
}

pub async fn list_client_quotas(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(client_id): Path<String>,
) -> Result<Json<Vec<QuotaView>>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let cid = parse_client_id(&client_id)?;
    // 015-client-stable-id (T037): a client-scoped route 404s for an
    // unknown id rather than returning an empty 200 (which would imply
    // the client exists with no quotas). Never leaks name collisions.
    resolve_client_name(&state, cid)?;
    let rows = state.traffic_quotas.list_for_client(&cid.to_string());
    Ok(Json(rows.into_iter().map(QuotaView::from_row).collect()))
}

pub async fn get_user_traffic(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(user_id): Path<String>,
    Query(q): Query<TrafficQuery>,
) -> Result<Json<TrafficResponse>, ApiError> {
    require_self_or_superadmin(&identity, &user_id)?;
    // An optional client_id filter must be a well-formed ULID.
    let client_id = match q.client_id.as_deref() {
        Some(raw) => Some(parse_client_id(raw)?.to_string()),
        None => None,
    };
    serve_traffic(
        &state,
        client_id.as_deref(),
        Some(&user_id),
        q.from,
        q.to,
        q.bucket.as_deref(),
    )
}

pub async fn get_client_traffic(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(client_id): Path<String>,
    Query(q): Query<TrafficQuery>,
) -> Result<Json<TrafficResponse>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let cid = parse_client_id(&client_id)?;
    serve_traffic(
        &state,
        Some(&cid.to_string()),
        q.user_id.as_deref(),
        q.from,
        q.to,
        q.bucket.as_deref(),
    )
}

/// `GET /v1/traffic/global?from=&to=&bucket=` — superadmin-only.
/// Returns bucketed traffic aggregated across **all** users and clients,
/// reusing the same wire shape `/v1/users/{id}/traffic` returns.
pub async fn get_global_traffic(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Query(q): Query<TrafficQuery>,
) -> Result<Json<TrafficResponse>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    serve_traffic(&state, None, None, q.from, q.to, q.bucket.as_deref())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn serve_traffic(
    state: &AppState,
    client_id: Option<&str>,
    user_id: Option<&str>,
    from: i64,
    to: i64,
    bucket: Option<&str>,
) -> Result<Json<TrafficResponse>, ApiError> {
    if from < 0 || to <= from {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_time_range",
            "from must be >= 0 and to > from",
        ));
    }
    let span = to - from;
    let chosen = match bucket {
        Some("1m") => SampleBucket::M1,
        Some("1h") => SampleBucket::H1,
        Some(other) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_bucket",
                format!("bucket must be 1m or 1h, got {other}"),
            ));
        }
        None => {
            if span <= 24 * 3600 {
                SampleBucket::M1
            } else {
                SampleBucket::H1
            }
        }
    };
    let now = now_unix_sec();
    let oldest_allowed = now - chosen.retention_seconds();
    if from < oldest_allowed {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "traffic_bucket_out_of_retention",
            format!(
                "from is older than {} retention ({} sec)",
                match chosen {
                    SampleBucket::M1 => "1m",
                    SampleBucket::H1 => "1h",
                },
                chosen.retention_seconds()
            ),
        ));
    }
    let rows = samples::query_samples(&state.store, chosen, user_id, client_id, from, to).map_err(
        |e| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_error",
                e.to_string(),
            )
        },
    )?;
    if rows.len() > 10_000 {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "traffic_too_many_rows",
            "narrow the time range or switch to the 1h bucket",
        ));
    }
    let total_bytes_in = rows.iter().map(|r| r.bytes_in).sum();
    let total_bytes_out = rows.iter().map(|r| r.bytes_out).sum();
    Ok(Json(TrafficResponse {
        bucket: chosen,
        samples: rows,
        total_bytes_in,
        total_bytes_out,
    }))
}

/// 403 unless the caller is superadmin or the user under the path.
/// User-scoped reads (status / list / traffic) accept both roles; CRUD
/// remains superadmin-only and uses `rbac::require_role` directly.
fn require_self_or_superadmin(identity: &OperatorIdentity, user_id: &str) -> Result<(), ApiError> {
    if identity.role == OperatorRole::Superadmin {
        return Ok(());
    }
    if identity.user_id.as_str() == user_id {
        return Ok(());
    }
    Err(ApiError::new(
        StatusCode::FORBIDDEN,
        "not_owner",
        "callers may only read their own quotas",
    ))
}

/// 015-client-stable-id (T020): client-scoped quota routes address the
/// client by its stable `client_id`. A malformed id is a 404 (the same
/// surface an unknown id gets — never leak whether a colliding name
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

/// Resolve a client's current display name (as a validated `ClientName`)
/// for grant / capability checks and wire rendering. An unknown id is a
/// 404.
fn resolve_client_name(state: &AppState, client_id: ClientId) -> Result<ClientName, ApiError> {
    match state.tokens.get_by_id(client_id) {
        Ok(Some(client)) => Ok(client.client_name),
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

/// 422 quota_target_not_found if no grant matches (user, client). A
/// wildcard grant (`ClientScope::Any`) accepts any client.
fn require_grant(state: &AppState, user_id: &str, client: &ClientName) -> Result<(), ApiError> {
    let parsed_user = match UserId::from_str(user_id) {
        Ok(u) => u,
        Err(_) => UserId::reserved(user_id.to_string()),
    };
    let grants = state.operator_store.list_grants(Some(&parsed_user));
    let any_match = grants.iter().any(|g| match &g.client {
        ClientScope::Any => true,
        ClientScope::Named(c) => c.as_str() == client.as_str(),
    });
    if any_match {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "quota_target_not_found",
            format!("no grant for user={user_id} client={client}"),
        ))
    }
}

/// 422 quota_unsupported_by_client when the target client's reported
/// version is below 1.4.0 (or it is offline / never connected).
async fn require_client_supports_quota(
    state: &AppState,
    client_id: ClientId,
    client: &ClientName,
) -> Result<(), ApiError> {
    let version = state.clients.client_version_of(&client_id).await;
    if version_at_least(version.as_deref(), 1, 4) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "quota_unsupported_by_client",
            format!(
                "target client {} client_version {} < 1.4.0",
                client,
                version.as_deref().unwrap_or("unknown")
            ),
        ))
    }
}

async fn push_quota_set(state: &AppState, client_id: ClientId, row: &TrafficQuotaRow) {
    let Some((outbound, _waiters)) = state.clients.handles(&client_id).await else {
        return;
    };
    let msg = crate::traffic_quotas::make_traffic_quota_set_msg(
        row,
        format!("quota-{}", ulid::Ulid::new()),
    );
    if outbound.send(Ok(msg)).await.is_err() {
        warn!(
            event = "traffic_quota.push_failed",
            user_id = %row.user_id,
            client_id = %client_id,
            action = "set",
        );
    }
}

async fn push_quota_remove(
    state: &AppState,
    user_id: &str,
    client_id: ClientId,
    client_name: &str,
) {
    let Some((outbound, _waiters)) = state.clients.handles(&client_id).await else {
        return;
    };
    let msg = crate::traffic_quotas::make_traffic_quota_remove_msg(
        user_id.to_string(),
        client_id.to_string(),
        client_name.to_string(),
        format!("quota-{}", ulid::Ulid::new()),
    );
    if outbound.send(Ok(msg)).await.is_err() {
        warn!(
            event = "traffic_quota.push_failed",
            user_id,
            client_id = %client_id,
            action = "remove",
        );
    }
}

fn map_store_error(e: crate::store::StoreError) -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store_error",
        e.to_string(),
    )
}

fn now_unix_sec() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traffic_quotas::cache::TrafficQuotaCache;
    use crate::traffic_quotas::store as quota_store;
    use axum::response::IntoResponse;
    use tempfile::tempdir;

    fn sample_row(monthly: i64, used: i64) -> TrafficQuotaRow {
        TrafficQuotaRow {
            user_id: "alice".into(),
            client_id: "edge-01".into(),
            client_name: "edge-01".into(),
            monthly_bytes: monthly,
            billing_anchor: 0,
            current_period_started_at: 0,
            current_period_bytes_used: used,
            exhausted_at: if used >= monthly { Some(10) } else { None },
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn quota_view_carries_budget_and_exhausted_flags() {
        let r = sample_row(1_000, 1_200);
        let v = QuotaView::from_row(r);
        assert_eq!(v.monthly_bytes, 1_000);
        assert_eq!(v.current_period_bytes_used, 1_200);
        assert_eq!(v.budget_remaining_bytes, -200);
        assert!(v.exhausted);
        assert_eq!(v.exhausted_at, Some(10));
    }

    #[test]
    fn quota_view_period_ends_at_uses_anchor_math() {
        // anchor = 2026-01-15 00:00:00 UTC -> period 0 ends 2026-02-15.
        let anchor = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap();
        let started = anchor.timestamp();
        let r = TrafficQuotaRow {
            user_id: "alice".into(),
            client_id: "edge-01".into(),
            client_name: "edge-01".into(),
            monthly_bytes: 1_000,
            billing_anchor: anchor.timestamp(),
            current_period_started_at: started,
            current_period_bytes_used: 0,
            exhausted_at: None,
            created_at: 0,
            updated_at: 0,
        };
        let v = QuotaView::from_row(r);
        let expected_end = Utc
            .with_ymd_and_hms(2026, 2, 15, 0, 0, 0)
            .unwrap()
            .timestamp();
        assert_eq!(v.current_period_ends_at, expected_end);
    }

    #[test]
    fn cache_crud_via_helpers_roundtrips() {
        let dir = tempdir().unwrap();
        let store = crate::store::Store::open(dir.path()).expect("open store");
        let cache = TrafficQuotaCache::load(store.clone()).expect("load cache");
        let r = sample_row(1_000, 0);
        cache.upsert(r.clone()).unwrap();
        let got = cache.get("alice", "edge-01").unwrap();
        assert_eq!(got.monthly_bytes, 1_000);

        // store-level row also present.
        let store_row = quota_store::get(&store, "alice", "edge-01")
            .unwrap()
            .unwrap();
        assert_eq!(store_row.monthly_bytes, 1_000);

        // delete clears both layers.
        assert!(cache.delete("alice", "edge-01").unwrap());
        assert!(cache.get("alice", "edge-01").is_none());
        assert!(
            quota_store::get(&store, "alice", "edge-01")
                .unwrap()
                .is_none()
        );
    }

    fn user_identity(uid: &str) -> OperatorIdentity {
        OperatorIdentity {
            user_id: UserId::from_str(uid).unwrap(),
            role: OperatorRole::User,
        }
    }

    fn superadmin_identity() -> OperatorIdentity {
        OperatorIdentity {
            user_id: UserId::superadmin(),
            role: OperatorRole::Superadmin,
        }
    }

    #[test]
    fn require_self_or_superadmin_allows_superadmin() {
        assert!(require_self_or_superadmin(&superadmin_identity(), "alice").is_ok());
    }

    #[test]
    fn require_self_or_superadmin_allows_caller_for_own_user() {
        assert!(require_self_or_superadmin(&user_identity("alice"), "alice").is_ok());
    }

    #[test]
    fn require_self_or_superadmin_rejects_other_users_403() {
        let err = require_self_or_superadmin(&user_identity("alice"), "bob").unwrap_err();
        // ApiError doesn't expose status publicly; render and inspect.
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn parse_client_id_rejects_malformed_with_404() {
        let err = parse_client_id("not-a-ulid").unwrap_err();
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn compute_period_end_falls_back_to_imax_when_unreachable() {
        // Billing anchor that won't match any period_start chain in <12000 steps:
        // pass a started timestamp far away from any anchor period_start.
        let weird = compute_period_end(0, 999_999_999_999);
        assert_eq!(weird, i64::MAX);
    }

    #[test]
    fn require_role_rejects_tenant_for_global_traffic() {
        let id = user_identity("alice");
        let err = rbac::require_role(&id, OperatorRole::Superadmin).unwrap_err();
        let api_err: ApiError = err.into();
        let resp = api_err.into_response();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn serve_traffic_with_no_filters_sums_across_users() {
        let dir = tempdir().unwrap();
        let store = crate::store::Store::open(dir.path()).expect("open store");
        let ts = 1_700_000_000_i64 - (1_700_000_000_i64 % 60);
        samples::upsert_1m_delta(&store, "alice", "edge-a", "edge-a", ts, 100, 200).unwrap();
        samples::upsert_1m_delta(&store, "bob", "edge-b", "edge-b", ts, 300, 400).unwrap();
        let rows =
            samples::query_samples(&store, SampleBucket::M1, None, None, ts - 1, ts + 60).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts, ts);
        assert_eq!(rows[0].bytes_in, 400);
        assert_eq!(rows[0].bytes_out, 600);
    }
}
