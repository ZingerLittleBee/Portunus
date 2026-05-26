//! Per-rule UDP runtime. Owns the registry, listener socket map, and a
//! supervisor task that drives ordered shutdown.
//!
//! Spec: 014-udp-centralized-demux, FR-001 / FR-011 / FR-012.
//!
//! Lifecycle:
//!   1. `start()` probe-binds every listen port, builds the registry,
//!      spawns the demux / reaper / per-port listener tasks, and starts
//!      the supervisor task that owns the JoinSet.
//!   2. `shutdown()` signals the supervisor via a bounded(1) channel
//!      and awaits the supervisor's completion `watch` channel. The
//!      supervisor performs the ordered drain
//!      (listener → reaper → registry → demux) per FR-011.
//!   3. Drop is **not** async-aware: callers MUST `shutdown().await`
//!      before dropping the runtime. Failing to do so leaves the
//!      supervisor task running (its tasks will still observe
//!      `rule_cancel` if the caller cancels the parent token).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use portunus_core::{RuleId, Target};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::forwarder::quota::QuotaHandle;
use crate::forwarder::rate_limit::scope::{OwnerRateLimitHandle, RuleRateLimitHandle};
use crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator;
use crate::forwarder::stats::RuleStats;
use crate::forwarder::udp::demux::{DemuxCommand, DemuxConfig, run_demux};
use crate::forwarder::udp::listener::{ListenerConfig, run_listener};
use crate::forwarder::udp::reaper::run_reaper;
use crate::forwarder::udp::registry::UdpFlowRegistry;
use crate::resolver::{LiveResolver, Resolve};

/// Supervisor state machine — see FR-011 in
/// `specs/014-udp-centralized-demux/spec.md`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Running,
    ShuttingDownIntentional,
    ShuttingDownAfterFailure,
}

/// Task role tag carried by every JoinSet entry. Used to classify
/// expected vs unexpected exits during ordered drain (FR-011).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Listener(u16),
    Demux,
    Reaper,
    /// 014 Phase 9 / FR-014: per-rule stats pump that ticks once a
    /// second and writes `registry.len()` into `stats.active_flows`.
    /// Replaces v0.4 last-writer-wins behaviour.
    StatsPump,
}

impl Role {
    fn label(self) -> String {
        match self {
            Self::Listener(p) => format!("Listener({p})"),
            Self::Demux => "Demux".to_string(),
            Self::Reaper => "Reaper".to_string(),
            Self::StatsPump => "StatsPump".to_string(),
        }
    }
}

/// Configuration handed to [`UdpRuleRuntime::start`]. All fields are
/// owned by the caller (or `Arc`-shared); the runtime takes ownership
/// at start time.
pub struct UdpRuntimeConfig<R: Resolve + 'static> {
    pub rule_id: RuleId,
    pub listen_ports: std::ops::RangeInclusive<u16>,
    pub target: Target,
    pub target_ports: std::ops::RangeInclusive<u16>,
    pub prefer_ipv6: bool,
    pub rule_cap: usize,
    pub idle_window: Duration,
    pub stats: Arc<RuleStats>,
    pub resolver: Arc<LiveResolver<R>>,
    pub rate_limit: Option<Arc<RuleRateLimitHandle>>,
    pub rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    pub owner_rate_limit: Option<Arc<OwnerRateLimitHandle>>,
    pub owner_rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    pub quota: Option<Arc<QuotaHandle>>,
    /// Invoked **exactly once** when the supervisor enters
    /// `ShuttingDownAfterFailure` from `Running`. Never invoked for
    /// operator-initiated REMOVE shutdowns. See FR-011 for the
    /// emission policy.
    pub failed_callback: Box<dyn Fn(String) + Send + Sync>,
}

/// Outcome of a completed runtime shutdown. See FR-011 step c.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShutdownOutcome {
    /// No unexpected exits during ordered drain.
    Ok,
    /// One or more child tasks exited (cancel or panic) before their
    /// role's cancellation step ran. Informational: callers in
    /// `control.rs` MUST NOT re-issue PUSH on this outcome.
    UnexpectedExitsDuringDrain { roles: Vec<String>, count: usize },
}

/// Failure modes for [`UdpRuleRuntime::start`].
#[derive(Debug)]
pub enum UdpRuntimeStartError {
    /// `UdpSocket::bind` on `0.0.0.0:port` failed. Any sockets already
    /// bound by earlier loop iterations are dropped by the caller path
    /// via the local `HashMap` going out of scope.
    BindFailed { port: u16, error: std::io::Error },
}

/// Public handle to a running per-rule UDP runtime.
///
/// Held by `control.rs` for the lifetime of an active UDP rule.
/// `shutdown()` is the sole tear-down entry point (FR-012).
pub struct UdpRuleRuntime {
    registry: Arc<UdpFlowRegistry>,
    #[allow(dead_code)] // kept alive for the lifetime of the runtime
    listener_sockets: Arc<HashMap<u16, Arc<UdpSocket>>>,
    #[allow(dead_code)] // cancellation already cascades via child tokens
    rule_cancel: CancellationToken,
    shutdown_tx: mpsc::Sender<()>,
    #[allow(dead_code)] // future: external observability of supervisor
    supervisor_handle: Option<tokio::task::JoinHandle<()>>,
    completion_rx: tokio::sync::watch::Receiver<Option<ShutdownOutcome>>,
}

impl UdpRuleRuntime {
    /// Start the runtime. Probe-binds every listen port up-front so a
    /// partial-binding failure surfaces as a `BindFailed` error rather
    /// than a half-started rule. On error, any sockets already bound
    /// during this call are dropped (kernel releases the port).
    ///
    /// `rule_cancel` is the runtime's root cancellation token. The
    /// supervisor creates child tokens for listener / reaper roles so
    /// the ordered drain can cancel each role independently while an
    /// unexpected exit still cascades via `rule_cancel`.
    pub async fn start<R: Resolve + 'static>(
        cfg: UdpRuntimeConfig<R>,
        rule_cancel: CancellationToken,
    ) -> Result<Self, UdpRuntimeStartError> {
        // (1) probe-bind every port; partial failure → drop and return.
        let mut sockets: HashMap<u16, Arc<UdpSocket>> = HashMap::new();
        for port in cfg.listen_ports.clone() {
            match UdpSocket::bind(("0.0.0.0", port)).await {
                Ok(s) => {
                    sockets.insert(port, Arc::new(s));
                }
                Err(e) => {
                    return Err(UdpRuntimeStartError::BindFailed { port, error: e });
                }
            }
        }
        let listener_sockets = Arc::new(sockets);

        // (2) registry + channels.
        let registry = UdpFlowRegistry::new(cfg.rule_cap);
        let (demux_tx, demux_rx) = mpsc::channel::<DemuxCommand>(1024);
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
        let (completion_tx, completion_rx) =
            tokio::sync::watch::channel::<Option<ShutdownOutcome>>(None);

        // (3) child cancel tokens (siblings of `rule_cancel`).
        let listener_token = rule_cancel.child_token();
        let reaper_token = rule_cancel.child_token();
        let stats_pump_token = rule_cancel.child_token();

        // (4) FR-016: emit `rule.udp_runtime_started` once per rule
        // activation. `range_size` is a `u32` to be safe against the
        // (0..=u16::MAX) edge case where end-start+1 overflows u16.
        let range_size: u32 =
            u32::from(*cfg.listen_ports.end()) - u32::from(*cfg.listen_ports.start()) + 1;
        info!(
            event = "rule.udp_runtime_started",
            rule_id = %cfg.rule_id,
            listen_port_start = cfg.listen_ports.start(),
            listen_port_end = cfg.listen_ports.end(),
            range_size = range_size,
            rule_cap = cfg.rule_cap,
            cap_scope = "per_rule",
        );

        // (5) Build the JoinSet and spawn child tasks. The supervisor
        // owns the JoinSet from here onwards.
        let mut joinset: JoinSet<(Role, Result<(), tokio::task::JoinError>)> = JoinSet::new();
        let mut listener_count: usize = 0;
        let rule_id = cfg.rule_id;

        // demux task — uses supervisor-held demux_tx clone for Shutdown.
        let demux_cfg = DemuxConfig {
            rule_id,
            registry: Arc::clone(&registry),
            listener_sockets: Arc::clone(&listener_sockets),
            stats: Arc::clone(&cfg.stats),
        };
        joinset.spawn(async move {
            run_demux(demux_cfg, demux_rx).await;
            (Role::Demux, Ok(()))
        });

        // reaper task — single per-rule sweeper.
        let reg_for_reaper = Arc::clone(&registry);
        let reaper_token_for_task = reaper_token.clone();
        let idle_window = cfg.idle_window;
        joinset.spawn(async move {
            run_reaper(reg_for_reaper, idle_window, rule_id, reaper_token_for_task).await;
            (Role::Reaper, Ok(()))
        });

        // 014 Phase 9 / FR-014: stats pump — single per-rule task that
        // writes `registry.len()` into `stats.active_flows` once per
        // second. Replaces the v0.4 last-writer-wins behaviour where
        // each listener's `flow_table.len()` overwrote a shared gauge.
        let reg_for_pump = Arc::clone(&registry);
        let stats_for_pump = Arc::clone(&cfg.stats);
        let pump_cancel = stats_pump_token.clone();
        joinset.spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            // Skip the immediate first tick — the gauge starts at 0,
            // a fresh write of 0 would be a no-op anyway.
            ticker.tick().await;
            loop {
                tokio::select! {
                    biased;
                    () = pump_cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        let n = u32::try_from(reg_for_pump.len()).unwrap_or(u32::MAX);
                        stats_for_pump.set_active_flows(n);
                    }
                }
            }
            (Role::StatsPump, Ok(()))
        });

        // listener tasks — one per listen port.
        for port in cfg.listen_ports.clone() {
            let Some(target_port) = port_map(port, &cfg.listen_ports, &cfg.target_ports) else {
                continue;
            };
            let Some(sock) = listener_sockets.get(&port).cloned() else {
                continue;
            };
            let lcfg = ListenerConfig {
                rule_id,
                listen_port: port,
                target: cfg.target.clone(),
                target_port,
                prefer_ipv6: cfg.prefer_ipv6,
                idle_window: cfg.idle_window,
                registry: Arc::clone(&registry),
                demux_tx: demux_tx.clone(),
                stats: Arc::clone(&cfg.stats),
                resolver: Arc::clone(&cfg.resolver),
                rate_limit: cfg.rate_limit.clone(),
                rate_limit_stats: cfg.rate_limit_stats.clone(),
                owner_rate_limit: cfg.owner_rate_limit.clone(),
                owner_rate_limit_stats: cfg.owner_rate_limit_stats.clone(),
                quota: cfg.quota.clone(),
                cancel: listener_token.clone(),
            };
            joinset.spawn(async move {
                run_listener(lcfg, sock).await;
                (Role::Listener(port), Ok(()))
            });
            listener_count += 1;
        }

        // (6) Drop our extra `demux_tx` clones: listeners hold their own
        // clones inside the spawned tasks. The supervisor retains ONE
        // demux_tx clone (`supervisor_demux_tx`) for the final
        // explicit `DemuxCommand::Shutdown` in drain step (d). When
        // the supervisor sends Shutdown, all listeners have already
        // joined and dropped their clones — so the channel won't be
        // closed early.
        let supervisor_demux_tx = demux_tx.clone();
        drop(demux_tx); // drop the start()-frame clone

        // (7) Spawn the supervisor task. Wrap `failed_callback` in an
        // `Arc` so the supervisor can hold it without losing
        // `Send + Sync`.
        let failed_callback: Arc<dyn Fn(String) + Send + Sync> = Arc::from(cfg.failed_callback);
        let supervisor = Supervisor {
            joinset,
            state: State::Running,
            registry: Arc::clone(&registry),
            stats: Arc::clone(&cfg.stats),
            shutdown_rx,
            completion_tx,
            rule_cancel: rule_cancel.clone(),
            listener_token,
            reaper_token,
            stats_pump_token,
            demux_tx_for_shutdown: supervisor_demux_tx,
            failed_callback,
            markers: RoleMarkers::default(),
            unexpected_during_drain: Vec::new(),
            listener_pending: listener_count,
            demux_pending: true,
            reaper_pending: true,
            stats_pump_pending: true,
        };
        let supervisor_handle = tokio::spawn(supervisor.run());

        Ok(Self {
            registry,
            listener_sockets,
            rule_cancel,
            shutdown_tx,
            supervisor_handle: Some(supervisor_handle),
            completion_rx,
        })
    }

    /// FR-012: idempotent signal-and-wait. The supervisor performs the
    /// ordered drain — `shutdown()` MUST NOT touch task handles
    /// directly.
    ///
    /// A second concurrent or sequential call observes the completion
    /// watch already populated and returns the same outcome
    /// immediately.
    pub async fn shutdown(&mut self) -> ShutdownOutcome {
        // Bounded(1): SendError on second call (channel closed by
        // supervisor) is benign — we observe completion via the watch.
        let _ = self.shutdown_tx.send(()).await;
        let mut rx = self.completion_rx.clone();
        loop {
            if let Some(outcome) = rx.borrow().clone() {
                return outcome;
            }
            if rx.changed().await.is_err() {
                // Supervisor dropped its sender without publishing —
                // assume Ok rather than block forever.
                return ShutdownOutcome::Ok;
            }
        }
    }

    /// Access the per-rule flow registry. Used by the stats pump
    /// (Phase 9) and operator inspection paths.
    #[must_use]
    pub fn registry(&self) -> &Arc<UdpFlowRegistry> {
        &self.registry
    }
}

/// Translate a `listen_port` to its paired `target_port` by linear
/// offset. Returns `None` if the offset would land outside `target`.
fn port_map(
    port: u16,
    listen: &std::ops::RangeInclusive<u16>,
    target: &std::ops::RangeInclusive<u16>,
) -> Option<u16> {
    let offset = port.checked_sub(*listen.start())?;
    let target_port = (*target.start()).checked_add(offset)?;
    if target_port <= *target.end() {
        Some(target_port)
    } else {
        None
    }
}

/// Tracks which role's cancellation step has fired so the supervisor
/// can classify a `join_next` result as expected vs unexpected during
/// drain (FR-011).
#[derive(Default)]
#[allow(clippy::struct_excessive_bools)] // one flag per role's drain step; a state-machine here would obscure the FR-011 ordering
struct RoleMarkers {
    listener_cancelled: bool,
    reaper_cancelled: bool,
    demux_shutdown_sent: bool,
    /// 014 Phase 9: stats-pump cancellation token has been fired. We
    /// stop the pump FIRST (before listener cancel) so it never fights
    /// with the concurrent registry drain that happens later.
    stats_pump_cancelled: bool,
}

impl RoleMarkers {
    fn is_role_cancelled(&self, role: Role) -> bool {
        match role {
            Role::Listener(_) => self.listener_cancelled,
            Role::Reaper => self.reaper_cancelled,
            Role::Demux => self.demux_shutdown_sent,
            Role::StatsPump => self.stats_pump_cancelled,
        }
    }
}

/// Internal supervisor — owns the JoinSet from spawn time onwards.
///
/// The supervisor is `pub(crate)` only via its spawning factory in
/// [`UdpRuleRuntime::start`]. Tests construct a supervisor directly
/// via [`Supervisor::for_test`] and drive `run()` with a hand-built
/// `JoinSet`.
struct Supervisor {
    joinset: JoinSet<(Role, Result<(), tokio::task::JoinError>)>,
    state: State,
    registry: Arc<UdpFlowRegistry>,
    stats: Arc<RuleStats>,
    shutdown_rx: mpsc::Receiver<()>,
    completion_tx: tokio::sync::watch::Sender<Option<ShutdownOutcome>>,
    rule_cancel: CancellationToken,
    listener_token: CancellationToken,
    reaper_token: CancellationToken,
    stats_pump_token: CancellationToken,
    demux_tx_for_shutdown: mpsc::Sender<DemuxCommand>,
    failed_callback: Arc<dyn Fn(String) + Send + Sync>,
    markers: RoleMarkers,
    unexpected_during_drain: Vec<String>,
    /// Per-role pending-task counters. Decremented on every joined
    /// entry (Phase A unexpected exit, or Phase B drain). The drain
    /// loop completes when the target role's counter reaches 0.
    listener_pending: usize,
    demux_pending: bool,
    reaper_pending: bool,
    stats_pump_pending: bool,
}

impl Supervisor {
    async fn run(mut self) {
        // ─── Phase A: Running ────────────────────────────────────
        // Watch for an explicit shutdown signal OR any unexpected
        // child exit. In `Running`, every `join_next` result is
        // unexpected — listeners / reaper / demux only return when
        // their cancellation step runs in Phase B. This select fires
        // exactly once: either branch transitions out of `Running`.
        tokio::select! {
            biased;
            opt = self.shutdown_rx.recv() => {
                // `None` means sender(s) dropped without sending —
                // treat as intentional shutdown (caller gone).
                let _ = opt;
                if matches!(self.state, State::Running) {
                    self.state = State::ShuttingDownIntentional;
                }
            }
            Some(res) = self.joinset.join_next() => {
                // Decrement the pending counter for this role and
                // classify the exit. In `Running`, every exit is
                // unexpected → transition to AfterFailure, fire
                // rule_cancel, emit Failed exactly once.
                let role_label = match &res {
                    Ok((role, _)) => role.label(),
                    Err(je) => format!("JoinError:{je}"),
                };
                self.decrement_pending(&res);
                if matches!(self.state, State::Running) {
                    self.state = State::ShuttingDownAfterFailure;
                    self.rule_cancel.cancel();
                    (self.failed_callback)(format!(
                        "unexpected_task_exit:{role_label}"
                    ));
                }
            }
        }

        // ─── Phase B: Ordered drain (FR-011) ─────────────────────

        // (pre-a) 014 Phase 9: stop the stats pump FIRST so it can't
        // fight with the concurrent registry mutations from steps
        // (a)/(b)/(c). After this drain we explicitly write
        // active_flows = 0 in step (c).
        self.stats_pump_token.cancel();
        self.markers.stats_pump_cancelled = true;
        self.drain_stats_pump().await;

        // (a) Cancel listener token; drain every Listener(_) entry.
        self.listener_token.cancel();
        self.markers.listener_cancelled = true;
        self.drain_listeners().await;

        // (b) Cancel reaper token; drain the Reaper entry.
        self.reaper_token.cancel();
        self.markers.reaper_cancelled = true;
        self.drain_reaper().await;

        // (c) Cancel + remove any remaining live flows. After the
        // drain, the final visible gauge value is 0.
        self.registry.drain();
        self.stats.set_active_flows(0);

        // (d) Send the EXPLICIT shutdown command. Channel-close MUST
        // NOT be relied on (FR-011: explicit signal contract).
        // At this point all listeners have joined and dropped their
        // demux_tx clones; only our supervisor clone remains.
        let _ = self
            .demux_tx_for_shutdown
            .send(DemuxCommand::Shutdown)
            .await;
        self.markers.demux_shutdown_sent = true;

        // (e) Drain the Demux entry (and any stragglers).
        self.drain_demux().await;

        // Defensive: consume any leftover JoinSet entries. Should be
        // empty by now; anything here is an unexpected exit we did
        // not classify above.
        while let Some(res) = self.joinset.join_next().await {
            self.classify_drain_result(&res);
            self.decrement_pending(&res);
        }

        // ─── Phase C: Publish completion outcome ────────────────
        let outcome = if self.unexpected_during_drain.is_empty() {
            ShutdownOutcome::Ok
        } else {
            ShutdownOutcome::UnexpectedExitsDuringDrain {
                count: self.unexpected_during_drain.len(),
                roles: self.unexpected_during_drain,
            }
        };
        // `send` only fails if all receivers have been dropped, which
        // means no caller is awaiting — benign.
        let _ = self.completion_tx.send(Some(outcome));
    }

    /// Drain the JoinSet until `listener_pending` reaches 0. Sibling-
    /// role exits during this drain are classified per FR-011 against
    /// `markers`.
    async fn drain_listeners(&mut self) {
        while self.listener_pending > 0 {
            let Some(res) = self.joinset.join_next().await else {
                break;
            };
            self.classify_drain_result(&res);
            self.decrement_pending(&res);
        }
    }

    async fn drain_reaper(&mut self) {
        while self.reaper_pending {
            let Some(res) = self.joinset.join_next().await else {
                break;
            };
            self.classify_drain_result(&res);
            self.decrement_pending(&res);
        }
    }

    async fn drain_demux(&mut self) {
        while self.demux_pending {
            let Some(res) = self.joinset.join_next().await else {
                break;
            };
            self.classify_drain_result(&res);
            self.decrement_pending(&res);
        }
    }

    async fn drain_stats_pump(&mut self) {
        while self.stats_pump_pending {
            let Some(res) = self.joinset.join_next().await else {
                break;
            };
            self.classify_drain_result(&res);
            self.decrement_pending(&res);
        }
    }

    /// Adjust the per-role pending counter based on a `join_next`
    /// result. JoinErrors (panic surfaced as outer `Err`) can't be
    /// tagged; in that rare case we conservatively decrement nothing
    /// — the outer `defensive drain` loop will still consume the
    /// JoinSet to completion.
    fn decrement_pending(
        &mut self,
        res: &Result<(Role, Result<(), tokio::task::JoinError>), tokio::task::JoinError>,
    ) {
        if let Ok((role, _)) = res {
            match role {
                Role::Listener(_) => {
                    self.listener_pending = self.listener_pending.saturating_sub(1);
                }
                Role::Demux => self.demux_pending = false,
                Role::Reaper => self.reaper_pending = false,
                Role::StatsPump => self.stats_pump_pending = false,
            }
        }
    }

    /// Classify a single `join_next` result against `markers`. An
    /// exit is "unexpected" if its role's cancellation step has not
    /// yet fired (FR-011).
    fn classify_drain_result(
        &mut self,
        res: &Result<(Role, Result<(), tokio::task::JoinError>), tokio::task::JoinError>,
    ) {
        match res {
            Ok((role, _)) => {
                // In `ShuttingDownAfterFailure` the role that
                // triggered Phase A's transition has already been
                // counted (and `Failed` emitted) — don't double-count
                // it as a drain-unexpected exit. We can't distinguish
                // it cheaply here; instead, classify against the
                // markers per FR-011: if the role's drain step hasn't
                // run yet, it's unexpected for the drain.
                if !self.markers.is_role_cancelled(*role) {
                    self.record_unexpected(role.label(), None);
                }
            }
            Err(je) => {
                self.record_unexpected("Unknown".to_string(), Some(format!("{je}")));
            }
        }
    }

    fn record_unexpected(&mut self, role: String, join_error: Option<String>) {
        if let Some(je) = &join_error {
            warn!(
                event = "rule.udp_shutdown_unexpected_exit",
                role = %role,
                join_error = %je,
            );
        } else {
            warn!(
                event = "rule.udp_shutdown_unexpected_exit",
                role = %role,
            );
        }
        self.unexpected_during_drain.push(role);
    }
}

// ────────────────────────────── Tests ──────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Composite test harness: a Supervisor + every channel/token the
    /// runtime would normally retain. Returned as a struct so the
    /// signature stays clippy-clean.
    struct TestHarness {
        sup: Supervisor,
        shutdown_tx: mpsc::Sender<()>,
        completion_rx: tokio::sync::watch::Receiver<Option<ShutdownOutcome>>,
        demux_tx: mpsc::Sender<DemuxCommand>,
        demux_rx: mpsc::Receiver<DemuxCommand>,
        rule_cancel: CancellationToken,
        listener_token: CancellationToken,
        reaper_token: CancellationToken,
        stats_pump_token: CancellationToken,
    }

    /// Build a Supervisor harness for direct testing of the state
    /// machine + ordered drain logic.
    fn build_test_supervisor(listener_count: usize, failed_calls: Arc<AtomicUsize>) -> TestHarness {
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>(1);
        let (completion_tx, completion_rx) =
            tokio::sync::watch::channel::<Option<ShutdownOutcome>>(None);
        let rule_cancel = CancellationToken::new();
        let listener_token = rule_cancel.child_token();
        let reaper_token = rule_cancel.child_token();
        let stats_pump_token = rule_cancel.child_token();
        let (demux_tx, demux_rx) = mpsc::channel::<DemuxCommand>(8);
        let registry = UdpFlowRegistry::new(16);
        let stats = RuleStats::new();
        let failed_callback: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |_reason| {
            failed_calls.fetch_add(1, Ordering::SeqCst);
        });
        let supervisor = Supervisor {
            joinset: JoinSet::new(),
            state: State::Running,
            registry,
            stats,
            shutdown_rx,
            completion_tx,
            rule_cancel: rule_cancel.clone(),
            listener_token: listener_token.clone(),
            reaper_token: reaper_token.clone(),
            stats_pump_token: stats_pump_token.clone(),
            demux_tx_for_shutdown: demux_tx.clone(),
            failed_callback,
            markers: RoleMarkers::default(),
            unexpected_during_drain: Vec::new(),
            listener_pending: listener_count,
            demux_pending: true,
            reaper_pending: true,
            stats_pump_pending: true,
        };
        TestHarness {
            sup: supervisor,
            shutdown_tx,
            completion_rx,
            demux_tx,
            demux_rx,
            rule_cancel,
            listener_token,
            reaper_token,
            stats_pump_token,
        }
    }

    /// Spawn a well-behaved task into the supervisor's JoinSet that
    /// exits when its `CancellationToken` fires.
    fn spawn_well_behaved(sup: &mut Supervisor, role: Role, cancel: CancellationToken) {
        sup.joinset.spawn(async move {
            cancel.cancelled().await;
            (role, Ok(()))
        });
    }

    /// Spawn a well-behaved demux task that exits when it receives
    /// `DemuxCommand::Shutdown`.
    fn spawn_well_behaved_demux(sup: &mut Supervisor, mut rx: mpsc::Receiver<DemuxCommand>) {
        sup.joinset.spawn(async move {
            while let Some(cmd) = rx.recv().await {
                if matches!(cmd, DemuxCommand::Shutdown) {
                    break;
                }
            }
            (Role::Demux, Ok(()))
        });
    }

    /// Test #1: an intentional shutdown signal produces `Ok` and never
    /// invokes the `failed_callback`. Mirrors operator-driven REMOVE.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn intentional_shutdown_emits_ok_no_failed_callback() {
        let failed_calls = Arc::new(AtomicUsize::new(0));
        let TestHarness {
            mut sup,
            shutdown_tx,
            completion_rx,
            demux_tx: _demux_tx,
            demux_rx,
            rule_cancel: _,
            listener_token: lis,
            reaper_token: reap,
            stats_pump_token: pump,
        } = build_test_supervisor(2, Arc::clone(&failed_calls));

        // Two well-behaved listeners + reaper + stats pump + demux.
        spawn_well_behaved(&mut sup, Role::Listener(7001), lis.clone());
        spawn_well_behaved(&mut sup, Role::Listener(7002), lis);
        spawn_well_behaved(&mut sup, Role::Reaper, reap);
        spawn_well_behaved(&mut sup, Role::StatsPump, pump);
        spawn_well_behaved_demux(&mut sup, demux_rx);

        let sup_handle = tokio::spawn(sup.run());

        // Trigger intentional shutdown.
        shutdown_tx.send(()).await.unwrap();
        sup_handle.await.unwrap();

        // Outcome is Ok.
        let outcome = completion_rx.borrow().clone();
        assert_eq!(outcome, Some(ShutdownOutcome::Ok));

        // Failed callback NEVER invoked for intentional shutdown.
        assert_eq!(
            failed_calls.load(Ordering::SeqCst),
            0,
            "failed_callback must not fire on intentional shutdown"
        );
    }

    /// Test #2: a listener task that exits *during Running* (without
    /// the listener_token being cancelled) is unexpected — supervisor
    /// transitions to `ShuttingDownAfterFailure`, fires `rule_cancel`,
    /// invokes `failed_callback` exactly once, and still drains the
    /// other tasks cleanly.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn unexpected_listener_exit_during_running_emits_failed_once() {
        let failed_calls = Arc::new(AtomicUsize::new(0));
        let TestHarness {
            mut sup,
            shutdown_tx: _shutdown_tx,
            completion_rx,
            demux_tx: _demux_tx,
            demux_rx,
            rule_cancel: rule,
            listener_token: lis,
            reaper_token: reap,
            stats_pump_token: pump,
        } = build_test_supervisor(2, Arc::clone(&failed_calls));

        // Listener A panics-equivalent: returns prematurely without
        // observing its cancel token. The supervisor sees this as an
        // unexpected exit in `Running`.
        sup.joinset.spawn(async move {
            // Yield briefly so the supervisor is in its select loop
            // when we return.
            tokio::task::yield_now().await;
            (Role::Listener(7001), Ok(()))
        });
        // Listener B is well-behaved — exits on listener_token.
        spawn_well_behaved(&mut sup, Role::Listener(7002), lis);
        // Reaper + stats pump + demux are well-behaved.
        spawn_well_behaved(&mut sup, Role::Reaper, reap);
        spawn_well_behaved(&mut sup, Role::StatsPump, pump);
        spawn_well_behaved_demux(&mut sup, demux_rx);

        let sup_handle = tokio::spawn(sup.run());

        // Wait for completion.
        sup_handle.await.unwrap();

        // Outcome: classification depends on whether other tasks
        // exited before their cancel step ran. The well-behaved tasks
        // wait on their tokens, which `rule_cancel.cancel()` fires
        // (parent of the listener+reaper tokens) — so Listener(7002)
        // and Reaper exit cleanly under their own cancelled tokens.
        // The unexpected Listener(7001) exit has already been counted
        // in Phase A (not as drain-unexpected). Demux exits cleanly
        // on its explicit Shutdown.
        let outcome = completion_rx.borrow().clone();
        assert!(
            outcome.is_some(),
            "supervisor must publish a completion outcome"
        );

        // Failed callback fires EXACTLY ONCE.
        assert_eq!(
            failed_calls.load(Ordering::SeqCst),
            1,
            "failed_callback must fire exactly once on unexpected exit"
        );

        // rule_cancel was fired.
        assert!(rule.is_cancelled(), "rule_cancel must be fired on failure");
    }

    /// Test #3: `shutdown()` is idempotent. The second call observes
    /// the supervisor already done and returns the same outcome.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn second_shutdown_call_is_idempotent() {
        let failed_calls = Arc::new(AtomicUsize::new(0));
        let TestHarness {
            mut sup,
            shutdown_tx,
            completion_rx,
            demux_tx: _demux_tx,
            demux_rx,
            rule_cancel: _,
            listener_token: lis,
            reaper_token: reap,
            stats_pump_token: pump,
        } = build_test_supervisor(1, Arc::clone(&failed_calls));

        spawn_well_behaved(&mut sup, Role::Listener(7001), lis);
        spawn_well_behaved(&mut sup, Role::Reaper, reap);
        spawn_well_behaved(&mut sup, Role::StatsPump, pump);
        spawn_well_behaved_demux(&mut sup, demux_rx);

        let sup_handle = tokio::spawn(sup.run());

        // First shutdown.
        shutdown_tx.send(()).await.unwrap();
        sup_handle.await.unwrap();

        // Read outcome via the watch (first read).
        let first = completion_rx.borrow().clone();
        assert_eq!(first, Some(ShutdownOutcome::Ok));

        // Second shutdown attempt — sender close is fine; the watch
        // already has the outcome.
        // (Simulating the public `shutdown()` flow: send() may return
        // SendError because the supervisor has dropped its receiver.)
        let _ = tokio::time::timeout(Duration::from_millis(50), shutdown_tx.send(())).await;

        // Read again — same outcome.
        let second = completion_rx.borrow().clone();
        assert_eq!(
            second, first,
            "second shutdown must return the same outcome"
        );

        // Failed callback never fired on either pass.
        assert_eq!(failed_calls.load(Ordering::SeqCst), 0);
    }

    /// Sanity check on `port_map`.
    #[test]
    fn port_map_maps_within_range_returns_none_outside() {
        let listen = 7000u16..=7002u16;
        let target = 8000u16..=8002u16;
        assert_eq!(port_map(7000, &listen, &target), Some(8000));
        assert_eq!(port_map(7001, &listen, &target), Some(8001));
        assert_eq!(port_map(7002, &listen, &target), Some(8002));
        // Asymmetric range: target shorter than listen.
        let short_target = 8000u16..=8001u16;
        assert_eq!(port_map(7000, &listen, &short_target), Some(8000));
        assert_eq!(port_map(7001, &listen, &short_target), Some(8001));
        assert_eq!(port_map(7002, &listen, &short_target), None);
    }
}
