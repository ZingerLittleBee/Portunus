//! Control-plane connection lifecycle: TLS dial, Welcome handshake,
//! reconnect with full-jitter exponential backoff, `RuleUpdate` dispatch
//! into the forwarder, and `RuleStatus` echoing on the outbound channel.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use forward_core::{PortRange, RuleId};
use forward_proto::v1::{
    ActivationOutcome, ClientMessage, Hello, PerPortStats as ProtoPerPortStats,
    PerTargetStats as ProtoPerTargetStats, Protocol, RuleAction, RuleStats as ProtoRuleStats,
    RuleStatus as ProtoRuleStatus, ServerMessage, StatsReport, client_message,
    control_client::ControlClient, server_message,
};
use rand::Rng;
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use tonic::Request;
use tonic::metadata::MetadataValue;
use tonic::transport::{Certificate, ClientTlsConfig, Endpoint};
use tracing::{error, info, warn};

use crate::bundle::CredentialBundle;
use crate::forwarder::stats::RuleStats;
use crate::forwarder::{self, ClientRule, RuleStatusEvent};

const PROTOCOL_VERSION: &str = "1.0.0";
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const HELLO_QUEUE_CAPACITY: usize = 16;

#[derive(Debug, Error)]
pub enum ControlError {
    #[error("tls: {0}")]
    Tls(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("auth: {0}")]
    Auth(String),
    #[error("welcome_missing")]
    WelcomeMissing,
    #[error("token_revoked")]
    TokenRevoked,
    #[error("io: {0}")]
    Io(String),
}

impl ControlError {
    /// Errors that should NOT trigger reconnect (operator must intervene).
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::TokenRevoked | Self::Auth(_))
    }
}

/// One-shot connection: dial TLS, attach bearer metadata, open Channel,
/// send Hello, await Welcome. Returns the live duplex once Welcome arrives.
pub async fn connect_once(
    bundle: &CredentialBundle,
    cancel: &CancellationToken,
) -> Result<LiveSession, ControlError> {
    // Pinning model: the bundle carries the server's self-signed leaf cert PEM
    // *and* its SHA-256 fingerprint. `CredentialBundle::read_from` already
    // verified that `sha256(DER(server_cert_pem)) == server_cert_sha256` at
    // load time, so trusting `server_cert_pem` here is equivalent to trusting
    // the pin. We pass it as the *only* CA — system roots are not consulted.
    let ca = Certificate::from_pem(bundle.server_cert_pem.as_bytes());
    let endpoint = Endpoint::from_shared(format!("https://{}", bundle.server_endpoint))
        .map_err(|e| ControlError::Transport(e.to_string()))?
        .tls_config(
            ClientTlsConfig::new()
                .ca_certificate(ca)
                .domain_name(extract_host(&bundle.server_endpoint)),
        )
        .map_err(|e| ControlError::Tls(e.to_string()))?;

    let channel = endpoint
        .connect()
        .await
        .map_err(|e| ControlError::Transport(format_chain(&e)))?;

    let token = bundle.token.clone();
    let mut grpc = ControlClient::with_interceptor(channel, move |mut req: Request<()>| {
        let value: MetadataValue<_> = format!("Bearer {token}")
            .parse()
            .map_err(|_| tonic::Status::unauthenticated("malformed_token"))?;
        req.metadata_mut().insert("authorization", value);
        Ok(req)
    });

    let (outbound_tx, outbound_rx) = mpsc::channel::<ClientMessage>(HELLO_QUEUE_CAPACITY);

    let hello = ClientMessage {
        payload: Some(forward_proto::v1::client_message::Payload::Hello(Hello {
            protocol_version: PROTOCOL_VERSION.to_string(),
            client_version: CLIENT_VERSION.to_string(),
            // T010: declare both forwarding capabilities so the server can
            // reject UDP push pre-wire for v0.3-only clients.
            supported_protocols: vec![Protocol::Tcp as i32, Protocol::Udp as i32],
        })),
    };
    outbound_tx
        .send(hello)
        .await
        .map_err(|e| ControlError::Io(e.to_string()))?;

    let outbound_stream = ReceiverStream::new(outbound_rx);
    let response = grpc
        .channel(Request::new(outbound_stream))
        .await
        .map_err(|status| status_to_error(&status))?;
    let mut inbound = response.into_inner();

    // Await Welcome.
    let first = tokio::select! {
        () = cancel.cancelled() => return Err(ControlError::Io("cancelled".into())),
        first = inbound.next() => first,
    };
    let welcome = match first {
        Some(Ok(msg)) => match msg.payload {
            Some(forward_proto::v1::server_message::Payload::Welcome(w)) => w,
            _ => return Err(ControlError::WelcomeMissing),
        },
        Some(Err(status)) => return Err(status_to_error(&status)),
        None => return Err(ControlError::WelcomeMissing),
    };
    info!(
        event = "control.connected",
        server_version = %welcome.server_version,
        udp_flow_idle_secs = welcome.udp_flow_idle_secs,
        udp_max_flows_per_rule = welcome.udp_max_flows_per_rule,
    );

    Ok(LiveSession {
        outbound: outbound_tx,
        inbound: Box::pin(inbound),
        udp_flow_idle_secs: welcome.udp_flow_idle_secs,
        udp_max_flows_per_rule: welcome.udp_max_flows_per_rule,
    })
}

fn format_chain<E: std::error::Error + std::fmt::Debug + 'static>(e: &E) -> String {
    let mut out = format!("{e} | debug={e:?}");
    let mut src: Option<&(dyn std::error::Error + 'static)> = e.source();
    while let Some(s) = src {
        out.push_str(" -> ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

fn extract_host(endpoint: &str) -> String {
    endpoint
        .rsplit_once(':')
        .map_or_else(|| endpoint.to_string(), |(h, _)| h.to_string())
}

fn status_to_error(status: &tonic::Status) -> ControlError {
    let msg = status.message().to_string();
    if status.code() == tonic::Code::Unauthenticated {
        if msg == "token_revoked" {
            ControlError::TokenRevoked
        } else {
            ControlError::Auth(msg)
        }
    } else {
        ControlError::Transport(format!("{:?}: {msg}", status.code()))
    }
}

pub struct LiveSession {
    pub outbound: mpsc::Sender<ClientMessage>,
    pub inbound: std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<ServerMessage, tonic::Status>> + Send + 'static>,
    >,
    /// 004-udp-forward T031: UDP runtime tunables sourced from the
    /// server's Welcome message. `0` (the proto3 default) means "use
    /// client compile-time default" — a v0.3 server that doesn't emit
    /// these fields lands here as 0/0 and the forwarder falls back to
    /// the 60 s / 1024-flow defaults baked into the client.
    pub udp_flow_idle_secs: u32,
    pub udp_max_flows_per_rule: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ReconnectConfig {
    pub initial_delay_ms: u64,
    pub max_delay_secs: u64,
    pub drain_timeout: Duration,
    /// Period between `StatsReport` messages sent on the bidi stream.
    pub stats_report_interval: Duration,
}

/// Reconnect loop with full-jitter exponential backoff.
pub async fn run_with_reconnect(
    bundle: Arc<CredentialBundle>,
    cfg: ReconnectConfig,
    cancel: CancellationToken,
) {
    // 003-domain-name-forward (T020): a single LiveResolver lives
    // for the entire process lifetime and is shared across every
    // forwarder. Cache state survives reconnects (DNS is a
    // client-local concern, not bound to the control-plane stream).
    let resolver = match crate::resolver::HickoryResolver::from_system(
        &crate::resolver::ResolverConfig::default(),
    ) {
        Ok(r) => Arc::new(crate::resolver::LiveResolver::new(
            Arc::new(r),
            crate::resolver::ResolverConfig::default(),
        )),
        Err(e) => {
            error!(event = "control.resolver_init_failed", error = %e);
            return;
        }
    };

    let mut attempt: u32 = 0;
    let max_delay = Duration::from_secs(cfg.max_delay_secs);
    loop {
        if cancel.is_cancelled() {
            return;
        }
        info!(event = "control.connecting", attempt = attempt + 1);
        match connect_once(&bundle, &cancel).await {
            Ok(session) => {
                attempt = 0;
                pump(
                    session,
                    Arc::clone(&resolver),
                    &cancel,
                    cfg.drain_timeout,
                    cfg.stats_report_interval,
                )
                .await;
                info!(event = "control.disconnected");
            }
            Err(e) if e.is_terminal() => {
                warn!(event = "control.terminal", error = %e);
                return;
            }
            Err(e) => {
                warn!(event = "control.connect_failed", error = %e);
            }
        }
        let delay = jittered_backoff(attempt, cfg.initial_delay_ms, max_delay);
        attempt = attempt.saturating_add(1);
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(delay) => {},
        }
    }
}

fn jittered_backoff(attempt: u32, base_ms: u64, max: Duration) -> Duration {
    let exp = base_ms.saturating_mul(2u64.saturating_pow(attempt));
    let cap = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
    let upper = exp.min(cap).max(1);
    let chosen = rand::thread_rng().gen_range(0..=upper);
    Duration::from_millis(chosen)
}

/// Per-rule forwarder bookkeeping. We keep the cancel token so we can stop the
/// listener on REMOVE (or process shutdown), and the `request_id` of the last
/// action so `RuleStatus` echoes carry the right correlation ID:
/// - PUSH's `request_id` is used for Activated/Failed
/// - REMOVE's `request_id` (if set) is used for Removed; otherwise the original
///   push's `request_id` is reused (covers pump-initiated drain on shutdown)
struct RuleSlot {
    cancel: CancellationToken,
    push_request_id: String,
    remove_request_id: Option<String>,
    /// Shared with the forwarder; the periodic stats task reads snapshots
    /// from this for `StatsReport`.
    stats: Arc<RuleStats>,
    /// `true` iff this rule's listen range spans more than one port.
    /// Used by `send_stats_report` to decide whether to emit the
    /// `per_port` proto field — single-port rules always send empty
    /// per-port to preserve the v0.1.0 wire shape.
    is_range: bool,
    /// 007-multi-target-failover (T033): per-target health + global
    /// failover counter. `Some` for multi-target rules, `None` for
    /// single-target — `send_stats_report` keys off this to decide
    /// whether to populate `per_target[]` and `target_failovers_total`.
    multi_target_obs: Option<Arc<crate::forwarder::MultiTargetObservability>>,
    /// 007-multi-target-failover (T038): cached targets list so the
    /// stats reporter can emit per-target host/port/priority alongside
    /// the per-target HealthState snapshot.
    targets_view: Vec<crate::forwarder::MultiTarget>,
}

async fn pump(
    mut session: LiveSession,
    resolver: Arc<crate::resolver::LiveResolver<crate::resolver::HickoryResolver>>,
    cancel: &CancellationToken,
    drain_timeout: Duration,
    stats_report_interval: Duration,
) {
    // 004-udp-forward T031/T061: capture the Welcome-derived UDP knobs
    // so every UDP rule pushed during this session uses the same values.
    // Reconnect refreshes them from the new Welcome (the LiveSession
    // is rebuilt on each connect).
    let udp_max_flows = session.udp_max_flows_per_rule;
    let udp_flow_idle_secs = session.udp_flow_idle_secs;
    let mut rules: HashMap<RuleId, RuleSlot> = HashMap::new();
    let (status_tx, mut status_rx) = mpsc::channel::<RuleStatusEvent>(64);
    let mut stats_tick = tokio::time::interval(stats_report_interval);
    // Skip the immediate first tick (interval fires at t=0).
    stats_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let _ = stats_tick.tick().await;

    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            msg = session.inbound.next() => match msg {
                Some(Ok(server_msg)) => {
                    handle_server_message(server_msg, &mut rules, Arc::clone(&resolver), &status_tx, drain_timeout, udp_max_flows, udp_flow_idle_secs);
                }
                Some(Err(status)) => {
                    warn!(event = "control.stream_error", error = %status);
                    break;
                }
                None => break,
            },
            event = status_rx.recv() => {
                if let Some(evt) = event {
                    forward_status(evt, &mut rules, &session.outbound).await;
                }
            }
            _ = stats_tick.tick() => {
                send_stats_report(&rules, &session.outbound).await;
            }
        }
    }

    // Process shutdown / stream loss: cancel every forwarder and drain their
    // Removed events for `drain_timeout` so the operator gets a final
    // RuleStatus before we go off-air. Forwarders will tear down their own
    // listeners and finish their own drain loops; we're just collecting the
    // tail end of the status echoes.
    for slot in rules.values() {
        slot.cancel.cancel();
    }
    let drain_deadline = tokio::time::sleep(drain_timeout);
    tokio::pin!(drain_deadline);
    while !rules.is_empty() {
        tokio::select! {
            () = &mut drain_deadline => break,
            event = status_rx.recv() => match event {
                Some(evt) => forward_status(evt, &mut rules, &session.outbound).await,
                None => break,
            }
        }
    }
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn handle_server_message(
    msg: ServerMessage,
    rules: &mut HashMap<RuleId, RuleSlot>,
    resolver: Arc<crate::resolver::LiveResolver<crate::resolver::HickoryResolver>>,
    status_tx: &mpsc::Sender<RuleStatusEvent>,
    drain_timeout: Duration,
    udp_max_flows: u32,
    udp_flow_idle_secs: u32,
) {
    let Some(server_message::Payload::RuleUpdate(update)) = msg.payload else {
        // Welcome is consumed before pump; any other variant is ignored.
        return;
    };
    let request_id = update.request_id;
    let action = RuleAction::try_from(update.action).unwrap_or(RuleAction::Unspecified);
    let Some(rule) = update.rule else {
        warn!(
            event = "control.rule_update_missing_rule",
            request_id = %request_id,
        );
        return;
    };
    let rule_id = RuleId(rule.rule_id);

    match action {
        RuleAction::Push => {
            if rules.contains_key(&rule_id) {
                warn!(
                    event = "control.rule_push_duplicate",
                    request_id = %request_id,
                    rule_id = %rule_id,
                );
                return;
            }
            let listen_port = u16::try_from(rule.listen_port).unwrap_or(0);
            let target_port = u16::try_from(rule.target_port).unwrap_or(0);
            // Lift single-port rules into the range path. The server
            // sends `0` (proto3 default) for the *_port_end fields when
            // the rule is single-port; treat that as "no end" and
            // collapse to a `PortRange::single`.
            let listen_end = u16::try_from(rule.listen_port_end)
                .ok()
                .filter(|e| *e != 0)
                .unwrap_or(listen_port);
            let target_end = u16::try_from(rule.target_port_end)
                .ok()
                .filter(|e| *e != 0)
                .unwrap_or(target_port);
            let listen_range = match PortRange::new(listen_port, listen_end) {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        event = "control.rule_push_invalid_range",
                        request_id = %request_id,
                        rule_id = %rule_id,
                        error = %e,
                    );
                    let _ = status_tx.try_send(RuleStatusEvent::Failed {
                        rule_id,
                        reason: "range_invalid".into(),
                    });
                    return;
                }
            };
            let target_range = match PortRange::new(target_port, target_end) {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        event = "control.rule_push_invalid_range",
                        request_id = %request_id,
                        rule_id = %rule_id,
                        error = %e,
                    );
                    let _ = status_tx.try_send(RuleStatusEvent::Failed {
                        rule_id,
                        reason: "range_invalid".into(),
                    });
                    return;
                }
            };
            let cancel = CancellationToken::new();
            // 003-domain-name-forward (T020): classify the operator
            // string into either an IP literal (resolver short-circuit)
            // or a validated DNS hostname (resolver path). The server
            // already rejected malformed values at push time (T021), so
            // a parse failure here means the wire was tampered with —
            // refuse the rule with a structured reason rather than
            // crashing the runtime.
            let target = match forward_core::Target::parse(&rule.target_host) {
                Ok(t) => t,
                Err(e) => {
                    warn!(
                        event = "control.rule_push_invalid_target",
                        request_id = %request_id,
                        rule_id = %rule_id,
                        error = %e,
                    );
                    let _ = status_tx.try_send(RuleStatusEvent::Failed {
                        rule_id,
                        reason: "invalid_target_host".into(),
                    });
                    return;
                }
            };
            let prefer_ipv6 = rule.prefer_ipv6.unwrap_or(false);
            // 004-udp-forward T031: decode the wire `protocol` field.
            // Unknown enum integers (proto3 forward-compat) fall back
            // to TCP — same shape v0.3 clients implicitly assumed.
            let protocol = Protocol::try_from(rule.protocol).unwrap_or(Protocol::Tcp);
            let protocol = match protocol {
                Protocol::Udp => Protocol::Udp,
                _ => Protocol::Tcp,
            };
            // 007-multi-target-failover T022: when the wire `Rule`
            // carries a non-empty `targets` list, pre-parse each
            // host into a `forward_core::Target` so the failover dial
            // loop never reparses. A parse failure on ANY entry
            // refuses the whole rule with `invalid_target_host` —
            // mirrors the single-target validation above.
            let multi_targets = if rule.targets.is_empty() {
                Vec::new()
            } else {
                let mut out: Vec<crate::forwarder::MultiTarget> =
                    Vec::with_capacity(rule.targets.len());
                let mut parse_err: Option<String> = None;
                for (idx, t) in rule.targets.iter().enumerate() {
                    match forward_core::Target::parse(&t.host) {
                        Ok(parsed) => {
                            let port = match u16::try_from(t.port) {
                                Ok(p) if p > 0 => p,
                                _ => {
                                    parse_err = Some(format!("target_invalid_port:{idx}"));
                                    break;
                                }
                            };
                            out.push(crate::forwarder::MultiTarget {
                                spec: forward_core::RuleTarget {
                                    host: t.host.clone(),
                                    port,
                                    priority: t.priority,
                                },
                                target: parsed,
                            });
                        }
                        Err(e) => {
                            warn!(
                                event = "control.rule_push_invalid_target",
                                request_id = %request_id,
                                rule_id = %rule_id,
                                target_index = idx,
                                error = %e,
                            );
                            parse_err = Some("invalid_target_host".to_string());
                            break;
                        }
                    }
                }
                if let Some(reason) = parse_err {
                    let _ = status_tx.try_send(RuleStatusEvent::Failed {
                        rule_id,
                        reason,
                    });
                    return;
                }
                out
            };
            let health_check_interval_secs = if rule.health_check_interval_secs == 0 {
                None
            } else {
                Some(rule.health_check_interval_secs)
            };
            // T033: build the per-target observability for multi-target
            // rules ONCE, here. The same Arc lands in both the
            // failover_path task (mutator) and the RuleSlot below
            // (snapshot reader on the StatsReport tick). For
            // single-target rules we build None so the legacy snapshot
            // path stays byte-identical.
            let multi_target_obs = if multi_targets.is_empty() {
                None
            } else {
                let states: std::sync::Arc<
                    Vec<tokio::sync::Mutex<crate::forwarder::failover::HealthState>>,
                > = std::sync::Arc::new(
                    (0..multi_targets.len())
                        .map(|_| {
                            tokio::sync::Mutex::new(crate::forwarder::failover::HealthState::new())
                        })
                        .collect(),
                );
                Some(std::sync::Arc::new(crate::forwarder::MultiTargetObservability {
                    target_failovers_total: std::sync::Arc::new(
                        std::sync::atomic::AtomicU64::new(0),
                    ),
                    states,
                }))
            };
            let targets_view = multi_targets.clone();
            let multi_target_obs_for_slot = multi_target_obs.clone();
            let client_rule = ClientRule {
                rule_id,
                listen_range,
                target_host: rule.target_host,
                target,
                target_range,
                prefer_ipv6,
                protocol,
                udp_max_flows,
                udp_flow_idle_secs,
                targets: multi_targets,
                health_check_interval_secs,
                multi_target_obs,
            };
            let task_cancel = cancel.clone();
            let task_status_tx = status_tx.clone();
            // Range-aware stats: one per-port slot per port in
            // `listen_range`. Single-port rules get a single-element
            // per-port slot — the wire emit logic in
            // `send_stats_report` strips it for backwards compat.
            let stats = RuleStats::for_range(listen_range);
            let task_stats = Arc::clone(&stats);
            tokio::spawn(async move {
                forwarder::run(
                    client_rule,
                    resolver,
                    task_status_tx,
                    task_cancel,
                    drain_timeout,
                    task_stats,
                )
                .await;
            });
            rules.insert(
                rule_id,
                RuleSlot {
                    cancel,
                    push_request_id: request_id,
                    remove_request_id: None,
                    stats,
                    is_range: listen_end > listen_port,
                    multi_target_obs: multi_target_obs_for_slot,
                    targets_view,
                },
            );
        }
        RuleAction::Remove => {
            if let Some(slot) = rules.get_mut(&rule_id) {
                slot.remove_request_id = Some(request_id);
                slot.cancel.cancel();
            } else {
                warn!(
                    event = "control.rule_remove_unknown",
                    request_id = %request_id,
                    rule_id = %rule_id,
                );
            }
        }
        RuleAction::Unspecified => warn!(
            event = "control.rule_update_unspecified_action",
            request_id = %request_id,
            rule_id = %rule_id,
        ),
    }
}

async fn forward_status(
    evt: RuleStatusEvent,
    rules: &mut HashMap<RuleId, RuleSlot>,
    outbound: &mpsc::Sender<ClientMessage>,
) {
    let (rule_id, outcome, reason, request_id, drop_slot) = match &evt {
        RuleStatusEvent::Activated { rule_id } => {
            let req = rules
                .get(rule_id)
                .map(|s| s.push_request_id.clone())
                .unwrap_or_default();
            (
                *rule_id,
                ActivationOutcome::Activated,
                String::new(),
                req,
                false,
            )
        }
        RuleStatusEvent::Failed { rule_id, reason } => {
            // Failed is terminal — the forwarder never emitted Activated, so
            // there is no listener to keep tracking. Drop the slot.
            let req = rules
                .remove(rule_id)
                .map(|s| s.push_request_id)
                .unwrap_or_default();
            (
                *rule_id,
                ActivationOutcome::Failed,
                reason.clone(),
                req,
                false,
            )
        }
        RuleStatusEvent::Removed { rule_id } => {
            let slot = rules.remove(rule_id);
            let req = slot
                .and_then(|s| s.remove_request_id.or(Some(s.push_request_id)))
                .unwrap_or_default();
            (
                *rule_id,
                ActivationOutcome::Removed,
                String::new(),
                req,
                true,
            )
        }
    };
    let _ = drop_slot; // already handled above

    let status_msg = ClientMessage {
        payload: Some(client_message::Payload::RuleStatus(ProtoRuleStatus {
            request_id,
            rule_id: rule_id.0,
            outcome: outcome as i32,
            reason,
        })),
    };
    if let Err(e) = outbound.send(status_msg).await {
        warn!(
            event = "control.status_send_failed",
            rule_id = %rule_id,
            error = %e,
        );
    }
}

/// Snapshot every active rule's counters and emit a single `StatsReport` on
/// the bidi stream. Sends only when at least one rule exists; an empty report
/// would be wasteful chatter.
/// 007-multi-target-failover T033: build the `per_target[]` array for
/// a multi-target rule. Single-target rules return an empty Vec
/// (invariant I-3 — single-target rules MUST emit `per_target: []`
/// regardless of `?per_target=true`).
fn build_per_target(slot: &RuleSlot) -> Vec<ProtoPerTargetStats> {
    let Some(obs) = slot.multi_target_obs.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(slot.targets_view.len());
    for (idx, t) in slot.targets_view.iter().enumerate() {
        // try_lock — the mutator (failover_path / probe) holds
        // the lock briefly. On contention we drop the snapshot for
        // this target this tick; next tick will pick it up. This
        // keeps the stats report off the data-plane critical path.
        let Ok(state) = obs.states[idx].try_lock() else {
            continue;
        };
        let last_failure_at_unix_ms = state
            .last_failure_at()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let last_success_at_unix_ms = state
            .last_success_at()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let (bytes_in, bytes_out) = state.snapshot_bytes();
        let connections_accepted = state.snapshot_connections();
        out.push(ProtoPerTargetStats {
            index: u32::try_from(idx).unwrap_or(u32::MAX),
            host: t.spec.host.clone(),
            port: u32::from(t.spec.port),
            priority: t.spec.priority,
            health: state.health().as_wire(),
            consecutive_failures: state.consecutive_failures(),
            last_failure_at_unix_ms,
            last_success_at_unix_ms,
            bytes_in,
            bytes_out,
            connections_accepted,
        });
    }
    out
}

async fn send_stats_report(
    rules: &HashMap<RuleId, RuleSlot>,
    outbound: &mpsc::Sender<ClientMessage>,
) {
    use std::sync::atomic::Ordering;
    if rules.is_empty() {
        return;
    }
    let stats: Vec<ProtoRuleStats> = rules
        .iter()
        .map(|(rule_id, slot)| {
            let (bin, bout, active) = slot.stats.snapshot();
            // Per-port detail (002-port-range-forward, T042). Range
            // rules emit one slot per listen port; single-port rules
            // emit empty for wire-shape stability with v0.1.0.
            let per_port = if slot.is_range {
                slot.stats
                    .snapshot_per_port_with_udp()
                    .into_iter()
                    .map(|(port, bin, bout, active, dgin, dgout)| ProtoPerPortStats {
                        listen_port: u32::from(port),
                        bytes_in: bin,
                        bytes_out: bout,
                        active_connections: active,
                        datagrams_in: dgin,
                        datagrams_out: dgout,
                    })
                    .collect()
            } else {
                Vec::new()
            };
            ProtoRuleStats {
                rule_id: rule_id.0,
                bytes_in: bin,
                bytes_out: bout,
                active_connections: active,
                per_port,
                // 003-domain-name-forward T048: monotonic per-rule
                // DNS-failure counter (FR-008). For IP-target rules
                // this is always 0 (resolver layer is short-circuited);
                // proto field 6 with default-zero stays absent on the
                // wire (verified by `dns_wire_compat::v0_2_0_rule_stats_byte_compatible_when_dns_failures_zero`).
                dns_failures: slot.stats.snapshot_dns_failures(),
                // 004-udp-forward T032: UDP counters. For TCP rules
                // these all stay at the proto3 default-zero (the UDP
                // helpers are never called) so
                // `udp_wire_compat::tcp_rule_stats_byte_compatible_when_udp_fields_zero`
                // (T005) holds for legacy traffic.
                datagrams_in: slot.stats.snapshot_datagrams_in(),
                datagrams_out: slot.stats.snapshot_datagrams_out(),
                active_flows: slot.stats.snapshot_active_flows(),
                flows_dropped_overflow: slot.stats.snapshot_flows_dropped_overflow(),
                // 007-multi-target-failover T033: multi-target rules
                // emit per-target snapshots from the shared
                // observability handle. Single-target rules emit 0 /
                // empty per the wire-compat invariant I-3.
                target_failovers_total: slot
                    .multi_target_obs
                    .as_ref()
                    .map_or(0, |o| o.target_failovers_total.load(Ordering::Relaxed)),
                per_target: build_per_target(slot),
            }
        })
        .collect();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let msg = ClientMessage {
        payload: Some(client_message::Payload::StatsReport(StatsReport {
            sent_at_unix_ms: now_ms,
            stats,
        })),
    };
    if let Err(e) = outbound.send(msg).await {
        warn!(event = "control.stats_send_failed", error = %e);
    }
}
