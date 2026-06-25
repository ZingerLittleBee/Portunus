//! Per-rule idle-flow reaper. Replaces v0.4's per-listener
//! `spawn_reaper` with a single rule-scoped task that consults the
//! shared `UdpFlowRegistry` instead of a per-listener `UdpFlowTable`.
//!
//! The reaper sweeps every `idle_window / 4`; for every live flow
//! whose `last_seen` exceeds `idle_window` it calls
//! `registry.remove` + `flow.cancel.cancel()` and emits a
//! `rule.udp_flow_closed_idle` event. Spec: 014, FR-010.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::time::interval;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::forwarder::udp::registry::UdpFlowRegistry;
use portunus_core::RuleId;

/// Run the per-rule reaper. Exits cleanly when `cancel` fires.
///
/// The first `interval` tick fires immediately; we explicitly skip it
/// so the very first eviction sweep happens one quarter-window after
/// the reaper starts (matches v0.4 reaper semantics — newly-built
/// flows always survive their first idle quarter).
pub async fn run_reaper(
    registry: Arc<UdpFlowRegistry>,
    idle_window: Duration,
    rule_id: RuleId,
    cancel: CancellationToken,
) {
    if idle_window.is_zero() {
        // Test/operator escape hatch: disable the reaper entirely.
        // Matches v0.4 semantics from `table.rs` `spawn_reaper`
        // (lines 209-230) — also avoids `tokio::time::interval`
        // panicking on `Duration::ZERO`.
        cancel.cancelled().await;
        return;
    }
    // Floor the sweep period at 25 ms so very short windows don't
    // create a tick storm (and never feed `Duration::ZERO` into
    // `interval`). Matches v0.4 `spawn_reaper` contract.
    let period = std::cmp::max(idle_window / 4, Duration::from_millis(25));
    let mut ticker = interval(period);
    ticker.tick().await; // skip the immediate tick
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = ticker.tick() => {
                sweep_once(&registry, idle_window, rule_id);
            }
        }
    }
}

fn sweep_once(registry: &Arc<UdpFlowRegistry>, idle_window: Duration, rule_id: RuleId) {
    let now = Instant::now();
    let snap = registry.snapshot_live();
    for (key, flow) in snap {
        let last = flow.last_seen_at();
        if now.saturating_duration_since(last) > idle_window && registry.remove(key).is_some() {
            flow.cancel.cancel();
            info!(
                event = "rule.udp_flow_closed_idle",
                rule_id = %rule_id,
                listen_port = key.listen_port,
                source = %key.src,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarder::udp::flow::UdpFlow;
    use crate::forwarder::udp::registry::{FlowKey, TryGetOrReserve};
    use std::net::SocketAddr;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn idle_flows_are_evicted_after_window() {
        let reg = UdpFlowRegistry::new(4);
        let src: SocketAddr = "127.0.0.1:50000".parse().unwrap();
        let key = FlowKey::new(8000, src);
        let reservation = match reg.try_get_or_reserve(key) {
            TryGetOrReserve::Reserved(r) => r,
            TryGetOrReserve::Existing(_) => panic!("expected Reserved, got Existing"),
            TryGetOrReserve::CapExhausted => panic!("expected Reserved, got CapExhausted"),
        };
        let flow = UdpFlow::for_test(src).await;
        // Backdate last_seen far enough that it's idle against a 100ms
        // window from the very first sweep.
        flow.force_last_seen(
            Instant::now()
                .checked_sub(Duration::from_secs(60))
                .expect("instant - 60s must not underflow on a running test"),
        );
        reg.commit(reservation, Arc::clone(&flow));

        let cancel = CancellationToken::new();
        let reg_ref = Arc::clone(&reg);
        let cancel_ref = cancel.clone();
        let h = tokio::spawn(async move {
            run_reaper(reg_ref, Duration::from_millis(100), RuleId(1), cancel_ref).await;
        });

        // Wait up to 1s for the reaper to evict.
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if reg.is_empty() {
                break;
            }
        }
        assert_eq!(reg.len(), 0, "reaper must evict the idle flow");
        assert!(flow.cancel.is_cancelled(), "flow.cancel must fire");

        cancel.cancel();
        h.await.unwrap();
    }

    /// Drive `sweep_once` directly so the eviction branch — `registry.remove`
    /// + `flow.cancel.cancel()` + the `rule.udp_flow_closed_idle` log — is
    /// exercised. A flow created "now" then left untouched past a tiny idle
    /// window is removed in a single sweep.
    #[tokio::test]
    async fn sweep_once_evicts_idle_flow_and_cancels() {
        let reg = UdpFlowRegistry::new(4);
        let src: SocketAddr = "127.0.0.1:50010".parse().unwrap();
        let key = FlowKey::new(8000, src);
        let reservation = match reg.try_get_or_reserve(key) {
            TryGetOrReserve::Reserved(r) => r,
            TryGetOrReserve::Existing(_) => panic!("expected Reserved, got Existing"),
            TryGetOrReserve::CapExhausted => panic!("expected Reserved, got CapExhausted"),
        };
        let flow = UdpFlow::for_test(src).await;
        reg.commit(reservation, Arc::clone(&flow));
        assert_eq!(reg.len(), 1, "flow is live before the sweep");

        // `for_test` stamps `last_seen` to creation time. Sleep past a
        // deliberately tiny window with real time (the sweep reads
        // `Instant::now()`) so the flow is unambiguously idle on every
        // platform; the 30ms / 2ms margin is robust to CI scheduling jitter,
        // which can only lengthen the sleep, never shorten it.
        std::thread::sleep(Duration::from_millis(30));
        sweep_once(&reg, Duration::from_millis(2), RuleId(7));

        assert_eq!(reg.len(), 0, "idle flow must be evicted by the sweep");
        assert!(reg.get(key).is_none(), "registry slot must be gone");
        assert!(flow.cancel.is_cancelled(), "flow.cancel must fire");
    }

    /// A fresh flow whose `last_seen` is within the idle window survives
    /// the sweep — covers the `false` arm of the idle predicate.
    #[tokio::test]
    async fn sweep_once_keeps_fresh_flow() {
        let reg = UdpFlowRegistry::new(4);
        let src: SocketAddr = "127.0.0.1:50011".parse().unwrap();
        let key = FlowKey::new(8001, src);
        let reservation = match reg.try_get_or_reserve(key) {
            TryGetOrReserve::Reserved(r) => r,
            TryGetOrReserve::Existing(_) => panic!("expected Reserved, got Existing"),
            TryGetOrReserve::CapExhausted => panic!("expected Reserved, got CapExhausted"),
        };
        let flow = UdpFlow::for_test(src).await;
        // `for_test` seeds `last_seen` to "now"; a generous window keeps
        // the flow well inside its idle budget.
        reg.commit(reservation, Arc::clone(&flow));

        sweep_once(&reg, Duration::from_secs(3600), RuleId(8));

        assert_eq!(reg.len(), 1, "fresh flow must survive the sweep");
        assert!(reg.get(key).is_some(), "registry slot must remain live");
        assert!(
            !flow.cancel.is_cancelled(),
            "fresh flow must not be cancelled"
        );
    }

    /// `sweep_once` over an empty registry is a no-op — exercises the
    /// loop-never-enters path without touching any flow.
    #[tokio::test]
    async fn sweep_once_on_empty_registry_is_noop() {
        let reg = UdpFlowRegistry::new(4);
        sweep_once(&reg, Duration::from_millis(50), RuleId(9));
        assert_eq!(reg.len(), 0);
        assert!(reg.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn zero_idle_window_disables_reaper_without_panic() {
        let reg = UdpFlowRegistry::new(1);
        let cancel = CancellationToken::new();
        let h = tokio::spawn(run_reaper(
            Arc::clone(&reg),
            Duration::ZERO,
            RuleId(1),
            cancel.clone(),
        ));
        // Give it a moment to ensure it doesn't panic on interval creation.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!h.is_finished(), "reaper should still be awaiting cancel");
        cancel.cancel();
        h.await.unwrap();
    }
}
