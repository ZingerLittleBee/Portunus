//! 006-management-web-ui T025: `GET /v1/rules/{rule_id}/stats/stream`
//! — text/event-stream of `RuleStatsSnapshot` events.
//!
//! Contract: `specs/006-management-web-ui/contracts/stats-stream-endpoint.md`.
//!
//! - Ownership check at connect time (same rule as the non-streaming
//!   `GET /v1/rules/{id}/stats`): superadmin always allowed; otherwise
//!   `identity.user_id` must equal `rule.owner_user_id`.
//! - First event (if any) is the cache's current snapshot, so a fresh
//!   subscriber doesn't have to wait one full `StatsReport` tick.
//! - Subsequent events are fan-out from `RuleStatsCache` via a per-rule
//!   `tokio::sync::broadcast` (see `metrics.rs` T011).
//! - Keepalive comment every 30 s so middleboxes don't drop the
//!   connection. Browsers ignore comment frames.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension,
    extract::{Path, State},
    response::sse::{Event, KeepAlive, Sse},
};
use forward_auth::OperatorIdentity;
use forward_core::RuleId;
use futures::stream::{self, Stream, StreamExt};
use tokio_stream::wrappers::BroadcastStream;
use tracing::warn;

use crate::metrics::RuleStatsSnapshot;
use crate::operator::http::ApiError;
use crate::operator::rbac;
use crate::state::AppState;

/// Subscribe to live snapshots for a rule. Auth at connect time;
/// stream closes when the rule is removed (broadcast sender dropped).
pub async fn get_rule_stats_stream(
    State(state): State<Arc<AppState>>,
    Extension(identity): Extension<OperatorIdentity>,
    Path(rule_id): Path<u64>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let rule = state
        .rules
        .get(RuleId(rule_id))
        .await
        .ok_or_else(|| ApiError::from(crate::operator::cli::OperatorError::RuleNotFound))?;
    rbac::enforce_read(&identity, &rule.owner_user_id)?;

    let initial_snapshot = state.stats_cache.get(RuleId(rule_id)).await;
    let receiver = state.stats_cache.subscribe(RuleId(rule_id)).await;

    let initial_stream = stream::iter(initial_snapshot.into_iter().map(Ok::<_, Infallible>));
    let live_stream = BroadcastStream::new(receiver).filter_map(|res| async move {
        match res {
            Ok(snap) => Some(Ok::<_, Infallible>(snap)),
            Err(err) => {
                // Slow consumer (Lagged) → log and continue. Sender
                // closed → end the stream naturally.
                warn!(
                    event = "stats_stream.lagged",
                    error = ?err,
                    "subscriber dropped one or more snapshots"
                );
                None
            }
        }
    });

    let combined = initial_stream.chain(live_stream).map(snapshot_to_event);

    Ok(Sse::new(combined).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(30))
            .text("keepalive"),
    ))
}

fn snapshot_to_event(item: Result<RuleStatsSnapshot, Infallible>) -> Result<Event, Infallible> {
    item.map(|snap| {
        Event::default()
            .event("stats")
            .json_data(snap)
            .unwrap_or_else(|_| Event::default().data("{}"))
    })
}
