//! Graceful-shutdown plumbing.
//!
//! A single root [`Shutdown`] is constructed at process start; child tokens
//! are derived for every long-lived task that needs to stop on signal. Any
//! `SIGINT` / `SIGTERM` propagates to all children. Constitution Principle IV:
//! shutdown drains in-flight connections before terminating.
//!
//! Wired into `serve` in T035.

#![allow(dead_code)]

use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct Shutdown {
    token: CancellationToken,
}

impl Default for Shutdown {
    fn default() -> Self {
        Self::new()
    }
}

impl Shutdown {
    #[must_use]
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    #[must_use]
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }

    #[must_use]
    pub fn child(&self) -> CancellationToken {
        self.token.child_token()
    }

    pub fn trigger(&self) {
        self.token.cancel();
    }

    /// Wait for either a SIGINT or a SIGTERM (Unix) and trigger cancel.
    /// On non-Unix platforms, awaits Ctrl-C.
    #[cfg(unix)]
    pub async fn signal_handler(self) {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigint = signal(SignalKind::interrupt()).expect("install SIGINT handler");
        let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
        tokio::select! {
            _ = sigint.recv() => tracing::info!(event = "shutdown.signal", signal = "SIGINT"),
            _ = sigterm.recv() => tracing::info!(event = "shutdown.signal", signal = "SIGTERM"),
        }
        self.trigger();
    }

    #[cfg(not(unix))]
    pub async fn signal_handler(self) {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!(event = "shutdown.signal", signal = "CTRL_C");
        self.trigger();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[tokio::test]
    async fn cancel_propagates_to_child() {
        let shutdown = Shutdown::new();
        let child = shutdown.child();
        let observed = Arc::new(AtomicBool::new(false));
        let observed_clone = Arc::clone(&observed);
        let join = tokio::spawn(async move {
            child.cancelled().await;
            observed_clone.store(true, Ordering::SeqCst);
        });
        shutdown.trigger();
        join.await.unwrap();
        assert!(observed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn trigger_is_idempotent() {
        let shutdown = Shutdown::new();
        shutdown.trigger();
        shutdown.trigger();
        assert!(shutdown.token().is_cancelled());
    }
}
