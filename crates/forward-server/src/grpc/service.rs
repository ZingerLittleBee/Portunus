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
use forward_core::RuleId;
use forward_proto::v1::{
    ClientMessage, Protocol, ProxyProtocolVersion, Rule, RuleAction, RuleUpdate, ServerMessage,
    Welcome, control_server::Control, server_message,
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
            replay_rules_for_client(&pump_state, &pump_identity, &_outbound).await;
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
                apply_unsolicited_rule_status(state, identity, &rs).await;
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
                    .observe_with_targets(
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
                        entry.target_failovers_total,
                        entry
                            .per_target
                            .iter()
                            .map(|p| crate::metrics::PerTargetSnapshot {
                                index: p.index,
                                host: p.host.clone(),
                                port: p.port,
                                priority: p.priority,
                                health: p.health,
                                consecutive_failures: p.consecutive_failures,
                                last_failure_at_unix_ms: p.last_failure_at_unix_ms,
                                last_success_at_unix_ms: p.last_success_at_unix_ms,
                                bytes_in: p.bytes_in,
                                bytes_out: p.bytes_out,
                                connections_accepted: p.connections_accepted,
                            })
                            .collect(),
                        &state.metrics,
                    )
                    .await;
                // 009-tls-sni-routing T080: fold per-rule SNI counters into
                // the new Prometheus collectors. Same delta semantics as the
                // existing observe path — saturating_sub guards against a
                // client-side rebaseline (e.g. process restart).
                state
                    .stats_cache
                    .observe_sni_per_rule(
                        &identity.client_name,
                        rule_id,
                        owner.as_str(),
                        entry.sni_route_exact_total,
                        entry.sni_route_wildcard_total,
                        entry.sni_route_fallback_total,
                        &state.metrics,
                    )
                    .await;
                // 011-rate-limiting-qos T023: fold per-rule rate-limit
                // stats into the three new collectors. v0.10 wire's
                // proto3 default-strip means `entry.rate_limit` stays
                // `None` for uncapped rules, so we skip the fold and
                // emit no series — preserves SC-006 cardinality budget.
                if let Some(rl) = entry.rate_limit.as_ref() {
                    let mut reject_totals = [0u64; 6];
                    for c in &rl.reject_total {
                        if let Ok(reason) =
                            forward_proto::v1::RateLimitRejectReason::try_from(c.reason)
                        {
                            let idx = match reason {
                                forward_proto::v1::RateLimitRejectReason::ConnConcurrent => 0,
                                forward_proto::v1::RateLimitRejectReason::ConnRate => 1,
                                forward_proto::v1::RateLimitRejectReason::UdpFlowRate => 2,
                                forward_proto::v1::RateLimitRejectReason::OwnerConcurrent => 3,
                                forward_proto::v1::RateLimitRejectReason::OwnerConnRate => 4,
                                forward_proto::v1::RateLimitRejectReason::OwnerUdpFlowRate => 5,
                                // Unspecified is the proto default; the
                                // client never emits it.
                                forward_proto::v1::RateLimitRejectReason::Unspecified => continue,
                            };
                            reject_totals[idx] = c.total;
                        }
                    }
                    state
                        .stats_cache
                        .observe_rate_limit_per_rule(
                            &identity.client_name,
                            rule_id,
                            owner.as_str(),
                            reject_totals,
                            rl.throttle_micros_in,
                            rl.throttle_micros_out,
                            rl.active_connections,
                            &state.metrics,
                        )
                        .await;
                }
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
            // 009-tls-sni-routing T080: per-listener SNI counters
            // (StatsReport.sni_listener_stats = 3). Independent of rule
            // identity — keyed on (client, listen_port) — so the cache
            // tracks them in a separate prev-state map.
            for listener in report.sni_listener_stats {
                let Ok(port) = u16::try_from(listener.listen_port) else {
                    continue;
                };
                state
                    .stats_cache
                    .observe_sni_listener(
                        &identity.client_name,
                        port,
                        listener.sni_route_miss_total,
                        listener.client_hello_parse_failures_total,
                        &listener.client_hello_peek_bucket_counts,
                        listener.client_hello_peek_sum_micros,
                        listener.client_hello_peek_count,
                        &state.metrics,
                    )
                    .await;
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

async fn replay_rules_for_client(
    state: &AppState,
    identity: &ClientIdentity,
    outbound: &OutboundSender,
) {
    let rules = state.rules.list(Some(&identity.client_name)).await;
    if rules.is_empty() {
        return;
    }
    let caps = state
        .clients
        .snapshot()
        .await
        .get(&identity.client_name)
        .map_or_else(HashSet::new, |c| c.supported_protocols.clone());
    let client_version = state.clients.client_version_of(&identity.client_name).await;
    for rule in rules {
        if !matches!(rule.state, crate::rules::RuleState::Active) {
            continue;
        }
        if let Err(reason) = replay_gate_reason(&rule, &caps, client_version.as_deref()) {
            let _ = state.rules.mark_failed(rule.id, reason.clone()).await;
            if let Some(updated) = state.rules.get(rule.id).await {
                let _ = state.rule_store.upsert_rule(&updated);
            }
            continue;
        }
        let _ = state.rules.mark_pending(rule.id).await;
        if let Some(updated) = state.rules.get(rule.id).await {
            let _ = state.rule_store.upsert_rule(&updated);
            let msg = ServerMessage {
                payload: Some(server_message::Payload::RuleUpdate(RuleUpdate {
                    request_id: format!("replay-{}", ulid::Ulid::new()),
                    action: RuleAction::Push as i32,
                    rule: Some(proto_rule_from_rule(&updated)),
                })),
            };
            if outbound.send(Ok(msg)).await.is_err() {
                break;
            }
        }
    }
}

fn replay_gate_reason(
    rule: &crate::rules::Rule,
    caps: &HashSet<Protocol>,
    client_version: Option<&str>,
) -> Result<(), String> {
    if matches!(rule.protocol, crate::rules::Protocol::Udp) && !caps.contains(&Protocol::Udp) {
        return Err("unsupported_protocol".into());
    }
    if !rule.targets.is_empty()
        && rule.targets.len() >= 2
        && !version_at_least(client_version, 0, 7)
    {
        return Err("multi_target_unsupported_by_client".into());
    }
    if rule.sni_pattern.is_some() && !version_at_least(client_version, 0, 9) {
        return Err("sni_unsupported_by_client".into());
    }
    if rule.targets.iter().any(|t| t.proxy_protocol.is_some())
        && !version_at_least(client_version, 0, 10)
    {
        return Err("proxy_protocol_unsupported_by_client".into());
    }
    // 011-rate-limiting-qos T008 / FR-006: refuse to replay a rule
    // carrying any rate-limit cap to a forward-client whose
    // self-reported client_version is below 0.11.0. Pre-0.11 readers
    // would silently drop `Rule.rate_limit = 12` on decode and the
    // rule would activate uncapped — violating the operator-visible
    // contract. The HTTP push path enforces the same gate at submit
    // time (operator/http.rs); this branch covers the post-restart
    // rule-replay path so a rule that survived persistence cannot
    // sneak past the gate after a server bounce.
    if rule.rate_limit.is_some() && !version_at_least(client_version, 0, 11) {
        return Err("rate_limit_unsupported_by_client".into());
    }
    Ok(())
}

fn version_at_least(version: Option<&str>, major_floor: u32, minor_floor: u32) -> bool {
    let Some(version) = version else {
        return false;
    };
    let trimmed = version.split(['-', '+']).next().unwrap_or("");
    let mut parts = trimmed.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor) >= (major_floor, minor_floor)
}

fn proto_rule_from_rule(rule: &crate::rules::Rule) -> Rule {
    Rule {
        rule_id: rule.id.0,
        listen_port: u32::from(rule.listen_port),
        target_host: rule.target_host.clone(),
        target_port: u32::from(rule.target_port),
        protocol: match rule.protocol {
            crate::rules::Protocol::Tcp => Protocol::Tcp as i32,
            crate::rules::Protocol::Udp => Protocol::Udp as i32,
        },
        listen_port_end: rule.listen_port_end.map_or(0, u32::from),
        target_port_end: rule.target_port_end.map_or(0, u32::from),
        prefer_ipv6: rule.prefer_ipv6,
        targets: rule
            .targets
            .iter()
            .map(|target| forward_proto::v1::Target {
                host: target.host.clone(),
                port: u32::from(target.port),
                priority: target.priority,
                proxy_protocol: target.proxy_protocol.map(|mode| match mode {
                    forward_core::ProxyProtocolVersion::V1 => ProxyProtocolVersion::V1 as i32,
                    forward_core::ProxyProtocolVersion::V2 => ProxyProtocolVersion::V2 as i32,
                }),
            })
            .collect(),
        health_check_interval_secs: rule.health_check_interval_secs.unwrap_or(0),
        sni_pattern: rule.sni_pattern.clone(),
        // 011-rate-limiting-qos T007: per-rule cap envelope. None on
        // both sides preserves byte-identical wire shape with v0.10
        // (proto3 default-stripping). Capability gate
        // (rate_limit_unsupported_by_client) refuses to send a non-
        // None envelope to a pre-0.11 client; that gate runs upstream
        // of this mapping in operator/http.rs and replay_gate_reason.
        rate_limit: rule.rate_limit.as_ref().map(rate_limit_to_proto),
    }
}

/// 011-rate-limiting-qos T007: encode a `forward_core::RateLimit` into
/// the wire-shape `forward_proto::v1::RateLimit`. Field tags are
/// 1-1 with the `RateLimit` definition in `proto/forward.proto` and
/// every cap is independently optional.
pub(crate) fn rate_limit_to_proto(rl: &forward_core::RateLimit) -> forward_proto::v1::RateLimit {
    forward_proto::v1::RateLimit {
        bandwidth_in_bps: rl.bandwidth_in_bps,
        bandwidth_out_bps: rl.bandwidth_out_bps,
        new_connections_per_sec: rl.new_connections_per_sec,
        concurrent_connections: rl.concurrent_connections,
        bandwidth_in_burst: rl.bandwidth_in_burst,
        bandwidth_out_burst: rl.bandwidth_out_burst,
        new_connections_burst: rl.new_connections_burst,
    }
}

/// 011-rate-limiting-qos T007: decode a wire `RateLimit` back into the
/// core envelope. Inverse of [`rate_limit_to_proto`]. Used by the HTTP
/// push handler (T016) to hydrate caps received from the operator API
/// — and any future inbound path that needs to read caps off the wire.
#[allow(dead_code)] // wired up in T030 (per-owner caps inbound path)
pub(crate) fn rate_limit_from_proto(p: &forward_proto::v1::RateLimit) -> forward_core::RateLimit {
    forward_core::RateLimit {
        bandwidth_in_bps: p.bandwidth_in_bps,
        bandwidth_out_bps: p.bandwidth_out_bps,
        new_connections_per_sec: p.new_connections_per_sec,
        concurrent_connections: p.concurrent_connections,
        bandwidth_in_burst: p.bandwidth_in_burst,
        bandwidth_out_burst: p.bandwidth_out_burst,
        new_connections_burst: p.new_connections_burst,
    }
}

async fn apply_unsolicited_rule_status(
    state: &AppState,
    identity: &ClientIdentity,
    status: &forward_proto::v1::RuleStatus,
) {
    let rule_id = RuleId(status.rule_id);
    let Some(existing) = state.rules.get(rule_id).await else {
        info!(
            event = "client.rule_status_unmatched",
            client_name = %identity.client_name,
            request_id = %status.request_id,
            rule_id = status.rule_id,
        );
        return;
    };
    if existing.client_name != identity.client_name {
        warn!(
            event = "client.rule_status_wrong_client",
            client_name = %identity.client_name,
            rule_owner = %existing.client_name,
            request_id = %status.request_id,
            rule_id = status.rule_id,
        );
        return;
    }
    let outcome = forward_proto::v1::ActivationOutcome::try_from(status.outcome)
        .unwrap_or(forward_proto::v1::ActivationOutcome::Unspecified);
    match outcome {
        forward_proto::v1::ActivationOutcome::Activated => {
            let _ = state.rules.mark_active(rule_id).await;
        }
        forward_proto::v1::ActivationOutcome::Failed => {
            let reason = if status.reason.is_empty() {
                "unspecified".to_string()
            } else {
                status.reason.clone()
            };
            let _ = state.rules.mark_failed(rule_id, reason).await;
        }
        forward_proto::v1::ActivationOutcome::Removed
        | forward_proto::v1::ActivationOutcome::Unspecified => {
            info!(
                event = "client.rule_status_unmatched",
                client_name = %identity.client_name,
                request_id = %status.request_id,
                rule_id = status.rule_id,
            );
            return;
        }
    }
    if let Some(rule) = state.rules.get(rule_id).await {
        let _ = state.rule_store.upsert_rule(&rule);
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
    use crate::clients::ConnectedClients;
    use crate::state::AppState;
    use crate::store::Store;
    use crate::store::operator_store::SqliteOperatorStore;
    use crate::store::token_store::SqliteTokenStore;
    use tempfile::tempdir;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

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

    fn build_state() -> Arc<AppState> {
        let dir = tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path()).unwrap());
        let tokens = Arc::new(SqliteTokenStore::new(Arc::clone(&store)));
        let operator_store = Arc::new(SqliteOperatorStore::new(Arc::clone(&store)));
        operator_store
            .bootstrap_legacy_superadmin("test-token")
            .unwrap();
        Arc::new(
            AppState::new(
                tokens,
                operator_store,
                ConnectedClients::default(),
                "127.0.0.1:0",
                "deadbeef",
                "-----BEGIN CERTIFICATE-----\n",
                16,
                store,
            )
            .unwrap(),
        )
    }

    async fn register_client(
        state: &Arc<AppState>,
        name: &str,
        version: &str,
    ) -> (
        ClientIdentity,
        OutboundSender,
        mpsc::Receiver<Result<ServerMessage, Status>>,
    ) {
        let client_name = forward_core::ClientName::new(name.to_string()).unwrap();
        let (tx, rx) = mpsc::channel(8);
        let waiters: StatusWaiters = Arc::new(Mutex::new(HashMap::new()));
        let session_id = state
            .clients
            .register(
                client_name.clone(),
                None,
                CancellationToken::new(),
                tx.clone(),
                waiters,
            )
            .await;
        let mut caps = HashSet::new();
        caps.insert(Protocol::Tcp);
        state
            .clients
            .set_supported_protocols(&client_name, session_id, caps)
            .await;
        state
            .clients
            .set_client_version(&client_name, session_id, version.to_string())
            .await;
        (ClientIdentity { client_name }, tx, rx)
    }

    #[tokio::test]
    async fn replay_rules_sends_proxy_protocol_targets_to_capable_client() {
        let state = build_state();
        let (identity, outbound, mut rx) = register_client(&state, "edge-replay", "0.10.0").await;
        let rule = crate::rules::Rule {
            id: forward_core::RuleId(7),
            client_name: identity.client_name.clone(),
            listen_port: 443,
            listen_port_end: None,
            target_host: "10.0.0.1".into(),
            target_port: 8443,
            target_port_end: None,
            prefer_ipv6: None,
            protocol: crate::rules::Protocol::Tcp,
            state: crate::rules::RuleState::Active,
            created_at: chrono::Utc::now(),
            last_state_change_at: chrono::Utc::now(),
            owner_user_id: forward_auth::UserId::reserved("alice"),
            targets: vec![forward_core::RuleTarget {
                host: "10.0.0.1".into(),
                port: 8443,
                priority: 0,
                proxy_protocol: Some(forward_core::ProxyProtocolVersion::V1),
            }],
            health_check_interval_secs: None,
            sni_pattern: None,
            rate_limit: None,
        };
        state.rules.hydrate(vec![rule.clone()]).await;

        replay_rules_for_client(&state, &identity, &outbound).await;
        let msg = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("replay arrives")
            .expect("message present")
            .expect("server message");
        let Some(server_message::Payload::RuleUpdate(update)) = msg.payload else {
            panic!("expected RuleUpdate");
        };
        let rule = update.rule.expect("rule");
        assert_eq!(rule.targets.len(), 1);
        assert_eq!(
            rule.targets[0].proxy_protocol,
            Some(ProxyProtocolVersion::V1 as i32)
        );
    }

    #[tokio::test]
    async fn replay_rules_marks_incompatible_proxy_rule_failed() {
        let state = build_state();
        let (identity, outbound, mut rx) =
            register_client(&state, "edge-replay-old", "0.8.0").await;
        let rule = crate::rules::Rule {
            id: forward_core::RuleId(8),
            client_name: identity.client_name.clone(),
            listen_port: 443,
            listen_port_end: None,
            target_host: "10.0.0.1".into(),
            target_port: 8443,
            target_port_end: None,
            prefer_ipv6: None,
            protocol: crate::rules::Protocol::Tcp,
            state: crate::rules::RuleState::Active,
            created_at: chrono::Utc::now(),
            last_state_change_at: chrono::Utc::now(),
            owner_user_id: forward_auth::UserId::reserved("alice"),
            targets: vec![forward_core::RuleTarget {
                host: "10.0.0.1".into(),
                port: 8443,
                priority: 0,
                proxy_protocol: Some(forward_core::ProxyProtocolVersion::V1),
            }],
            health_check_interval_secs: None,
            sni_pattern: None,
            rate_limit: None,
        };
        state.rules.hydrate(vec![rule]).await;

        replay_rules_for_client(&state, &identity, &outbound).await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "incompatible rule should not replay"
        );
        let loaded = state
            .rules
            .get(forward_core::RuleId(8))
            .await
            .expect("rule");
        match loaded.state {
            crate::rules::RuleState::Failed { reason } => {
                assert_eq!(reason, "proxy_protocol_unsupported_by_client");
            }
            other => panic!("expected failed state, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn replay_rules_skips_non_active_rules() {
        let state = build_state();
        let (identity, outbound, mut rx) =
            register_client(&state, "edge-replay-skip", "0.10.0").await;
        let now = chrono::Utc::now();
        state
            .rules
            .hydrate(vec![
                crate::rules::Rule {
                    id: forward_core::RuleId(9),
                    client_name: identity.client_name.clone(),
                    listen_port: 443,
                    listen_port_end: None,
                    target_host: "10.0.0.1".into(),
                    target_port: 8443,
                    target_port_end: None,
                    prefer_ipv6: None,
                    protocol: crate::rules::Protocol::Tcp,
                    state: crate::rules::RuleState::Failed {
                        reason: "port_in_use".into(),
                    },
                    created_at: now,
                    last_state_change_at: now,
                    owner_user_id: forward_auth::UserId::reserved("alice"),
                    targets: vec![forward_core::RuleTarget {
                        host: "10.0.0.1".into(),
                        port: 8443,
                        priority: 0,
                        proxy_protocol: None,
                    }],
                    health_check_interval_secs: None,
                    sni_pattern: None,
                    rate_limit: None,
                },
                crate::rules::Rule {
                    id: forward_core::RuleId(10),
                    client_name: identity.client_name.clone(),
                    listen_port: 444,
                    listen_port_end: None,
                    target_host: "10.0.0.2".into(),
                    target_port: 8444,
                    target_port_end: None,
                    prefer_ipv6: None,
                    protocol: crate::rules::Protocol::Tcp,
                    state: crate::rules::RuleState::Pending,
                    created_at: now,
                    last_state_change_at: now,
                    owner_user_id: forward_auth::UserId::reserved("alice"),
                    targets: vec![forward_core::RuleTarget {
                        host: "10.0.0.2".into(),
                        port: 8444,
                        priority: 0,
                        proxy_protocol: None,
                    }],
                    health_check_interval_secs: None,
                    sni_pattern: None,
                    rate_limit: None,
                },
            ])
            .await;

        replay_rules_for_client(&state, &identity, &outbound).await;

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
                .await
                .is_err(),
            "non-active rules should not replay"
        );
        assert!(matches!(
            state
                .rules
                .get(forward_core::RuleId(9))
                .await
                .expect("failed rule")
                .state,
            crate::rules::RuleState::Failed { .. }
        ));
        assert!(matches!(
            state
                .rules
                .get(forward_core::RuleId(10))
                .await
                .expect("pending rule")
                .state,
            crate::rules::RuleState::Pending
        ));
    }

    #[tokio::test]
    async fn unsolicited_rule_status_ignores_rules_owned_by_other_client() {
        let state = build_state();
        let (identity, _outbound, _rx) = register_client(&state, "edge-a", "0.10.0").await;
        let other_client = forward_core::ClientName::new("edge-b".to_string()).unwrap();
        let now = chrono::Utc::now();
        state
            .rules
            .hydrate(vec![crate::rules::Rule {
                id: forward_core::RuleId(11),
                client_name: other_client,
                listen_port: 443,
                listen_port_end: None,
                target_host: "10.0.0.1".into(),
                target_port: 8443,
                target_port_end: None,
                prefer_ipv6: None,
                protocol: crate::rules::Protocol::Tcp,
                state: crate::rules::RuleState::Active,
                created_at: now,
                last_state_change_at: now,
                owner_user_id: forward_auth::UserId::reserved("alice"),
                targets: vec![forward_core::RuleTarget {
                    host: "10.0.0.1".into(),
                    port: 8443,
                    priority: 0,
                    proxy_protocol: None,
                }],
                health_check_interval_secs: None,
                sni_pattern: None,
                rate_limit: None,
            }])
            .await;

        apply_unsolicited_rule_status(
            &state,
            &identity,
            &forward_proto::v1::RuleStatus {
                request_id: "stale-or-forged".into(),
                rule_id: 11,
                outcome: forward_proto::v1::ActivationOutcome::Failed as i32,
                reason: "forged_failure".into(),
            },
        )
        .await;

        assert!(matches!(
            state
                .rules
                .get(forward_core::RuleId(11))
                .await
                .expect("other client rule")
                .state,
            crate::rules::RuleState::Active
        ));
    }
}
