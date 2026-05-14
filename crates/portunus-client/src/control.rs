//! Control-plane connection lifecycle: TLS dial, Welcome handshake,
//! reconnect with full-jitter exponential backoff, `RuleUpdate` dispatch
//! into the forwarder, and `RuleStatus` echoing on the outbound channel.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use portunus_core::{PortRange, RuleId};
use portunus_proto::v1::{
    ActivationOutcome, ClientMessage, Hello, OwnerRateLimitAction,
    PerPortStats as ProtoPerPortStats, PerTargetStats as ProtoPerTargetStats, Protocol, RuleAction,
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
use tracing::{error, info, warn};

use crate::bundle::CredentialBundle;
use crate::forwarder::stats::RuleStats;
use crate::forwarder::{self, ClientRule, RuleStatusEvent};
use crate::port_groups::PortGroupManager;

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
        payload: Some(portunus_proto::v1::client_message::Payload::Hello(Hello {
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
            Some(portunus_proto::v1::server_message::Payload::Welcome(w)) => w,
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

    // 011-rate-limiting-qos T031: per-owner registry lives for the
    // entire process lifetime — owner caps are an operator-driven
    // tenant-isolation control and MUST survive control-plane
    // reconnects. The server re-pushes the current owner state on
    // reconnect (mirroring the rule-replay convention from v0.1.0)
    // so a stale entry would be overwritten on the next push.
    let owner_rate_limit_scope =
        Arc::new(crate::forwarder::rate_limit::scope::OwnerRateLimitScopeManager::new());
    let rule_rate_limit_scope =
        Arc::new(crate::forwarder::rate_limit::scope::RateLimitScopeManager::new());
    // 013-traffic-quotas D2: per-(user, client) quota registry lives
    // for the entire process lifetime. Reconnect replay (C5) re-pushes
    // every quota row BEFORE any rule, so a stale entry would be
    // overwritten on the next push and a removed quota survives only
    // until the next reconnect.
    let quota_scope = Arc::new(crate::forwarder::quota::scope::QuotaScopeManager::new());
    // 011-rate-limiting-qos T032: per-owner stats registry parallels
    // the limiter registry. Aggregation across rules sharing the same
    // owner happens here — multiple rules call `get_or_create` for
    // the same OwnerId and increment one shared counter set, which
    // surfaces as a single `OwnerRateLimitStats` entry on
    // `StatsReport.owner_rate_limit_stats` (FR-014). Lives for the
    // process lifetime so cumulative counters persist across
    // reconnects.
    let owner_rate_limit_stats_registry =
        Arc::new(crate::forwarder::rate_limit::scope::OwnerRateLimitStatsRegistry::new());

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
                let pump_context = PumpContext {
                    resolver: Arc::clone(&resolver),
                    rule_rate_limit_scope: Arc::clone(&rule_rate_limit_scope),
                    owner_rate_limit_scope: Arc::clone(&owner_rate_limit_scope),
                    owner_rate_limit_stats: Arc::clone(&owner_rate_limit_stats_registry),
                    quota_scope: Arc::clone(&quota_scope),
                    drain_timeout: cfg.drain_timeout,
                    stats_report_interval: cfg.stats_report_interval,
                };
                pump(session, pump_context, &cancel).await;
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
    /// 011-rate-limiting-qos (T022): per-rule rate-limit accumulator.
    /// `Some` for capped rules; the periodic stats reporter calls
    /// `drain_to_proto()` and stamps the result onto
    /// `RuleStats.rate_limit`. `None` for uncapped rules — the wire
    /// keeps proto3 default-stripping semantics so v0.10 readers see
    /// an unchanged byte stream (T005).
    rate_limit_stats: Option<Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>>,
    /// 011-rate-limiting-qos: dynamic per-rule limiter handle. The
    /// stats drainer snapshots the current limiter so hot-reload swaps
    /// are reflected in later reports.
    rate_limit_limiter: Option<Arc<crate::forwarder::rate_limit::scope::RuleRateLimitHandle>>,
}

struct PumpContext {
    resolver: Arc<crate::resolver::LiveResolver<crate::resolver::HickoryResolver>>,
    rule_rate_limit_scope: Arc<crate::forwarder::rate_limit::scope::RateLimitScopeManager>,
    owner_rate_limit_scope: Arc<crate::forwarder::rate_limit::scope::OwnerRateLimitScopeManager>,
    owner_rate_limit_stats: Arc<crate::forwarder::rate_limit::scope::OwnerRateLimitStatsRegistry>,
    /// 013-traffic-quotas D2: per-(user, client) quota registry.
    quota_scope: Arc<crate::forwarder::quota::scope::QuotaScopeManager>,
    drain_timeout: Duration,
    stats_report_interval: Duration,
}

async fn pump(mut session: LiveSession, context: PumpContext, cancel: &CancellationToken) {
    // 004-udp-forward T031/T061: capture the Welcome-derived UDP knobs
    // so every UDP rule pushed during this session uses the same values.
    // Reconnect refreshes them from the new Welcome (the LiveSession
    // is rebuilt on each connect).
    let udp_max_flows = session.udp_max_flows_per_rule;
    let udp_flow_idle_secs = session.udp_flow_idle_secs;
    let mut rules: HashMap<RuleId, RuleSlot> = HashMap::new();
    let mut port_groups = PortGroupManager::new();
    let (status_tx, mut status_rx) = mpsc::channel::<RuleStatusEvent>(64);
    let mut stats_tick = tokio::time::interval(context.stats_report_interval);
    // Skip the immediate first tick (interval fires at t=0).
    stats_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let _ = stats_tick.tick().await;

    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            msg = session.inbound.next() => match msg {
                Some(Ok(server_msg)) => {
                    handle_server_message(
                        server_msg,
                        &mut rules,
                        &mut port_groups,
                        Arc::clone(&context.resolver),
                        &context.rule_rate_limit_scope,
                        &context.owner_rate_limit_scope,
                        &context.owner_rate_limit_stats,
                        &context.quota_scope,
                        &status_tx,
                        context.drain_timeout,
                        udp_max_flows,
                        udp_flow_idle_secs,
                    );
                }
                Some(Err(status)) => {
                    warn!(event = "control.stream_error", error = %status);
                    break;
                }
                None => break,
            },
            event = status_rx.recv() => {
                if let Some(evt) = event {
                    relay_status(evt, &mut rules, &session.outbound).await;
                }
            }
            _ = stats_tick.tick() => {
                send_stats_report(&rules, &port_groups, &context.owner_rate_limit_stats, &session.outbound).await;
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
    port_groups.shutdown();
    let drain_deadline = tokio::time::sleep(context.drain_timeout);
    tokio::pin!(drain_deadline);
    while !rules.is_empty() {
        tokio::select! {
            () = &mut drain_deadline => break,
            event = status_rx.recv() => match event {
                Some(evt) => relay_status(evt, &mut rules, &session.outbound).await,
                None => break,
            }
        }
    }
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
fn handle_server_message(
    msg: ServerMessage,
    rules: &mut HashMap<RuleId, RuleSlot>,
    port_groups: &mut PortGroupManager,
    resolver: Arc<crate::resolver::LiveResolver<crate::resolver::HickoryResolver>>,
    rule_rate_limit_scope: &Arc<crate::forwarder::rate_limit::scope::RateLimitScopeManager>,
    owner_rate_limit_scope: &Arc<crate::forwarder::rate_limit::scope::OwnerRateLimitScopeManager>,
    owner_rate_limit_stats: &crate::forwarder::rate_limit::scope::OwnerRateLimitStatsRegistry,
    quota_scope: &crate::forwarder::quota::scope::QuotaScopeManager,
    status_tx: &mpsc::Sender<RuleStatusEvent>,
    drain_timeout: Duration,
    udp_max_flows: u32,
    udp_flow_idle_secs: u32,
) {
    let update = match msg.payload {
        Some(server_message::Payload::RuleUpdate(u)) => u,
        // 011-rate-limiting-qos T031: absorb owner-cap server pushes
        // and update the process-lifetime registry. Action SET installs
        // (or hot-reload-swaps with carryover); REMOVE drops the entry
        // so the layered cascade short-circuits the owner branch on
        // the next accept. Per FR-013 the registry is the single
        // source of truth for the per-owner ceiling that binds before
        // any per-rule cap.
        Some(server_message::Payload::OwnerRateLimitUpdate(update)) => {
            apply_owner_rate_limit_update(update, owner_rate_limit_scope);
            return;
        }
        // 013-traffic-quotas D3: per-(user, client) quota state push.
        // SET installs or hot-swaps the QuotaHandle's atomic budget;
        // REMOVE drops the registry entry so the data-plane hooks
        // short-circuit the consume branch. Reconnect replay (C5)
        // delivers these BEFORE the first RuleUpdate.
        Some(server_message::Payload::TrafficQuotaUpdate(update)) => {
            apply_traffic_quota_update(update, quota_scope);
            return;
        }
        // Welcome is consumed before pump; any other variant is ignored.
        _ => return,
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
    let incoming_rate_limit = rule
        .rate_limit
        .as_ref()
        .map(|envelope| portunus_core::RateLimit {
            bandwidth_in_bps: envelope.bandwidth_in_bps,
            bandwidth_out_bps: envelope.bandwidth_out_bps,
            new_connections_per_sec: envelope.new_connections_per_sec,
            concurrent_connections: envelope.concurrent_connections,
            bandwidth_in_burst: envelope.bandwidth_in_burst,
            bandwidth_out_burst: envelope.bandwidth_out_burst,
            new_connections_burst: envelope.new_connections_burst,
        });

    match action {
        RuleAction::Push => {
            if let Some(slot) = rules.get_mut(&rule_id) {
                rule_rate_limit_scope.update(rule_id, incoming_rate_limit.as_ref());
                slot.push_request_id = request_id;
                slot.rate_limit_limiter = incoming_rate_limit.as_ref().map(|_| {
                    Arc::new(
                        crate::forwarder::rate_limit::scope::RuleRateLimitHandle::new(
                            rule_id,
                            Arc::clone(rule_rate_limit_scope),
                        ),
                    )
                });
                if slot.rate_limit_stats.is_none() && incoming_rate_limit.is_some() {
                    slot.rate_limit_stats = Some(Arc::new(
                        crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator::new(),
                    ));
                }
                let _ = status_tx.try_send(RuleStatusEvent::Activated { rule_id });
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
            let target = match portunus_core::Target::parse(&rule.target_host) {
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
            // host into a `portunus_core::Target` so the failover dial
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
                    match portunus_core::Target::parse(&t.host) {
                        Ok(parsed) => {
                            let port = match u16::try_from(t.port) {
                                Ok(p) if p > 0 => p,
                                _ => {
                                    parse_err = Some(format!("target_invalid_port:{idx}"));
                                    break;
                                }
                            };
                            out.push(crate::forwarder::MultiTarget {
                                spec: portunus_core::RuleTarget {
                                    host: t.host.clone(),
                                    port,
                                    priority: t.priority,
                                    proxy_protocol: t
                                        .proxy_protocol
                                        .and_then(|v| {
                                            portunus_proto::v1::ProxyProtocolVersion::try_from(v)
                                                .ok()
                                        })
                                        .and_then(|mode| match mode {
                                            portunus_proto::v1::ProxyProtocolVersion::V1 => {
                                                Some(portunus_core::ProxyProtocolVersion::V1)
                                            }
                                            portunus_proto::v1::ProxyProtocolVersion::V2 => {
                                                Some(portunus_core::ProxyProtocolVersion::V2)
                                            }
                                            portunus_proto::v1::ProxyProtocolVersion::Unspecified => None,
                                        }),
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
                    let _ = status_tx.try_send(RuleStatusEvent::Failed { rule_id, reason });
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
                Some(std::sync::Arc::new(
                    crate::forwarder::MultiTargetObservability {
                        target_failovers_total: std::sync::Arc::new(
                            std::sync::atomic::AtomicU64::new(0),
                        ),
                        states,
                    },
                ))
            };
            let targets_view = multi_targets.clone();
            let multi_target_obs_for_slot = multi_target_obs.clone();
            // 011-rate-limiting-qos T019: build the per-rule limiter +
            // stats accumulator from the wire envelope. Both stay
            // `None` for uncapped rules so the no-cap fast path is a
            // null check on each accept; capped rules pay one Arc
            // clone per spawned accept loop.
            let rate_limit_envelope = incoming_rate_limit.clone();
            rule_rate_limit_scope.install(rule_id, rate_limit_envelope.as_ref());
            let rate_limit_limiter = rate_limit_envelope.as_ref().map(|_| {
                Arc::new(
                    crate::forwarder::rate_limit::scope::RuleRateLimitHandle::new(
                        rule_id,
                        Arc::clone(rule_rate_limit_scope),
                    ),
                )
            });
            let rate_limit_stats = rate_limit_envelope.as_ref().map(|_| {
                std::sync::Arc::new(
                    crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator::new(),
                )
            });
            // 011-rate-limiting-qos T031: build this rule's dynamic
            // per-owner limiter handle. The handle snapshots the
            // current owner limiter from the process-lifetime registry
            // on each admission / bandwidth acquire, so later
            // OwnerRateLimitUpdate pushes affect already-activated
            // rules without requiring a rule re-push.
            let owner_id_str = rule.owner_id.as_ref().filter(|s| !s.is_empty()).cloned();
            let owner_rate_limit = owner_id_str.as_ref().map(|owner_id| {
                Arc::new(
                    crate::forwarder::rate_limit::scope::OwnerRateLimitHandle::new(
                        crate::forwarder::rate_limit::scope::OwnerId::new(owner_id.clone()),
                        Arc::clone(owner_rate_limit_scope),
                    ),
                )
            });
            // 011-rate-limiting-qos T032: per-owner stats are looked
            // up from the shared registry so multiple rules sharing
            // the same owner aggregate into one accumulator. We create
            // the accumulator whenever `owner_id` is present so later
            // owner-cap installs have somewhere to record throttle /
            // reject events without rebuilding the rule.
            let rule_owner_rate_limit_stats = owner_id_str.as_ref().map(|owner_id| {
                owner_rate_limit_stats.get_or_create(
                    &crate::forwarder::rate_limit::scope::OwnerId::new(owner_id.clone()),
                )
            });
            // 013-traffic-quotas E2: resolve the per-(user, client)
            // quota handle from the process-lifetime registry. None
            // when the rule is unowned OR no quota has been installed
            // for this owner yet — copy_uncapped then stays on the
            // byte-identical splice / userspace fast path. Reconnect
            // replay (C5) re-installs every quota BEFORE any rule, so
            // an owner_id present here either has a quota or genuinely
            // has none on the server.
            let rule_quota = owner_id_str
                .as_ref()
                .and_then(|uid| quota_scope.lookup(uid));
            // Hold a clone of the rate-limit handles for the RuleSlot
            // (the periodic stats reporter and SNI/legacy paths both
            // need to keep observing them after `client_rule` moves
            // into the forwarder spawn).
            let slot_rate_limit_limiter = rate_limit_limiter.clone();
            let slot_rate_limit_stats = rate_limit_stats.clone();
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
                // 009-tls-sni-routing T039: thread the wire-side
                // sni_pattern through unchanged. The legacy
                // forwarder ignores it; the SNI listener (T040)
                // reads it when the port-group manager (T042)
                // routes this rule into the SNI dispatch path.
                sni_pattern: rule.sni_pattern.clone(),
                // 011-rate-limiting-qos T019: per-rule rate-limit
                // limiter + stats. None keeps the byte-stable
                // v0.10 forwarding path.
                rate_limit: rate_limit_limiter,
                rate_limit_stats,
                // 011-rate-limiting-qos T031: per-owner limiter + stats
                // resolved from the process-lifetime registry via
                // Rule.owner_id (additive wire field 13). For rules
                // pushed without a v0.11 cap signal the server omits
                // owner_id, the lookup yields None, and the layered
                // cascade short-circuits the owner branch — preserving
                // the v0.10 forwarding path byte-for-byte.
                owner_rate_limit,
                owner_rate_limit_stats: rule_owner_rate_limit_stats,
                quota: rule_quota,
            };
            let task_cancel = cancel.clone();
            let task_status_tx = status_tx.clone();
            // 009-tls-sni-routing T043: route TCP single-port rules
            // through the PortGroupManager when EITHER the rule
            // carries `sni_pattern` OR the port already runs in SNI
            // mode (so a `sni_pattern = None` fallback joins the
            // existing listener). All other shapes (UDP, port-range,
            // and pure-legacy single-port) keep the v0.7 byte-stable
            // per-rule spawn path.
            let routes_via_sni = matches!(protocol, Protocol::Tcp)
                && listen_end == listen_port
                && (client_rule.sni_pattern.is_some() || port_groups.is_sni_port(listen_port));
            if routes_via_sni {
                match port_groups.apply_push(client_rule.clone(), Arc::clone(&resolver)) {
                    Ok(stats) => {
                        rules.insert(
                            rule_id,
                            RuleSlot {
                                cancel,
                                push_request_id: request_id,
                                remove_request_id: None,
                                stats,
                                is_range: false,
                                multi_target_obs: multi_target_obs_for_slot,
                                targets_view,
                                rate_limit_stats: slot_rate_limit_stats.clone(),
                                rate_limit_limiter: slot_rate_limit_limiter.clone(),
                            },
                        );
                        // Emit Activated synthetically — the SNI
                        // listener bound + table populated, so from
                        // the operator's perspective the rule is live.
                        let _ = status_tx.try_send(RuleStatusEvent::Activated { rule_id });
                    }
                    Err(e) => {
                        warn!(
                            event = "control.sni_push_failed",
                            request_id = %request_id,
                            rule_id = %rule_id,
                            error = ?e,
                        );
                        let reason = match e {
                            crate::port_groups::PortGroupError::BindFailed(io) => {
                                format!("bind_failed:{io}")
                            }
                            crate::port_groups::PortGroupError::ModeChangeUnsupported => {
                                "mode_change_unsupported".into()
                            }
                            crate::port_groups::PortGroupError::DuplicateRuleId(_) => {
                                "duplicate_rule_id".into()
                            }
                            crate::port_groups::PortGroupError::UnknownRuleId(_) => {
                                "unknown_rule_id".into()
                            }
                        };
                        let _ = status_tx.try_send(RuleStatusEvent::Failed { rule_id, reason });
                    }
                }
                return;
            }

            // Legacy / multi-target / UDP / range path — unchanged.
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
                    rate_limit_stats: slot_rate_limit_stats,
                    rate_limit_limiter: slot_rate_limit_limiter,
                },
            );
        }
        RuleAction::Remove => {
            // 009-tls-sni-routing T043: ask the manager to drop the
            // rule's SNI slot first. `apply_remove` is a no-op for
            // legacy rules (UnknownRuleId), so swallowing the error
            // is correct. The slot.cancel below still fires for
            // rules on the legacy path; for SNI rules the cancel
            // is wired to a dummy token (the listener task is owned
            // by the manager) so the same cleanup path works for
            // the operator-visible Removed event.
            let _ = port_groups.apply_remove(rule_id);
            if let Some(slot) = rules.get_mut(&rule_id) {
                slot.remove_request_id = Some(request_id);
                slot.cancel.cancel();
                // Synthesise Removed for the SNI path — the legacy
                // forwarder emits Removed itself, but SNI listeners
                // run inside the manager and have no direct status
                // channel. We can't tell here whether `rule_id` is
                // SNI vs legacy without a reverse index, so we send
                // Removed unconditionally; the legacy path's own
                // Removed event is idempotent in `relay_status`.
                let _ = status_tx.try_send(RuleStatusEvent::Removed { rule_id });
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

/// 011-rate-limiting-qos T031: apply a single `OwnerRateLimitUpdate`
/// server-push to the process-lifetime owner-cap registry. SET uses
/// `update` so an existing entry hot-reload-swaps the bucket while
/// preserving live `tokens` / `last_refill` / `active_connections`
/// (R-008); a fresh `(client, owner)` falls through to a from-envelope
/// build. REMOVE drops the entry idempotently so the layered cascade
/// short-circuits the owner branch.
/// 013-traffic-quotas D3: apply a single `TrafficQuotaUpdate` server
/// push. SET threads the wire `TrafficQuotaState` into
/// `QuotaScopeManager::install`, which atomically `replace()`s any
/// existing entry's `QuotaHandle` state in-place — in-flight
/// forwarders observe the new budget via the shared `Arc`. REMOVE
/// drops the registry entry idempotently.
fn apply_traffic_quota_update(
    update: portunus_proto::v1::TrafficQuotaUpdate,
    quota_scope: &crate::forwarder::quota::scope::QuotaScopeManager,
) {
    use portunus_proto::v1::TrafficQuotaAction;
    let action =
        TrafficQuotaAction::try_from(update.action).unwrap_or(TrafficQuotaAction::Unspecified);
    match action {
        TrafficQuotaAction::Set => {
            let Some(state) = update.state else {
                warn!(
                    event = "control.traffic_quota_update_set_without_state",
                    user = %update.user_id,
                    client = %update.client_name,
                );
                return;
            };
            let qs = crate::forwarder::quota::QuotaState {
                monthly_bytes: state.monthly_bytes,
                budget_remaining_bytes: state.budget_remaining_bytes,
                exhausted: state.exhausted,
            };
            quota_scope.install(&update.user_id, &update.client_name, qs);
            info!(
                event = "control.traffic_quota_set",
                user = %update.user_id,
                client = %update.client_name,
                remaining = state.budget_remaining_bytes,
                exhausted = state.exhausted,
            );
        }
        TrafficQuotaAction::Remove => {
            quota_scope.remove(&update.user_id);
            info!(
                event = "control.traffic_quota_remove",
                user = %update.user_id,
                client = %update.client_name,
            );
        }
        TrafficQuotaAction::Unspecified => warn!(
            event = "control.traffic_quota_unspecified_action",
            user = %update.user_id,
            client = %update.client_name,
        ),
    }
}

fn apply_owner_rate_limit_update(
    update: portunus_proto::v1::OwnerRateLimitUpdate,
    owner_rate_limit_scope: &crate::forwarder::rate_limit::scope::OwnerRateLimitScopeManager,
) {
    use crate::forwarder::rate_limit::scope::OwnerId;
    let owner_id = OwnerId::new(update.owner_id.clone());
    let action =
        OwnerRateLimitAction::try_from(update.action).unwrap_or(OwnerRateLimitAction::Unspecified);
    match action {
        OwnerRateLimitAction::Set => {
            let envelope = update
                .rate_limit
                .as_ref()
                .map(|p| portunus_core::RateLimit {
                    bandwidth_in_bps: p.bandwidth_in_bps,
                    bandwidth_out_bps: p.bandwidth_out_bps,
                    new_connections_per_sec: p.new_connections_per_sec,
                    concurrent_connections: p.concurrent_connections,
                    bandwidth_in_burst: p.bandwidth_in_burst,
                    bandwidth_out_burst: p.bandwidth_out_burst,
                    new_connections_burst: p.new_connections_burst,
                });
            owner_rate_limit_scope.update(&owner_id, envelope.as_ref());
            info!(
                event = "control.owner_rate_limit_set",
                client_name = %update.client_name,
                owner_id = %owner_id,
                has_caps = envelope.is_some(),
            );
        }
        OwnerRateLimitAction::Remove => {
            owner_rate_limit_scope.remove(&owner_id);
            info!(
                event = "control.owner_rate_limit_remove",
                client_name = %update.client_name,
                owner_id = %owner_id,
            );
        }
        OwnerRateLimitAction::Unspecified => warn!(
            event = "control.owner_rate_limit_unspecified_action",
            client_name = %update.client_name,
            owner_id = %owner_id,
        ),
    }
}

async fn relay_status(
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
    port_groups: &PortGroupManager,
    owner_rate_limit_stats: &crate::forwarder::rate_limit::scope::OwnerRateLimitStatsRegistry,
    outbound: &mpsc::Sender<ClientMessage>,
) {
    use std::sync::atomic::Ordering;
    if rules.is_empty() && !port_groups.has_any_listener() {
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
                // 009-tls-sni-routing T077: per-rule SNI hit counters.
                // Legacy plain-TCP rules and UDP rules see all three at
                // 0 (the listener never bumps them) → proto3 default-
                // stripping keeps the wire shape byte-identical with
                // v0.8 (verified by
                // sni_wire_compat::t008_rule_stats_sni_counters_zero_omits_tags).
                sni_route_exact_total: slot.stats.sni_route_exact_total.load(Ordering::Relaxed),
                sni_route_wildcard_total: slot
                    .stats
                    .sni_route_wildcard_total
                    .load(Ordering::Relaxed),
                sni_route_fallback_total: slot
                    .stats
                    .sni_route_fallback_total
                    .load(Ordering::Relaxed),
                // 011-rate-limiting-qos T019/T022: drain the per-rule
                // accumulator into the wire field. Returns `None` for
                // uncapped rules (or capped rules whose counters are
                // all still zero) so v0.10 wire shape stays byte-
                // identical. Capped rules with any reject / throttle
                // event get a populated payload; the gauge is mirrored
                // from the limiter's source-of-truth atomic.
                rate_limit: slot.rate_limit_stats.as_ref().and_then(|acc| {
                    if let Some(limiter) = slot.rate_limit_limiter.as_ref() {
                        acc.set_active_connections(limiter.active_connections());
                    }
                    acc.drain_to_proto()
                }),
            }
        })
        .collect();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let sni_listener_stats = port_groups.snapshot_listener_stats();
    let msg = ClientMessage {
        payload: Some(client_message::Payload::StatsReport(StatsReport {
            sent_at_unix_ms: now_ms,
            stats,
            // 009-tls-sni-routing T078: per-listener SNI counters
            // (miss, parse_failures) snapshotted from the manager.
            // Empty when no SNI listener has bound yet — proto3
            // default-stripping keeps wire byte-stable with v0.8.
            sni_listener_stats,
            // 011-rate-limiting-qos T032: drain per-owner counters
            // from the shared registry. Owners whose accumulators are
            // empty (no event ever fired and gauge zero) are skipped
            // by `drain_to_proto`, so a deployment with no owner caps
            // emits an empty Vec → proto3 default-stripping keeps the
            // wire byte-identical with v0.10 (validated by
            // rate_limit_wire_compat::t005_v010_stats_report_byte_identical_when_owner_stats_empty).
            owner_rate_limit_stats: owner_rate_limit_stats.drain_to_proto(),
        })),
    };
    if let Err(e) = outbound.send(msg).await {
        warn!(event = "control.stats_send_failed", error = %e);
    }
}

#[cfg(test)]
mod tests {
    //! 011-rate-limiting-qos T031: control-plane absorption tests for
    //! the `OwnerRateLimitUpdate` server-push variant.

    use super::*;
    use crate::forwarder::rate_limit::scope::{OwnerId, OwnerRateLimitScopeManager};
    use portunus_proto::v1::{OwnerRateLimitAction, OwnerRateLimitUpdate, RateLimit};

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

    #[test]
    fn t031_set_installs_owner_limiter() {
        let mgr = OwnerRateLimitScopeManager::new();
        let update = OwnerRateLimitUpdate {
            client_name: "edge-01".into(),
            owner_id: "alice".into(),
            rate_limit: Some(full_envelope()),
            action: OwnerRateLimitAction::Set as i32,
        };
        apply_owner_rate_limit_update(update, &mgr);
        assert!(mgr.get(&OwnerId::new("alice")).is_some());
    }

    #[test]
    fn t031_remove_drops_owner_limiter() {
        let mgr = OwnerRateLimitScopeManager::new();
        // Pre-install via SET so REMOVE has something to clear.
        apply_owner_rate_limit_update(
            OwnerRateLimitUpdate {
                client_name: "edge-01".into(),
                owner_id: "alice".into(),
                rate_limit: Some(full_envelope()),
                action: OwnerRateLimitAction::Set as i32,
            },
            &mgr,
        );
        assert!(mgr.get(&OwnerId::new("alice")).is_some());

        apply_owner_rate_limit_update(
            OwnerRateLimitUpdate {
                client_name: "edge-01".into(),
                owner_id: "alice".into(),
                rate_limit: None,
                action: OwnerRateLimitAction::Remove as i32,
            },
            &mgr,
        );
        assert!(mgr.get(&OwnerId::new("alice")).is_none());
    }

    #[test]
    fn t031_remove_idempotent_when_owner_unknown() {
        // REMOVE for an owner with no installed limiter must not panic
        // — the registry's own `remove` is idempotent, but T031's
        // wrapper must not assume prior installation.
        let mgr = OwnerRateLimitScopeManager::new();
        apply_owner_rate_limit_update(
            OwnerRateLimitUpdate {
                client_name: "edge-01".into(),
                owner_id: "ghost".into(),
                rate_limit: None,
                action: OwnerRateLimitAction::Remove as i32,
            },
            &mgr,
        );
        assert!(mgr.is_empty());
    }

    #[test]
    fn t031_set_with_no_envelope_clears_owner() {
        // SET with rate_limit = None means "this owner is uncapped".
        // The wrapper translates that to a registry update that drops
        // the entry so the layered cascade short-circuits the owner
        // branch on the next accept.
        let mgr = OwnerRateLimitScopeManager::new();
        apply_owner_rate_limit_update(
            OwnerRateLimitUpdate {
                client_name: "edge-01".into(),
                owner_id: "alice".into(),
                rate_limit: Some(full_envelope()),
                action: OwnerRateLimitAction::Set as i32,
            },
            &mgr,
        );
        assert!(mgr.get(&OwnerId::new("alice")).is_some());

        apply_owner_rate_limit_update(
            OwnerRateLimitUpdate {
                client_name: "edge-01".into(),
                owner_id: "alice".into(),
                rate_limit: None,
                action: OwnerRateLimitAction::Set as i32,
            },
            &mgr,
        );
        assert!(mgr.get(&OwnerId::new("alice")).is_none());
    }

    #[test]
    fn t031_set_replaces_owner_limiter_with_carryover() {
        // Subsequent SET with a different cap value rebuilds the
        // limiter via OwnerRateLimitScopeManager::update, which
        // preserves live state (R-008). We assert installation
        // succeeds — the carryover semantics themselves are pinned by
        // scope.rs's own unit tests.
        let mgr = OwnerRateLimitScopeManager::new();
        apply_owner_rate_limit_update(
            OwnerRateLimitUpdate {
                client_name: "edge-01".into(),
                owner_id: "alice".into(),
                rate_limit: Some(full_envelope()),
                action: OwnerRateLimitAction::Set as i32,
            },
            &mgr,
        );
        let first = mgr.get(&OwnerId::new("alice")).unwrap();

        let mut tighter = full_envelope();
        tighter.concurrent_connections = Some(2);
        apply_owner_rate_limit_update(
            OwnerRateLimitUpdate {
                client_name: "edge-01".into(),
                owner_id: "alice".into(),
                rate_limit: Some(tighter),
                action: OwnerRateLimitAction::Set as i32,
            },
            &mgr,
        );
        let second = mgr.get(&OwnerId::new("alice")).unwrap();
        assert!(
            !Arc::ptr_eq(&first, &second),
            "SET on existing owner must allocate a fresh Arc",
        );
    }

    #[test]
    fn t031_unspecified_action_is_noop() {
        let mgr = OwnerRateLimitScopeManager::new();
        apply_owner_rate_limit_update(
            OwnerRateLimitUpdate {
                client_name: "edge-01".into(),
                owner_id: "alice".into(),
                rate_limit: Some(full_envelope()),
                action: OwnerRateLimitAction::Unspecified as i32,
            },
            &mgr,
        );
        assert!(mgr.is_empty());
    }

    // 013-traffic-quotas D3 tests --------------------------------------------

    use crate::forwarder::quota::scope::QuotaScopeManager;
    use portunus_proto::v1::{TrafficQuotaAction, TrafficQuotaState, TrafficQuotaUpdate};

    fn quota_state(monthly: i64, remaining: i64, exhausted: bool) -> TrafficQuotaState {
        TrafficQuotaState {
            monthly_bytes: monthly,
            budget_remaining_bytes: remaining,
            period_started_at_unix_sec: 0,
            period_ends_at_unix_sec: 0,
            exhausted,
        }
    }

    #[test]
    fn d3_set_installs_quota_handle() {
        let mgr = QuotaScopeManager::new();
        let update = TrafficQuotaUpdate {
            request_id: "r1".into(),
            user_id: "alice".into(),
            client_name: "edge-01".into(),
            action: TrafficQuotaAction::Set as i32,
            state: Some(quota_state(1_000, 750, false)),
        };
        apply_traffic_quota_update(update, &mgr);
        let h = mgr.lookup("alice").expect("installed");
        assert_eq!(h.remaining(), 750);
        assert!(!h.is_exhausted());
    }

    #[test]
    fn d3_set_hot_swaps_handle_in_place() {
        let mgr = QuotaScopeManager::new();
        apply_traffic_quota_update(
            TrafficQuotaUpdate {
                request_id: "r1".into(),
                user_id: "alice".into(),
                client_name: "edge-01".into(),
                action: TrafficQuotaAction::Set as i32,
                state: Some(quota_state(1_000, 100, false)),
            },
            &mgr,
        );
        let h1 = mgr.lookup("alice").unwrap();

        apply_traffic_quota_update(
            TrafficQuotaUpdate {
                request_id: "r2".into(),
                user_id: "alice".into(),
                client_name: "edge-01".into(),
                action: TrafficQuotaAction::Set as i32,
                state: Some(quota_state(10_000, 10_000, false)),
            },
            &mgr,
        );
        let h2 = mgr.lookup("alice").unwrap();
        assert!(Arc::ptr_eq(&h1, &h2));
        assert_eq!(h1.remaining(), 10_000);
    }

    #[test]
    fn d3_remove_drops_handle() {
        let mgr = QuotaScopeManager::new();
        apply_traffic_quota_update(
            TrafficQuotaUpdate {
                request_id: "r1".into(),
                user_id: "alice".into(),
                client_name: "edge-01".into(),
                action: TrafficQuotaAction::Set as i32,
                state: Some(quota_state(1_000, 750, false)),
            },
            &mgr,
        );
        apply_traffic_quota_update(
            TrafficQuotaUpdate {
                request_id: "r2".into(),
                user_id: "alice".into(),
                client_name: "edge-01".into(),
                action: TrafficQuotaAction::Remove as i32,
                state: None,
            },
            &mgr,
        );
        assert!(mgr.lookup("alice").is_none());
    }

    #[test]
    fn d3_set_without_state_is_warning_only() {
        // SET payload that lost its `state` field on the wire must not
        // panic — the contract says the server always carries state for
        // SET, but a malformed push must degrade safely.
        let mgr = QuotaScopeManager::new();
        apply_traffic_quota_update(
            TrafficQuotaUpdate {
                request_id: "r1".into(),
                user_id: "alice".into(),
                client_name: "edge-01".into(),
                action: TrafficQuotaAction::Set as i32,
                state: None,
            },
            &mgr,
        );
        assert!(mgr.lookup("alice").is_none());
    }

    #[test]
    fn d3_unspecified_action_is_ignored() {
        let mgr = QuotaScopeManager::new();
        apply_traffic_quota_update(
            TrafficQuotaUpdate {
                request_id: "r1".into(),
                user_id: "alice".into(),
                client_name: "edge-01".into(),
                action: TrafficQuotaAction::Unspecified as i32,
                state: Some(quota_state(1_000, 750, false)),
            },
            &mgr,
        );
        assert!(mgr.lookup("alice").is_none());
    }
}
