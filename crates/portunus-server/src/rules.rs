//! Server-side rule registry.
//!
//! Owns the authoritative runtime state of every forwarding rule. The
//! operator path mirrors this state into the SQLite rule store so active
//! rules can be replayed after a server restart. `Failed` is terminal-ish
//! for port reuse: it blocks `(client_name, listen_port)` until an explicit
//! `remove-rule`.
//!
//! Range support (002-port-range-forward): rules may now span a
//! contiguous listen-port range. Single-port rules are the degenerate
//! case where `listen_port_end == None` (or equivalently
//! `listen_range().len() == 1`). Conflict detection covers
//! single↔single, single↔range, range↔range overlap symmetrically via
//! a per-client `BTreeMap<u16, RuleId>` index keyed on each rule's
//! listen-range start.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use portunus_core::{ClientId, ClientName, PortRange, PortRangeError, RuleId};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

/// Flatten `Option<bool>` to a wire `bool` so HTTP responses
/// always emit `prefer_ipv6` even when the operator did not set it
/// (003-domain-name-forward / `contracts/operator-api.md` §
/// "Response (additive)").
fn serialize_prefer_ipv6_as_bool<S: serde::Serializer>(
    v: &Option<bool>,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.serialize_bool(v.unwrap_or(false))
}

pub use portunus_core::Protocol;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleState {
    Pending,
    Active,
    Failed { reason: String },
    Removed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: RuleId,
    /// Stable, opaque owner id (015-client-stable-id, T015). This — not
    /// the display name — is the canonical key the in-memory registry,
    /// the persisted `rules.client_id` column, and the reconnect-replay
    /// path key on, so a client rename never orphans its rules.
    pub client_id: ClientId,
    /// Free-form display name, kept for operator-facing messages / logs.
    pub client_name: ClientName,
    /// Range start (inclusive). For single-port rules this is also the
    /// only port (`listen_port_end == None`).
    pub listen_port: u16,
    /// Range end (inclusive). `None` for single-port rules
    /// (preserves v0.1.0 persistence verbatim — FR-005).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_port_end: Option<u16>,
    pub target_host: String,
    pub target_port: u16,
    /// Range end on the target side (symmetric to `listen_port_end`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_port_end: Option<u16>,
    /// Address-family preference for DNS-target rules
    /// (003-domain-name-forward, FR-007). Absent → IPv4-first.
    /// `Some(true)` → IPv6-first. Silently ignored for IP-literal
    /// targets.
    ///
    /// Wire form per `contracts/operator-api.md`: HTTP responses
    /// ALWAYS include the field as a flat `bool` (None → `false`)
    /// so generic operator tooling can rely on it being present.
    /// Internally we keep `Option<bool>` to distinguish
    /// operator-explicit-false from default-because-absent — needed
    /// later for persistence migrations and for log diagnostics.
    #[serde(default, serialize_with = "serialize_prefer_ipv6_as_bool")]
    pub prefer_ipv6: Option<bool>,
    pub protocol: Protocol,
    pub state: RuleState,
    pub created_at: DateTime<Utc>,
    pub last_state_change_at: DateTime<Utc>,
    /// Owner user id (005-multi-user-rbac, FR-014). Stamped by the
    /// HTTP push handler from the verified `OperatorIdentity`. Required
    /// (no Option) because every rule has an owner — superadmin-pushed
    /// rules are stamped with `UserId::superadmin()`. In-memory only;
    /// rules don't persist across restart, so neither does this field.
    pub owner_user_id: portunus_auth::UserId,

    /// Multi-target failover entries (007-multi-target-failover, FR-001).
    ///
    /// Empty `Vec` (the default) means "single-target rule —
    /// `target_host`/`target_port` carry the upstream and the
    /// single-target hot path applies, byte-identical to v0.6.0".
    /// Non-empty means a multi-target rule; in that case
    /// `target_host`/`target_port` are NOT mirrored from `targets[0]`
    /// — readers detect "multi-target" by `!targets.is_empty()`
    /// (encoding contract R-002 in `research.md`).
    ///
    /// Persistence: a v0.6.0 rules.json (no `targets` key, single-
    /// target only) deserialises with `targets: vec![]`, which the
    /// `targets_view()` helper promotes to a one-element view at
    /// read time. New multi-target rules persist with the field
    /// populated (atomic write, mode 0600 — same as v0.6.0).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<portunus_core::RuleTarget>,

    /// Optional active TCP-connect probe interval, seconds
    /// (007-multi-target-failover, FR-013). `None` (the default)
    /// means "passive failure detection only — no probe task is
    /// scheduled" (FR-015). When `Some(n)`, the client spawns a
    /// per-rule prober that probes each target round-robin at the
    /// configured cadence; `n` MUST be in `1..=3600`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_check_interval_secs: Option<u32>,

    /// 009-tls-sni-routing FR-001: optional Server Name Indication
    /// pattern for TLS dispatch. Only valid on TCP single-port rules
    /// (FR-002). `None` means plain TCP forward / TLS-only fallback,
    /// depending on listener mode (Mode-Locked Lifetime, FR-014).
    /// Always lowercased ASCII; grammar validated by the operator
    /// HTTP handler before persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sni_pattern: Option<String>,

    /// 011-rate-limiting-qos: optional per-rule cap envelope
    /// (`Rule.rate_limit = 12`). `None` means uncapped on every
    /// dimension — the legacy v0.10 hot path applies and the wire
    /// shape stays byte-identical to v0.10.
    ///
    /// Validation lives in `portunus_core::rate_limit::validate` and
    /// is run by the operator HTTP handler before persistence; the
    /// capability gate (`rate_limit_unsupported_by_client`) refuses
    /// to push a non-`None` envelope to a portunus-client whose
    /// last-known `Hello.client_version` is `< 0.11.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<portunus_core::RateLimit>,
}

impl crate::operator::rbac::HasOwner for Rule {
    fn owner(&self) -> &portunus_auth::UserId {
        &self.owner_user_id
    }
}

impl Rule {
    /// Listen-side range. For single-port rules this is a range of
    /// size 1 (`PortRange::single`).
    #[must_use]
    pub fn listen_range(&self) -> PortRange {
        match self.listen_port_end {
            Some(end) if end > self.listen_port => PortRange::new(self.listen_port, end)
                .unwrap_or_else(|_| PortRange::single(self.listen_port)),
            _ => PortRange::single(self.listen_port),
        }
    }

    /// Target-side range. Symmetric to `listen_range`. Currently
    /// unused on the server side (the gRPC handler reconstructs
    /// `target_range` from the proto), but kept for parity with
    /// `listen_range` and for future server-side validation.
    #[must_use]
    #[allow(dead_code)]
    pub fn target_range(&self) -> PortRange {
        match self.target_port_end {
            Some(end) if end > self.target_port => PortRange::new(self.target_port, end)
                .unwrap_or_else(|_| PortRange::single(self.target_port)),
            _ => PortRange::single(self.target_port),
        }
    }

    /// Number of listen ports in this rule (1 for single-port rules).
    /// Currently unused outside tests; surfaced for `--per-port`
    /// rendering helpers we expect to add.
    #[must_use]
    #[allow(dead_code)]
    pub fn range_size(&self) -> u32 {
        self.listen_range().len()
    }

    /// `true` iff the rule actually spans more than one port. Reserved
    /// for diagnostics that haven't shipped yet.
    #[must_use]
    #[allow(dead_code)]
    pub fn is_range(&self) -> bool {
        self.range_size() > 1
    }

    /// Read-side view of the rule's targets. Synthesises a one-element
    /// list from the legacy `target_host`/`target_port` fields when
    /// `targets` is empty (back-compat for v0.6.0-shaped rules — both
    /// freshly-pushed legacy rules AND rules loaded from a v0.6.0
    /// `rules.json`).
    ///
    /// Returns `None` only for the impossible case of an empty `targets`
    /// list combined with an empty `target_host` — which the validator
    /// would have rejected at push time.
    ///
    /// 007-multi-target-failover Phase 2 (T009).
    #[must_use]
    pub fn targets_view(&self) -> Vec<portunus_core::RuleTarget> {
        if self.targets.is_empty() {
            vec![portunus_core::RuleTarget {
                host: self.target_host.clone(),
                port: self.target_port,
                priority: 0,
                proxy_protocol: None,
            }]
        } else {
            self.targets.clone()
        }
    }

    /// True for rules that opt into the multi-target failover code
    /// path (i.e. carry an explicit `targets` list). Single-target
    /// rules — including the v0.6.0 legacy shape — return `false` and
    /// stay on the byte-identical hot path (Constitution Principle II).
    #[must_use]
    pub fn is_multi_target(&self) -> bool {
        !self.targets.is_empty()
    }
}

#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum RuleStoreError {
    /// A pushed rule overlaps an existing `Active` or `Failed` rule on
    /// the same client. `offending_port` names one port that is in
    /// conflict (the first one inside the overlap region). The HTTP /
    /// CLI surfaces include this in the error message so operators can
    /// pinpoint the collision (FR-010, US4).
    #[error("port_in_use: port {offending_port} already in use")]
    PortInUse { offending_port: u16 },

    #[error("rule_not_found")]
    NotFound,

    #[error("invalid_state_transition")]
    InvalidTransition,

    /// Pushed range size exceeds the operator-configured cap (FR-008).
    #[error("exceeds_cap: requested={requested} cap={cap}")]
    ExceedsCap { requested: u32, cap: u32 },

    /// Range failed structural validation (inverted, length mismatch, etc.).
    #[error("range_invalid: {0}")]
    RangeInvalid(PortRangeError),

    /// 004-udp-forward T017: target client did not declare the requested
    /// protocol in its `Hello.supported_protocols`. Surfaced to the
    /// operator as HTTP 422 / exit 3 with code `unsupported_protocol`
    /// (see `contracts/operator-api.md`). Carries both the client name
    /// (so the operator knows which deployment is stale) and the
    /// protocol string (`"udp"` in v0.4.0; reserved for future
    /// protocols).
    ///
    /// Reserved on the public surface: capability gating currently
    /// lives in `operator::cli::push_rule` (it has direct access to
    /// `ConnectedClients`). This variant is kept so a future caller
    /// that wants `ServerRuleStore` to enforce gating internally has a
    /// stable error code to thread through.
    #[allow(dead_code)]
    #[error("unsupported_protocol: client {client_name} does not support protocol {protocol}")]
    UnsupportedProtocol {
        client_name: ClientName,
        protocol: &'static str,
    },

    /// 009-tls-sni-routing: a candidate SNI rule has the same
    /// `sni_pattern` as an existing sibling on `(client, listen_port)`.
    /// Surfaced to the operator as HTTP 409 / `conflict.sni_route_duplicate`.
    #[error(
        "sni_route_duplicate: client {client_name} listen_port {listen_port} sni_pattern {sni_pattern} already in use"
    )]
    SniRouteDuplicate {
        client_name: ClientName,
        listen_port: u16,
        sni_pattern: String,
    },

    /// 009-tls-sni-routing: a candidate fallback rule (`sni_pattern = None`)
    /// is being pushed to a listener that already has a fallback slot.
    /// Surfaced as HTTP 409 / `conflict.sni_fallback_duplicate`.
    #[error(
        "sni_fallback_duplicate: client {client_name} listen_port {listen_port} already has a fallback rule"
    )]
    SniFallbackDuplicate {
        client_name: ClientName,
        listen_port: u16,
    },

    /// 009-tls-sni-routing FR-015: a candidate would flip an active
    /// listener's mode (legacy plain-TCP ↔ SNI dispatch). Refused with
    /// HTTP 409 / `conflict.legacy_to_sni_unsupported`. Operator must
    /// remove the existing rule first, then push the new shape onto a
    /// freshly bound listener.
    #[error(
        "legacy_to_sni_unsupported: client {client_name} listen_port {listen_port} has an active rule in {existing_mode} mode; remove it first before pushing in {candidate_mode} mode"
    )]
    LegacyToSniUnsupported {
        client_name: ClientName,
        listen_port: u16,
        existing_mode: &'static str,
        candidate_mode: &'static str,
    },
}

/// In-memory rule store. Cheap to clone (`Arc` internal); thread-safe via
/// `tokio::sync::RwLock`.
#[derive(Debug, Clone, Default)]
pub struct ServerRuleStore {
    inner: Arc<RwLock<Inner>>,
    next_id: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct Inner {
    rules: HashMap<RuleId, Rule>,
    /// Per-client interval index keyed on each rule's listen-range
    /// `start` port. Walks `range(..=candidate.end)` to find candidate
    /// overlaps in O(log N + matches). Tracks rules in `Active` or
    /// `Failed` state per Q4 (002-port-range-forward extends the same
    /// semantics to ranges).
    ///
    /// 009-tls-sni-routing T026: the value is `Vec<RuleId>` so a SNI
    /// listener can hold multiple TCP-single-port rules sharing the
    /// same `(client, listen_port)` (each with a distinct
    /// `sni_pattern`). For range rules and UDP rules the vec stays
    /// length-1 (overlap is still rejected the v0.7 way).
    by_client_listen_start: HashMap<ClientId, BTreeMap<u16, Vec<RuleId>>>,
}

impl ServerRuleStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a single-port rule (v0.1.0 compat shim). Equivalent to
    /// [`push_range`] with `PortRange::single` on both sides. Kept as
    /// a convenience for legacy tests and future single-port callers.
    /// The owner defaults to the built-in superadmin identity — only
    /// the test surface uses this path.
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub async fn push(
        &self,
        client_id: ClientId,
        client_name: ClientName,
        listen_port: u16,
        target_host: String,
        target_port: u16,
        protocol: Protocol,
        prefer_ipv6: Option<bool>,
    ) -> Result<Rule, RuleStoreError> {
        self.push_range(
            client_id,
            client_name,
            PortRange::single(listen_port),
            target_host,
            PortRange::single(target_port),
            protocol,
            prefer_ipv6,
            // No cap enforcement on the legacy single-port path —
            // size 1 is always under any positive cap. We pass
            // u32::MAX so callers that don't know the cap (tests,
            // legacy paths) aren't artificially blocked.
            u32::MAX,
            portunus_auth::UserId::superadmin(),
        )
        .await
    }

    /// 009-tls-sni-routing: same as [`push`] but with an explicit
    /// `sni_pattern`. Used by the SNI overlap-matrix tests.
    /// 009-tls-sni-routing: push a single-port TCP rule with an
    /// optional `sni_pattern`. Thin shim over `push_range_with_targets`
    /// for tests and future SNI-aware callers. Available outside the
    /// test cfg so integration tests in `tests/` can reach it.
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub async fn push_with_sni(
        &self,
        client_id: ClientId,
        client_name: ClientName,
        listen_port: u16,
        target_host: String,
        target_port: u16,
        protocol: Protocol,
        sni_pattern: Option<String>,
    ) -> Result<Rule, RuleStoreError> {
        self.push_range_with_targets(
            client_id,
            client_name,
            PortRange::single(listen_port),
            target_host,
            PortRange::single(target_port),
            protocol,
            None,
            u32::MAX,
            portunus_auth::UserId::superadmin(),
            Vec::new(),
            None,
            sni_pattern,
            None,
        )
        .await
    }

    /// Push a (potentially range) rule. Validates structure, enforces
    /// the configured cap, and rejects overlaps with any existing
    /// `Active`/`Failed` rule on the same client. The `owner_user_id`
    /// is stamped on the new rule (005-multi-user-rbac, FR-014).
    ///
    /// 007-multi-target-failover (Phase 3 / T022): single-target callers
    /// use this thin shim which forwards `targets: vec![]` and
    /// `health_check_interval_secs: None`, preserving the v0.6.0
    /// behaviour byte-for-byte. Multi-target callers use
    /// `push_range_with_targets` directly.
    #[allow(clippy::too_many_arguments)]
    pub async fn push_range(
        &self,
        client_id: ClientId,
        client_name: ClientName,
        listen: PortRange,
        target_host: String,
        target: PortRange,
        protocol: Protocol,
        prefer_ipv6: Option<bool>,
        range_cap: u32,
        owner_user_id: portunus_auth::UserId,
    ) -> Result<Rule, RuleStoreError> {
        self.push_range_with_targets(
            client_id,
            client_name,
            listen,
            target_host,
            target,
            protocol,
            prefer_ipv6,
            range_cap,
            owner_user_id,
            Vec::new(),
            None,
            None,
            None,
        )
        .await
    }

    /// Multi-target-aware variant of `push_range` (007-multi-target-failover).
    /// Pass `targets: Vec::new()` for the legacy single-target shape — the
    /// stored `Rule` will carry no `targets` entries and downstream
    /// readers see the byte-identical v0.6.0 shape.
    ///
    /// `health_check_interval_secs` is forwarded verbatim — `None` keeps
    /// passive-only failover (FR-015), `Some(n)` opts the rule into the
    /// active TCP-connect probe at the configured cadence.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub async fn push_range_with_targets(
        &self,
        client_id: ClientId,
        client_name: ClientName,
        listen: PortRange,
        target_host: String,
        target: PortRange,
        protocol: Protocol,
        prefer_ipv6: Option<bool>,
        range_cap: u32,
        owner_user_id: portunus_auth::UserId,
        targets: Vec<portunus_core::RuleTarget>,
        health_check_interval_secs: Option<u32>,
        sni_pattern: Option<String>,
        // 011-rate-limiting-qos T015/T016: optional per-rule cap
        // envelope. Already validated by the operator HTTP handler
        // (`portunus_core::rate_limit::validate`) before reaching this
        // helper. None preserves v0.10 byte-stable wire shape.
        rate_limit: Option<portunus_core::RateLimit>,
    ) -> Result<Rule, RuleStoreError> {
        // Structural validation (length match etc.).
        let (listen, target) =
            PortRange::pair(listen, target).map_err(RuleStoreError::RangeInvalid)?;

        let size = listen.len();
        if size > range_cap {
            return Err(RuleStoreError::ExceedsCap {
                requested: size,
                cap: range_cap,
            });
        }

        let candidate_is_single_tcp = protocol == Protocol::Tcp && listen.len() == 1;
        let candidate_listen_port = listen.start();

        let mut guard = self.inner.write().await;

        // Conflict check via the per-client interval index. We walk
        // every entry whose `start <= candidate.end` and inspect the
        // associated rule. Any rule whose listen_range overlaps the
        // candidate AND is in Active/Failed state AND uses the SAME
        // protocol blocks the push. 004-udp-forward T036: cross-protocol
        // rules on the same port coexist (UDP:6000 alongside TCP:6000
        // is legal because the kernel demuxes by protocol).
        //
        // 009-tls-sni-routing T026: TCP single-port candidates are
        // evaluated against the §Overlap matrix
        // (`specs/009-tls-sni-routing/data-model.md`):
        //   * legacy plain (None) ↔ SNI (Some) on the same port → reject
        //     with LegacyToSniUnsupported (mode-locked lifetime)
        //   * sni_pattern collision (same Some value or two None
        //     fallbacks) → reject with SniRouteDuplicate /
        //     SniFallbackDuplicate
        //   * distinct sni_pattern siblings → ACCEPT (the listener fans
        //     them out into one SniRoutingTable)
        if let Some(index) = guard.by_client_listen_start.get(&client_id) {
            for (_start, existing_ids) in index.range(..=listen.end()) {
                for existing_id in existing_ids {
                    let Some(existing) = guard.rules.get(existing_id) else {
                        continue;
                    };
                    if existing.protocol != protocol {
                        continue;
                    }
                    if !matches!(existing.state, RuleState::Active | RuleState::Failed { .. }) {
                        continue;
                    }
                    let existing_range = existing.listen_range();
                    if !existing_range.overlaps(listen) {
                        continue;
                    }

                    // 009-tls-sni-routing: SNI-aware overlap matrix
                    // applies only when BOTH sides are TCP single-port
                    // rules on the same listen_port. Anything else
                    // falls back to the v0.7 PortInUse decision.
                    let existing_is_single_tcp =
                        existing.protocol == Protocol::Tcp && existing.listen_port_end.is_none();
                    let existing_listen_port = existing.listen_port;
                    let same_single_port = candidate_is_single_tcp
                        && existing_is_single_tcp
                        && existing_listen_port == candidate_listen_port;

                    if same_single_port {
                        match (&existing.sni_pattern, &sni_pattern) {
                            (None, None) => {
                                // Two legacy fallback rules — duplicate.
                                // Caller will surface this as either
                                // PortInUse (back-compat) or
                                // SniFallbackDuplicate (sibling case).
                                // We default to PortInUse to preserve
                                // v0.7 behaviour for pure-legacy ports;
                                // the sni_fallback_duplicate code is
                                // reserved for the case where the
                                // EXISTING listener already carries SNI
                                // siblings — which can't happen here
                                // because both rules are None.
                                return Err(RuleStoreError::PortInUse {
                                    offending_port: candidate_listen_port,
                                });
                            }
                            (None, Some(_)) => {
                                return Err(RuleStoreError::LegacyToSniUnsupported {
                                    client_name: client_name.clone(),
                                    listen_port: candidate_listen_port,
                                    existing_mode: "legacy",
                                    candidate_mode: "sni",
                                });
                            }
                            (Some(_), None) => {
                                // Existing port is SNI mode; candidate
                                // is a fallback (None). Two outcomes:
                                //   - existing port already has a
                                //     fallback sibling (some other rule
                                //     with sni_pattern = None on this
                                //     port) → SniFallbackDuplicate.
                                //   - existing port has only SNI rules
                                //     and no fallback → ACCEPT (the
                                //     candidate becomes the fallback).
                                let mut has_fallback = false;
                                if let Some(siblings) = guard.by_client_listen_start.get(&client_id)
                                    && let Some(sibling_ids) = siblings.get(&candidate_listen_port)
                                {
                                    for sid in sibling_ids {
                                        if let Some(sib) = guard.rules.get(sid)
                                            && sib.protocol == Protocol::Tcp
                                            && sib.listen_port_end.is_none()
                                            && sib.sni_pattern.is_none()
                                        {
                                            has_fallback = true;
                                            break;
                                        }
                                    }
                                }
                                if has_fallback {
                                    return Err(RuleStoreError::SniFallbackDuplicate {
                                        client_name: client_name.clone(),
                                        listen_port: candidate_listen_port,
                                    });
                                }
                                // Else fall through; outer loop
                                // continues and ultimately the rule is
                                // accepted because no further conflict
                                // exists. Mark by setting a flag below.
                            }
                            (Some(existing_pat), Some(candidate_pat)) => {
                                if existing_pat == candidate_pat {
                                    return Err(RuleStoreError::SniRouteDuplicate {
                                        client_name: client_name.clone(),
                                        listen_port: candidate_listen_port,
                                        sni_pattern: candidate_pat.clone(),
                                    });
                                }
                                // Distinct SNI siblings — accept; the
                                // outer loop continues only to surface
                                // OTHER conflicts (e.g. an overlapping
                                // range rule on the same client/port).
                            }
                        }
                    } else {
                        // Either sides are not both TCP single-port,
                        // or the listen ports differ within an
                        // overlapping range. v0.7 semantics apply:
                        // any overlap of an Active/Failed rule blocks.
                        let offending = listen.start().max(existing_range.start());
                        return Err(RuleStoreError::PortInUse {
                            offending_port: offending,
                        });
                    }
                }
            }
        }

        let id = RuleId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let now = Utc::now();
        let listen_port_end = if listen.len() > 1 {
            Some(listen.end())
        } else {
            None
        };
        let target_port_end = if target.len() > 1 {
            Some(target.end())
        } else {
            None
        };
        let rule = Rule {
            id,
            client_id,
            client_name: client_name.clone(),
            listen_port: listen.start(),
            listen_port_end,
            target_host,
            target_port: target.start(),
            target_port_end,
            prefer_ipv6,
            protocol,
            state: RuleState::Pending,
            created_at: now,
            last_state_change_at: now,
            owner_user_id,
            targets,
            health_check_interval_secs,
            sni_pattern,
            // 011-rate-limiting-qos T015/T016: caps received from the
            // operator HTTP handler land here. None on a legacy push
            // preserves the v0.10 hot path byte-for-byte.
            rate_limit,
        };
        guard
            .by_client_listen_start
            .entry(client_id)
            .or_default()
            .entry(listen.start())
            .or_default()
            .push(id);
        guard.rules.insert(id, rule.clone());
        Ok(rule)
    }

    pub async fn mark_active(&self, id: RuleId) -> Result<(), RuleStoreError> {
        let mut guard = self.inner.write().await;
        let rule = guard.rules.get_mut(&id).ok_or(RuleStoreError::NotFound)?;
        if matches!(rule.state, RuleState::Removed) {
            return Err(RuleStoreError::InvalidTransition);
        }
        rule.state = RuleState::Active;
        rule.last_state_change_at = Utc::now();
        Ok(())
    }

    pub async fn mark_failed(&self, id: RuleId, reason: String) -> Result<(), RuleStoreError> {
        let mut guard = self.inner.write().await;
        let rule = guard.rules.get_mut(&id).ok_or(RuleStoreError::NotFound)?;
        if matches!(rule.state, RuleState::Removed) {
            return Err(RuleStoreError::InvalidTransition);
        }
        rule.state = RuleState::Failed { reason };
        rule.last_state_change_at = Utc::now();
        Ok(())
    }

    pub async fn mark_pending(&self, id: RuleId) -> Result<(), RuleStoreError> {
        let mut guard = self.inner.write().await;
        let rule = guard.rules.get_mut(&id).ok_or(RuleStoreError::NotFound)?;
        if matches!(rule.state, RuleState::Removed) {
            return Err(RuleStoreError::InvalidTransition);
        }
        rule.state = RuleState::Pending;
        rule.last_state_change_at = Utc::now();
        Ok(())
    }

    /// Remove the rule and free its conflict-index entry. Returns
    /// `NotFound` if the id is unknown — callers (the operator CLI)
    /// map that to exit 8.
    ///
    /// 009-tls-sni-routing T026: with multiple SNI rules per port, the
    /// per-(client, port) slot is a `Vec<RuleId>`; we drop only the
    /// matching id and remove the slot when the vec becomes empty.
    pub async fn remove(&self, id: RuleId) -> Result<Rule, RuleStoreError> {
        let mut guard = self.inner.write().await;
        let rule = guard.rules.remove(&id).ok_or(RuleStoreError::NotFound)?;
        if let Some(index) = guard.by_client_listen_start.get_mut(&rule.client_id) {
            if let Some(ids) = index.get_mut(&rule.listen_port) {
                ids.retain(|x| *x != id);
                if ids.is_empty() {
                    index.remove(&rule.listen_port);
                }
            }
            if index.is_empty() {
                guard.by_client_listen_start.remove(&rule.client_id);
            }
        }
        Ok(rule)
    }

    pub async fn get(&self, id: RuleId) -> Option<Rule> {
        self.inner.read().await.rules.get(&id).cloned()
    }

    /// Update only the per-rule rate-limit envelope in place. Used by
    /// the operator hot-update path (`PUT /v1/rules/{id}`), which keeps
    /// rule identity and listener shape unchanged while the data plane
    /// swaps limiter state under the same rule id.
    pub async fn update_rate_limit(
        &self,
        id: RuleId,
        rate_limit: Option<portunus_core::RateLimit>,
    ) -> Result<Rule, RuleStoreError> {
        let mut guard = self.inner.write().await;
        let rule = guard.rules.get_mut(&id).ok_or(RuleStoreError::NotFound)?;
        rule.rate_limit = rate_limit;
        Ok(rule.clone())
    }

    pub async fn hydrate(&self, rules: Vec<Rule>) {
        let mut guard = self.inner.write().await;
        let mut max_id = self.next_id.load(Ordering::Relaxed);
        for rule in rules {
            max_id = max_id.max(rule.id.0.saturating_add(1));
            guard
                .by_client_listen_start
                .entry(rule.client_id)
                .or_default()
                .entry(rule.listen_port)
                .or_default()
                .push(rule.id);
            guard.rules.insert(rule.id, rule);
        }
        self.next_id.store(max_id, Ordering::Relaxed);
    }

    /// 005-multi-user-rbac T036: snapshot of every rule owned by `user_id`.
    /// Used by the grant-revoke cascade to re-evaluate which of the user's
    /// rules survive without the dropped grant.
    pub async fn list_owned_by(&self, user_id: &portunus_auth::UserId) -> Vec<Rule> {
        self.inner
            .read()
            .await
            .rules
            .values()
            .filter(|r| &r.owner_user_id == user_id)
            .cloned()
            .collect()
    }

    /// 005-multi-user-rbac T033 cascade helper: remove every rule owned by
    /// `user_id` and return the freed `RuleId`s. Used by the user-removal
    /// path AFTER the operator-side identity flush has committed (R-006).
    pub async fn remove_owned_by(&self, user_id: &portunus_auth::UserId) -> Vec<u64> {
        let mut guard = self.inner.write().await;
        let to_remove: Vec<RuleId> = guard
            .rules
            .values()
            .filter(|r| &r.owner_user_id == user_id)
            .map(|r| r.id)
            .collect();
        let mut out = Vec::with_capacity(to_remove.len());
        for id in to_remove {
            if let Some(rule) = guard.rules.remove(&id) {
                if let Some(index) = guard.by_client_listen_start.get_mut(&rule.client_id) {
                    if let Some(ids) = index.get_mut(&rule.listen_port) {
                        ids.retain(|x| *x != id);
                        if ids.is_empty() {
                            index.remove(&rule.listen_port);
                        }
                    }
                    if index.is_empty() {
                        guard.by_client_listen_start.remove(&rule.client_id);
                    }
                }
                out.push(id.0);
            }
        }
        out
    }

    /// 015-client-stable-id: refresh the denormalized `client_name` on every
    /// in-memory rule owned by `client_id` after an identity-safe rename, so
    /// live `/v1/rules` snapshots (and any reader of `Rule.client_name`)
    /// reflect the new display name without waiting for a restart/hydration.
    /// Returns the number of rules updated.
    pub async fn rename_client(
        &self,
        client_id: &ClientId,
        new_name: &portunus_core::ClientName,
    ) -> usize {
        let mut guard = self.inner.write().await;
        let mut n = 0;
        for rule in guard.rules.values_mut() {
            if &rule.client_id == client_id {
                rule.client_name = new_name.clone();
                n += 1;
            }
        }
        n
    }

    /// Snapshot of every rule. `client_filter` narrows by stable id.
    pub async fn list(&self, client_filter: Option<&ClientId>) -> Vec<Rule> {
        let guard = self.inner.read().await;
        let mut out: Vec<Rule> = guard
            .rules
            .values()
            .filter(|r| client_filter.is_none_or(|c| &r.client_id == c))
            .cloned()
            .collect();
        out.sort_by_key(|r| r.id.0);
        out
    }

    /// 009-tls-sni-routing T081: count rules whose `sni_pattern` is set.
    /// Source of truth for the `portunus_tls_sni_routes_active` gauge,
    /// refreshed each `/metrics` scrape.
    #[must_use]
    pub async fn count_with_sni(&self) -> usize {
        let guard = self.inner.read().await;
        guard
            .rules
            .values()
            .filter(|r| r.sni_pattern.is_some())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn name(s: &str) -> ClientName {
        ClientName::from_str(s).unwrap()
    }

    /// 015-client-stable-id: deterministically map a display name to a
    /// stable `client_id` so a test that pushes several rules under one
    /// name keys them on the same id (mirroring the real one-id-per-name
    /// invariant), while distinct names get distinct ids.
    fn cid(s: &str) -> ClientId {
        let mut h: u128 = 0;
        for b in s.bytes() {
            h = h.wrapping_mul(31).wrapping_add(u128::from(b));
        }
        ClientId(ulid::Ulid::from(h))
    }

    async fn push_one(store: &ServerRuleStore) -> Rule {
        store
            .push(
                cid("edge-01"),
                name("edge-01"),
                18080,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn push_initial_state_is_pending() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        assert!(matches!(r.state, RuleState::Pending));
        assert_eq!(r.client_name, name("edge-01"));
        assert_eq!(r.listen_port, 18080);
        assert_eq!(r.listen_port_end, None);
        assert_eq!(r.target_port_end, None);
        assert_eq!(r.range_size(), 1);
        assert!(!r.is_range());
    }

    #[tokio::test]
    async fn pending_can_become_active() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        store.mark_active(r.id).await.unwrap();
        let after = store.get(r.id).await.unwrap();
        assert!(matches!(after.state, RuleState::Active));
        assert!(after.last_state_change_at >= r.last_state_change_at);
    }

    #[tokio::test]
    async fn pending_can_become_failed() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        store.mark_failed(r.id, "port_in_use".into()).await.unwrap();
        let after = store.get(r.id).await.unwrap();
        match after.state {
            RuleState::Failed { reason } => assert_eq!(reason, "port_in_use"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn active_can_be_replayed_or_fail_during_replay() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        store.mark_active(r.id).await.unwrap();
        store.mark_active(r.id).await.unwrap();
        let active = store.get(r.id).await.unwrap();
        assert!(matches!(active.state, RuleState::Active));

        store.mark_failed(r.id, "x".into()).await.unwrap();
        let failed = store.get(r.id).await.unwrap();
        assert!(matches!(failed.state, RuleState::Failed { reason } if reason == "x"));
    }

    #[tokio::test]
    async fn duplicate_active_blocks_push() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        store.mark_active(r.id).await.unwrap();
        let err = store
            .push(
                cid("edge-01"),
                name("edge-01"),
                18080,
                "x".into(),
                1,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap_err();
        match err {
            RuleStoreError::PortInUse { offending_port } => assert_eq!(offending_port, 18080),
            other => panic!("expected PortInUse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn failed_blocks_port_until_removed() {
        // Q4: Failed rules block port reuse until explicitly removed.
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        store.mark_failed(r.id, "port_in_use".into()).await.unwrap();
        // Re-push: blocked.
        assert!(matches!(
            store
                .push(
                    cid("edge-01"),
                    name("edge-01"),
                    18080,
                    "x".into(),
                    1,
                    Protocol::Tcp,
                    None,
                )
                .await,
            Err(RuleStoreError::PortInUse { .. })
        ));
        // Remove releases the slot.
        store.remove(r.id).await.unwrap();
        let r2 = push_one(&store).await;
        assert_ne!(r.id, r2.id, "RuleId must change across removes");
    }

    #[tokio::test]
    async fn remove_unknown_returns_not_found() {
        let store = ServerRuleStore::new();
        assert!(matches!(
            store.remove(RuleId(999)).await,
            Err(RuleStoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn list_filters_by_client() {
        let store = ServerRuleStore::new();
        store
            .push(
                cid("edge-a"),
                name("edge-a"),
                1000,
                "x".into(),
                1,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap();
        store
            .push(
                cid("edge-b"),
                name("edge-b"),
                1001,
                "x".into(),
                1,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap();
        assert_eq!(store.list(None).await.len(), 2);
        assert_eq!(store.list(Some(&cid("edge-a"))).await.len(), 1);
    }

    // --- T015 / T020: range push behavior ---

    async fn push_range(
        store: &ServerRuleStore,
        client: &str,
        l: u16,
        le: u16,
        t: u16,
        te: u16,
    ) -> Result<Rule, RuleStoreError> {
        store
            .push_range(
                cid(client),
                name(client),
                PortRange::new(l, le).unwrap(),
                "10.0.0.5".into(),
                PortRange::new(t, te).unwrap(),
                Protocol::Tcp,
                None,
                1024,
                portunus_auth::UserId::superadmin(),
            )
            .await
    }

    #[tokio::test]
    async fn push_range_rule_returns_single_id() {
        let store = ServerRuleStore::new();
        let r = push_range(&store, "edge-01", 30000, 30050, 30000, 30050)
            .await
            .unwrap();
        assert_eq!(r.range_size(), 51);
        assert!(r.is_range());
        assert_eq!(r.listen_port, 30000);
        assert_eq!(r.listen_port_end, Some(30050));
        assert_eq!(r.target_port_end, Some(30050));
        assert_eq!(store.list(None).await.len(), 1);
    }

    #[tokio::test]
    async fn push_range_assigns_pending_state() {
        let store = ServerRuleStore::new();
        let r = push_range(&store, "edge-01", 30000, 30050, 40000, 40050)
            .await
            .unwrap();
        assert!(matches!(r.state, RuleState::Pending));
    }

    #[tokio::test]
    async fn push_inverted_range_rejected_with_range_invalid() {
        // Constructing the PortRange itself fails — caller catches.
        // Here we exercise the store path: an explicit length-mismatch
        // is the structural error the store reports as RangeInvalid.
        let store = ServerRuleStore::new();
        let err = store
            .push_range(
                cid("edge-01"),
                name("edge-01"),
                PortRange::new(30000, 30050).unwrap(),
                "10.0.0.5".into(),
                PortRange::new(40000, 40000).unwrap(), // length 1 vs 51
                Protocol::Tcp,
                None,
                1024,
                portunus_auth::UserId::superadmin(),
            )
            .await
            .unwrap_err();
        match err {
            RuleStoreError::RangeInvalid(PortRangeError::LengthMismatch {
                listen_len,
                target_len,
            }) => {
                assert_eq!(listen_len, 51);
                assert_eq!(target_len, 1);
            }
            other => panic!("expected RangeInvalid(LengthMismatch), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn push_length_mismatch_rejected() {
        // Same shape as the above, just renaming the test for the spec
        // mapping (T015).
        let store = ServerRuleStore::new();
        let err = store
            .push_range(
                cid("edge-01"),
                name("edge-01"),
                PortRange::new(30000, 30002).unwrap(),
                "h".into(),
                PortRange::new(40000, 40005).unwrap(),
                Protocol::Tcp,
                None,
                1024,
                portunus_auth::UserId::superadmin(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            RuleStoreError::RangeInvalid(PortRangeError::LengthMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn push_exceeds_cap_rejected_with_named_limit() {
        let store = ServerRuleStore::new();
        let err = store
            .push_range(
                cid("edge-01"),
                name("edge-01"),
                PortRange::new(30000, 30100).unwrap(),
                "h".into(),
                PortRange::new(40000, 40100).unwrap(),
                Protocol::Tcp,
                None,
                50,
                portunus_auth::UserId::superadmin(),
            )
            .await
            .unwrap_err();
        match err {
            RuleStoreError::ExceedsCap { requested, cap } => {
                assert_eq!(requested, 101);
                assert_eq!(cap, 50);
            }
            other => panic!("expected ExceedsCap, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn push_range_size_1_behaves_like_single_port() {
        // Degenerate range with start == end → no listen_port_end.
        let store = ServerRuleStore::new();
        let r = store
            .push_range(
                cid("edge-01"),
                name("edge-01"),
                PortRange::single(18080),
                "10.0.0.5".into(),
                PortRange::single(8080),
                Protocol::Tcp,
                None,
                1024,
                portunus_auth::UserId::superadmin(),
            )
            .await
            .unwrap();
        assert_eq!(r.range_size(), 1);
        assert_eq!(r.listen_port_end, None);
        assert_eq!(r.target_port_end, None);
        assert!(!r.is_range());
    }

    // --- T049 (US4): overlap detection ---

    #[tokio::test]
    async fn range_overlapping_existing_range_returns_port_in_use_with_offending_port() {
        let store = ServerRuleStore::new();
        let a = push_range(&store, "edge-01", 30000, 30010, 40000, 40010)
            .await
            .unwrap();
        store.mark_active(a.id).await.unwrap();
        let err = push_range(&store, "edge-01", 30005, 30015, 40005, 40015)
            .await
            .unwrap_err();
        match err {
            RuleStoreError::PortInUse { offending_port } => {
                assert_eq!(offending_port, 30005);
            }
            other => panic!("expected PortInUse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn range_overlapping_existing_single_port_returns_port_in_use() {
        let store = ServerRuleStore::new();
        let single = push_one(&store).await; // listen_port = 18080
        store.mark_active(single.id).await.unwrap();
        let err = push_range(&store, "edge-01", 18075, 18085, 28075, 28085)
            .await
            .unwrap_err();
        match err {
            RuleStoreError::PortInUse { offending_port } => {
                // Overlap region is [18080, 18080]; offending port is
                // max(existing.start=18080, candidate.start=18075) = 18080.
                assert_eq!(offending_port, 18080);
            }
            other => panic!("expected PortInUse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn range_adjacent_no_overlap_succeeds() {
        // 30000-30010 is active; 30011-30020 is adjacent but disjoint.
        let store = ServerRuleStore::new();
        let a = push_range(&store, "edge-01", 30000, 30010, 40000, 40010)
            .await
            .unwrap();
        store.mark_active(a.id).await.unwrap();
        let b = push_range(&store, "edge-01", 30011, 30020, 40011, 40020)
            .await
            .unwrap();
        assert_ne!(a.id, b.id);
        assert_eq!(store.list(None).await.len(), 2);
    }

    #[tokio::test]
    async fn re_push_after_remove_succeeds() {
        // T034: removing a range frees ALL its ports for reuse.
        let store = ServerRuleStore::new();
        let a = push_range(&store, "edge-01", 30000, 30010, 40000, 40010)
            .await
            .unwrap();
        store.mark_active(a.id).await.unwrap();
        store.remove(a.id).await.unwrap();
        // A subset of the freed range should push successfully.
        let b = push_range(&store, "edge-01", 30005, 30008, 40005, 40008)
            .await
            .unwrap();
        assert_eq!(b.range_size(), 4);
    }

    // ---- 004-udp-forward T036 ----

    #[tokio::test]
    async fn udp_and_tcp_on_same_port_coexist() {
        // The kernel demuxes by protocol, so UDP:6000 and TCP:6000 on
        // the same client are legal. Pre-T036 the index was protocol-
        // agnostic and would have raised PortInUse here.
        let store = ServerRuleStore::new();
        let tcp = store
            .push(
                cid("edge-01"),
                name("edge-01"),
                6000,
                "127.0.0.1".into(),
                9999,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap();
        store.mark_active(tcp.id).await.unwrap();
        let udp = store
            .push(
                cid("edge-01"),
                name("edge-01"),
                6000,
                "127.0.0.1".into(),
                9999,
                Protocol::Udp,
                None,
            )
            .await
            .expect("UDP on same port MUST be accepted alongside TCP");
        assert_ne!(tcp.id, udp.id);
        assert_eq!(udp.protocol, Protocol::Udp);
    }

    #[tokio::test]
    async fn same_protocol_same_port_still_conflicts() {
        // Pinning the v0.1.0 invariant: UDP:6000 + UDP:6000 still fails.
        let store = ServerRuleStore::new();
        let first = store
            .push(
                cid("edge-01"),
                name("edge-01"),
                6000,
                "127.0.0.1".into(),
                9999,
                Protocol::Udp,
                None,
            )
            .await
            .unwrap();
        store.mark_active(first.id).await.unwrap();
        let err = store
            .push(
                cid("edge-01"),
                name("edge-01"),
                6000,
                "127.0.0.1".into(),
                9999,
                Protocol::Udp,
                None,
            )
            .await
            .unwrap_err();
        match err {
            RuleStoreError::PortInUse { offending_port } => assert_eq!(offending_port, 6000),
            other => panic!("expected PortInUse, got {other:?}"),
        }
    }

    /// 004-udp-forward T048: a UDP push of equal-length listen and
    /// target ranges succeeds; mismatched lengths return
    /// `mismatched_range` (exit 3) — same v0.2 validator path the TCP
    /// range push hits, just exercised under `Protocol::Udp` to pin
    /// down "no UDP escape hatch".
    #[tokio::test]
    async fn udp_range_push_validates_lengths() {
        let store = ServerRuleStore::new();
        // Equal lengths → accepted.
        let listen = PortRange::new(6010, 6019).unwrap();
        let target = PortRange::new(9990, 9999).unwrap();
        let rule = store
            .push_range(
                cid("edge-01"),
                name("edge-01"),
                listen,
                "127.0.0.1".into(),
                target,
                Protocol::Udp,
                None,
                u32::MAX,
                portunus_auth::UserId::superadmin(),
            )
            .await
            .expect("equal-length UDP range push must succeed");
        assert_eq!(rule.protocol, Protocol::Udp);
        assert_eq!(rule.listen_port, 6010);
        assert_eq!(rule.listen_port_end, Some(6019));
        assert_eq!(rule.target_port, 9990);
        assert_eq!(rule.target_port_end, Some(9999));

        // Mismatched lengths → mismatched_range (PortRange::pair guard).
        let bad_target = PortRange::new(9990, 9991).unwrap();
        let err = store
            .push_range(
                cid("edge-02"),
                name("edge-02"),
                listen,
                "127.0.0.1".into(),
                bad_target,
                Protocol::Udp,
                None,
                u32::MAX,
                portunus_auth::UserId::superadmin(),
            )
            .await
            .expect_err("mismatched ranges MUST be rejected for UDP too");
        match err {
            RuleStoreError::RangeInvalid(PortRangeError::LengthMismatch {
                listen_len,
                target_len,
            }) => {
                assert_eq!(listen_len, 10);
                assert_eq!(target_len, 2);
            }
            other => panic!("expected RangeInvalid(LengthMismatch), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ranges_on_different_clients_do_not_conflict() {
        let store = ServerRuleStore::new();
        let a = push_range(&store, "edge-a", 30000, 30010, 40000, 40010)
            .await
            .unwrap();
        store.mark_active(a.id).await.unwrap();
        // Same listen ports on a DIFFERENT client should succeed.
        let b = push_range(&store, "edge-b", 30000, 30010, 40000, 40010)
            .await
            .unwrap();
        assert_ne!(a.id, b.id);
    }

    // --- Direct Rule accessor coverage ---

    /// Build a single-target TCP `Rule` directly for the pure accessor
    /// helpers that never round-trip through the store push path.
    fn fixture_rule() -> Rule {
        let now = Utc::now();
        Rule {
            id: RuleId(1),
            client_id: cid("edge-01"),
            client_name: name("edge-01"),
            listen_port: 18080,
            listen_port_end: Some(18090),
            target_host: "10.0.0.5".into(),
            target_port: 8080,
            target_port_end: Some(8090),
            prefer_ipv6: None,
            protocol: Protocol::Tcp,
            state: RuleState::Pending,
            created_at: now,
            last_state_change_at: now,
            owner_user_id: portunus_auth::UserId::superadmin(),
            targets: Vec::new(),
            health_check_interval_secs: None,
            sni_pattern: None,
            rate_limit: None,
        }
    }

    #[test]
    fn has_owner_returns_stamped_owner() {
        use crate::operator::rbac::HasOwner;
        let rule = fixture_rule();
        assert_eq!(rule.owner(), &portunus_auth::UserId::superadmin());
    }

    #[test]
    fn listen_range_spans_explicit_end() {
        let rule = fixture_rule();
        let lr = rule.listen_range();
        assert_eq!(lr.start(), 18080);
        assert_eq!(lr.end(), 18090);
        assert_eq!(lr.len(), 11);
    }

    #[test]
    fn target_range_spans_explicit_end() {
        let rule = fixture_rule();
        let tr = rule.target_range();
        assert_eq!(tr.start(), 8080);
        assert_eq!(tr.end(), 8090);
        assert_eq!(tr.len(), 11);
    }

    #[test]
    fn target_range_single_port_when_end_absent() {
        let mut rule = fixture_rule();
        rule.target_port_end = None;
        let tr = rule.target_range();
        assert_eq!(tr.start(), 8080);
        assert_eq!(tr.end(), 8080);
        assert_eq!(tr.len(), 1);
    }

    #[test]
    fn is_multi_target_tracks_targets_list() {
        let mut rule = fixture_rule();
        assert!(!rule.is_multi_target());
        rule.targets = vec![portunus_core::RuleTarget {
            host: "10.0.0.6".into(),
            port: 8080,
            priority: 0,
            proxy_protocol: None,
        }];
        assert!(rule.is_multi_target());
        // targets_view reflects the explicit list when populated.
        let view = rule.targets_view();
        assert_eq!(view.len(), 1);
        assert_eq!(view[0].host, "10.0.0.6");
    }

    // --- State-transition error arms ---

    #[tokio::test]
    async fn mark_active_unknown_returns_not_found() {
        let store = ServerRuleStore::new();
        assert!(matches!(
            store.mark_active(RuleId(999)).await,
            Err(RuleStoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn mark_failed_unknown_returns_not_found() {
        let store = ServerRuleStore::new();
        assert!(matches!(
            store.mark_failed(RuleId(999), "x".into()).await,
            Err(RuleStoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn mark_pending_unknown_returns_not_found() {
        let store = ServerRuleStore::new();
        assert!(matches!(
            store.mark_pending(RuleId(999)).await,
            Err(RuleStoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn mark_pending_resets_active_rule() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        store.mark_active(r.id).await.unwrap();
        store.mark_pending(r.id).await.unwrap();
        let after = store.get(r.id).await.unwrap();
        assert!(matches!(after.state, RuleState::Pending));
    }

    /// Once a rule is in the `Removed` terminal state, every transition
    /// helper refuses with `InvalidTransition`. We force the state by
    /// hydrating a pre-built `Removed` rule (the public push path never
    /// yields `Removed`).
    async fn store_with_removed_rule() -> (ServerRuleStore, RuleId) {
        let store = ServerRuleStore::new();
        let mut rule = fixture_rule();
        rule.id = RuleId(7);
        rule.listen_port_end = None;
        rule.target_port_end = None;
        rule.state = RuleState::Removed;
        let id = rule.id;
        store.hydrate(vec![rule]).await;
        (store, id)
    }

    #[tokio::test]
    async fn mark_active_on_removed_is_invalid_transition() {
        let (store, id) = store_with_removed_rule().await;
        assert!(matches!(
            store.mark_active(id).await,
            Err(RuleStoreError::InvalidTransition)
        ));
    }

    #[tokio::test]
    async fn mark_failed_on_removed_is_invalid_transition() {
        let (store, id) = store_with_removed_rule().await;
        assert!(matches!(
            store.mark_failed(id, "x".into()).await,
            Err(RuleStoreError::InvalidTransition)
        ));
    }

    #[tokio::test]
    async fn mark_pending_on_removed_is_invalid_transition() {
        let (store, id) = store_with_removed_rule().await;
        assert!(matches!(
            store.mark_pending(id).await,
            Err(RuleStoreError::InvalidTransition)
        ));
    }

    // --- update_rate_limit ---

    #[tokio::test]
    async fn update_rate_limit_unknown_returns_not_found() {
        let store = ServerRuleStore::new();
        assert!(matches!(
            store
                .update_rate_limit(RuleId(999), Some(portunus_core::RateLimit::default()))
                .await,
            Err(RuleStoreError::NotFound)
        ));
    }

    #[tokio::test]
    async fn update_rate_limit_replaces_envelope_in_place() {
        let store = ServerRuleStore::new();
        let r = push_one(&store).await;
        assert!(r.rate_limit.is_none());
        let envelope = portunus_core::RateLimit {
            bandwidth_in_bps: Some(1_000),
            ..portunus_core::RateLimit::default()
        };
        let updated = store
            .update_rate_limit(r.id, Some(envelope.clone()))
            .await
            .unwrap();
        assert_eq!(updated.id, r.id);
        assert_eq!(updated.rate_limit, Some(envelope));
        // Clearing back to None is also supported.
        let cleared = store.update_rate_limit(r.id, None).await.unwrap();
        assert!(cleared.rate_limit.is_none());
    }

    // --- remove() index cleanup paths ---

    #[tokio::test]
    async fn remove_one_of_two_sni_siblings_keeps_slot() {
        // Two distinct-SNI siblings share (client, port). Removing one
        // retains the slot (vec non-empty); removing the second drops
        // the slot and the per-client index entry.
        let store = ServerRuleStore::new();
        let a = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                Some("a.example.com".into()),
            )
            .await
            .unwrap();
        store.mark_active(a.id).await.unwrap();
        let b = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                Some("b.example.com".into()),
            )
            .await
            .unwrap();
        store.mark_active(b.id).await.unwrap();

        // Remove the first sibling: slot survives because b still holds it.
        store.remove(a.id).await.unwrap();
        assert_eq!(store.list(None).await.len(), 1);
        // Removing the second empties the slot AND the per-client index.
        store.remove(b.id).await.unwrap();
        assert!(store.list(None).await.is_empty());
        // Re-pushing the freed port now succeeds (index entry was dropped).
        let c = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap();
        assert_ne!(c.id, a.id);
    }

    // --- remove_owned_by cascade ---

    #[tokio::test]
    async fn remove_owned_by_drops_only_matching_owner() {
        let store = ServerRuleStore::new();
        let alice = portunus_auth::UserId::superadmin();
        let bob = portunus_auth::UserId::from_str("bob").unwrap();

        // Two rules owned by superadmin (alice) and one by bob.
        let r1 = store
            .push_range(
                cid("edge-01"),
                name("edge-01"),
                PortRange::single(7001),
                "10.0.0.5".into(),
                PortRange::single(8001),
                Protocol::Tcp,
                None,
                u32::MAX,
                alice.clone(),
            )
            .await
            .unwrap();
        let r2 = store
            .push_range(
                cid("edge-01"),
                name("edge-01"),
                PortRange::single(7002),
                "10.0.0.5".into(),
                PortRange::single(8002),
                Protocol::Tcp,
                None,
                u32::MAX,
                alice.clone(),
            )
            .await
            .unwrap();
        let r3 = store
            .push_range(
                cid("edge-02"),
                name("edge-02"),
                PortRange::single(7003),
                "10.0.0.5".into(),
                PortRange::single(8003),
                Protocol::Tcp,
                None,
                u32::MAX,
                bob.clone(),
            )
            .await
            .unwrap();

        // list_owned_by sees exactly alice's two rules.
        let owned = store.list_owned_by(&alice).await;
        assert_eq!(owned.len(), 2);

        let mut freed = store.remove_owned_by(&alice).await;
        assert_eq!(freed.len(), 2);
        freed.sort_unstable();
        let mut expected = vec![r1.id.0, r2.id.0];
        expected.sort_unstable();
        assert_eq!(freed, expected);

        // Bob's rule survives; alice's are gone.
        let remaining = store.list(None).await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, r3.id);
        assert!(store.list_owned_by(&alice).await.is_empty());
    }

    #[tokio::test]
    async fn remove_owned_by_unknown_owner_is_noop() {
        let store = ServerRuleStore::new();
        push_one(&store).await;
        let ghost = portunus_auth::UserId::from_str("ghost").unwrap();
        assert!(store.remove_owned_by(&ghost).await.is_empty());
        assert_eq!(store.list(None).await.len(), 1);
    }

    // --- rename_client denormalization ---

    #[tokio::test]
    async fn rename_client_updates_all_matching_rules() {
        let store = ServerRuleStore::new();
        // Two rules under one client id, one under another.
        store
            .push(
                cid("edge-01"),
                name("edge-01"),
                7100,
                "10.0.0.5".into(),
                8100,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap();
        store
            .push(
                cid("edge-01"),
                name("edge-01"),
                7101,
                "10.0.0.5".into(),
                8101,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap();
        store
            .push(
                cid("edge-02"),
                name("edge-02"),
                7102,
                "10.0.0.5".into(),
                8102,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap();

        let renamed = store.rename_client(&cid("edge-01"), &name("renamed")).await;
        assert_eq!(renamed, 2);

        for rule in store.list(Some(&cid("edge-01"))).await {
            assert_eq!(rule.client_name, name("renamed"));
        }
        // The other client is untouched.
        for rule in store.list(Some(&cid("edge-02"))).await {
            assert_eq!(rule.client_name, name("edge-02"));
        }
    }

    #[tokio::test]
    async fn rename_client_unknown_returns_zero() {
        let store = ServerRuleStore::new();
        push_one(&store).await;
        let n = store.rename_client(&cid("nobody"), &name("x")).await;
        assert_eq!(n, 0);
    }

    // --- count_with_sni gauge source ---

    #[tokio::test]
    async fn count_with_sni_counts_only_sni_rules() {
        let store = ServerRuleStore::new();
        assert_eq!(store.count_with_sni().await, 0);
        // Plain rule does not count.
        push_one(&store).await;
        assert_eq!(store.count_with_sni().await, 0);
        // SNI rule bumps the count.
        store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                Some("a.example.com".into()),
            )
            .await
            .unwrap();
        assert_eq!(store.count_with_sni().await, 1);
    }

    // --- 009-tls-sni-routing overlap matrix ---

    #[tokio::test]
    async fn sni_distinct_siblings_accepted_duplicate_pattern_rejected() {
        let store = ServerRuleStore::new();
        let a = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                Some("a.example.com".into()),
            )
            .await
            .unwrap();
        store.mark_active(a.id).await.unwrap();
        // Distinct pattern on the same listen port is accepted.
        let b = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                Some("b.example.com".into()),
            )
            .await
            .unwrap();
        store.mark_active(b.id).await.unwrap();
        assert_ne!(a.id, b.id);

        // Re-using an existing pattern is a SniRouteDuplicate.
        let err = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                Some("a.example.com".into()),
            )
            .await
            .unwrap_err();
        match err {
            RuleStoreError::SniRouteDuplicate {
                listen_port,
                sni_pattern,
                ..
            } => {
                assert_eq!(listen_port, 8443);
                assert_eq!(sni_pattern, "a.example.com");
            }
            other => panic!("expected SniRouteDuplicate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sni_fallback_then_second_fallback_is_duplicate() {
        // Existing port carries an SNI rule plus a fallback (None)
        // sibling. Pushing a second fallback hits the
        // SniFallbackDuplicate arm (existing has_fallback == true).
        let store = ServerRuleStore::new();
        let sni = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                Some("a.example.com".into()),
            )
            .await
            .unwrap();
        store.mark_active(sni.id).await.unwrap();
        // First fallback accepted (existing port is SNI mode, no
        // fallback yet) — exercises the Some/None accept fall-through.
        let fb = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap();
        store.mark_active(fb.id).await.unwrap();
        // Second fallback now collides with the existing fallback sibling.
        let err = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap_err();
        match err {
            RuleStoreError::SniFallbackDuplicate { listen_port, .. } => {
                assert_eq!(listen_port, 8443);
            }
            other => panic!("expected SniFallbackDuplicate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn legacy_plain_then_sni_is_unsupported_mode_flip() {
        // Existing legacy plain-TCP rule (None) on a port; pushing an
        // SNI rule (Some) onto the same active listener flips the mode
        // and is rejected with LegacyToSniUnsupported.
        let store = ServerRuleStore::new();
        let legacy = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap();
        store.mark_active(legacy.id).await.unwrap();
        let err = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                Some("a.example.com".into()),
            )
            .await
            .unwrap_err();
        match err {
            RuleStoreError::LegacyToSniUnsupported {
                listen_port,
                existing_mode,
                candidate_mode,
                ..
            } => {
                assert_eq!(listen_port, 8443);
                assert_eq!(existing_mode, "legacy");
                assert_eq!(candidate_mode, "sni");
            }
            other => panic!("expected LegacyToSniUnsupported, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn two_legacy_fallbacks_same_port_is_port_in_use() {
        // Both candidate and existing are None on the same TCP single
        // port → the (None, None) arm surfaces PortInUse (v0.7 compat).
        let store = ServerRuleStore::new();
        let first = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap();
        store.mark_active(first.id).await.unwrap();
        let err = store
            .push_with_sni(
                cid("edge-01"),
                name("edge-01"),
                8443,
                "10.0.0.5".into(),
                8080,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap_err();
        match err {
            RuleStoreError::PortInUse { offending_port } => assert_eq!(offending_port, 8443),
            other => panic!("expected PortInUse, got {other:?}"),
        }
    }

    // --- hydrate replay keeps next_id monotonic ---

    #[tokio::test]
    async fn hydrate_advances_next_id_past_replayed_rules() {
        let store = ServerRuleStore::new();
        let mut rule = fixture_rule();
        rule.id = RuleId(42);
        rule.listen_port_end = None;
        rule.target_port_end = None;
        rule.state = RuleState::Active;
        store.hydrate(vec![rule]).await;
        // A subsequent push must take an id strictly greater than 42.
        let next = store
            .push(
                cid("edge-99"),
                name("edge-99"),
                19000,
                "10.0.0.5".into(),
                9000,
                Protocol::Tcp,
                None,
            )
            .await
            .unwrap();
        assert!(next.id.0 > 42, "next_id must exceed the hydrated max");
        // The hydrated rule is still retrievable.
        assert!(store.get(RuleId(42)).await.is_some());
    }
}
