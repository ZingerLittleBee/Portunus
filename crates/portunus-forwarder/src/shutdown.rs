//! Mirror of `portunus-server/src/shutdown.rs` for the client process.
//!
//! Constitution Principle IV: shutdown drains in-flight forwarded
//! connections before terminating.
//!
//! Wired into `main` in T038.

#![allow(dead_code)]

use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Default)]
pub struct Shutdown {
    token: CancellationToken,
}

impl Shutdown {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
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
}
