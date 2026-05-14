//! Active TCP-connect health prober for multi-target rules.
//! (007-multi-target-failover, T029.)
//!
//! Opt-in per rule via `Rule.health_check_interval_secs`. When `None`
//! the prober task never spawns — passive failure detection from the
//! data path is sufficient (FR-015). When `Some(n)`, one task per
//! rule probes each target round-robin at the configured cadence.
//!
//! Probe work: `tokio::net::TcpStream::connect((target.host, target.port))`
//! with the same connect timeout the data plane uses (FR-014).
//! Probe results feed the same `HealthState::record_failure` /
//! `record_success` machinery the TCP/UDP data paths use, so the
//! Healthy↔Failed transitions and `target_failovers_total` increments
//! stay unified across passive + active signals.
//!
//! Probe-overlap policy (R-008): per-target lock. We hold the per-
//! target `Mutex<HealthState>` for the full probe duration so a slow
//! probe can NEVER overlap itself; the round-robin walk just skips
//! ahead to the next target.

#![allow(clippy::similar_names)]

use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant, SystemTime};

use portunus_core::RuleId;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::MultiTarget;
use super::failover::HealthState;
use crate::resolver::{ConnectError, LiveResolver, Resolve};

/// Same-as-data-plane connect attempt budget (FR-014). Single-attempt
/// probe; the round-robin walk picks up failures across cycles.
const PROBE_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Spawn the per-rule active prober. The task drains on `cancel`.
///
/// `interval_secs` MUST be in `1..=3600` (server-side validation
/// rejects out-of-range values, V-R6). Internally clamped to a 1 s
/// minimum to keep tests honest in the unlikely event a `0` slips
/// through.
#[allow(clippy::too_many_arguments)]
pub fn spawn<R: Resolve + 'static>(
    rule_id: RuleId,
    targets: Arc<Vec<MultiTarget>>,
    health_states: Arc<Vec<Mutex<HealthState>>>,
    target_failovers_total: Arc<AtomicU64>,
    prefer_ipv6: bool,
    interval_secs: u32,
    resolver: Arc<LiveResolver<R>>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let interval = Duration::from_secs(u64::from(interval_secs.max(1)));
    tokio::spawn(async move {
        run(
            rule_id,
            &targets,
            &health_states,
            &target_failovers_total,
            prefer_ipv6,
            interval,
            resolver.as_ref(),
            cancel,
        )
        .await;
    })
}

#[allow(clippy::too_many_arguments)]
async fn run<R: Resolve>(
    rule_id: RuleId,
    targets: &[MultiTarget],
    health_states: &[Mutex<HealthState>],
    target_failovers_total: &AtomicU64,
    prefer_ipv6: bool,
    interval: Duration,
    resolver: &LiveResolver<R>,
    cancel: CancellationToken,
) {
    debug_assert_eq!(targets.len(), health_states.len());

    let mut tick = time::interval(interval);
    // Skip the immediate-fire tick — the data path has had no chance
    // to land on any target yet, so probing pre-traffic is wasteful.
    tick.set_missed_tick_behavior(time::MissedTickBehavior::Delay);

    let mut next_idx: usize = 0;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = tick.tick() => {
                // Round-robin one target per tick. With N targets and a
                // T-second interval the per-target probe cadence is
                // (N * T) seconds — operators should pick T accordingly
                // (interval-per-cycle, not per-target).
                if targets.is_empty() {
                    continue;
                }
                let idx = next_idx % targets.len();
                next_idx = next_idx.wrapping_add(1);
                probe_one(
                    rule_id,
                    idx,
                    &targets[idx],
                    &health_states[idx],
                    target_failovers_total,
                    prefer_ipv6,
                    resolver,
                ).await;
            }
        }
    }
}

async fn probe_one<R: Resolve>(
    rule_id: RuleId,
    idx: usize,
    target: &MultiTarget,
    state: &Mutex<HealthState>,
    target_failovers_total: &AtomicU64,
    prefer_ipv6: bool,
    resolver: &LiveResolver<R>,
) {
    // R-008: hold the per-target lock for the full probe so we can
    // never overlap. A slow probe just blocks the next round on this
    // target; other targets are free to probe in their own ticks.
    let mut guard = state.lock().await;

    let dial = time::timeout(
        PROBE_CONNECT_TIMEOUT,
        connect_via_resolver(rule_id, target, resolver, prefer_ipv6),
    )
    .await;

    let now = Instant::now();
    let wall = SystemTime::now();
    match dial {
        Ok(Ok(_stream)) => {
            // Probe success — feed the recovery counter.
            guard.record_success(now, wall, target_failovers_total);
            debug!(
                event = "rule.target.probe_ok",
                rule_id = %rule_id,
                target_index = idx,
                target_host = %target.spec.host,
                target_port = target.spec.port,
            );
        }
        Ok(Err(e)) => {
            guard.record_failure(now, wall, target_failovers_total);
            warn!(
                event = "rule.target.probe_failed",
                rule_id = %rule_id,
                target_index = idx,
                target_host = %target.spec.host,
                target_port = target.spec.port,
                error = %e,
            );
        }
        Err(_elapsed) => {
            guard.record_failure(now, wall, target_failovers_total);
            warn!(
                event = "rule.target.probe_timeout",
                rule_id = %rule_id,
                target_index = idx,
                target_host = %target.spec.host,
                target_port = target.spec.port,
                timeout_secs = PROBE_CONNECT_TIMEOUT.as_secs(),
            );
        }
    }
}

async fn connect_via_resolver<R: Resolve>(
    rule_id: RuleId,
    target: &MultiTarget,
    resolver: &LiveResolver<R>,
    prefer_ipv6: bool,
) -> Result<TcpStream, ConnectError> {
    let (stream, _src) = resolver
        .connect_target(rule_id, &target.target, target.spec.port, prefer_ipv6)
        .await?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::{ResolveAnswer, ResolverConfig, ResolverError};
    use portunus_core::{Hostname, Target};
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::Ordering;
    use tokio::net::TcpListener;

    #[derive(Debug)]
    struct StubResolver;

    #[async_trait::async_trait]
    impl Resolve for StubResolver {
        async fn resolve(&self, _name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
            Ok(ResolveAnswer {
                addrs: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                ttl: Duration::from_secs(60),
            })
        }
    }

    fn ip_resolver() -> Arc<LiveResolver<StubResolver>> {
        Arc::new(LiveResolver::new(
            Arc::new(StubResolver),
            ResolverConfig::default(),
        ))
    }

    fn target(host: &str, port: u16) -> MultiTarget {
        MultiTarget {
            spec: portunus_core::RuleTarget {
                host: host.to_string(),
                port,
                priority: 0,
                proxy_protocol: None,
            },
            target: Target::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        }
    }

    #[tokio::test]
    async fn probe_success_recovers_a_failed_target() {
        // Stand up a real listener so the probe connect succeeds.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // Don't block; just keep the listener open.
        tokio::spawn(async move {
            loop {
                let _ = listener.accept().await;
            }
        });

        let counter = Arc::new(AtomicU64::new(0));
        // Drive the state to Failed via 3 manual failures.
        let mut state = HealthState::new();
        let now = Instant::now();
        let wall = SystemTime::now();
        for i in 0..3 {
            state.record_failure(now + Duration::from_millis(i * 10), wall, &counter);
        }
        assert_eq!(state.health(), super::super::failover::Health::Failed);
        assert_eq!(counter.load(Ordering::Relaxed), 1);

        let states = Arc::new(vec![Mutex::new(state)]);
        let targets = Arc::new(vec![target("127.0.0.1", port)]);
        let resolver = ip_resolver();
        let cancel = CancellationToken::new();

        // Two probe ticks at 100 ms cadence (we override interval below
        // by calling probe_one directly to keep wall-clock tight).
        for _ in 0..2 {
            probe_one(
                RuleId(1),
                0,
                &targets[0],
                &states[0],
                &counter,
                false,
                resolver.as_ref(),
            )
            .await;
        }
        assert_eq!(
            states[0].lock().await.health(),
            super::super::failover::Health::Healthy,
            "two probe successes should restore Healthy",
        );
        // Failed → Healthy is the second counter bump.
        assert_eq!(counter.load(Ordering::Relaxed), 2);
        cancel.cancel();
    }

    #[tokio::test]
    async fn probe_failure_drives_healthy_to_failed() {
        // No listener — connect refuses.
        let counter = Arc::new(AtomicU64::new(0));
        let states = Arc::new(vec![Mutex::new(HealthState::new())]);
        // Use a port we know is unbound — bind+drop trick.
        let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let dead_port = probe.local_addr().unwrap().port();
        drop(probe);

        let targets = Arc::new(vec![target("127.0.0.1", dead_port)]);
        let resolver = ip_resolver();

        for _ in 0..3 {
            probe_one(
                RuleId(2),
                0,
                &targets[0],
                &states[0],
                &counter,
                false,
                resolver.as_ref(),
            )
            .await;
        }
        assert_eq!(
            states[0].lock().await.health(),
            super::super::failover::Health::Failed,
            "three probe failures should mark target Failed",
        );
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }
}
