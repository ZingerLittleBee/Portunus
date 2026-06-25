//! Standalone signal handler — SIGINT/SIGTERM trigger shutdown,
//! SIGHUP is a no-op (logged), and the handler also exits cleanly when
//! some other actor triggers `shutdown` (avoids signal_task.await
//! deadlock — spec §5 finding 1).

use std::io;
use tokio::task::JoinHandle;

use portunus_forwarder::Shutdown;

#[cfg(unix)]
pub fn install_standalone_signal_handler(shutdown: Shutdown) -> io::Result<JoinHandle<()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigint = signal(SignalKind::interrupt())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;
    let cancel = shutdown.token();
    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    tracing::debug!(event = "standalone.signal_handler_exit",
                                    reason = "shutdown_triggered_externally");
                    return;
                }
                _ = sigint.recv() => {
                    tracing::info!(event = "shutdown.signal", signal = "SIGINT");
                    shutdown.trigger();
                    return;
                }
                _ = sigterm.recv() => {
                    tracing::info!(event = "shutdown.signal", signal = "SIGTERM");
                    shutdown.trigger();
                    return;
                }
                _ = sighup.recv() => {
                    tracing::info!(event = "standalone.sighup_ignored");
                    // loop continues — SIGHUP is a no-op in standalone (no config reload)
                }
            }
        }
    }))
}

#[cfg(not(unix))]
pub fn install_standalone_signal_handler(shutdown: Shutdown) -> io::Result<JoinHandle<()>> {
    let cancel = shutdown.token();
    Ok(tokio::spawn(async move {
        tokio::select! {
            () = cancel.cancelled() => {
                tracing::debug!(event = "standalone.signal_handler_exit",
                                reason = "shutdown_triggered_externally");
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!(event = "shutdown.signal", signal = "CTRL_C");
                shutdown.trigger();
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handler_exits_when_shutdown_cancelled_externally() {
        let shutdown = Shutdown::new();
        let handle = install_standalone_signal_handler(shutdown.clone())
            .expect("signal install ok in test env");
        shutdown.trigger();
        let r = tokio::time::timeout(Duration::from_secs(2), handle).await;
        assert!(r.is_ok(), "signal task must exit when shutdown cancelled");
    }

    // NOTE: the SIGINT/SIGTERM/SIGHUP recv arms are intentionally left to the
    // process-level integration tests. Driving them from a unit test requires
    // raising a real signal at the test process (`libc::raise`), which both
    // needs an `unsafe` block — forbidden workspace-wide by `-D unsafe_code` —
    // and mutates process-global signal disposition shared across the whole
    // test binary. The externally-cancelled exit path above is the safely
    // unit-testable branch.
}
