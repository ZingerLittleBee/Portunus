//! T036 (005-multi-user-rbac, US2) — grant management HTTP handlers.
//!
//! Superadmin-only (FR-007). The cascade in `delete_grant` re-evaluates
//! every rule owned by the grant's user against the remaining grants;
//! rules that no longer pass `enforce_push` are removed before the HTTP
//! response returns (synchronous per R-006 ordering: identity flush
//! commits FIRST, then rule cascade).

use std::str::FromStr;
use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::Utc;
use portunus_auth::IdentityStoreError;
use portunus_auth::{
    ClientScope, Grant, GrantId, OperatorAuthenticator, OperatorIdentity, OperatorRole,
    ProtocolSet, RbacError, UserId,
};
use portunus_core::ClientName;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::operator::cli::OperatorError;
use crate::operator::http::ApiError;
use crate::operator::rbac;
use crate::operator::user_ids::parse_stored_user_id;
use crate::state::AppState;

fn api_rbac(e: RbacError) -> ApiError {
    ApiError::from(OperatorError::Rbac(e))
}

fn api_store(e: IdentityStoreError) -> ApiError {
    if let Some(rbac) = e.as_rbac() {
        return api_rbac(rbac);
    }
    let (status, code) = match &e {
        IdentityStoreError::InvalidPortRange { .. } => {
            (StatusCode::UNPROCESSABLE_ENTITY, "invalid_port_range")
        }
        IdentityStoreError::HashCollision => (StatusCode::CONFLICT, "hash_collision"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    };
    ApiError::new(status, code, e.to_string())
}

#[derive(Debug, Deserialize)]
pub struct CreateGrantBody {
    pub user_id: String,
    /// `"client-a"` or `"*"` (wildcard).
    pub client: String,
    pub listen_port_start: u16,
    pub listen_port_end: u16,
    pub protocols: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct GrantView {
    pub grant_id: String,
    pub user_id: String,
    pub client: String,
    pub listen_port_start: u16,
    pub listen_port_end: u16,
    pub protocols: Vec<String>,
    pub note: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<Grant> for GrantView {
    fn from(g: Grant) -> Self {
        let client = match &g.client {
            ClientScope::Any => "*".to_string(),
            ClientScope::Named(n) => n.as_str().to_string(),
        };
        let mut protocols = Vec::with_capacity(2);
        if g.protocols.contains(ProtocolSet::TCP) {
            protocols.push("tcp".to_string());
        }
        if g.protocols.contains(ProtocolSet::UDP) {
            protocols.push("udp".to_string());
        }
        Self {
            grant_id: g.id.to_string(),
            user_id: g.user_id.as_str().to_string(),
            client,
            listen_port_start: g.listen_port_start,
            listen_port_end: g.listen_port_end,
            protocols,
            note: g.note,
            created_at: g.created_at,
        }
    }
}

pub async fn post_grants(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Json(body): Json<CreateGrantBody>,
) -> Result<(StatusCode, Json<GrantView>), ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let user_id = UserId::from_str(&body.user_id).map_err(api_rbac)?;

    let client = if body.client == "*" {
        ClientScope::Any
    } else {
        ClientScope::Named(
            ClientName::new(body.client).map_err(|_| api_rbac(RbacError::InvalidClient))?,
        )
    };

    if body.listen_port_start == 0 || body.listen_port_start > body.listen_port_end {
        return Err(api_rbac(RbacError::InvalidPortRange));
    }

    let mut bits = ProtocolSet::empty();
    for p in &body.protocols {
        match p.as_str() {
            "tcp" => bits |= ProtocolSet::TCP,
            "udp" => bits |= ProtocolSet::UDP,
            _ => return Err(api_rbac(RbacError::EmptyProtocolSet)),
        }
    }
    let protocols = ProtocolSet::non_empty(bits).map_err(api_rbac)?;

    let grant = Grant {
        id: GrantId::new(),
        user_id: user_id.clone(),
        client,
        listen_port_start: body.listen_port_start,
        listen_port_end: body.listen_port_end,
        protocols,
        note: body.note,
        created_at: Utc::now(),
    };
    state
        .operator_store
        .add_grant(grant.clone())
        .map_err(api_store)?;

    info!(
        event = "operator.grant_added",
        actor = %identity.user_id,
        target = %user_id,
        grant_id = %grant.id,
        outcome = "ok",
    );

    Ok((StatusCode::CREATED, Json(GrantView::from(grant))))
}

#[derive(Debug, Deserialize, Default)]
pub struct GrantsQuery {
    #[serde(default)]
    pub user_id: Option<String>,
}

pub async fn get_grants(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Query(q): Query<GrantsQuery>,
) -> Result<Json<Vec<GrantView>>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    // Accept reserved IDs (e.g. `_superadmin`) as a filter so the operator
    // UI can list grants for the bootstrap superadmin without a 400; mirrors
    // the `parse_stored_user_id` handling in the users/credentials handlers.
    let filter = match q.user_id.as_deref() {
        Some(s) => Some(parse_stored_user_id(s).map_err(api_rbac)?),
        None => None,
    };
    let grants = state.operator_store.list_grants(filter.as_ref());
    Ok(Json(grants.into_iter().map(GrantView::from).collect()))
}

#[derive(Debug, Serialize)]
pub struct DeleteGrantResponse {
    pub grant_id: String,
    pub removed_rule_ids: Vec<u64>,
}

pub async fn delete_grant(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(grant_id): Path<String>,
) -> Result<Json<DeleteGrantResponse>, ApiError> {
    rbac::require_role(&identity, OperatorRole::Superadmin)?;
    let gid = ulid::Ulid::from_string(&grant_id)
        .map(GrantId)
        .map_err(|_| api_rbac(RbacError::GrantNotFound))?;

    let grant = state.operator_store.revoke_grant(&gid).map_err(api_store)?;

    // Cascade: re-evaluate every rule owned by `grant.user_id` against
    // the user's REMAINING grants. Any rule that no longer passes
    // enforce_push is removed.
    let owner = grant.user_id.clone();
    let remaining = state.operator_store.grants_for(&owner);
    // We need an OperatorIdentity for enforce_push. Build a stand-in
    // matching the grant's owner with role User (Superadmin would
    // bypass the check).
    let synthetic = OperatorIdentity {
        user_id: owner.clone(),
        role: OperatorRole::User,
    };
    let owned_rules = state.rules.list_owned_by(&owner).await;
    let mut removed = Vec::new();
    for rule in owned_rules {
        let push_req = rbac::PushRequest {
            client: &rule.client_name,
            listen_port_start: rule.listen_port,
            listen_port_end: rule.listen_port_end.unwrap_or(rule.listen_port),
            protocol: match rule.protocol {
                crate::rules::Protocol::Tcp => rbac::PushProtocol::Tcp,
                crate::rules::Protocol::Udp => rbac::PushProtocol::Udp,
            },
        };
        if rbac::enforce_push(&synthetic, &push_req, &remaining).is_err() {
            // Best-effort remove; skip on NotFound (concurrent removal).
            let _ = state.rules.remove(rule.id).await;
            let _ = state.rule_store.delete_rule(rule.id);
            removed.push(rule.id.0);
        }
    }

    info!(
        event = "operator.grant_revoked",
        actor = %identity.user_id,
        grant_id = %gid,
        target = %owner,
        cascaded_rules = removed.len(),
        outcome = "ok",
    );

    Ok(Json(DeleteGrantResponse {
        grant_id,
        removed_rule_ids: removed,
    }))
}
