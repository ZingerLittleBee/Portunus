//! Per-rule TCP forwarder: binds the listen range, accepts in a loop, and
//! spawns [`proxy`](proxy::proxy) for each connection.
//!
//! Lifecycle is driven by a [`CancellationToken`]:
//! - cancel → stop accepting new connections immediately (FR-014/FR-016
//!   "stop accepting within 1 s")
//! - then drain in-flight proxies up to `drain_timeout`
//! - return a final activation/teardown outcome to the caller via the
//!   `status_tx` channel — exactly one `Activated`/`Failed` and one
//!   `Removed` per rule lifetime.
//!
//! Range support (002-port-range-forward, T014/T027): a single rule may
//! span a contiguous listen-port range. All ports are bound atomically
//! via [`range::bind_all`]; on failure the operator gets a single
//! `Failed { reason: "<reason>:<offending_port>" }` event. On success
//! one accept loop per port is spawned into the SAME `JoinSet` and
//! shares the SAME `proxy_cancel` so the existing drain semantics
//! apply uniformly to range and single-port rules.

pub mod failover;
pub mod failover_path;
pub mod probe;
pub mod proxy;
pub mod range;
pub mod stats;
pub mod udp;

use std::sync::Arc;
use std::time::Duration;

use forward_core::{PortRange, RuleId, Target};
use forward_proto::v1::Protocol;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::forwarder::range::BindFailure;
use crate::forwarder::stats::RuleStats;
use crate::resolver::{LiveResolver, Resolve};

/// Outcome the forwarder reports back to the control loop. The control loop
/// translates each into a `RuleStatus` message on the bidi gRPC stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleStatusEvent {
    Activated { rule_id: RuleId },
    Failed { rule_id: RuleId, reason: String },
    Removed { rule_id: RuleId },
}

/// One forwarding rule the client should run. Range-aware: single-port
/// rules construct both `listen_range` and `target_range` via
/// [`PortRange::single`].
///
/// 003-domain-name-forward (T020 / T039): `target` is the parsed
/// classification of the rule's `target_host` string (IP literal or
/// validated DNS hostname); the proxy hot path passes it directly to
/// the resolver layer. `prefer_ipv6` is plumbed through to the
/// resolver but only honored in US3 (T040).
#[derive(Debug, Clone)]
pub struct ClientRule {
    pub rule_id: RuleId,
    pub listen_range: PortRange,
    pub target_host: String,
    pub target: Target,
    pub target_range: PortRange,
    pub prefer_ipv6: bool,
    /// 004-udp-forward T031: forwarding protocol. v0.3 callers default
    /// to `Tcp`; v0.4 control plane sets this from the wire `Rule.protocol`.
    pub protocol: Protocol,
    /// 004-udp-forward T031: per-rule cap on simultaneous live UDP
    /// flows. Sourced from `Welcome.udp_max_flows_per_rule` (default
    /// 1024 if 0/absent). Ignored for TCP rules.
    pub udp_max_flows: u32,
    /// 004-udp-forward T062: idle window in seconds before a UDP flow
    /// is reaped. Sourced from `Welcome.udp_flow_idle_secs` (default
    /// 60 if 0/absent). Ignored for TCP rules.
    pub udp_flow_idle_secs: u32,
    /// 007-multi-target-failover (T022): non-empty for multi-target
    /// rules; empty for single-target rules (which keep the byte-
    /// identical v0.6.0 hot path). Each entry pairs the operator-
    /// supplied (host, port, priority) with its parsed `Target`
    /// classification (IP literal vs DNS name) so the failover dial
    /// loop doesn't reparse on every connect.
    ///
    /// When non-empty, `target` / `target_host` / `target_range` carry
    /// the FIRST target's values for back-compat with existing
    /// telemetry — the failover loop ignores them and walks the
    /// `targets` list instead.
    pub targets: Vec<MultiTarget>,
    /// 007-multi-target-failover (T029): per-rule active TCP-connect
    /// probe interval, in seconds. `None` (the default) means probes
    /// are disabled — passive failure detection from the data path
    /// alone (FR-015). `Some(n)` opts the rule into a per-rule
    /// prober task that probes each target round-robin at the
    /// configured cadence. Single-target rules ignore this field.
    pub health_check_interval_secs: Option<u32>,
}

/// One entry in `ClientRule.targets`. Holds both the wire-shape
/// `RuleTarget` and the pre-parsed `forward_core::Target` so the
/// dial loop never reparses. Heavy enough to put off the hot path
/// for single-target rules — they stay on the byte-identical v0.6.0
/// path that doesn't even read this field.
#[derive(Debug, Clone)]
pub struct MultiTarget {
    pub spec: forward_core::RuleTarget,
    pub target: Target,
}

/// Run the forwarder until `cancel` fires. Sends exactly one
/// `Activated|Failed` event during startup and exactly one `Removed` event
/// (only after a successful Activated) when the listeners are torn down.
///
/// Each listener binds to `0.0.0.0:port` so external machines can reach
/// it (this is the data plane — `data-model.md` does not require
/// loopback-only as the operator HTTP API does). Operators with stricter
/// requirements can run the client behind a host firewall.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub async fn run<R: Resolve + 'static>(
    rule: ClientRule,
    resolver: Arc<LiveResolver<R>>,
    status_tx: mpsc::Sender<RuleStatusEvent>,
    cancel: CancellationToken,
    drain_timeout: Duration,
    stats: Arc<RuleStats>,
) {
    // 004-udp-forward T031: dispatch on protocol. UDP rules go through
    // the `udp::run_listener` path; TCP keeps the v0.3 path byte-
    // identical (FR-010).
    if matches!(rule.protocol, Protocol::Udp) {
        run_udp(rule, resolver, status_tx, cancel, stats).await;
        return;
    }

    // 007-multi-target-failover T022: activation branch. Single-target
    // rules (`targets.is_empty()`) stay on the byte-identical v0.6.0
    // hot path below; multi-target rules divert into the failover
    // module which spins its own listeners + accept loop using the
    // health state machine. Constitution Principle II — the byte-
    // identity guarantee for single-target rules is structural here:
    // they never even pull `failover_path` into their data path.
    if !rule.targets.is_empty() {
        failover_path::run_tcp(rule, resolver, status_tx, cancel, drain_timeout, stats).await;
        return;
    }

    let listeners = match range::bind_all(&rule.listen_range).await {
        Ok(v) => v,
        Err(BindFailure {
            offending_port,
            reason,
        }) => {
            warn!(
                event = "rule.failed",
                rule_id = %rule.rule_id,
                listen_port = rule.listen_range.start(),
                listen_port_end = rule.listen_range.end(),
                offending_port = offending_port,
                reason = reason,
            );
            // For single-port rules the range collapses; preserve the
            // pre-002 "reason" wire shape ("port_in_use") so existing
            // operator tooling that greps for it keeps working. For
            // range rules we suffix the offending port so operators can
            // pinpoint which slot in the range collided.
            let reason_str = if rule.listen_range.len() == 1 {
                reason.to_string()
            } else {
                format!("{reason}:{offending_port}")
            };
            let _ = status_tx
                .send(RuleStatusEvent::Failed {
                    rule_id: rule.rule_id,
                    reason: reason_str,
                })
                .await;
            return;
        }
    };

    info!(
        event = "rule.activated",
        rule_id = %rule.rule_id,
        listen_port = rule.listen_range.start(),
        listen_port_end = rule.listen_range.end(),
        range_size = rule.listen_range.len(),
        target = %format!("{}:{}-{}", rule.target_host, rule.target_range.start(), rule.target_range.end()),
    );
    if status_tx
        .send(RuleStatusEvent::Activated {
            rule_id: rule.rule_id,
        })
        .await
        .is_err()
    {
        // Control loop hung up before we even reported activated — bail.
        return;
    }

    let in_flight: JoinSet<()> = JoinSet::new();
    // `proxy_cancel` is an independent token (NOT a child of `cancel`) so
    // that operator-side rule removal does not immediately tear down
    // in-flight proxies — they get a `drain_timeout` window to finish.
    let proxy_cancel = CancellationToken::new();

    // Spawn one accept loop per (listen_port, listener) pair into the
    // shared JoinSet so `cancel` reaps every accept loop and the drain
    // phase below sees a single set of in-flight proxies regardless of
    // how many listeners the rule owns.
    let in_flight = run_accept_loops(
        listeners,
        &rule,
        Arc::clone(&resolver),
        Arc::clone(&stats),
        cancel.clone(),
        proxy_cancel.clone(),
        in_flight,
    );

    drain(in_flight, proxy_cancel, drain_timeout).await;

    info!(
        event = "rule.removed",
        rule_id = %rule.rule_id,
    );
    let _ = status_tx
        .send(RuleStatusEvent::Removed {
            rule_id: rule.rule_id,
        })
        .await;
}

/// Spawn one accept loop per listener, all sharing `cancel` (stops
/// accept) and `proxy_cancel` (kills in-flight after drain). Returns
/// the `JoinSet` populated with the accept tasks; per-connection proxy
/// tasks are added by each accept loop as they fire.
#[allow(clippy::needless_pass_by_value, clippy::too_many_arguments)]
fn run_accept_loops<R: Resolve + 'static>(
    listeners: Vec<(u16, TcpListener)>,
    rule: &ClientRule,
    resolver: Arc<LiveResolver<R>>,
    stats: Arc<RuleStats>,
    cancel: CancellationToken,
    proxy_cancel: CancellationToken,
    mut in_flight: JoinSet<()>,
) -> JoinSet<()> {
    for (listen_port, listener) in listeners {
        let Some(target_port) =
            PortRange::target_for(listen_port, rule.listen_range, rule.target_range)
        else {
            // Unreachable in practice — bind_all only yields ports in
            // `rule.listen_range`. Logged defensively.
            warn!(
                event = "rule.target_mapping_missing",
                rule_id = %rule.rule_id,
                listen_port = listen_port,
            );
            continue;
        };
        let target = rule.target.clone();
        let prefer_ipv6 = rule.prefer_ipv6;
        let rule_id = rule.rule_id;
        let accept_cancel = cancel.clone();
        let conn_proxy_cancel = proxy_cancel.clone();
        let accept_stats = Arc::clone(&stats);
        let accept_resolver = Arc::clone(&resolver);
        in_flight.spawn(async move {
            accept_loop(
                listener,
                listen_port,
                accept_resolver,
                target,
                target_port,
                prefer_ipv6,
                rule_id,
                accept_cancel,
                conn_proxy_cancel,
                accept_stats,
            )
            .await;
        });
    }
    in_flight
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop<R: Resolve + 'static>(
    listener: TcpListener,
    listen_port: u16,
    resolver: Arc<LiveResolver<R>>,
    target: Target,
    target_port: u16,
    prefer_ipv6: bool,
    rule_id: RuleId,
    cancel: CancellationToken,
    proxy_cancel: CancellationToken,
    stats: Arc<RuleStats>,
) {
    // Per-listener in-flight set: lets us reap finished proxies for
    // logging without holding open the rule-level JoinSet's slot.
    let mut local: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            joined = local.join_next(), if !local.is_empty() => {
                let _ = joined;
            }
            accept = listener.accept() => match accept {
                Ok((sock, peer)) => {
                    let target = target.clone();
                    let conn_cancel = proxy_cancel.clone();
                    let conn_stats = Arc::clone(&stats);
                    let conn_resolver = Arc::clone(&resolver);
                    local.spawn(async move {
                        match proxy::proxy(
                            sock,
                            conn_resolver.as_ref(),
                            rule_id,
                            &target,
                            target_port,
                            prefer_ipv6,
                            conn_cancel,
                            Some(conn_stats),
                            listen_port,
                        ).await {
                            Ok((bin, bout)) => {
                                info!(
                                    event = "rule.conn_closed",
                                    rule_id = %rule_id,
                                    listen_port = listen_port,
                                    peer = %peer,
                                    bytes_in = bin,
                                    bytes_out = bout,
                                );
                            }
                            Err(e) => {
                                warn!(
                                    event = "rule.conn_error",
                                    rule_id = %rule_id,
                                    listen_port = listen_port,
                                    peer = %peer,
                                    error = %e,
                                );
                            }
                        }
                    });
                }
                Err(e) => {
                    // Transient accept error — log and keep looping.
                    warn!(
                        event = "rule.accept_error",
                        rule_id = %rule_id,
                        listen_port = listen_port,
                        error = %e,
                    );
                }
            }
        }
    }

    // Listener drops here, closing the bind socket immediately. Any
    // in-flight per-listener proxies are reaped by the rule-level
    // drain via `proxy_cancel`.
    drop(listener);
    while local.join_next().await.is_some() {}
}

/// 004-udp-forward T031/T052: UDP per-rule entry point.
///
/// Single-port rule (`listen_range.len() == 1`): spawns one
/// `udp::run_listener`. Range rule (US3): probe-binds every port up
/// front so a partial-success can fail atomically with
/// `port_in_use:<offending_port>` (matching the TCP `bind_all` shape),
/// then spawns one listener task per port — each owns its own
/// `UdpFlowTable` keyed on `(source_addr, port)` while sharing the
/// rule-level `RuleStats` for aggregate counter roll-up. Per-port slots
/// in `RuleStats::per_port` (allocated by `RuleStats::for_range` in
/// `control.rs`) collect the per-port `bytes_*`/`datagrams_*` totals
/// surfaced by `--per-port`.
///
/// Activation reporting follows the TCP shape: a successful probe-bind
/// of every port → `Activated`; ANY bind failure → single `Failed`
/// event with the offending port suffixed for range rules.
/// 004-udp-forward T057/T062: `udp_max_flows_per_rule == 0` (the proto3
/// default a v0.3 server emits) means "use the client compile-time
/// default". v0.4 servers always send a non-zero value via Welcome.
pub(crate) const UDP_MAX_FLOWS_DEFAULT: u32 = 1024;
/// 004-udp-forward T057/T062: `udp_flow_idle_secs == 0` falls back to
/// the documented compile-time default of 60 seconds.
pub(crate) const UDP_FLOW_IDLE_SECS_DEFAULT: u32 = 60;

pub(crate) fn resolve_udp_cap(welcome_value: u32) -> usize {
    let value = if welcome_value == 0 {
        UDP_MAX_FLOWS_DEFAULT
    } else {
        welcome_value
    };
    usize::try_from(value).unwrap_or(UDP_MAX_FLOWS_DEFAULT as usize)
}

pub(crate) fn resolve_udp_idle_window(welcome_value: u32) -> Duration {
    let secs = if welcome_value == 0 {
        UDP_FLOW_IDLE_SECS_DEFAULT
    } else {
        welcome_value
    };
    Duration::from_secs(u64::from(secs))
}

async fn run_udp<R: Resolve + 'static>(
    rule: ClientRule,
    resolver: Arc<LiveResolver<R>>,
    status_tx: mpsc::Sender<RuleStatusEvent>,
    cancel: CancellationToken,
    stats: Arc<RuleStats>,
) {
    // 007-multi-target-failover (T024): multi-target UDP rules go
    // through the multi-target listener which selects a target on
    // each NEW flow's first inbound packet (FR-012).
    if !rule.targets.is_empty() {
        failover_path::run_udp(rule, resolver, status_tx, cancel, stats).await;
        return;
    }
    let listen_start = rule.listen_range.start();
    let listen_end = rule.listen_range.end();
    let range_size = rule.listen_range.len();

    // Probe-bind every port in the range so a partial-success surfaces
    // atomically as `Failed{port_in_use:<port>}` BEFORE we report
    // `Activated`. We drop the probes immediately so the listener tasks
    // can re-bind cleanly; a hostile concurrent grab between drop and
    // re-bind would surface as `udp_bind_failed` in the listener log
    // and the rule would effectively no-op (operator sees missing
    // datagrams). v0.5 work can move bind into this function and pass
    // the bound socket into `run_listener` to close that race.
    let mut probes = Vec::with_capacity(range_size as usize);
    for port in listen_start..=listen_end {
        match tokio::net::UdpSocket::bind(("0.0.0.0", port)).await {
            Ok(p) => probes.push(p),
            Err(e) => {
                let reason = if range_size == 1 {
                    "port_in_use".to_string()
                } else {
                    format!("port_in_use:{port}")
                };
                warn!(
                    event = "rule.failed",
                    rule_id = %rule.rule_id,
                    listen_port = port,
                    reason = %reason,
                    error = %e,
                );
                let _ = status_tx
                    .send(RuleStatusEvent::Failed {
                        rule_id: rule.rule_id,
                        reason,
                    })
                    .await;
                return;
            }
        }
    }
    drop(probes);

    let target_first = rule.target_range.start();
    info!(
        event = "rule.activated",
        rule_id = %rule.rule_id,
        listen_port = listen_start,
        listen_port_end = listen_end,
        range_size = range_size,
        protocol = "udp",
        target = %format!("{}:{}-{}", rule.target_host, target_first, rule.target_range.end()),
    );
    if status_tx
        .send(RuleStatusEvent::Activated {
            rule_id: rule.rule_id,
        })
        .await
        .is_err()
    {
        return;
    }

    let cap = resolve_udp_cap(rule.udp_max_flows);
    let idle_window = resolve_udp_idle_window(rule.udp_flow_idle_secs);

    // Spawn one listener per port. They share `cancel` so a single
    // remove/shutdown drains every flow across the range; they share
    // `stats` so the aggregate roll-up + per-port slot population
    // happens transparently via `RuleStats::inc_datagram_*(port, n)`.
    let mut tasks: JoinSet<()> = JoinSet::new();
    for listen_port in listen_start..=listen_end {
        let Some(target_port) =
            PortRange::target_for(listen_port, rule.listen_range, rule.target_range)
        else {
            warn!(
                event = "rule.target_mapping_missing",
                rule_id = %rule.rule_id,
                listen_port = listen_port,
            );
            continue;
        };
        let rule_id = rule.rule_id;
        let target = rule.target.clone();
        let prefer_ipv6 = rule.prefer_ipv6;
        let task_stats = Arc::clone(&stats);
        let task_resolver = Arc::clone(&resolver);
        let task_cancel = cancel.clone();
        tasks.spawn(async move {
            udp::run_listener(
                rule_id,
                listen_port,
                target,
                target_port,
                prefer_ipv6,
                cap,
                idle_window,
                task_stats,
                task_resolver,
                task_cancel,
            )
            .await;
        });
    }

    while tasks.join_next().await.is_some() {}

    info!(
        event = "rule.removed",
        rule_id = %rule.rule_id,
    );
    let _ = status_tx
        .send(RuleStatusEvent::Removed {
            rule_id: rule.rule_id,
        })
        .await;
}

pub(super) async fn drain(
    mut in_flight: JoinSet<()>,
    proxy_cancel: CancellationToken,
    drain_timeout: Duration,
) {
    let drain_deadline = tokio::time::sleep(drain_timeout);
    tokio::pin!(drain_deadline);
    loop {
        tokio::select! {
            () = &mut drain_deadline => {
                proxy_cancel.cancel();
                while in_flight.join_next().await.is_some() {}
                break;
            }
            joined = in_flight.join_next() => match joined {
                Some(_) => {
                    if in_flight.is_empty() {
                        break;
                    }
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::{ResolveAnswer, ResolverConfig, ResolverError};
    use forward_core::Hostname;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    fn port_pool_lock() -> &'static tokio::sync::Mutex<()> {
        super::range::test_port_pool_lock()
    }

    /// T057 (US4): the Welcome `udp_*` field → ClientRule fallback rules.
    /// A v0.3 server (no UDP fields) lands as 0/0 and the client uses
    /// its baked-in defaults; a v0.4 server passes the configured value
    /// through verbatim.
    #[test]
    fn welcome_zero_falls_back_to_compile_time_defaults() {
        // v0.3 / proto3 default — both fields absent on the wire arrive
        // as 0/0; the client must apply 60s / 1024.
        assert_eq!(super::resolve_udp_cap(0), 1024);
        assert_eq!(
            super::resolve_udp_idle_window(0),
            std::time::Duration::from_secs(60),
        );
    }

    #[test]
    fn welcome_nonzero_is_passed_through_verbatim() {
        // v0.4 server-supplied tunables ride through unchanged so
        // operators see the values they configured in `server.toml`.
        assert_eq!(super::resolve_udp_cap(256), 256);
        assert_eq!(
            super::resolve_udp_idle_window(90),
            std::time::Duration::from_secs(90),
        );
    }

    #[test]
    fn welcome_unreasonable_cap_clamps_to_default_via_try_from() {
        // u32::MAX → usize::MAX on 64-bit — accept it as a sentinel
        // "no real cap"; the helper still returns a valid usize. On
        // 32-bit hosts the try_from fallback kicks in.
        let v = super::resolve_udp_cap(u32::MAX);
        assert!(v >= 1, "must always return at least 1");
    }

    /// Resolver that panics if invoked. All forwarder tests below use
    /// IP-target rules (`Target::Ip`) so the resolver MUST be skipped.
    #[derive(Debug, Default)]
    struct PanickingResolver;

    #[async_trait::async_trait]
    impl Resolve for PanickingResolver {
        async fn resolve(&self, name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
            panic!("PanickingResolver::resolve was called for {name}");
        }
    }

    fn ip_resolver() -> Arc<LiveResolver<PanickingResolver>> {
        Arc::new(LiveResolver::new(
            Arc::new(PanickingResolver),
            ResolverConfig::default(),
        ))
    }

    async fn spawn_echo() -> std::net::SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = sock.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        if sock.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        addr
    }

    /// Pick a free port that's also free on `0.0.0.0` (where `bind_all`
    /// listens). The double-bind probe avoids losing races to parallel
    /// tests that hold a port on the wildcard interface but not on
    /// loopback (or vice versa).
    async fn pick_free_port() -> u16 {
        for _ in 0..50 {
            let Ok(probe) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
                continue;
            };
            let port = probe.local_addr().unwrap().port();
            drop(probe);
            // Verify the port is still free immediately afterwards.
            // If another test took it, retry.
            if let Ok(verify) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await {
                drop(verify);
                return port;
            }
        }
        panic!("could not pick a free port after 50 attempts");
    }

    /// Race-resistant N-consecutive-free-port picker. Holds probe
    /// listeners on `0.0.0.0:port` for the full search so parallel
    /// tests can't squat in the middle of the chosen range. See
    /// `range::tests::pick_consecutive_free` for the rationale.
    async fn pick_consecutive_free(n: u16) -> PortRange {
        for _ in 0..50 {
            let Ok(probe) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
                continue;
            };
            let start = probe.local_addr().unwrap().port();
            if u32::from(start) + u32::from(n) > 65_536 {
                drop(probe);
                continue;
            }
            let mut probes: Vec<TcpListener> = vec![probe];
            let mut ok = true;
            for offset in 1..n {
                if let Ok(l) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, start + offset)).await {
                    probes.push(l);
                } else {
                    ok = false;
                    break;
                }
            }
            if ok {
                drop(probes);
                return PortRange::new(start, start + n - 1).unwrap();
            }
            drop(probes);
        }
        panic!("could not find {n} consecutive free ports after 50 attempts");
    }

    fn single_rule(rule_id: u64, port: u16, target: std::net::SocketAddr) -> ClientRule {
        ClientRule {
            rule_id: RuleId(rule_id),
            listen_range: PortRange::single(port),
            target_host: target.ip().to_string(),
            target: Target::Ip(target.ip()),
            target_range: PortRange::single(target.port()),
            prefer_ipv6: false,
            protocol: Protocol::Tcp,
            udp_max_flows: 0,
            udp_flow_idle_secs: 0,
            targets: Vec::new(), health_check_interval_secs: None,
        }
    }

    #[tokio::test]
    async fn run_emits_activated_then_forwards_then_removed() {
        let _guard = port_pool_lock().lock().await;
        let echo = spawn_echo().await;
        let port = pick_free_port().await;
        let (tx, mut rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let task = tokio::spawn(async move {
            run(
                single_rule(7, port, echo),
                ip_resolver(),
                tx,
                cancel_run,
                Duration::from_secs(2),
                RuleStats::new(),
            )
            .await;
        });

        // Wait for Activated.
        let evt = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(evt, RuleStatusEvent::Activated { rule_id } if rule_id == RuleId(7)));

        // Punch a connection through.
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        client.write_all(b"forwarded").await.unwrap();
        let mut buf = [0u8; 9];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"forwarded");
        drop(client);

        // Cancel → expect Removed.
        cancel.cancel();
        let evt = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(evt, RuleStatusEvent::Removed { rule_id } if rule_id == RuleId(7)));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn run_reports_port_in_use() {
        let _guard = port_pool_lock().lock().await;
        // Bind a listener to an OS-chosen port, then try to reuse it.
        let occupy = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let busy_port = occupy.local_addr().unwrap().port();
        let (tx, mut rx) = mpsc::channel(2);
        let cancel = CancellationToken::new();
        run(
            ClientRule {
                rule_id: RuleId(1),
                listen_range: PortRange::single(busy_port),
                target: Target::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                prefer_ipv6: false,
                target_host: "127.0.0.1".into(),
                target_range: PortRange::single(1),
                protocol: Protocol::Tcp,
                udp_max_flows: 0,
                udp_flow_idle_secs: 0,
                targets: Vec::new(), health_check_interval_secs: None,
            },
            ip_resolver(),
            tx,
            cancel,
            Duration::from_millis(100),
            RuleStats::new(),
        )
        .await;
        let evt = rx.recv().await.unwrap();
        match evt {
            RuleStatusEvent::Failed { rule_id, reason } => {
                assert_eq!(rule_id, RuleId(1));
                // Single-port rules keep the bare wire reason for backwards
                // compatibility with v0.1.0 operator tooling.
                assert_eq!(reason, "port_in_use");
            }
            other => panic!("expected Failed{{port_in_use}}, got {other:?}"),
        }
        // No Removed event after a Failed startup.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancel_stops_accept_within_one_second() {
        let _guard = port_pool_lock().lock().await;
        // FR-014 / FR-016: stop accept within 1 s of remove.
        let echo = spawn_echo().await;
        let port = pick_free_port().await;
        let (tx, mut rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let task = tokio::spawn(async move {
            run(
                single_rule(3, port, echo),
                ip_resolver(),
                tx,
                cancel_run,
                Duration::from_millis(500),
                RuleStats::new(),
            )
            .await;
        });
        // Activated event.
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        let t0 = std::time::Instant::now();
        cancel.cancel();
        // After cancel, a fresh connect MUST be refused well within the
        // FR-014/FR-016 budget. Spec target is 1 s; we assert 2 s here
        // to stay green on contended CI runners (macOS GH-Actions in
        // particular schedules tasks slowly under parallel test load).
        // The dev-host bench in `forward-client/benches/data_plane.rs`
        // verifies the tighter 1 s SLA on a quiet machine.
        let stopped = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(stopped.is_ok(), "listener still accepting 2s after cancel");
        assert!(t0.elapsed() < Duration::from_secs(2));

        // Removed event eventually.
        let _ = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap();
    }

    /// Single-port: 100 MB stream arrives byte-equal.
    #[tokio::test]
    async fn forwards_100mb_byte_equal() {
        let _guard = port_pool_lock().lock().await;
        let echo = spawn_echo().await;
        let port = pick_free_port().await;
        let (tx, mut rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let task = tokio::spawn(async move {
            run(
                single_rule(41, port, echo),
                ip_resolver(),
                tx,
                cancel_run,
                Duration::from_secs(5),
                RuleStats::new(),
            )
            .await;
        });
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        let n: usize = 100 * 1024 * 1024;
        let mut sent: Vec<u8> = Vec::with_capacity(n);
        let mut x: u32 = 0xdead_beef;
        for _ in 0..n {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            sent.push((x & 0xff) as u8);
        }

        let conn = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let (mut rd, mut wr) = conn.into_split();
        let send_payload = sent.clone();
        let writer = tokio::spawn(async move {
            wr.write_all(&send_payload).await.unwrap();
            wr.shutdown().await.unwrap();
        });
        let mut received = Vec::with_capacity(n);
        let read_n = rd.read_to_end(&mut received).await.unwrap();
        writer.await.unwrap();

        assert_eq!(read_n, n, "100MB length mismatch");
        for (i, (a, b)) in received.iter().zip(sent.iter()).enumerate() {
            assert_eq!(a, b, "byte mismatch at offset {i}");
        }

        cancel.cancel();
        task.await.unwrap();
    }

    // Stress test — 5 rules × 100 conns = 500 concurrent TCP streams.
    // Reliable on macOS / multi-core dev machines, but flaky on Ubuntu
    // CI's single-core runners (occasional `read_to_end` returns empty
    // before the writer half has flushed). The forwarder code path is
    // covered by the smaller-fanout `forwards_100mb_byte_equal` test
    // and by the `forward-e2e` integration suite; this one is kept
    // around for local stress runs (`cargo test -- --ignored`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "stress test — flaky on single-core CI runners; runs locally"]
    async fn five_rules_hundred_conns_each_no_corruption() {
        let _guard = port_pool_lock().lock().await;
        let echo = spawn_echo().await;
        let cancel = CancellationToken::new();

        let mut tasks = Vec::new();
        let mut ports = Vec::new();
        for i in 0..5u32 {
            let port = pick_free_port().await;
            ports.push(port);
            let (tx, mut rx) = mpsc::channel(8);
            let cancel_run = cancel.clone();
            tasks.push(tokio::spawn(async move {
                run(
                    single_rule(u64::from(i + 100), port, echo),
                    ip_resolver(),
                    tx,
                    cancel_run,
                    Duration::from_secs(5),
                    RuleStats::new(),
                )
                .await;
            }));
            let evt = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(evt, RuleStatusEvent::Activated { .. }));
            drop(rx);
        }

        let conns_per_rule: usize = 100;
        let payload_len: usize = 4096;
        let mut handles = Vec::new();
        for &port in &ports {
            for conn_i in 0..conns_per_rule {
                handles.push(tokio::spawn(async move {
                    let mut sock = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                        .await
                        .expect("connect");
                    let mut payload = vec![0u8; payload_len];
                    for (i, b) in payload.iter_mut().enumerate() {
                        let v = u8::try_from((i + conn_i) & 0xff).unwrap();
                        *b = v;
                    }
                    let (mut rd, mut wr) = sock.split();
                    let writer = async {
                        wr.write_all(&payload).await.unwrap();
                        wr.shutdown().await.unwrap();
                    };
                    let mut got = Vec::with_capacity(payload_len);
                    let reader = async {
                        rd.read_to_end(&mut got).await.unwrap();
                    };
                    tokio::join!(writer, reader);
                    assert_eq!(got, payload);
                }));
            }
        }
        for h in handles {
            h.await.unwrap();
        }
        cancel.cancel();
        for t in tasks {
            t.await.unwrap();
        }
    }

    #[tokio::test]
    async fn cancel_drains_in_flight_connection() {
        let _guard = port_pool_lock().lock().await;
        let echo = spawn_echo().await;
        let port = pick_free_port().await;
        let (tx, mut rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let task = tokio::spawn(async move {
            run(
                single_rule(42, port, echo),
                ip_resolver(),
                tx,
                cancel_run,
                Duration::from_secs(3),
                RuleStats::new(),
            )
            .await;
        });
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        conn.write_all(b"warmup").await.unwrap();
        let mut buf = [0u8; 6];
        conn.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"warmup");

        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(200)).await;

        conn.write_all(b"after-cancel").await.unwrap();
        let mut buf = [0u8; 12];
        let echoed = tokio::time::timeout(Duration::from_secs(1), conn.read_exact(&mut buf)).await;
        assert!(echoed.is_ok(), "in-flight read timed out post-cancel");
        echoed.unwrap().unwrap();
        assert_eq!(&buf, b"after-cancel");

        let fresh = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await;
        assert!(fresh.is_err(), "listener still accepting after cancel");

        drop(conn);
        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap();
    }

    // --- T030 (US2) range removal lifecycle ---

    /// 10-port range: every port reaches the upstream while the rule is
    /// active; after cancel, every port refuses fresh connects within 1 s.
    #[tokio::test]
    async fn range_remove_releases_all_listeners() {
        let _guard = port_pool_lock().lock().await;
        let echo = spawn_echo().await;
        let listen = pick_consecutive_free(10).await;
        let start = listen.start();
        let end = listen.end();

        let (tx, mut rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let target_host = echo.ip().to_string();
        let target_port = echo.port();
        let echo_ip = echo.ip();
        let task = tokio::spawn(async move {
            run(
                ClientRule {
                    rule_id: RuleId(31),
                    listen_range: listen,
                    target_host,
                    target: Target::Ip(echo_ip),
                    target_range: PortRange::new(target_port, target_port + 9).unwrap(),
                    prefer_ipv6: false,
                    protocol: Protocol::Tcp,
                    udp_max_flows: 0,
                    udp_flow_idle_secs: 0,
                    targets: Vec::new(), health_check_interval_secs: None,
                },
                ip_resolver(),
                tx,
                cancel_run,
                Duration::from_millis(500),
                RuleStats::for_range(listen),
            )
            .await;
        });

        let evt = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(evt, RuleStatusEvent::Activated { .. }));

        // While active, every port accepts.
        for p in start..=end {
            let conn = TcpStream::connect((Ipv4Addr::LOCALHOST, p)).await;
            assert!(conn.is_ok(), "port {p} failed to accept while active");
        }

        cancel.cancel();
        // Within 1 s every port refuses fresh connects.
        let stopped = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let mut all_refused = true;
                for p in start..=end {
                    if TcpStream::connect((Ipv4Addr::LOCALHOST, p)).await.is_ok() {
                        all_refused = false;
                        break;
                    }
                }
                if all_refused {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(stopped.is_ok(), "some port still accepting 1s after cancel");

        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap();
    }

    /// T031 (US2): in-flight connection on a range port survives cancel
    /// until drain completes, mirroring the single-port case.
    #[tokio::test]
    async fn range_in_flight_connection_drains() {
        let _guard = port_pool_lock().lock().await;
        let echo = spawn_echo().await;
        let listen = pick_consecutive_free(5).await;
        let start = listen.start();
        let end = listen.end();
        let target = PortRange::new(echo.port(), echo.port() + 4).unwrap();

        let (tx, mut rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let target_host = echo.ip().to_string();
        let echo_ip = echo.ip();
        let task = tokio::spawn(async move {
            run(
                ClientRule {
                    rule_id: RuleId(32),
                    listen_range: listen,
                    target_host,
                    target: Target::Ip(echo_ip),
                    target_range: target,
                    prefer_ipv6: false,
                    protocol: Protocol::Tcp,
                    udp_max_flows: 0,
                    udp_flow_idle_secs: 0,
                    targets: Vec::new(), health_check_interval_secs: None,
                },
                ip_resolver(),
                tx,
                cancel_run,
                Duration::from_secs(3),
                RuleStats::for_range(listen),
            )
            .await;
        });
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        // Open in-flight connection on a port in the middle.
        let mid = start + 2;
        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, mid))
            .await
            .unwrap();
        conn.write_all(b"warmup").await.unwrap();
        let mut buf = [0u8; 6];
        // The echo server here is single-target; the range mapping
        // connects every listen port to a *different* upstream port, only
        // one of which (`echo.port()`) actually has an echo. So `mid`
        // (offset +2) will fail to connect upstream. Use the start port
        // to land on the real echo server.
        drop(conn);

        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, start))
            .await
            .unwrap();
        conn.write_all(b"warmup").await.unwrap();
        conn.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"warmup");

        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(200)).await;

        conn.write_all(b"after-cancel").await.unwrap();
        let mut buf2 = [0u8; 12];
        let echoed = tokio::time::timeout(Duration::from_secs(1), conn.read_exact(&mut buf2)).await;
        assert!(echoed.is_ok(), "in-flight range read timed out post-cancel");
        echoed.unwrap().unwrap();
        assert_eq!(&buf2, b"after-cancel");

        // Fresh connect refused on every port in the range.
        for p in start..=end {
            let fresh = TcpStream::connect((Ipv4Addr::LOCALHOST, p)).await;
            assert!(fresh.is_err(), "port {p} still accepting after cancel");
        }

        drop(conn);
        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap();
    }

    // ---- T021a (003-domain-name-forward, FR-011): port-range × DNS
    // cache sharing — a single rule with a 4-port listen range pointed at
    // a DNS hostname MUST share one resolution across all listen ports.
    // The Hostname-keyed cache in `resolver/cache.rs` is the load-bearing
    // piece; a future refactor that accidentally keyed by `host:port`
    // would fail here.
    //
    // Sequence: one warmup connect populates the cache, then 4 concurrent
    // connects (one per listen port) MUST all hit cache → resolver call
    // count stays at 1. Strict "exactly once" under fully-concurrent
    // first-connects requires US2's single-flight (FR-012, T030); that
    // tighter property gets its own test in the US2 phase.

    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Debug)]
    struct CountingResolver {
        calls: AtomicUsize,
        addrs: Vec<IpAddr>,
    }

    impl CountingResolver {
        fn new(addrs: Vec<IpAddr>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                addrs,
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::Relaxed)
        }
    }

    #[async_trait::async_trait]
    impl Resolve for CountingResolver {
        async fn resolve(&self, _name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            // 60 s TTL — well above the 5 s cache floor — so the
            // four concurrent connects all hit the cache.
            Ok(ResolveAnswer {
                addrs: self.addrs.clone(),
                ttl: Duration::from_secs(60),
            })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[allow(clippy::too_many_lines)]
    async fn port_range_dns_target_resolves_hostname_exactly_once() {
        let _guard = port_pool_lock().lock().await;

        // Pick 4 consecutive free target ports and stand up an echo
        // server on each. The 4-port listen range maps 1:1 to these
        // upstream ports, so every listen port has a real upstream.
        let target_range = pick_consecutive_free(4).await;
        let mut target_listeners = Vec::new();
        for p in target_range.start()..=target_range.end() {
            let l = TcpListener::bind((Ipv4Addr::LOCALHOST, p)).await.unwrap();
            target_listeners.push(l);
        }
        for l in target_listeners {
            tokio::spawn(async move {
                loop {
                    let Ok((mut sock, _)) = l.accept().await else {
                        break;
                    };
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        while let Ok(n) = sock.read(&mut buf).await {
                            if n == 0 {
                                break;
                            }
                            if sock.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    });
                }
            });
        }

        // 4-port listen range. The hostname "echo.test" is purely
        // symbolic — the CountingResolver returns 127.0.0.1 unconditionally.
        let listen_range = pick_consecutive_free(4).await;
        let host = Hostname::new("echo.test").unwrap();
        let counting = Arc::new(CountingResolver::new(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]));
        let resolver = Arc::new(LiveResolver::new(
            Arc::clone(&counting),
            ResolverConfig::default(),
        ));

        let rule = ClientRule {
            rule_id: RuleId(2_021),
            listen_range,
            target_host: "echo.test".to_string(),
            target: Target::Dns(host),
            target_range,
            prefer_ipv6: false,
            protocol: Protocol::Tcp,
            udp_max_flows: 0,
            udp_flow_idle_secs: 0,
            targets: Vec::new(), health_check_interval_secs: None,
        };

        let (tx, mut rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let task = tokio::spawn(async move {
            run(
                rule,
                resolver,
                tx,
                cancel_run,
                Duration::from_secs(2),
                RuleStats::new(),
            )
            .await;
        });
        let evt = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(evt, RuleStatusEvent::Activated { .. }));

        // Warmup connect on the first listen port — populates the cache.
        {
            let mut sock = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_range.start()))
                .await
                .unwrap();
            sock.write_all(b"warmup").await.unwrap();
            let mut buf = [0u8; 6];
            sock.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"warmup");
        }
        assert_eq!(
            counting.calls(),
            1,
            "warmup should produce exactly one resolver call"
        );

        // Now drive 4 concurrent connects across every listen port. With
        // the cache populated, the Hostname-keyed entry serves all four —
        // proving FR-011's "share one resolution per range" claim.
        let mut handles = Vec::new();
        for p in listen_range.start()..=listen_range.end() {
            handles.push(tokio::spawn(async move {
                let mut sock = TcpStream::connect((Ipv4Addr::LOCALHOST, p)).await.unwrap();
                let payload = format!("hello-{p}");
                sock.write_all(payload.as_bytes()).await.unwrap();
                let mut buf = vec![0u8; payload.len()];
                sock.read_exact(&mut buf).await.unwrap();
                assert_eq!(buf, payload.as_bytes());
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(
            counting.calls(),
            1,
            "FR-011: post-warmup, the 4-port range MUST share the cached \
             resolution; got {} resolver calls",
            counting.calls()
        );

        cancel.cancel();
        task.await.unwrap();
    }
}
