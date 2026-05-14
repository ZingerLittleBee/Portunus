//! Standalone reporter — periodic tracing dump per rule.
//! Each tick reads `RuleStats::snapshot_basic()`; lock-poison errors
//! warn-and-skip-tick (no panic).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use portunus_core::RuleId;
use portunus_forwarder::RuleStats;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

pub fn spawn_standalone_reporter(
    rule_stats: Arc<RwLock<HashMap<RuleId, Arc<RuleStats>>>>,
    registry: Arc<HashMap<RuleId, String>>,
    interval: Duration,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                _ = tick.tick() => {
                    let map = match rule_stats.read() {
                        Ok(g) => g,
                        Err(e) => {
                            tracing::warn!(event = "standalone.reporter_lock_poisoned",
                                           error = %e);
                            continue;
                        }
                    };
                    for (rule_id, rs) in map.iter() {
                        let snap = rs.snapshot_basic();
                        let name = registry.get(rule_id).map(String::as_str).unwrap_or("?");
                        tracing::info!(
                            event = "standalone.stats",
                            rule = %rule_id,
                            rule_name = %name,
                            in_bytes = snap.bytes_in,
                            out_bytes = snap.bytes_out,
                            active_conns = snap.active_connections,
                            datagrams_in = snap.datagrams_in,
                            datagrams_out = snap.datagrams_out,
                            active_flows = snap.active_flows,
                        );
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use portunus_forwarder::Shutdown;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reporter_exits_on_cancel() {
        let stats_map: Arc<RwLock<HashMap<RuleId, Arc<RuleStats>>>> =
            Arc::new(RwLock::new(HashMap::new()));
        let registry: Arc<HashMap<RuleId, String>> = Arc::new(HashMap::new());
        let shutdown = Shutdown::new();
        let h = spawn_standalone_reporter(
            stats_map,
            registry,
            Duration::from_millis(50),
            shutdown.token(),
        );
        shutdown.trigger();
        let r = tokio::time::timeout(Duration::from_secs(2), h).await;
        assert!(r.is_ok(), "reporter must exit promptly on cancel");
    }
}
