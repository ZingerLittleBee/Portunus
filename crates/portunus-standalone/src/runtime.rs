//! Standalone runtime — startup gate, fatal channel, biased select shutdown.

use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use portunus_core::RuleId;
use portunus_forwarder::{LiveResolver, RuleStats, RuleStatusEvent, Shutdown, run_forwarder};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::reporter::spawn_standalone_reporter;
use crate::signal::install_standalone_signal_handler;

#[allow(clippy::too_many_lines)]
pub async fn run(cfg: Config, registry: HashMap<RuleId, String>) -> ExitCode {
    let registry = Arc::new(registry);
    let shutdown = Shutdown::new();

    let signal_task = match install_standalone_signal_handler(shutdown.clone()) {
        Ok(j) => j,
        Err(e) => {
            error!(event = "standalone.signal_install_failed", error = %e);
            return ExitCode::from(1);
        }
    };

    let resolver = match LiveResolver::with_system_defaults() {
        Ok(r) => Arc::new(r),
        Err(e) => {
            error!(event = "standalone.resolver_init_failed", error = %e);
            shutdown.trigger();
            let _ = signal_task.await;
            return ExitCode::from(1);
        }
    };

    #[cfg(unix)]
    log_fd_limit();

    let drain = Duration::from_secs(cfg.global.shutdown_drain_secs);
    let rule_stats_handles: Arc<RwLock<HashMap<RuleId, Arc<RuleStats>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let reporter_handle = spawn_standalone_reporter(
        Arc::clone(&rule_stats_handles),
        Arc::clone(&registry),
        Duration::from_secs(60),
        shutdown.token(),
    );

    let (status_tx, mut status_rx) = mpsc::channel(64);
    let (fatal_tx, mut fatal_rx) = mpsc::channel::<()>(1);

    let mut joinset = JoinSet::new();
    let expected: HashSet<RuleId> = registry.keys().copied().collect();
    let stats_for_main = Arc::clone(&rule_stats_handles);

    // into_iter_rules can fail if a rule can't be parsed; treat as fatal config
    // error (validate() would have caught most issues, but belt-and-suspenders).
    let rules_iter = match cfg.into_iter_rules() {
        Ok(it) => it,
        Err(e) => {
            error!(event = "standalone.rules_iter_failed", error = %e);
            shutdown.trigger();
            let _ = reporter_handle.await;
            let _ = signal_task.await;
            return ExitCode::from(1);
        }
    };

    for parsed in rules_iter {
        let rule_id = parsed.rule_id;
        let rule = parsed.into_client_rule();
        let listen_range = rule.listen_range; // PortRange: Copy
        // for_range already returns Arc<RuleStats>
        let stats = RuleStats::for_range(listen_range);
        match stats_for_main.write() {
            Ok(mut g) => {
                g.insert(rule_id, Arc::clone(&stats));
            }
            Err(e) => {
                error!(
                    event = "standalone.stats_registry_poisoned",
                    %rule_id,
                    error = %e
                );
            }
        }
        joinset.spawn(run_forwarder(
            rule,
            Arc::clone(&resolver),
            status_tx.clone(),
            shutdown.token(),
            drain,
            stats,
        ));
    }
    drop(status_tx);

    // ─── Startup gate ───
    let mut pending = expected;
    let mut startup_failures: Vec<(RuleId, String)> = Vec::new();
    while !pending.is_empty() {
        match status_rx.recv().await {
            Some(RuleStatusEvent::Activated { rule_id }) => {
                pending.remove(&rule_id);
            }
            Some(RuleStatusEvent::Failed { rule_id, reason }) => {
                pending.remove(&rule_id);
                startup_failures.push((rule_id, reason));
            }
            Some(RuleStatusEvent::Removed { rule_id }) => {
                warn!(event = "standalone.unexpected_removed", %rule_id);
                pending.remove(&rule_id);
            }
            None => break,
        }
    }
    if !startup_failures.is_empty() {
        eprintln!("error: {} rule(s) failed to bind:", startup_failures.len());
        for (id, why) in &startup_failures {
            let name = registry.get(id).map_or("?", String::as_str);
            eprintln!("  - {name} ({id}): {why}");
        }
        shutdown.trigger();
        while joinset.join_next().await.is_some() {}
        let _ = reporter_handle.await;
        let _ = signal_task.await;
        return ExitCode::from(1);
    }

    // ─── Run-time status forwarder ───
    let registry_clone = Arc::clone(&registry);
    let fatal_tx_clone = fatal_tx.clone();
    tokio::spawn(async move {
        while let Some(ev) = status_rx.recv().await {
            match ev {
                RuleStatusEvent::Failed { rule_id, reason } => {
                    let name = registry_clone.get(&rule_id).map_or("?", String::as_str);
                    error!(event = "rule.failed", %rule_id, rule_name = %name, %reason);
                    let _ = fatal_tx_clone.try_send(());
                }
                RuleStatusEvent::Removed { rule_id } => {
                    let name = registry_clone.get(&rule_id).map_or("?", String::as_str);
                    info!(event = "rule.removed", %rule_id, rule_name = %name);
                }
                RuleStatusEvent::Activated { rule_id } => {
                    let name = registry_clone.get(&rule_id).map_or("?", String::as_str);
                    info!(event = "rule.reactivated", %rule_id, rule_name = %name);
                }
            }
        }
    });
    drop(fatal_tx);

    // ─── Main select ───
    let mut fatal_flag = false;
    loop {
        tokio::select! {
            biased;
            Some(()) = fatal_rx.recv() => {
                error!(event = "standalone.fatal_shutdown");
                fatal_flag = true;
                shutdown.trigger();
            }
            join = joinset.join_next() => {
                match join {
                    Some(Err(e)) => {
                        error!(event = "standalone.task_panic", error = %e);
                        fatal_flag = true;
                        shutdown.trigger();
                    }
                    Some(Ok(())) => {}
                    None => break,
                }
            }
        }
    }

    if !shutdown.token().is_cancelled() {
        shutdown.trigger();
    }
    let _ = reporter_handle.await;
    let _ = signal_task.await;
    info!(event = "standalone.stopped");
    if fatal_flag {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(unix)]
#[allow(unsafe_code, clippy::cast_sign_loss)]
fn log_fd_limit() {
    let mut rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit is thread-safe POSIX; pointer is a valid mutable
    // reference to a local rlimit struct.
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut rlim) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        tracing::debug!(event = "standalone.rlimit_query_failed", error = %err);
        return;
    }
    tracing::info!(
        event = "standalone.rlimit_nofile",
        soft = rlim.rlim_cur,
        hard = rlim.rlim_max
    );
    if rlim.rlim_cur < 4096 {
        tracing::warn!(
            event = "standalone.rlimit_nofile_low",
            soft = rlim.rlim_cur,
            "set LimitNOFILE / --ulimit nofile to at least 4096"
        );
    }
}
