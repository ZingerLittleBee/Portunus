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
//! Probe-overlap policy (R-008): probes never overlap per target
//! *structurally*. There is exactly one prober task per rule and it
//! walks the rule's targets round-robin, `await`ing each `probe_one`
//! sequentially — so a given target is never probed concurrently with
//! itself. We therefore do NOT need to hold the per-target
//! `Mutex<HealthState>` across the (up to `PROBE_CONNECT_TIMEOUT`) dial
//! to serialise probes; the lock is taken only briefly, after the dial,
//! to record the outcome (#47). Holding it across the dial would stall
//! every new inbound connection's health snapshot — the data path locks
//! each target's `HealthState` to compute its dial-order preference
//! (`failover_path`) — for the full 5 s whenever a target SYN-blackholes,
//! even when other targets are healthy.

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
    // Dial FIRST with no lock held (#47). Per-target probe overlap is
    // prevented structurally (one prober task per rule, sequential
    // round-robin — see the module docs / R-008), so we don't hold the
    // `HealthState` lock across the dial. Holding it would block the
    // data path's dial-order snapshot for the full connect timeout when
    // a target SYN-blackholes.
    let dial = time::timeout(
        PROBE_CONNECT_TIMEOUT,
        connect_via_resolver(rule_id, target, resolver, prefer_ipv6),
    )
    .await;

    // Capture the outcome instant/wall-clock after the dial completes
    // (unchanged from before #47), then take the lock only to record
    // the result — failure-detection semantics are identical.
    let now = Instant::now();
    let wall = SystemTime::now();
    let mut guard = state.lock().await;
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
    async fn probe_dial_does_not_hold_health_lock() {
        // #47 regression: while a probe dial is in flight, the per-target
        // `HealthState` lock MUST stay free so the data path's dial-order
        // snapshot (failover_path) is not stalled for the full connect
        // timeout. We hang the dial with a resolver that never returns —
        // the cache does not time-box `resolve`, so `connect_via_resolver`
        // parks inside `probe_one`'s `time::timeout(PROBE_CONNECT_TIMEOUT)`.
        // Under the pre-fix code (lock held across the dial) the lock
        // acquisition below times out; under the fix it is immediate.
        #[derive(Debug)]
        struct HangingResolver;

        #[async_trait::async_trait]
        impl Resolve for HangingResolver {
            async fn resolve(&self, _name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
                std::future::pending().await
            }
        }

        let resolver = Arc::new(LiveResolver::new(
            Arc::new(HangingResolver),
            ResolverConfig::default(),
        ));
        let counter = Arc::new(AtomicU64::new(0));
        let state = Arc::new(Mutex::new(HealthState::new()));
        let targets = Arc::new(vec![MultiTarget {
            spec: portunus_core::RuleTarget {
                host: "hang.example".to_string(),
                port: 443,
                priority: 0,
                proxy_protocol: None,
            },
            target: Target::Dns(Hostname::new("hang.example").unwrap()),
        }]);

        let probe_state = Arc::clone(&state);
        let probe_targets = Arc::clone(&targets);
        let probe_counter = Arc::clone(&counter);
        let probe_resolver = Arc::clone(&resolver);
        let handle = tokio::spawn(async move {
            probe_one(
                RuleId(7),
                0,
                &probe_targets[0],
                probe_state.as_ref(),
                &probe_counter,
                false,
                probe_resolver.as_ref(),
            )
            .await;
        });

        // Let the probe task enter the (hanging) dial.
        time::sleep(Duration::from_millis(50)).await;

        // The lock must be immediately acquirable: the probe takes it only
        // to record an outcome, which cannot happen while the dial hangs.
        let _guard = time::timeout(Duration::from_millis(500), state.lock())
            .await
            .expect("health lock must stay free while the probe dial is in flight");
        // No outcome recorded yet — the dial never completed.
        assert_eq!(counter.load(Ordering::Relaxed), 0);

        handle.abort();
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
