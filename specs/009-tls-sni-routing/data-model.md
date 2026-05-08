# Phase 1 — Data Model: TLS SNI Routing

**Feature**: 009-tls-sni-routing
**Phase**: 1 (Design & Contracts)
**Authoritative reference**: [`design.md`](./design.md), [`research.md`](./research.md)

This document captures every entity, schema delta, and in-memory data
structure introduced by the feature, plus the validation rules and state
transitions that bind them.

---

## 1. Persistent entities (server-side)

### 1.1 `Rule` (extended)

The existing v0.8 `Rule` resource gains exactly one optional attribute.

| Field | Type | Notes |
|---|---|---|
| `id` | `INTEGER PK AUTOINCREMENT` | unchanged |
| `listen_port` | `INTEGER NOT NULL` | unchanged |
| `listen_port_end` | `INTEGER NULL` | unchanged; `NULL` ⇒ single-port |
| `target_host` | `TEXT NOT NULL` | unchanged |
| `target_port` | `INTEGER NOT NULL` | unchanged |
| `target_port_end` | `INTEGER NULL` | unchanged |
| `protocol` | `TEXT CHECK IN ('tcp','udp')` | unchanged |
| `owner_user_id` | `TEXT FK → users(user_id)` | unchanged |
| `health_check_interval_secs` | `INTEGER NULL` | unchanged |
| `created_at` / `updated_at` | `TEXT NOT NULL` | unchanged |
| **`sni_pattern`** | `TEXT NULL` | **NEW** (V002 migration) |

#### Validation rules (server-side, enforced before persist)

V-1. `sni_pattern` MAY be `NULL` (always valid; semantics depend on group).

V-2. `sni_pattern` MUST be `NULL` when:
- `protocol = 'udp'` — no TLS in UDP rules in v0.9 (FR-002).
- `listen_port_end IS NOT NULL` — port-range rules are scope-rejected (FR-002).

V-3. When `sni_pattern IS NOT NULL`, the value MUST satisfy the grammar:
- ASCII only, length ≤ 253 chars after lowercasing
- One of:
  - **Exact host**: at least one label, no leading `*`, hostname-label
    grammar (RFC 1035 letters + digits + `-`, with `-` not at edges,
    label length 1..=63, label count ≥ 1)
  - **Single-level wildcard**: starts with `*.`, no other `*`, suffix
    after `*.` is itself a valid host with **at least two labels**
    (so `*.example.com` is valid; `*.com` is not)

V-4. Storage canonicalisation: `sni_pattern` is **lowercased on write**;
input is rejected at validation if it contains characters outside
`a-z 0-9 . - * A-Z` (uppercase tolerated only because we lowercase).

V-5. **Per-`(client_name, listen_port)` uniqueness** within a route group:
- Two rules pointing at the same `(client, port)` MUST NOT have the same
  `sni_pattern` (NULL vs. NULL is also a duplicate — see overlap matrix below).

V-6. **Mode lock**: when an existing rule is on `(client, port)`, a new push
MUST satisfy the §Overlap matrix (no online mode flip).

V-7. **Capability gate**: a push with `sni_pattern IS NOT NULL` to a
`client_name` whose last `Hello.client_version` is below `0.9.0` MUST be
refused with `422 sni_unsupported_by_client { client_name, client_version }`
**before** the rule is persisted.

#### Overlap matrix (per `(client_name, listen_port)`, TCP single-port)

| Existing on the port | Candidate `sni_pattern` | Outcome |
|---|---|---|
| (empty) | any | **Accept** — defines the listener mode |
| Single rule, `NULL` | `NULL` | 409 `conflict.duplicate_rule` |
| Single rule, `NULL` | `Some(pat)` | 409 `conflict.legacy_to_sni_unsupported` |
| ≥1 rule, all `Some(_)` | `Some(pat)` not in group | **Accept** |
| ≥1 rule, all `Some(_)` | `Some(pat)` already in group | 409 `conflict.sni_route_duplicate` |
| ≥1 SNI rule + `NULL` fallback | `Some(pat)` not in group | **Accept** |
| ≥1 SNI rule + `NULL` fallback | `NULL` | 409 `conflict.sni_fallback_duplicate` |
| ≥1 SNI rule, no `NULL` | `NULL` | **Accept** — adds the fallback |
| Range rule overlapping `port`, same protocol | any | 409 `conflict.port_in_use` (existing v0.7 check) |

> `legacy_to_sni_unsupported` is the only "unusual" outcome: it forces the
> operator to remove the legacy plain-TCP rule first (draining its
> connections), then push SNI rules onto a freshly bound listener.

#### State transitions

`Rule` has no per-row state machine. The **route group**'s state is
derived from its members (see §3) and the listener's state is derived
from the group (see §4).

### 1.2 SQLite schema migration

`crates/forward-server/src/store/migrations/V002__add_sni_pattern.sql`:

```sql
ALTER TABLE rules ADD COLUMN sni_pattern TEXT;

-- Helper for "list rules with SNI on port P" admin queries; does not
-- affect the legacy non-SNI scan plan because it is partial.
CREATE INDEX rules_sni_lookup
    ON rules(listen_port, sni_pattern)
    WHERE sni_pattern IS NOT NULL;

-- NOTE (R-003): no UNIQUE constraint here. The `rules` table has no
-- client_name column; per-(client, listen_port) uniqueness is enforced
-- authoritatively by ServerRuleStore in memory. Adding client_name to
-- rules is out of scope for v0.9.
```

**Schema-version handshake**: supported range shifts `[1,1] → [1,2]`. A
v0.8 binary opening a `state.db` migrated to V002 reads the new column as
NULL (since v0.8 doesn't request it); a v0.9 binary opening a v0.8
`state.db` runs V002 once at boot.

### 1.3 No new tables

`SniListenerStats` is a **wire** message (see §5), not a persisted entity.
Per-listener counters are reconstructed from runtime state on each
`StatsReport` tick; they reset on client restart, matching the v0.7
convention for runtime counters.

---

## 2. In-memory routing entities (forward-client)

### 2.1 `ClientRule` (extended)

```rust
pub struct ClientRule {
    // ... existing fields
    pub sni_pattern: Option<String>,
}
```

Set on construction from the wire `Rule.sni_pattern`. Lowercased on the
wire (server canonicalisation per V-4); the client trusts the server's
canonicalisation and does not re-lowercase.

### 2.2 `SniRoutingTable`

```rust
pub struct SniRoutingTable {
    /// Exact hostname → rule_id. O(1) lookup.
    exact: HashMap<String, RuleId>,
    /// Wildcards stored as the suffix AFTER `*.`, sorted longest-first.
    /// Looked up by walking the slice (early-exit on the first match
    /// that satisfies the single-label-remainder rule).
    wildcards: Vec<(String, RuleId)>,
    /// At most one fallback rule (sni_pattern = NULL). None when the
    /// SNI listener has no fallback configured.
    fallback: Option<RuleId>,
}
```

#### Lookup algorithm

```text
fn lookup(&self, sni: Option<&str>) -> SniMatch {
    let host = sni.map(|s| s.to_ascii_lowercase());
    if let Some(host) = host.as_deref() {
        if let Some(&id) = self.exact.get(host) {
            return SniMatch::Hit { rule_id: id, kind: Exact };
        }
        for (suffix, id) in &self.wildcards {
            // host must end with `.<suffix>` and the prefix before it
            // must contain no `.` (single-label wildcard rule)
            if host.len() > suffix.len() + 1
                && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
                && host.ends_with(suffix.as_str())
            {
                let prefix = &host[..host.len() - suffix.len() - 1];
                if !prefix.contains('.') {
                    return SniMatch::Hit { rule_id: *id, kind: Wildcard };
                }
            }
        }
    }
    if let Some(id) = self.fallback {
        return SniMatch::Hit { rule_id: id, kind: Fallback };
    }
    SniMatch::Miss
}
```

#### Build / rebuild

Tables are immutable once built. Rebuilds happen in the control task
(NOT the accept loop) on every group mutation. The control task
constructs the new `Arc<SniRoutingTable>` and calls
`watch::Sender::send_replace`. In-flight connections keep their snapshot.

### 2.3 `SniListener` (data-plane)

```rust
pub struct SniListener {
    listener: TcpListener,
    table_rx: tokio::sync::watch::Receiver<Arc<SniRoutingTable>>,
    cancel: CancellationToken,
    stats: Arc<SniListenerCounters>,  // miss + parse-failure aggregates
}
```

Per-connection accept handler:
1. `accept()` → `(stream, peer_addr)`
2. `peek::read_client_hello(&mut stream, 3 s, 64 KiB)`
3. On error → bump `SniListenerCounters.parse_failures` or
   `.client_hello_timeout` (both fold into
   `SniListenerStats.client_hello_parse_failures_total` on the wire),
   emit `tracing` event, close connection.
4. Snapshot `table_rx.borrow().clone()` → `Arc<SniRoutingTable>`.
5. `lookup(sni)` →
   - `Hit { rule_id, kind }` → bump per-rule counter for `kind`,
     emit `tls.sni_routed`, dispatch into existing
     `proxy::proxy(stream, preread, rule)`.
   - `Miss` → bump `SniListenerCounters.miss`, emit
     `tls.sni_no_match`, close connection.

### 2.4 `PortGroupManager`

The single ownership root for SNI/legacy listeners on a forward-client.

```rust
pub struct PortGroupManager {
    /// (client_name, listen_port) is implicit (one client per process).
    /// Key by listen_port only.
    groups: HashMap<u16, GroupState>,
    /// Reverse index for RuleUpdate(REMOVE) which carries only rule_id.
    rule_to_port: HashMap<RuleId, u16>,
}

enum GroupState {
    Legacy {
        rule_id: RuleId,                 // exactly one
        forwarder_handle: ForwarderHandle,
    },
    Sni {
        members: HashMap<RuleId, Option<String>>,  // rule_id → sni_pattern
        table_tx: tokio::sync::watch::Sender<Arc<SniRoutingTable>>,
        listener_handle: SniListenerHandle,
        stats: Arc<SniListenerCounters>,
    },
}
```

#### Operations

`apply_push(rule: ClientRule) -> Result<(), PushRejection>`:
1. Look up `rule.listen_range.start()` in `groups`.
2. Decide outcome by §Overlap matrix (the server has already enforced
   it, but the client re-checks defensively per FR-015).
3. **Empty group + PUSH** → bind listener in the appropriate mode:
   - `sni_pattern = None` → `Legacy { rule_id, forwarder_handle }`
   - `sni_pattern = Some(_)` → build a single-member
     `SniRoutingTable`, spawn `SniListener`, store `Sni { … }`.
4. **Non-empty same-mode + PUSH** →
   - Legacy: this case is impossible (any second rule is either same
     mode = duplicate, or mode-flip = rejected); answer
     `RuleStatus { Failed, "mode_change_unsupported" }`.
   - SNI: insert into `members`, rebuild `SniRoutingTable`,
     `table_tx.send_replace(Arc::new(new_table))`.
5. Update `rule_to_port[rule_id] = listen_port`.

`apply_remove(rule_id: RuleId) -> Result<(), RemoveRejection>`:
1. Look up `rule_id` in `rule_to_port` → `listen_port` → group.
2. Drop the member.
3. If group becomes empty → cancel listener task, drain, drop the
   group entry; remove the reverse-index entry.
4. Else (only meaningful for `Sni`) → rebuild table + `send_replace`.

#### Invariants

- INV-1 (Mode-Lock): a `GroupState` does not change variant for its
  lifetime — it is dropped and re-created when the last member leaves.
- INV-2 (Reverse index consistency): for every `rule_id` known to a
  `GroupState`, `rule_to_port[rule_id]` exists and equals the group's
  listen_port. Tested in `sni_remove_by_rule_id.rs`.
- INV-3 (Accept-loop insulation): `SniRoutingTable` is reachable from
  the accept loop only via `watch::Receiver::borrow()`; rebuilds happen
  in the control task. There is no shared mutable state between accept
  and control.

---

## 3. Wire entities (forward-proto)

See [`contracts/wire.md`](./contracts/wire.md) for the full proto delta.
Summary:

| Message | Field | Tag | Notes |
|---|---|---|---|
| `Rule` | `sni_pattern` | 11 | `optional string`; lowercased ASCII; ≤253 chars |
| `RuleStats` | `sni_route_exact_total` | 13 | `uint64`; monotonic |
| `RuleStats` | `sni_route_wildcard_total` | 14 | `uint64`; monotonic |
| `RuleStats` | `sni_route_fallback_total` | 15 | `uint64`; monotonic |
| `SniListenerStats` (NEW) | `listen_port` | 1 | `uint32` |
| `SniListenerStats` (NEW) | `sni_route_miss_total` | 2 | `uint64`; monotonic |
| `SniListenerStats` (NEW) | `client_hello_parse_failures_total` | 3 | `uint64`; monotonic |
| `StatsReport` | `sni_listener_stats` | 3 | `repeated SniListenerStats` |

All additive; default-zero / empty for v0.8 traffic.

---

## 4. Lifecycle / state diagrams

### 4.1 `(client, listen_port)` group lifecycle

```text
[no group]
    │ first PUSH(rule.sni_pattern = None)
    ▼
[Legacy]
    │ all members removed
    ▼
[no group]
    │ first PUSH(rule.sni_pattern = Some)
    ▼
[Sni]
    │ PUSH(another sni_pattern, distinct) ┐
    │ PUSH(None) when no fallback        ├─→ [Sni] (rebuild table)
    │ REMOVE(member, group still ≥1)     ┘
    │
    │ all members removed
    ▼
[no group]
```

### 4.2 Per-connection state (SNI listener)

```text
accept ── peek ──[Truncated, read more]──┐
                                          ▼
                                       …loop until parse complete or budget exhausted
                                          │
                ┌─[Ok(Some(host))]────────┤
                ▼                         ├─[Ok(None)]──── lookup(None)
              lookup(Some)                │
                                          ├─[NotTls / Malformed / TimedOut / SizeCap]
                                          │     │
                                          │     ▼
                                          │   close + tracing event + bump listener counter
                                          ▼
                                       Hit { rule_id, kind }
                                          │
                                          ▼
                                       proxy::proxy(stream, preread, rule)  // v0.7 unchanged
                                          │
                                          ▼
                                       splice_bidirectional
```

---

## 5. Cross-cutting validation summary

| ID | Validation | Enforced at | Test |
|---|---|---|---|
| V-1 | `sni_pattern` may be NULL | n/a | implicit |
| V-2 | UDP / range + sni_pattern → reject | `OperatorService::validate_rule` | `sni_rule_validation.rs` |
| V-3 | Pattern grammar | `OperatorService::validate_rule` | `sni_rule_validation.rs` |
| V-4 | Lowercase canonicalisation | `OperatorService::validate_rule` | `sni_rule_validation.rs` |
| V-5 | (client, port, pattern) uniqueness | `ServerRuleStore::push` | `sni_overlap_matrix.rs` |
| V-6 | Mode-lock at push time | `ServerRuleStore::push` | `sni_overlap_matrix.rs`, `sni_legacy_to_sni_unsupported.rs` |
| V-7 | Client-version capability gate | `OperatorService::push_rule` | `sni_capability_gate.rs` |
| INV-1 | Group mode immutable | `PortGroupManager` | `sni_hot_reload.rs` |
| INV-2 | Reverse index consistent | `PortGroupManager` | `sni_remove_by_rule_id.rs` |
| INV-3 | Accept loop reads via watch only | `SniListener` | covered by hot-reload + perf bench |
