//! Control-plane connection lifecycle: TLS dial, Welcome handshake,
//! reconnect with full-jitter exponential backoff, `RuleUpdate` dispatch
//! into the forwarder, and `RuleStatus` echoing on the outbound channel.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use forward_core::{PortRange, RuleId};
use forward_proto::v1::{
    ActivationOutcome, ClientMessage, Hello, PerPortStats as ProtoPerPortStats, RuleAction,
    RuleStats as ProtoRuleStats, RuleStatus as ProtoRuleStatus, ServerMessage, StatsReport,
    client_message, control_client::ControlClient, server_message,
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
use tracing::{info, warn};

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
    );

    Ok(LiveSession {
        outbound: outbound_tx,
        inbound: Box::pin(inbound),
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
}

async fn pump(
    mut session: LiveSession,
    cancel: &CancellationToken,
    drain_timeout: Duration,
    stats_report_interval: Duration,
) {
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
                    handle_server_message(server_msg, &mut rules, &status_tx, drain_timeout);
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

#[allow(clippy::too_many_lines)]
fn handle_server_message(
    msg: ServerMessage,
    rules: &mut HashMap<RuleId, RuleSlot>,
    status_tx: &mpsc::Sender<RuleStatusEvent>,
    drain_timeout: Duration,
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
            let client_rule = ClientRule {
                rule_id,
                listen_range,
                target_host: rule.target_host,
                target_range,
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
async fn send_stats_report(
    rules: &HashMap<RuleId, RuleSlot>,
    outbound: &mpsc::Sender<ClientMessage>,
) {
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
                    .snapshot_per_port()
                    .into_iter()
                    .map(|(port, bin, bout, active)| ProtoPerPortStats {
                        listen_port: u32::from(port),
                        bytes_in: bin,
                        bytes_out: bout,
                        active_connections: active,
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
