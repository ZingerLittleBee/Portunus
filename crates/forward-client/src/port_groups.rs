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

// Module-wide allows: the SNI listener / port-group code mixes fields
// named `listener` / `listener_task`, `members` / `member`, and tests
// that drive the manager use `match` on single-variant patterns plus
// `panic!` in `if`-then for fatal-fixture branches. Any of those
// patterns trigger pedantic clippy lints whose autofix would either
// rename load-bearing identifiers or worsen readability.
#![allow(
    clippy::similar_names,
    clippy::single_match_else,
    clippy::single_match,
    clippy::manual_assert,
    clippy::match_same_arms,
    clippy::collapsible_if,
    clippy::redundant_pattern_matching,
    clippy::uninlined_format_args,
    clippy::no_effect_underscore_binding
)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use forward_core::{PortRange, RuleId, Target};
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
    /// REMOVE referenced an unknown rule_id. Inner value is the
    /// rule_id surfaced in the structured warn log.
    UnknownRuleId(#[allow(dead_code)] RuleId),
    /// Bind on `listen_port` failed. Inner value carries the
    /// underlying `io::Error` for the structured failure log.
    BindFailed(#[allow(dead_code)] std::io::Error),
    /// A duplicate rule_id was pushed. Inner value is the rule_id
    /// surfaced in the structured warn log.
    DuplicateRuleId(#[allow(dead_code)] RuleId),
}

/// One member rule of a SNI group. Holds the per-rule data plane
/// context the listener needs to dispatch a connection.
#[derive(Clone)]
struct GroupMember {
    rule_id: RuleId,
    sni_pattern: Option<String>,
    target: Target,
    target_port: u16,
    proxy_protocol: Option<forward_core::ProxyProtocolVersion>,
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
    _joins: Vec<tokio::task::JoinHandle<()>>,
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
    pub fn apply_push<R: Resolve + 'static>(
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
            proxy_protocol: rule
                .targets
                .first()
                .and_then(|target| target.spec.proxy_protocol),
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
                // First member — bind both IPv4 and IPv6 listeners where the
                // platform supports them, sharing one routing table.
                let listeners = crate::forwarder::range::bind_all(&PortRange::single(listen_port))
                    .map_err(|err| {
                        PortGroupError::BindFailed(std::io::Error::new(
                            std::io::ErrorKind::AddrInUse,
                            err.reason,
                        ))
                    })?;
                let counters = Arc::new(SniListenerCounters::default());
                let cancel = CancellationToken::new();
                let (table_tx, table_rx) = watch::channel(Arc::new(SniRoutingTable::default()));
                let (resolver_tx, resolver_rx) =
                    watch::channel(Arc::new(SniRouteResolver::default()));
                let mut joins = Vec::with_capacity(listeners.len());
                for (_port, listener) in listeners {
                    let listener_task = SniListener {
                        listen_port,
                        counters: Arc::clone(&counters),
                        table_rx: table_rx.clone(),
                        resolver_rx: resolver_rx.clone(),
                        cancel: cancel.clone(),
                    };
                    let live_resolver = Arc::clone(&resolver);
                    joins.push(tokio::spawn(async move {
                        listener_task.run(listener, live_resolver).await;
                    }));
                }

                let mut state = GroupState {
                    listen_port,
                    members: HashMap::new(),
                    table_tx,
                    resolver_tx,
                    counters,
                    cancel,
                    _joins: joins,
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

    /// Look up the per-rule stats so tests can read SNI counters
    /// from the same `Arc<RuleStats>` the listener bumps. Production
    /// code reads counters via the `RuleStats` already held in the
    /// control loop's `RuleSlot` map; this helper is only needed by
    /// the inline emission tests (T070).
    #[cfg(test)]
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
            .map(|(port, group)| {
                let (peek_buckets, peek_sum_micros, peek_count) =
                    group.counters.peek_histogram.snapshot();
                forward_proto::v1::SniListenerStats {
                    client_hello_peek_bucket_counts: peek_buckets,
                    client_hello_peek_sum_micros: peek_sum_micros,
                    client_hello_peek_count: peek_count,
                    listen_port: u32::from(*port),
                    sni_route_miss_total: group.counters.miss.load(Ordering::Relaxed),
                    client_hello_parse_failures_total: group
                        .counters
                        .parse_failures
                        .load(Ordering::Relaxed),
                }
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
                proxy_protocol: m.proxy_protocol,
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
            rate_limit: None,
            rate_limit_stats: None,
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
        // Use port 0 isn't possible; use ephemeral by trying a high
        // port. If the bind fails the test is still sensitive to the
        // logic — we wrap it in a port-pick loop.
        for port in 50_000..50_100 {
            let mut mgr = PortGroupManager::new();
            let r1 = rule(1, port, 9001, Some("api.example.com"));
            if let Ok(_) = mgr.apply_push(r1, live_resolver()) {
                assert!(mgr.is_sni_port(port));
                let r2 = rule(2, port, 9002, Some("admin.example.com"));
                let res = mgr.apply_push(r2, live_resolver());
                assert!(res.is_ok(), "second push must share listener: {res:?}");
                mgr.shutdown();
                return;
            }
        }
        panic!("could not bind a free port in 50000..50100");
    }

    #[tokio::test]
    async fn sni_resolver_slot_carries_first_target_proxy_protocol() {
        for port in 50_100..50_200 {
            let mut mgr = PortGroupManager::new();
            let mut rule = rule(1, port, 9001, Some("api.example.com"));
            rule.targets = vec![crate::forwarder::MultiTarget {
                spec: forward_core::RuleTarget {
                    host: "127.0.0.1".into(),
                    port: 9001,
                    priority: 0,
                    proxy_protocol: Some(forward_core::ProxyProtocolVersion::V2),
                },
                target: Target::Ip(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST)),
            }];

            if mgr.apply_push(rule, live_resolver()).is_ok() {
                {
                    let group = mgr.groups.get(&port).expect("group");
                    let routes = group.resolver_tx.borrow();
                    let slot = routes.slots.get(&RuleId(1)).expect("slot");
                    assert_eq!(
                        slot.proxy_protocol,
                        Some(forward_core::ProxyProtocolVersion::V2)
                    );
                }
                mgr.shutdown();
                return;
            }
        }
        panic!("could not bind a free port in 50100..50200");
    }

    #[tokio::test]
    async fn remove_last_member_unbinds_listener() {
        for port in 50_200..50_300 {
            let mut mgr = PortGroupManager::new();
            let r1 = rule(1, port, 9001, Some("api.example.com"));
            if let Ok(_) = mgr.apply_push(r1, live_resolver()) {
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

    fn make_rule(
        rule_id: u64,
        listen_port: u16,
        target: SocketAddr,
        sni: Option<&str>,
    ) -> ClientRule {
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
            rate_limit: None,
            rate_limit_stats: None,
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
        .expect("push api");
        mgr.apply_push(
            make_rule(2, listen_port, addr_b, Some("web.example.com")),
            live_resolver(),
        )
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
        .expect("push exact");
        mgr.apply_push(make_rule(2, listen_port, addr_fb, None), live_resolver())
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

    /// 009-tls-sni-routing T051 / T055: wildcard end-to-end. One
    /// `*.web.example.com` rule on a single port; three SNIs:
    ///   - `tenant.web.example.com` — single-label match → forwarded
    ///   - `web.example.com`        — no leading label   → dropped
    ///   - `a.b.web.example.com`    — extra label        → dropped
    /// Asserts the matching client's bytes reach the backend and
    /// the two misses leave the backend empty.
    #[tokio::test]
    async fn t051_wildcard_route_single_label_only() {
        let (addr_match, cap_match) = spawn_capture_backend().await;
        let listen_port = ephemeral_port().await;
        let mut mgr = PortGroupManager::new();
        mgr.apply_push(
            make_rule(1, listen_port, addr_match, Some("*.web.example.com")),
            live_resolver(),
        )
        .expect("push wildcard");

        // Match: tenant.web.example.com (one label before suffix).
        let bytes_match = build_client_hello(Some("tenant.web.example.com"));
        {
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .expect("connect match");
            conn.write_all(&bytes_match).await.expect("write match");
            drop(conn);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if cap_match.lock().await.len() >= bytes_match.len() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timeout: wildcard match did not reach backend ({}/{})",
                    cap_match.lock().await.len(),
                    bytes_match.len()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(cap_match.lock().await.as_slice(), bytes_match.as_slice());

        // Miss 1: web.example.com (no leading label).
        let bytes_no_left = build_client_hello(Some("web.example.com"));
        {
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .expect("connect no-leading");
            conn.write_all(&bytes_no_left).await.ok();
            let mut sink = [0u8; 1];
            let _ = conn.read(&mut sink).await;
            drop(conn);
        }

        // Miss 2: a.b.web.example.com (two labels before suffix).
        let bytes_extra = build_client_hello(Some("a.b.web.example.com"));
        {
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .expect("connect extra-label");
            conn.write_all(&bytes_extra).await.ok();
            let mut sink = [0u8; 1];
            let _ = conn.read(&mut sink).await;
            drop(conn);
        }

        // Allow any in-flight forwarding to settle, then assert the
        // backend received exactly the matching client's bytes —
        // nothing more.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let captured = cap_match.lock().await.clone();
        assert_eq!(
            captured.as_slice(),
            bytes_match.as_slice(),
            "wildcard misses must not forward bytes; expected {} bytes, got {}",
            bytes_match.len(),
            captured.len()
        );

        mgr.shutdown();
    }

    // =================================================================
    // 009-tls-sni-routing — Phase 7 (US5) emission tests.
    // =================================================================

    /// 009-tls-sni-routing T070: per-rule counters bump correctly.
    /// Two rules on one port (one exact, one fallback). Drive 3 exact
    /// hits + 2 fallback hits, then read the per-rule
    /// `sni_route_*_total` counters via `stats_for`.
    #[tokio::test]
    async fn t070_per_rule_counters_bump() {
        let (addr_exact, _cap_exact) = spawn_capture_backend().await;
        let (addr_fb, _cap_fb) = spawn_capture_backend().await;
        let listen_port = ephemeral_port().await;

        let mut mgr = PortGroupManager::new();
        mgr.apply_push(
            make_rule(1, listen_port, addr_exact, Some("api.example.com")),
            live_resolver(),
        )
        .expect("push exact");
        mgr.apply_push(make_rule(2, listen_port, addr_fb, None), live_resolver())
            .expect("push fallback");

        // 3 exact hits.
        for _ in 0..3 {
            let bytes = build_client_hello(Some("api.example.com"));
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .unwrap();
            conn.write_all(&bytes).await.ok();
            drop(conn);
        }
        // 2 fallback hits — unmatched SNI lands on the None rule.
        for _ in 0..2 {
            let bytes = build_client_hello(Some("nope.example.com"));
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .unwrap();
            conn.write_all(&bytes).await.ok();
            drop(conn);
        }

        // Wait for the listener to consume the connections; counters
        // are bumped synchronously inside `handle_accept` before
        // dispatch, so a short sleep is enough.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let s1 = mgr.stats_for(RuleId(1)).expect("stats r1");
            let s2 = mgr.stats_for(RuleId(2)).expect("stats r2");
            use std::sync::atomic::Ordering;
            let exact = s1.sni_route_exact_total.load(Ordering::Relaxed);
            let fb = s2.sni_route_fallback_total.load(Ordering::Relaxed);
            if exact >= 3 && fb >= 2 {
                assert_eq!(exact, 3);
                assert_eq!(fb, 2);
                // Cross-rule isolation: rule 1's fallback counter and
                // wildcard counter must stay at 0; rule 2's exact /
                // wildcard counters must stay at 0.
                assert_eq!(s1.sni_route_wildcard_total.load(Ordering::Relaxed), 0);
                assert_eq!(s1.sni_route_fallback_total.load(Ordering::Relaxed), 0);
                assert_eq!(s2.sni_route_exact_total.load(Ordering::Relaxed), 0);
                assert_eq!(s2.sni_route_wildcard_total.load(Ordering::Relaxed), 0);
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timeout: per-rule counters never reached expected values \
                     (exact={}, fallback={})",
                    exact, fb
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        mgr.shutdown();
    }

    /// 009-tls-sni-routing T071: listener-level counters bump on
    /// SNI miss (no fallback) and on ClientHello parse failure
    /// (plain HTTP request).
    #[tokio::test]
    async fn t071_listener_counters_bump() {
        let (addr_exact, _cap) = spawn_capture_backend().await;
        let listen_port = ephemeral_port().await;

        let mut mgr = PortGroupManager::new();
        mgr.apply_push(
            make_rule(1, listen_port, addr_exact, Some("api.example.com")),
            live_resolver(),
        )
        .expect("push");

        // 4 SNI misses (unmatched SNI, no fallback rule).
        for _ in 0..4 {
            let bytes = build_client_hello(Some("nope.example.com"));
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .unwrap();
            conn.write_all(&bytes).await.ok();
            let mut sink = [0u8; 1];
            let _ = conn.read(&mut sink).await;
            drop(conn);
        }
        // 3 plain-HTTP requests (parse failure).
        for _ in 0..3 {
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .unwrap();
            conn.write_all(b"GET / HTTP/1.1\r\nHost: api.example.com\r\n\r\n")
                .await
                .ok();
            let mut sink = [0u8; 1];
            let _ = conn.read(&mut sink).await;
            drop(conn);
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let snap = mgr.snapshot_listener_stats();
            let row = snap
                .iter()
                .find(|r| r.listen_port == u32::from(listen_port));
            if let Some(r) = row {
                if r.sni_route_miss_total >= 4 && r.client_hello_parse_failures_total >= 3 {
                    assert_eq!(r.sni_route_miss_total, 4);
                    assert_eq!(r.client_hello_parse_failures_total, 3);
                    break;
                }
            }
            if std::time::Instant::now() > deadline {
                let s = mgr.snapshot_listener_stats();
                panic!("timeout waiting for listener counters: {s:?}");
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        mgr.shutdown();
    }

    /// 009-tls-sni-routing T073: REMOVE-by-rule_id consistency
    /// (data-model.md INV-2). Push two SNI rules on the same port,
    /// REMOVE the second by rule_id (no port hint), confirm the
    /// listener stays bound for the first rule and the reverse
    /// `rule_to_port` index stays consistent.
    #[tokio::test]
    async fn t073_remove_by_rule_id_keeps_listener_for_survivor() {
        let (addr_a, cap_a) = spawn_capture_backend().await;
        let (addr_b, cap_b) = spawn_capture_backend().await;
        let listen_port = ephemeral_port().await;

        let mut mgr = PortGroupManager::new();
        mgr.apply_push(
            make_rule(1, listen_port, addr_a, Some("a.example.com")),
            live_resolver(),
        )
        .expect("push 1");
        mgr.apply_push(
            make_rule(2, listen_port, addr_b, Some("b.example.com")),
            live_resolver(),
        )
        .expect("push 2");
        assert!(mgr.is_sni_port(listen_port));

        // REMOVE rule 2 by id only — same wire shape as RuleUpdate(REMOVE).
        mgr.apply_remove(RuleId(2)).expect("remove 2");
        assert!(
            mgr.is_sni_port(listen_port),
            "listener must stay bound while rule 1 survives"
        );
        assert!(
            mgr.stats_for(RuleId(1)).is_some(),
            "rule 1 must still be tracked"
        );
        assert!(
            mgr.stats_for(RuleId(2)).is_none(),
            "rule 2 must be gone from the reverse index"
        );

        // Drive an a-host connection — must reach backend A.
        let bytes_a = build_client_hello(Some("a.example.com"));
        {
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .unwrap();
            conn.write_all(&bytes_a).await.ok();
            drop(conn);
        }
        // Drive a b-host connection — backend B must NOT be hit
        // (rule 2 is gone; no fallback exists).
        let bytes_b = build_client_hello(Some("b.example.com"));
        {
            let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .unwrap();
            conn.write_all(&bytes_b).await.ok();
            let mut sink = [0u8; 1];
            let _ = conn.read(&mut sink).await;
            drop(conn);
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if cap_a.lock().await.len() >= bytes_a.len() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timeout: surviving rule did not forward bytes ({}/{})",
                    cap_a.lock().await.len(),
                    bytes_a.len()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        assert_eq!(cap_a.lock().await.as_slice(), bytes_a.as_slice());
        assert!(
            cap_b.lock().await.is_empty(),
            "removed rule's backend must not be reached"
        );

        // Removing rule 1 too tears down the listener.
        mgr.apply_remove(RuleId(1)).expect("remove 1");
        assert!(!mgr.is_sni_port(listen_port));
    }

    /// 009-tls-sni-routing T076 part 1: a TCP connection that sits
    /// idle past `read_client_hello` deadline emits
    /// `tls.client_hello_timeout` (WARN, target = "tls_sni") and
    /// bumps the listener's parse-failure counter. The connection
    /// is closed without reaching any backend.
    ///
    /// Note: peek::READ_TIMEOUT is on the order of seconds; we
    /// just connect and never write. Test deliberately keeps the
    /// timeout short by checking the counter rather than waiting
    /// for the exact event.
    #[tokio::test]
    async fn t076_idle_connection_does_not_reach_backend() {
        let (addr_a, cap_a) = spawn_capture_backend().await;
        let listen_port = ephemeral_port().await;

        let mut mgr = PortGroupManager::new();
        mgr.apply_push(
            make_rule(1, listen_port, addr_a, Some("a.example.com")),
            live_resolver(),
        )
        .expect("push");

        // Connect and write nothing. The listener will eventually
        // time out the peek; we only check that the backend never
        // sees any bytes within a reasonable window.
        let conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
            .await
            .expect("connect");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        drop(conn);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            cap_a.lock().await.is_empty(),
            "idle connection must not deliver bytes to any backend"
        );
        mgr.shutdown();
    }

    /// 009-tls-sni-routing T076 part 2: a connection that sends a
    /// plain-HTTP request (`GET / HTTP/1.1\r\n\r\n`) is detected as
    /// non-TLS, bumps `client_hello_parse_failures_total`, and
    /// never reaches a backend.
    #[tokio::test]
    async fn t076_plain_http_is_rejected_at_peek() {
        let (addr_a, cap_a) = spawn_capture_backend().await;
        let listen_port = ephemeral_port().await;

        let mut mgr = PortGroupManager::new();
        mgr.apply_push(
            make_rule(1, listen_port, addr_a, Some("a.example.com")),
            live_resolver(),
        )
        .expect("push");

        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
            .await
            .expect("connect");
        conn.write_all(b"GET / HTTP/1.1\r\nHost: a.example.com\r\n\r\n")
            .await
            .ok();
        // The peek code rejects on the first non-TLS byte; close
        // before the listener can route anywhere.
        let mut sink = [0u8; 1];
        let _ = conn.read(&mut sink).await;
        drop(conn);

        // Allow listener to run handle_accept to completion.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let snap = mgr.snapshot_listener_stats();
            let row = snap
                .iter()
                .find(|r| r.listen_port == u32::from(listen_port));
            if let Some(r) = row {
                if r.client_hello_parse_failures_total >= 1 {
                    break;
                }
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timeout: parse-failure counter did not bump (snap={:?})",
                    mgr.snapshot_listener_stats()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            cap_a.lock().await.is_empty(),
            "non-TLS request must not reach any backend"
        );
        mgr.shutdown();
    }

    /// 009-tls-sni-routing T072: hot-reload preserves in-flight.
    ///
    /// Open a long-running SNI connection (rule 1 → backend A),
    /// wait until backend A has received the ClientHello bytes (proves
    /// the proxy is wired up), then push a NEW rule on the same
    /// listener and verify:
    ///   - The in-flight connection keeps delivering bytes to backend
    ///     A (the `send_replace` swap honours existing `Arc<…>`
    ///     snapshots — see `rebuild_watches`).
    ///   - A NEW connection that names the newly-pushed rule's SNI
    ///     reaches backend B.
    ///
    /// `data-model.md` §INV-2 / R-004: the listener task is owned by
    /// the manager; route-table swaps are atomic via
    /// `watch::Sender::send_replace`. This test catches any
    /// regression where a swap accidentally invalidates an in-flight
    /// `proxy_with_preread` task.
    #[tokio::test]
    async fn t072_hot_reload_preserves_in_flight_and_serves_new() {
        let (addr_a, cap_a) = spawn_capture_backend().await;
        let listen_port = ephemeral_port().await;

        let mut mgr = PortGroupManager::new();
        mgr.apply_push(
            make_rule(1, listen_port, addr_a, Some("a.example.com")),
            live_resolver(),
        )
        .expect("push 1");

        // Open the long-running connection. We send the ClientHello
        // first so the listener can route us; then we leave the
        // socket alive and stream a small payload AFTER the mutation
        // lands.
        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
            .await
            .expect("connect");
        let bytes_hello = build_client_hello(Some("a.example.com"));
        conn.write_all(&bytes_hello).await.expect("write hello");

        // Wait for backend A to capture the ClientHello bytes — that's
        // the signal that the proxy task has fully connected through.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if cap_a.lock().await.len() >= bytes_hello.len() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timeout: backend A did not receive ClientHello ({} / {})",
                    cap_a.lock().await.len(),
                    bytes_hello.len()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        // Apply the mutation: add rule 2 (new SNI → backend B). The
        // existing connection is now mid-flight — it must not be
        // disrupted.
        let (addr_b, cap_b) = spawn_capture_backend().await;
        mgr.apply_push(
            make_rule(2, listen_port, addr_b, Some("b.example.com")),
            live_resolver(),
        )
        .expect("push 2 mid-flight");

        // Stream a payload over the in-flight connection. Each byte
        // travels through the same `proxy_with_preread` task that was
        // dispatched against the pre-mutation routing table — the
        // table swap must NOT redirect it.
        let after_mutation = b"after-mutation-payload-for-rule-1".to_vec();
        conn.write_all(&after_mutation)
            .await
            .expect("write post-swap");
        drop(conn);

        // Wait for backend A to receive the full payload (ClientHello +
        // post-mutation bytes).
        let expected_total = bytes_hello.len() + after_mutation.len();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if cap_a.lock().await.len() >= expected_total {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timeout: in-flight connection lost bytes after mutation ({} / {})",
                    cap_a.lock().await.len(),
                    expected_total
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let captured_a = cap_a.lock().await.clone();
        assert!(
            captured_a.starts_with(&bytes_hello),
            "ClientHello must arrive byte-identically"
        );
        assert!(
            captured_a.ends_with(&after_mutation),
            "post-mutation bytes must reach backend A — hot-reload broke in-flight forwarding"
        );

        // Open a fresh connection with the NEW SNI. It must land on
        // backend B (rule 2), proving the table swap is visible to
        // every `accept` after the mutation.
        let bytes_b = build_client_hello(Some("b.example.com"));
        {
            let mut conn_b = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port))
                .await
                .expect("connect new");
            conn_b.write_all(&bytes_b).await.expect("write new");
            drop(conn_b);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if cap_b.lock().await.len() >= bytes_b.len() {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!(
                    "timeout: new connection did not reach backend B ({} / {})",
                    cap_b.lock().await.len(),
                    bytes_b.len()
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(cap_b.lock().await.as_slice(), bytes_b.as_slice());

        mgr.shutdown();
    }
}
