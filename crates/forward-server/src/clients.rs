//! In-memory registry of currently-connected clients.
//!
//! See `data-model.md` § `ConnectedClient`. Bounded by ≤100 entries (SC-004a).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use forward_core::ClientName;
use forward_proto::v1::{RuleStatus as ProtoRuleStatus, ServerMessage};
use tokio::sync::{Mutex, RwLock, oneshot};
use tokio_util::sync::CancellationToken;
use tonic::Status;

/// Channel used by the operator path to push `ServerMessage`s into a connected
/// session's outbound stream.
pub type OutboundSender = tokio::sync::mpsc::Sender<Result<ServerMessage, Status>>;

/// Per-session map of `request_id` → oneshot waiter for the matching client
/// `RuleStatus` echo. Cleared when the session ends (waiters get dropped,
/// callers see `await` return `Err`, which they translate to `ack_timeout`
/// or `client_not_connected` depending on phase).
pub type StatusWaiters = Arc<Mutex<HashMap<String, oneshot::Sender<ProtoRuleStatus>>>>;

#[derive(Debug, Clone)]
pub struct ConnectedClient {
    // Indexed by ClientName in the map, so the field is technically redundant
    // here — kept for symmetric snapshot consumers (operator API in US2/US3).
    #[allow(dead_code)]
    pub client_name: ClientName,
    pub remote_addr: Option<SocketAddr>,
    pub connected_at: DateTime<Utc>,
    pub cancel_token: CancellationToken,
    pub session_id: u64,
    pub outbound: OutboundSender,
    pub status_waiters: StatusWaiters,
}

#[derive(Debug, Clone)]
pub struct ConnectedClients {
    inner: Arc<RwLock<HashMap<ClientName, ConnectedClient>>>,
    next_session: Arc<AtomicU64>,
    session_root: CancellationToken,
}

impl Default for ConnectedClients {
    fn default() -> Self {
        Self {
            inner: Arc::default(),
            next_session: Arc::default(),
            session_root: CancellationToken::new(),
        }
    }
}

#[allow(dead_code)] // `new`, `len`, `is_empty` are exercised by tests and US3 metrics.
impl ConnectedClients {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a freshly-authenticated client. If a previous session for the
    /// same name is still tracked, its `cancel_token` is fired so the old
    /// stream tears down — only one live session per client at a time.
    pub async fn register(
        &self,
        client_name: ClientName,
        remote_addr: Option<SocketAddr>,
        cancel_token: CancellationToken,
        outbound: OutboundSender,
        status_waiters: StatusWaiters,
    ) -> u64 {
        let session_id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let entry = ConnectedClient {
            client_name: client_name.clone(),
            remote_addr,
            connected_at: Utc::now(),
            cancel_token,
            session_id,
            outbound,
            status_waiters,
        };
        let mut guard = self.inner.write().await;
        if let Some(prev) = guard.insert(client_name, entry) {
            prev.cancel_token.cancel();
        }
        session_id
    }

    /// Snapshot the (outbound, waiters) handles for a connected client, used
    /// by the operator path to push a `RuleUpdate` and await the matching
    /// `RuleStatus`.
    pub async fn handles(
        &self,
        client_name: &ClientName,
    ) -> Option<(OutboundSender, StatusWaiters)> {
        let guard = self.inner.read().await;
        guard
            .get(client_name)
            .map(|c| (c.outbound.clone(), c.status_waiters.clone()))
    }

    /// Remove the named client iff `session_id` matches the value seen at
    /// `register` time. Guards against a reconnect overwriting our entry
    /// before the previous session's drop runs.
    pub async fn unregister(&self, client_name: &ClientName, session_id: u64) {
        let mut guard = self.inner.write().await;
        if let Some(existing) = guard.get(client_name)
            && existing.session_id == session_id
        {
            guard.remove(client_name);
        }
    }

    /// Fire the connected client's cancel token (if any) — used by the
    /// `revoke` operator action.
    pub async fn disconnect(&self, client_name: &ClientName) -> bool {
        let guard = self.inner.read().await;
        if let Some(c) = guard.get(client_name) {
            c.cancel_token.cancel();
            true
        } else {
            false
        }
    }

    pub async fn snapshot(&self) -> HashMap<ClientName, ConnectedClient> {
        self.inner.read().await.clone()
    }

    pub async fn len(&self) -> usize {
        self.inner.read().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.inner.read().await.is_empty()
    }

    /// Root token whose `child()` is given to each client session. Cancel
    /// the root to tear down every connected client (server shutdown).
    #[must_use]
    pub fn session_root_token(&self) -> &CancellationToken {
        &self.session_root
    }

    pub fn shutdown(&self) {
        self.session_root.cancel();
    }
}
