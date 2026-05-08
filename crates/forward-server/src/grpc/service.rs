//! `Control` service implementation.
//!
//! For US1 the `Channel` rpc handles handshake (await `Hello`, send
//! `Welcome`) and registers the client in [`crate::clients`]. Rule push
//! and stats handling land in US2/US3.

use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use forward_auth::ClientIdentity;
use forward_proto::v1::{
    ClientMessage, Protocol, ServerMessage, Welcome, control_server::Control, server_message,
};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};
use tracing::{info, warn};

use crate::clients::StatusWaiters;
use crate::state::AppState;

/// Channel from the operator-side push path into the per-client send-half.
/// Used by US2 to push `RuleUpdates` from the operator HTTP API to a live session.
#[allow(dead_code)]
pub type OutboundSender = mpsc::Sender<Result<ServerMessage, Status>>;

const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const OUTBOUND_QUEUE_CAPACITY: usize = 32;

pub struct ControlService {
    pub state: Arc<AppState>,
}

impl ControlService {
    #[must_use]
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

#[tonic::async_trait]
impl Control for ControlService {
    type ChannelStream =
        Pin<Box<dyn Stream<Item = Result<ServerMessage, Status>> + Send + 'static>>;

    #[allow(clippy::too_many_lines)]
    async fn channel(
        &self,
        request: Request<Streaming<ClientMessage>>,
    ) -> Result<Response<Self::ChannelStream>, Status> {
        let identity = request
            .extensions()
            .get::<ClientIdentity>()
            .cloned()
            .ok_or_else(|| Status::unauthenticated("missing_identity"))?;
        let remote_addr = request.remote_addr();
        let mut inbound = request.into_inner();
        let state = Arc::clone(&self.state);

        let (tx, rx) = mpsc::channel::<Result<ServerMessage, Status>>(OUTBOUND_QUEUE_CAPACITY);
        let status_waiters: StatusWaiters = Arc::new(Mutex::new(HashMap::new()));

        let cancel_token = state.clients.session_root_token().child_token();
        let session_id = state
            .clients
            .register(
                identity.client_name.clone(),
                remote_addr,
                cancel_token.clone(),
                tx.clone(),
                status_waiters.clone(),
            )
            .await;
        state.metrics.clients_connected.inc();

        // 004-udp-forward T008: peek the first inbound message before
        // sending Welcome. If it is Hello, harvest `supported_protocols`
        // into the ConnectedClient row so push-rule can gate UDP rules
        // pre-wire (HIGH-1 review fix). If the first message is anything
        // else (e.g. v0.3 client jumping straight to StatsReport), the
        // session keeps the registration default `{TCP}` and the original
        // message is fed into the existing handler.
        let pending_first_msg: Option<ClientMessage> = match inbound.next().await {
            Some(Ok(client_msg)) => match &client_msg.payload {
                Some(forward_proto::v1::client_message::Payload::Hello(h)) => {
                    let caps = capabilities_from_hello(&h.supported_protocols);
                    state
                        .clients
                        .set_supported_protocols(&identity.client_name, session_id, caps.clone())
                        .await;
                    // 007-multi-target-failover (R-007): track the
                    // client binary version so the operator HTTP guard
                    // can refuse multi-target push to a < 0.7.0 client.
                    if !h.client_version.is_empty() {
                        state
                            .clients
                            .set_client_version(
                                &identity.client_name,
                                session_id,
                                h.client_version.clone(),
                            )
                            .await;
                    }
                    info!(
                        event = "client.hello",
                        client_name = %identity.client_name,
                        protocol_version = %h.protocol_version,
                        client_version = %h.client_version,
                        supported_protocols = ?caps_for_log(&caps),
                    );
                    None
                }
                _ => Some(client_msg),
            },
            Some(Err(e)) => {
                state
                    .clients
                    .unregister(&identity.client_name, session_id)
                    .await;
                state.metrics.clients_connected.dec();
                warn!(
                    event = "client.transport_error",
                    client_name = %identity.client_name,
                    error = %e,
                );
                return Err(Status::cancelled("transport_error_before_welcome"));
            }
            None => {
                state
                    .clients
                    .unregister(&identity.client_name, session_id)
                    .await;
                state.metrics.clients_connected.dec();
                return Err(Status::cancelled("client_dropped_before_hello"));
            }
        };

        let session_caps = state
            .clients
            .snapshot()
            .await
            .get(&identity.client_name)
            .map_or_else(
                || {
                    let mut s = HashSet::new();
                    s.insert(Protocol::Tcp);
                    s
                },
                |c| c.supported_protocols.clone(),
            );
        info!(
            event = "client.connected",
            client_name = %identity.client_name,
            remote_addr = ?remote_addr,
            session_id,
            supported_protocols = ?caps_for_log(&session_caps),
        );

        // Send Welcome with UDP tunables sourced from server config (T013).
        // 0 means "use client default" — for v0.4.0 servers we always
        // emit the resolved positive integers from ServerConfig.
        let (idle_secs, max_flows) = state.server_config.as_ref().map_or((0, 0), |c| {
            (c.udp_flow_idle_secs(), c.udp_max_flows_per_rule())
        });
        let welcome = ServerMessage {
            payload: Some(server_message::Payload::Welcome(Welcome {
                server_version: SERVER_VERSION.to_string(),
                server_time_unix_ms: now_ms(),
                udp_flow_idle_secs: idle_secs,
                udp_max_flows_per_rule: max_flows,
            })),
        };
        if tx.send(Ok(welcome)).await.is_err() {
            state
                .clients
                .unregister(&identity.client_name, session_id)
                .await;
            state.metrics.clients_connected.dec();
            return Err(Status::cancelled("client_dropped_before_welcome"));
        }

        let pump_state = Arc::clone(&state);
        let pump_identity = identity.clone();
        let pump_cancel = cancel_token.clone();
        let pump_waiters = status_waiters.clone();
        // Move `tx` into the pump task so the response stream stays open for
        // the lifetime of the session. Without this the sender drops at the
        // end of `channel()`, the receiver yields None right after Welcome,
        // and the client immediately reconnects in a tight loop. (US2 also
        // sends rule pushes through this same channel.)
        tokio::spawn(async move {
            let _outbound = tx;
            // If the first inbound message wasn't a Hello, replay it
            // through the existing handler now (v0.3 client back-compat).
            if let Some(replay) = pending_first_msg {
                handle_client_message(&pump_state, &pump_identity, &pump_waiters, replay).await;
            }
            loop {
                tokio::select! {
                    () = pump_cancel.cancelled() => {
                        break;
                    }
                    msg = inbound.next() => {
                        match msg {
                            Some(Ok(client_msg)) => {
                                handle_client_message(
                                    &pump_state,
                                    &pump_identity,
                                    &pump_waiters,
                                    client_msg,
                                )
                                .await;
                            }
                            Some(Err(e)) => {
                                warn!(
                                    event = "client.transport_error",
                                    client_name = %pump_identity.client_name,
                                    error = %e,
                                );
                                break;
                            }
                            None => break, // graceful EOF from client
                        }
                    }
                }
            }
            pump_state
                .clients
                .unregister(&pump_identity.client_name, session_id)
                .await;
            pump_state.metrics.clients_connected.dec();
            info!(
                event = "client.disconnected",
                client_name = %pump_identity.client_name,
                session_id,
            );
        });

        let outbound = ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(outbound) as Self::ChannelStream))
    }
}

/// Convert the wire `repeated Protocol supported_protocols` (i32 enum
/// values) into a `HashSet<Protocol>`. Unknown integers are silently
/// dropped (proto3 forward-compat); `Protocol::Unspecified` is also
/// dropped so it can never satisfy a `supports()` check.
fn capabilities_from_hello(values: &[i32]) -> HashSet<Protocol> {
    let mut out = HashSet::new();
    for v in values {
        if let Ok(p) = Protocol::try_from(*v)
            && !matches!(p, Protocol::Unspecified)
        {
            out.insert(p);
        }
    }
    out
}

/// Stable, sorted list rendering for log output — `HashSet` iteration
/// order is non-deterministic, which would defeat log scrubbing in
/// integration tests.
fn caps_for_log(caps: &HashSet<Protocol>) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = caps
        .iter()
        .map(|p| match p {
            Protocol::Unspecified => "UNSPECIFIED",
            Protocol::Tcp => "TCP",
            Protocol::Udp => "UDP",
        })
        .collect();
    v.sort_unstable();
    v
}

async fn handle_client_message(
    state: &AppState,
    identity: &ClientIdentity,
    waiters: &StatusWaiters,
    msg: ClientMessage,
) {
    use forward_core::RuleId;
    use forward_proto::v1::client_message::Payload;
    match msg.payload {
        Some(Payload::Hello(h)) => {
            // A Hello can only legitimately appear as the first message
            // (consumed by `channel()` before Welcome). Anything later
            // is a protocol error from the client; log and ignore.
            warn!(
                event = "client.hello_after_welcome",
                client_name = %identity.client_name,
                protocol_version = %h.protocol_version,
                client_version = %h.client_version,
            );
        }
        Some(Payload::RuleStatus(rs)) => {
            // Hand the status to the operator path waiting on `request_id`.
            // If the waiter isn't there, this is either a late arrival
            // (post-timeout) or the unsolicited Removed echo when the
            // listener's drain finished — both are fine; we just log.
            let request_id = rs.request_id.clone();
            let mut guard = waiters.lock().await;
            if let Some(tx) = guard.remove(&request_id) {
                let _ = tx.send(rs);
            } else {
                info!(
                    event = "client.rule_status_unmatched",
                    client_name = %identity.client_name,
                    request_id = %request_id,
                    rule_id = rs.rule_id,
                );
            }
        }
        Some(Payload::StatsReport(report)) => {
            // T060: fold each per-rule entry into the cache + Prometheus
            // counters. `observe` handles delta computation and rebaseline on
            // client restart.
            // T045 (002-port-range-forward): the same report carries
            // optional `per_port` detail for range rules; route those
            // into `state.per_port_stats` for the operator's `--per-port`
            // view. Aggregate counters keep their existing path so the
            // Prometheus cardinality budget (SC-002) is unaffected.
            let entries = report.stats.len();
            for entry in report.stats {
                let rule_id = RuleId(entry.rule_id);
                // 005-multi-user-rbac T045: thread the rule's owner
                // user_id through the metrics path so the per-rule
                // collectors carry an `owner` label. If the rule was
                // removed between the StatsReport leaving the client
                // and arriving here, the lookup misses — fall back to
                // `_unknown` so cardinality stays bounded.
                let owner = state
                    .rules
                    .get(rule_id)
                    .await
                    .map_or_else(|| "_unknown".to_string(), |r| r.owner_user_id.to_string());
                state
                    .stats_cache
                    .observe(
                        &identity.client_name,
                        rule_id,
                        owner.as_str(),
                        entry.bytes_in,
                        entry.bytes_out,
                        entry.active_connections,
                        // 003-domain-name-forward T050: per-rule
                        // DNS-failure counter (FR-008). Always
                        // present in the proto; 0 for IP-target
                        // rules where the resolver layer is bypassed.
                        entry.dns_failures,
                        // 004-udp-forward T039: UDP cumulative readings
                        // straight off the wire (proto3 zero for TCP
                        // rules — SC-004 cardinality holds because the
                        // observe path skips collector writes when
                        // every delta is 0).
                        entry.datagrams_in,
                        entry.datagrams_out,
                        entry.active_flows,
                        entry.flows_dropped_overflow,
                        &state.metrics,
                    )
                    .await;
                if !entry.per_port.is_empty() {
                    let snapshots = entry
                        .per_port
                        .into_iter()
                        .filter_map(|p| {
                            let port = u16::try_from(p.listen_port).ok()?;
                            Some(crate::operator::per_port_stats::PerPortSnapshot {
                                listen_port: port,
                                bytes_in: p.bytes_in,
                                bytes_out: p.bytes_out,
                                active_connections: p.active_connections,
                                datagrams_in: p.datagrams_in,
                                datagrams_out: p.datagrams_out,
                                updated_at: chrono::Utc::now(),
                            })
                        })
                        .collect::<Vec<_>>();
                    state.per_port_stats.update(rule_id, snapshots).await;
                }
            }
            info!(
                event = "client.stats_report",
                client_name = %identity.client_name,
                rule_count = entries,
                sent_at_unix_ms = report.sent_at_unix_ms,
            );
        }
        None => {}
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 004-udp-forward T009. The end-to-end Hello-gated path is exercised
    // through the existing register/handle integration suites in
    // `forward-e2e`; here we lock down the pure helpers that decide
    // capability membership at the wire boundary.

    #[test]
    fn capabilities_from_hello_with_tcp_and_udp() {
        let caps = capabilities_from_hello(&[Protocol::Tcp as i32, Protocol::Udp as i32]);
        assert!(caps.contains(&Protocol::Tcp));
        assert!(caps.contains(&Protocol::Udp));
        assert_eq!(caps.len(), 2);
    }

    #[test]
    fn capabilities_from_hello_drops_unknown_enum_values() {
        // Wire integer 99 is not defined in the enum — proto3 forward-
        // compat semantics say silently ignore. The set MUST NOT
        // contain a coerced value.
        let caps = capabilities_from_hello(&[Protocol::Tcp as i32, 99]);
        assert!(caps.contains(&Protocol::Tcp));
        assert_eq!(caps.len(), 1, "unknown enum integer must be dropped");
    }

    #[test]
    fn capabilities_from_hello_drops_unspecified() {
        // Even an explicitly-emitted PROTOCOL_UNSPECIFIED (= 0) is
        // useless for capability checks — `supports()` always returns
        // false for it; we drop at parse time so it can never bloat
        // the set or appear in audit logs.
        let caps = capabilities_from_hello(&[Protocol::Unspecified as i32, Protocol::Udp as i32]);
        assert!(caps.contains(&Protocol::Udp));
        assert!(!caps.contains(&Protocol::Unspecified));
        assert_eq!(caps.len(), 1);
    }

    #[test]
    fn capabilities_from_hello_empty_list_yields_empty_set() {
        // A v0.3.0 client whose Hello arrives with no
        // `supported_protocols` is treated as TCP-only at the
        // ConnectedClient level (the registration default seeded by
        // `register()`), but THIS function reflects only what the wire
        // said. The caller decides the back-compat default.
        let caps = capabilities_from_hello(&[]);
        assert!(caps.is_empty());
    }

    #[test]
    fn caps_for_log_is_sorted_and_human_readable() {
        let mut caps = HashSet::new();
        caps.insert(Protocol::Udp);
        caps.insert(Protocol::Tcp);
        // HashSet iteration is non-deterministic; the rendering must
        // be stable so log scrapers can pin on it.
        assert_eq!(caps_for_log(&caps), vec!["TCP", "UDP"]);
    }
}
