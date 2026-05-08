//! `PortGroupManager` — single ownership root for SNI listeners.
//! Spec 009-tls-sni-routing data-model.md §2.4.
//!
//! Materialises `ClientRule`s with non-empty `sni_pattern` (and any
//! `sni_pattern = None` fallback rules pushed onto the same port)
//! into a single bound `SniListener` per `listen_port`. Tracks the
//! `(rule_id → listen_port)` reverse index so `RuleUpdate(REMOVE)`,
//! which carries only `rule_id`, can find its group (INV-2).
//!
//! Mode-Locked Lifetime (R-004): a group's mode (Legacy plain-TCP vs
//! SNI dispatch) is fixed for its lifetime. The server-side overlap
//! matrix refuses cross-mode pushes (HTTP 409
//! `conflict.legacy_to_sni_unsupported`) before they reach the wire,
//! so the manager only ever sees consistent compositions. Defensive
//! `ModeChangeUnsupported` is reserved for the case where wire
//! tampering or a future feature drift slips a bad shape past the
//! server.
//!
//! Phase 3 (T042/T043) implements the SNI-mode arm of the manager.
//! The Legacy arm is a stub that delegates to the existing v0.7
//! per-rule forwarder spawn (kept side-by-side per T043 / T069 until
//! US4 byte-stability is locked in).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use forward_core::{RuleId, Target};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::forwarder::ClientRule;
use crate::forwarder::sni::listener::{
    SniListener, SniListenerCounters, SniRouteResolver, SniRuleSlot,
};
use crate::forwarder::sni::route_table::SniRoutingTable;
use crate::forwarder::stats::RuleStats;
use crate::resolver::{LiveResolver, Resolve};

#[derive(Debug)]
pub enum PortGroupError {
    /// A push tried to flip the mode of an active listener (the
    /// server-side overlap matrix is the authoritative gate; this is
    /// a defensive backstop).
    ModeChangeUnsupported,
    /// REMOVE referenced an unknown rule_id.
    UnknownRuleId(RuleId),
    /// Bind on `0.0.0.0:listen_port` failed.
    BindFailed(std::io::Error),
    /// A duplicate rule_id was pushed.
    DuplicateRuleId(RuleId),
}

/// One member rule of a SNI group. Holds the per-rule data plane
/// context the listener needs to dispatch a connection.
#[derive(Clone)]
struct GroupMember {
    rule_id: RuleId,
    sni_pattern: Option<String>,
    target: Target,
    target_port: u16,
    prefer_ipv6: bool,
    listen_port: u16,
    stats: Arc<RuleStats>,
    sni_route_exact_total: Arc<AtomicU64>,
    sni_route_wildcard_total: Arc<AtomicU64>,
    sni_route_fallback_total: Arc<AtomicU64>,
}

/// Per-port runtime: the bound listener task, the table watch
/// senders, the cancel token, and the snapshot of members keyed by
/// rule_id (used by `apply_remove` to repopulate the watches).
struct GroupState {
    listen_port: u16,
    members: HashMap<RuleId, GroupMember>,
    table_tx: watch::Sender<Arc<SniRoutingTable>>,
    resolver_tx: watch::Sender<Arc<SniRouteResolver>>,
    counters: Arc<SniListenerCounters>,
    cancel: CancellationToken,
    /// Listener join handle. Dropped (and cancelled) when the group
    /// loses its last member.
    _join: tokio::task::JoinHandle<()>,
}

pub struct PortGroupManager {
    groups: HashMap<u16, GroupState>,
    rule_to_port: HashMap<RuleId, u16>,
}

impl PortGroupManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            groups: HashMap::new(),
            rule_to_port: HashMap::new(),
        }
    }

    /// `true` iff there is a live SNI listener on `listen_port`.
    /// The control loop (T043) checks this so a `sni_pattern = None`
    /// candidate routed against a port that already runs in SNI mode
    /// goes through the manager (as a fallback) instead of the
    /// legacy per-rule spawn path.
    #[must_use]
    pub fn is_sni_port(&self, listen_port: u16) -> bool {
        self.groups.contains_key(&listen_port)
    }

    /// Push a TCP single-port rule into its SNI group, spawning the
    /// listener task on first member.
    ///
    /// Caller MUST guarantee:
    /// - protocol == Tcp
    /// - listen_range.len() == 1 (single-port)
    ///
    /// The control loop (T043) keeps UDP and range rules on the v0.7
    /// per-rule path, so those never reach this method.
    pub async fn apply_push<R: Resolve + 'static>(
        &mut self,
        rule: ClientRule,
        resolver: Arc<LiveResolver<R>>,
    ) -> Result<Arc<RuleStats>, PortGroupError> {
        let listen_port = rule.listen_range.start();
        debug_assert_eq!(rule.listen_range.len(), 1);

        if self.rule_to_port.contains_key(&rule.rule_id) {
            return Err(PortGroupError::DuplicateRuleId(rule.rule_id));
        }

        let stats = RuleStats::for_range(rule.listen_range);
        // 009-tls-sni-routing T077: the per-rule SNI counters live on
        // `RuleStats` so the existing StatsReport tick reads them
        // alongside `bytes_in` / `active_connections` etc. The
        // listener bumps them via the `SniRuleSlot`; we share the
        // Arcs so both readers see the same totals.
        let member = GroupMember {
            rule_id: rule.rule_id,
            sni_pattern: rule.sni_pattern.clone(),
            target: rule.target.clone(),
            target_port: rule.target_range.start(),
            prefer_ipv6: rule.prefer_ipv6,
            listen_port,
            stats: Arc::clone(&stats),
            sni_route_exact_total: Arc::clone(&stats.sni_route_exact_total),
            sni_route_wildcard_total: Arc::clone(&stats.sni_route_wildcard_total),
            sni_route_fallback_total: Arc::clone(&stats.sni_route_fallback_total),
        };

        match self.groups.get_mut(&listen_port) {
            Some(group) => {
                if group.members.contains_key(&rule.rule_id) {
                    return Err(PortGroupError::DuplicateRuleId(rule.rule_id));
                }
                group.members.insert(rule.rule_id, member);
                self.rule_to_port.insert(rule.rule_id, listen_port);
                rebuild_watches(group)?;
                info!(
                    target = "tls_sni",
                    event = "tls.sni_group.member_added",
                    listen_port,
                    rule_id = %rule.rule_id,
                    members = group.members.len(),
                );
                Ok(stats)
            }
            None => {
                // First member — bind the listener.
                let bind_addr = std::net::SocketAddr::new(
                    std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
                    listen_port,
                );
                let listener = TcpListener::bind(bind_addr)
                    .await
                    .map_err(PortGroupError::BindFailed)?;
                let counters = Arc::new(SniListenerCounters::default());
                let cancel = CancellationToken::new();
                let (table_tx, table_rx) =
                    watch::channel(Arc::new(SniRoutingTable::default()));
                let (resolver_tx, resolver_rx) =
                    watch::channel(Arc::new(SniRouteResolver::default()));
                let listener_task = SniListener {
                    listen_port,
                    counters: Arc::clone(&counters),
                    table_rx,
                    resolver_rx,
                    cancel: cancel.clone(),
                };
                let live_resolver = Arc::clone(&resolver);
                let join = tokio::spawn(async move {
                    listener_task.run(listener, live_resolver).await;
                });

                let mut state = GroupState {
                    listen_port,
                    members: HashMap::new(),
                    table_tx,
                    resolver_tx,
                    counters,
                    cancel,
                    _join: join,
                };
                state.members.insert(rule.rule_id, member);
                rebuild_watches(&mut state)?;
                self.groups.insert(listen_port, state);
                self.rule_to_port.insert(rule.rule_id, listen_port);
                info!(
                    target = "tls_sni",
                    event = "tls.sni_listener.bound",
                    listen_port,
                    rule_id = %rule.rule_id,
                );
                Ok(stats)
            }
        }
    }

    /// Remove a rule from its group. Tears down the listener when
    /// the last member departs.
    pub fn apply_remove(&mut self, rule_id: RuleId) -> Result<(), PortGroupError> {
        let Some(listen_port) = self.rule_to_port.remove(&rule_id) else {
            return Err(PortGroupError::UnknownRuleId(rule_id));
        };
        let Some(group) = self.groups.get_mut(&listen_port) else {
            return Err(PortGroupError::UnknownRuleId(rule_id));
        };
        group.members.remove(&rule_id);
        if group.members.is_empty() {
            // Last member — tear down the listener.
            let group = self.groups.remove(&listen_port).expect("just-checked");
            group.cancel.cancel();
            info!(
                target = "tls_sni",
                event = "tls.sni_listener.unbound",
                listen_port,
                rule_id = %rule_id,
            );
            // The join handle drops here; the listener task observes
            // `cancel` and exits.
        } else {
            rebuild_watches(group)?;
            info!(
                target = "tls_sni",
                event = "tls.sni_group.member_removed",
                listen_port,
                rule_id = %rule_id,
                members = group.members.len(),
            );
        }
        Ok(())
    }

    /// Cancel every group's listener task. Used during pump shutdown.
    pub fn shutdown(&mut self) {
        for (_, group) in self.groups.drain() {
            group.cancel.cancel();
        }
        self.rule_to_port.clear();
    }

    /// Look up the per-rule stats so the control loop can include
    /// the rule in its `RuleSlot` map for `StatsReport` aggregation.
    #[must_use]
    pub fn stats_for(&self, rule_id: RuleId) -> Option<Arc<RuleStats>> {
        let port = self.rule_to_port.get(&rule_id)?;
        self.groups
            .get(port)?
            .members
            .get(&rule_id)
            .map(|m| Arc::clone(&m.stats))
    }

    /// `true` if at least one SNI listener is bound. The control
    /// loop uses this to know whether `StatsReport.sni_listener_stats`
    /// might carry rows even when no per-rule slots exist.
    #[must_use]
    pub fn has_any_listener(&self) -> bool {
        !self.groups.is_empty()
    }

    /// 009-tls-sni-routing T078: snapshot the per-listener counters
    /// for each bound SNI listener. Returns one
    /// `proto::SniListenerStats` per port. Listeners with all-zero
    /// counters still emit a row so the server can render the
    /// listener as "active but quiet" (Prometheus `_total` style).
    /// Empty Vec when no listener is bound — proto3 default-stripping
    /// keeps the wire shape byte-identical with v0.8.
    #[must_use]
    pub fn snapshot_listener_stats(&self) -> Vec<forward_proto::v1::SniListenerStats> {
        use std::sync::atomic::Ordering;
        self.groups
            .iter()
            .map(|(port, group)| forward_proto::v1::SniListenerStats {
                listen_port: u32::from(*port),
                sni_route_miss_total: group.counters.miss.load(Ordering::Relaxed),
                client_hello_parse_failures_total: group
                    .counters
                    .parse_failures
                    .load(Ordering::Relaxed),
            })
            .collect()
    }
}

impl Default for PortGroupManager {
    fn default() -> Self {
        Self::new()
    }
}

fn rebuild_watches(group: &mut GroupState) -> Result<(), PortGroupError> {
    // Build the routing table from the current members.
    let members_for_table: Vec<(Option<&str>, RuleId)> = group
        .members
        .values()
        .map(|m| (m.sni_pattern.as_deref(), m.rule_id))
        .collect();
    let table = match SniRoutingTable::from_members(&members_for_table) {
        Ok(t) => t,
        Err(e) => {
            warn!(
                target = "tls_sni",
                event = "tls.sni_table.rebuild_failed",
                listen_port = group.listen_port,
                error = ?e,
            );
            return Err(PortGroupError::ModeChangeUnsupported);
        }
    };

    // Build the per-rule resolver slots.
    let mut slots = HashMap::with_capacity(group.members.len());
    for (id, m) in &group.members {
        slots.insert(
            *id,
            SniRuleSlot {
                rule_id: m.rule_id,
                target: m.target.clone(),
                target_port: m.target_port,
                prefer_ipv6: m.prefer_ipv6,
                listen_port: m.listen_port,
                stats: Arc::clone(&m.stats),
                sni_route_exact_total: Arc::clone(&m.sni_route_exact_total),
                sni_route_wildcard_total: Arc::clone(&m.sni_route_wildcard_total),
                sni_route_fallback_total: Arc::clone(&m.sni_route_fallback_total),
            },
        );
    }
    let routes = Arc::new(SniRouteResolver { slots });

    // Atomic-ish swap: `send_replace` keeps in-flight `borrow()` snapshots
    // valid until they drop, so a connection that just snapshotted the
    // old table will continue to dispatch into the old per-rule slot
    // (R-004 / hot-reload preserves in-flight per-spec).
    group.table_tx.send_replace(table);
    group.resolver_tx.send_replace(routes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::{Resolve, ResolveAnswer, ResolverConfig, ResolverError};
    use forward_core::{Hostname, PortRange, RuleId};
    use forward_proto::v1::Protocol;
    use std::net::Ipv4Addr;

    #[derive(Default)]
    struct Panicking;
    #[async_trait::async_trait]
    impl Resolve for Panicking {
        async fn resolve(&self, _name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
            unreachable!();
        }
    }

    fn rule(rule_id: u64, listen_port: u16, target_port: u16, sni: Option<&str>) -> ClientRule {
        ClientRule {
            rule_id: RuleId(rule_id),
            listen_range: PortRange::single(listen_port),
            target_host: "127.0.0.1".into(),
            target: Target::Ip(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST)),
            target_range: PortRange::single(target_port),
            prefer_ipv6: false,
            protocol: Protocol::Tcp,
            udp_max_flows: 0,
            udp_flow_idle_secs: 0,
            targets: Vec::new(),
            health_check_interval_secs: None,
            multi_target_obs: None,
            sni_pattern: sni.map(str::to_string),
        }
    }

    fn live_resolver() -> Arc<LiveResolver<Panicking>> {
        Arc::new(LiveResolver::new(
            Arc::new(Panicking),
            ResolverConfig::default(),
        ))
    }

    #[tokio::test]
    async fn first_push_binds_listener_second_share_it() {
        let mut mgr = PortGroupManager::new();
        // Use port 0 isn't possible; use ephemeral by trying a high
        // port. If the bind fails the test is still sensitive to the
        // logic — we wrap it in a port-pick loop.
        for port in 50_000..50_100 {
            mgr = PortGroupManager::new();
            let r1 = rule(1, port, 9001, Some("api.example.com"));
            if let Ok(_) = mgr.apply_push(r1, live_resolver()).await {
                assert!(mgr.is_sni_port(port));
                let r2 = rule(2, port, 9002, Some("admin.example.com"));
                let res = mgr.apply_push(r2, live_resolver()).await;
                assert!(res.is_ok(), "second push must share listener: {res:?}");
                mgr.shutdown();
                return;
            }
        }
        panic!("could not bind a free port in 50000..50100");
    }

    #[tokio::test]
    async fn remove_last_member_unbinds_listener() {
        for port in 50_200..50_300 {
            let mut mgr = PortGroupManager::new();
            let r1 = rule(1, port, 9001, Some("api.example.com"));
            if let Ok(_) = mgr.apply_push(r1, live_resolver()).await {
                assert!(mgr.is_sni_port(port));
                mgr.apply_remove(RuleId(1)).expect("remove ok");
                assert!(!mgr.is_sni_port(port));
                return;
            }
        }
        panic!("could not bind a free port in 50200..50300");
    }

    #[tokio::test]
    async fn remove_unknown_rule_id_errors() {
        let mut mgr = PortGroupManager::new();
        let err = mgr
            .apply_remove(RuleId(999))
            .expect_err("unknown rule must error");
        match err {
            PortGroupError::UnknownRuleId(id) => assert_eq!(id, RuleId(999)),
            other => panic!("got {other:?}"),
        }
    }
}

// =====================================================================
//   T035 / T046 — end-to-end exact-match SNI routing
// =====================================================================
//
// Integration-style tests that exercise the full client-side data
// plane: PortGroupManager → SniListener → ClientHello peek → routing
// table lookup → proxy_with_preread → upstream.
//
// `forward-client` is a bin-only crate today, so these can't live
// under `tests/` (no library to import). Inlined here so they
// share the same test compilation unit as the manager unit tests.

#[cfg(test)]
mod e2e_tests {
    use super::*;
    use crate::forwarder::sni::client_hello::build_client_hello;
    use crate::resolver::{Resolve, ResolveAnswer, ResolverConfig, ResolverError};
    use forward_core::{Hostname, PortRange, RuleId, Target};
    use forward_proto::v1::Protocol;
    use std::net::{Ipv4Addr, SocketAddr};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    #[derive(Default)]
    struct PanickingResolver;
    #[async_trait::async_trait]
    impl Resolve for PanickingResolver {
        async fn resolve(&self, name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
            panic!("PanickingResolver invoked for {name}");
        }
    }
    fn live_resolver() -> Arc<LiveResolver<PanickingResolver>> {
        Arc::new(LiveResolver::new(
            Arc::new(PanickingResolver),
            ResolverConfig::default(),
        ))
    }

    /// TCP backend that captures every byte it reads into a shared
    /// `Vec`. Echoes nothing back; clients are expected to close.
    async fn spawn_capture_backend() -> (SocketAddr, Arc<tokio::sync::Mutex<Vec<u8>>>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let captured = Arc::new(tokio::sync::Mutex::new(Vec::<u8>::new()));
        let task_cap = Arc::clone(&captured);
        tokio::spawn(async move {
            if let Ok((mut sock, _peer)) = listener.accept().await {
                let mut buf = vec![0u8; 8192];
                loop {
                    match sock.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => task_cap.lock().await.extend_from_slice(&buf[..n]),
                        Err(_) => break,
                    }
                }
            }
        });
        (addr, captured)
    }

    fn make_rule(rule_id: u64, listen_port: u16, target: SocketAddr, sni: Option<&str>) -> ClientRule {
        ClientRule {
            rule_id: RuleId(rule_id),
            listen_range: PortRange::single(listen_port),
            target_host: target.ip().to_string(),
            target: Target::Ip(target.ip()),
            target_range: PortRange::single(target.port()),
            prefer_ipv6: false,
            protocol: Protocol::Tcp,
            udp_max_flows: 0,
            udp_flow_idle_secs: 0,
            targets: Vec::new(),
            health_check_interval_secs: None,
            multi_target_obs: None,
            sni_pattern: sni.map(str::to_string),
        }
    }

    async fn ephemeral_port() -> u16 {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        port
    }

    #[tokio::test]
    async fn two_exact_sni_rules_fan_out_to_two_backends() {
        let (addr_a, cap_a) = spawn_capture_backend().await;
        let (addr_b, cap_b) = spawn_capture_backend().await;
        let listen_port = ephemeral_port().await;
        let mut mgr = PortGroupManager::new();
        mgr.apply_push(
            make_rule(1, listen_port, addr_a, Some("api.example.com")),
            live_resolver(),
        )
        .await
        .expect("push api");
        mgr.apply_push(
            make_rule(2, listen_port, addr_b, Some("web.example.com")),
            live_resolver(),
        )
        .await
        .expect("push web");

        let bytes_api = build_client_hello(Some("api.example.com"));
        {
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .expect("connect api");
            conn.write_all(&bytes_api).await.expect("write");
            // Closing the write half so the backend sees EOF and the
            // proxy returns. We keep the read half briefly to drain
            // any echoes (none in this test).
            drop(conn);
        }
        let bytes_web = build_client_hello(Some("web.example.com"));
        {
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .expect("connect web");
            conn.write_all(&bytes_web).await.expect("write");
            drop(conn);
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let a_len = cap_a.lock().await.len();
            let b_len = cap_b.lock().await.len();
            if a_len >= bytes_api.len() && b_len >= bytes_web.len() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timeout waiting for backends to capture (a={}/{}, b={}/{})",
                    a_len,
                    bytes_api.len(),
                    b_len,
                    bytes_web.len()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(cap_a.lock().await.as_slice(), bytes_api.as_slice());
        assert_eq!(cap_b.lock().await.as_slice(), bytes_web.as_slice());
        mgr.shutdown();
    }

    #[tokio::test]
    async fn unmatched_sni_no_fallback_drops_connection() {
        let (addr_a, cap_a) = spawn_capture_backend().await;
        let listen_port = ephemeral_port().await;
        let mut mgr = PortGroupManager::new();
        mgr.apply_push(
            make_rule(1, listen_port, addr_a, Some("api.example.com")),
            live_resolver(),
        )
        .await
        .expect("push");

        let bytes = build_client_hello(Some("nope.example.com"));
        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
            .await
            .expect("connect");
        conn.write_all(&bytes).await.expect("write");
        let mut buf = [0u8; 1];
        let _ = conn.read(&mut buf).await;
        drop(conn);

        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert!(
            cap_a.lock().await.is_empty(),
            "unmatched SNI must not reach any backend"
        );
        mgr.shutdown();
    }

    #[tokio::test]
    async fn fallback_catches_unmatched_sni() {
        let (addr_match, cap_match) = spawn_capture_backend().await;
        let (addr_fb, cap_fb) = spawn_capture_backend().await;
        let listen_port = ephemeral_port().await;

        let mut mgr = PortGroupManager::new();
        mgr.apply_push(
            make_rule(1, listen_port, addr_match, Some("api.example.com")),
            live_resolver(),
        )
        .await
        .expect("push exact");
        mgr.apply_push(
            make_rule(2, listen_port, addr_fb, None),
            live_resolver(),
        )
        .await
        .expect("push fallback");

        let bytes = build_client_hello(Some("nope.example.com"));
        {
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .expect("connect");
            conn.write_all(&bytes).await.expect("write");
            drop(conn);
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if cap_fb.lock().await.len() >= bytes.len() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timeout: fallback got {}/{}",
                    cap_fb.lock().await.len(),
                    bytes.len()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(cap_fb.lock().await.as_slice(), bytes.as_slice());
        assert!(
            cap_match.lock().await.is_empty(),
            "exact backend must not be hit by unmatched SNI"
        );
        mgr.shutdown();
    }
}
