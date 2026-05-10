//! In-memory registry of currently-connected clients.
//!
//! See `data-model.md` § `ConnectedClient`. Bounded by ≤100 entries (SC-004a).

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use portunus_core::ClientName;
use portunus_proto::v1::{Protocol, RuleStatus as ProtoRuleStatus, ServerMessage};
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
    /// Forwarding protocols this client can activate. Defaults to
    /// `{Protocol::Tcp}` for v0.3.0 clients that never send Hello;
    /// v0.4.0 clients populate this from `Hello.supported_protocols`.
    /// Used by `push-rule` validation to reject UDP rules pre-wire
    /// (HIGH-1 review fix). See `data-model.md` § Capability negotiation.
    pub supported_protocols: HashSet<Protocol>,

    /// Client binary version (007-multi-target-failover, R-007).
    /// Populated from `Hello.client_version` once the service layer has
    /// parsed the first inbound message; `None` for clients that have
    /// not yet sent (or never send) a Hello. Used by the operator HTTP
    /// guard to refuse multi-target push to a `< 0.7.0` client (which
    /// cannot decode `Rule.targets` and would activate a broken
    /// single-target rule with empty `target_host`).
    pub client_version: Option<String>,
}

impl ConnectedClient {
    /// Capability check used by `push-rule` validation. Returns false
    /// for `Protocol::Unspecified` regardless of what the set holds.
    #[must_use]
    pub fn supports(&self, p: Protocol) -> bool {
        if matches!(p, Protocol::Unspecified) {
            return false;
        }
        self.supported_protocols.contains(&p)
    }
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
        // Default to v0.3.0 capability set (TCP only) until/unless a
        // Hello carrying `supported_protocols` is observed. Service-layer
        // (T008) calls `set_supported_protocols` once it has parsed the
        // first inbound message.
        let mut default_caps = HashSet::new();
        default_caps.insert(Protocol::Tcp);
        let entry = ConnectedClient {
            client_name: client_name.clone(),
            remote_addr,
            connected_at: Utc::now(),
            cancel_token,
            session_id,
            outbound,
            status_waiters,
            supported_protocols: default_caps,
            client_version: None,
        };
        let mut guard = self.inner.write().await;
        if let Some(prev) = guard.insert(client_name, entry) {
            prev.cancel_token.cancel();
        }
        session_id
    }

    /// Replace the registered client's `supported_protocols` (called by
    /// the service layer once a Hello message has been parsed). The
    /// `session_id` guard mirrors `unregister`: a late-arriving Hello
    /// from a torn-down session must not clobber a freshly reconnected
    /// session's capabilities. Returns true on apply, false if the
    /// client/session pair is no longer current.
    pub async fn set_supported_protocols(
        &self,
        client_name: &ClientName,
        session_id: u64,
        caps: HashSet<Protocol>,
    ) -> bool {
        let mut guard = self.inner.write().await;
        if let Some(existing) = guard.get_mut(client_name)
            && existing.session_id == session_id
        {
            existing.supported_protocols = caps;
            return true;
        }
        false
    }

    /// Capability check used by the operator-side push-rule path. Looks
    /// up the connected client and asks whether it can activate
    /// `protocol`. Returns `None` when the client is not connected.
    pub async fn supports(&self, client_name: &ClientName, protocol: Protocol) -> Option<bool> {
        let guard = self.inner.read().await;
        guard.get(client_name).map(|c| c.supports(protocol))
    }

    /// Replace the registered client's `client_version` (called by the
    /// service layer once it has parsed the first Hello). Mirrors
    /// `set_supported_protocols` semantics — the `session_id` guard
    /// rejects late-arriving Hellos from a torn-down session.
    /// 007-multi-target-failover (R-007).
    pub async fn set_client_version(
        &self,
        client_name: &ClientName,
        session_id: u64,
        version: String,
    ) -> bool {
        let mut guard = self.inner.write().await;
        if let Some(existing) = guard.get_mut(client_name)
            && existing.session_id == session_id
        {
            existing.client_version = Some(version);
            return true;
        }
        false
    }

    /// Snapshot the connected client's last-known `client_version`.
    /// Returns `None` when the client is not connected, OR when the
    /// client has not yet sent a Hello with `client_version`.
    /// 007-multi-target-failover (R-007).
    pub async fn client_version_of(&self, client_name: &ClientName) -> Option<String> {
        let guard = self.inner.read().await;
        guard
            .get(client_name)
            .and_then(|c| c.client_version.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn make_client() -> ConnectedClient {
        let (tx, _rx) = mpsc::channel(1);
        let mut caps = HashSet::new();
        caps.insert(Protocol::Tcp);
        ConnectedClient {
            client_name: "edge-01".parse().expect("client name"),
            remote_addr: None,
            connected_at: Utc::now(),
            cancel_token: CancellationToken::new(),
            session_id: 0,
            outbound: tx,
            status_waiters: Arc::default(),
            supported_protocols: caps,
            client_version: None,
        }
    }

    #[test]
    fn fresh_connected_client_supports_only_tcp_by_default() {
        let c = make_client();
        assert!(c.supports(Protocol::Tcp));
        assert!(!c.supports(Protocol::Udp));
        // PROTOCOL_UNSPECIFIED is never reported as supported, even if
        // (somehow) inserted into the set — defence-in-depth for
        // `supports()`'s contract.
        assert!(!c.supports(Protocol::Unspecified));
    }

    #[test]
    fn adding_udp_to_capabilities_makes_supports_true() {
        let mut c = make_client();
        c.supported_protocols.insert(Protocol::Udp);
        assert!(c.supports(Protocol::Tcp));
        assert!(c.supports(Protocol::Udp));
    }

    #[test]
    fn supports_unspecified_is_always_false_even_if_in_set() {
        let mut c = make_client();
        c.supported_protocols.insert(Protocol::Unspecified);
        assert!(!c.supports(Protocol::Unspecified));
    }

    #[tokio::test]
    async fn register_defaults_to_tcp_only_until_set_supported_protocols() {
        let registry = ConnectedClients::new();
        let (tx, _rx) = mpsc::channel(1);
        let session_id = registry
            .register(
                "edge-01".parse().unwrap(),
                None,
                CancellationToken::new(),
                tx,
                Arc::default(),
            )
            .await;

        let name: ClientName = "edge-01".parse().unwrap();
        assert_eq!(registry.supports(&name, Protocol::Tcp).await, Some(true));
        assert_eq!(registry.supports(&name, Protocol::Udp).await, Some(false));

        let mut caps = HashSet::new();
        caps.insert(Protocol::Tcp);
        caps.insert(Protocol::Udp);
        assert!(
            registry
                .set_supported_protocols(&name, session_id, caps)
                .await
        );

        assert_eq!(registry.supports(&name, Protocol::Tcp).await, Some(true));
        assert_eq!(registry.supports(&name, Protocol::Udp).await, Some(true));
    }

    #[tokio::test]
    async fn set_supported_protocols_rejects_stale_session() {
        let registry = ConnectedClients::new();
        let (tx, _rx) = mpsc::channel(1);
        let _session_id = registry
            .register(
                "edge-01".parse().unwrap(),
                None,
                CancellationToken::new(),
                tx,
                Arc::default(),
            )
            .await;

        let name: ClientName = "edge-01".parse().unwrap();
        let mut caps = HashSet::new();
        caps.insert(Protocol::Udp);
        // Session id 99 was never issued — must be rejected so a late
        // Hello from a torn-down session can't clobber a reconnect.
        assert!(!registry.set_supported_protocols(&name, 99, caps).await);
        assert_eq!(registry.supports(&name, Protocol::Udp).await, Some(false));
    }
}
