# Design: TLS SNI Routing

**Feature Branch (proposed)**: `009-tls-sni-routing`
**Created**: 2026-05-08 (revised twice after code review)
**Status**: Brainstorm output — pending user review, then `/speckit-specify` or direct `writing-plans`
**Predecessor**: v0.8.0 (`008-sqlite-storage`, merged 2026-05-08)

## Goal

Add Server Name Indication (SNI) based routing to forward-rs so a single TCP listen
port (typically 443) can fan out to different upstream targets based on the TLS
hostname the client sends in its `ClientHello`. forward-rs remains a pure L4
byte-passthrough — never decrypts, terminates, or re-encrypts TLS.

## Non-Goals

- TLS termination, reverse-proxy, HTTP-aware routing — backlog L7 work, out of scope.
- QUIC / HTTP/3 (UDP) SNI routing — different parser path, deferred.
- SNI on TCP port-range rules — see D5; relaxable later.
- Connection rate limiting / QoS, PROXY protocol — separate backlog items.
- A `/metrics` endpoint on forward-client — out of scope; SNI metrics piggy-back
  on the existing `RuleStats` → server-side Prometheus path (see §Observability).
- Adding `client_name` to the `rules` SQLite table — see §Data Model.
- Online conversion between legacy plain-TCP and SNI mode for an active port —
  see §"Mode-Locked Listener Lifetime" — operators remove first, then re-push.

## Where This Feature Lives

The data plane (listener bind, accept loop, proxy/splice) lives in
**forward-client** (`crates/forward-client/src/forwarder/`), driven by
`ClientRule`s pushed over the bidi gRPC stream. forward-server is control-plane
only. Therefore:

- forward-client gets the new SNI peek + routing modules and a per-port
  group-aware listener.
- forward-proto's `Rule` gains `optional string sni_pattern = 11;` and
  `RuleStats` gains five SNI counter fields.
- forward-server gets schema/validation/RBAC/CLI/Web-UI changes and a
  client-version capability gate (mirrors v0.7's multi-target precedent at
  `crates/forward-server/src/operator/http.rs:353`).
- The wire RuleUpdate remains single-rule; clients group by `listen_port`
  locally (see §Mode-Locked Listener Lifetime).

## Decisions

| # | Topic | Choice | Rationale |
|---|---|---|---|
| D1 | Match model | Exact host **+** single-level wildcard `*.example.com` **+** NULL fallback | Aligns with TLS SAN; minimal hot path. |
| D2 | Match priority | Exact → Wildcard (longest matching suffix wins; remainder must contain no `.`) → Fallback (NULL) → Miss | "More specific wins"; explicit single-label rule prevents `a.b.example.com` from matching `*.example.com`. |
| D3 | ClientHello parser | Hand-rolled, ~150 LOC, **zero new deps** | Small audit surface; only `server_name` extension consumed. |
| D4 | Read budget | 3 s timeout **and** 64 KiB cap per ClientHello peek | Handshake messages can span multiple TLS records; 64 KiB covers PQ-hybrid groups while bounding slowloris. |
| D5 | Rule scope | TCP single-port only | UDP rejected (no TLS); range rejected by validation. May relax later. |
| D6 | Failure semantics | Timeout → reject; non-TLS → reject; no SNI → fallback else reject; no match → fallback else reject. | Reject by default; fallback opt-in via explicit NULL rule. |
| D7 | Hot reload primitive | `tokio::sync::watch::Sender<Arc<SniRoutingTable>>` per listener | tokio is already a workspace dep; no new crate. Reads cheap (Arc clone), writes are full-table swap. |
| D8 | Wire compat | `optional string sni_pattern = 11;` (proto3 optional); RuleUpdate stays single-rule | Old clients drop the field on decode → silently treated as plain TCP. Mitigated by D9 gate + Mode-Locked Lifetime. |
| D9 | Client capability gate | Refuse `push-rule` with `sni_pattern.is_some()` to a client whose last `Hello.client_version < 0.9.0` → HTTP 422 `sni_unsupported_by_client` | Mirrors R-007 (v0.7 multi-target gate). Without it an old client would activate the rule as plain TCP. |
| D10 | SQL uniqueness | **No SQL UNIQUE** on `(listen_port, sni_pattern)`; uniqueness enforced by `ServerRuleStore` per `(client_name, listen_port)` | `rules` has no `client_name` column; adding one for v0.9 is out-of-scope. App-layer index is already client-scoped. |
| D11a | RuleStats wire (hits) | Add three `uint64` counter fields to `RuleStats` at field numbers **13, 14, 15** (`sni_route_exact_total`, `sni_route_wildcard_total`, `sni_route_fallback_total`) | Fields 11 (`target_failovers_total`) and 12 (`per_target`) are taken by v0.7. Hits are attributable to the matched `rule_id`, so they belong on `RuleStats`. |
| D11b | Listener-level counters | Add a new `SniListenerStats { listen_port, sni_route_miss_total, client_hello_parse_failures_total }` message; carry it in `StatsReport.sni_listener_stats = 3` | Miss / parse-failure happen *before* a rule is selected and have no `rule_id`. Forcing them onto `RuleStats` would require a fake rule attribution and confuse operators. |
| D12 | Peek-duration histogram | **Deferred to v0.10+** | Histogram doesn't fit the monotonic-counter shape of `RuleStats` / `SniListenerStats`; needs a separate channel. Counters give enough operational signal for v0.9. |
| D13 | Data-plane events surface | client `tracing` events + Prometheus counters **only**; **NOT** the SQLite `audit` ring | `crates/forward-server/src/operator/audit.rs:16-31` documents that high-frequency client-side events stay in the structured tracing log; SQLite `audit` is reserved for operator allow/deny actions. Same precedent as v0.7's `rule.target.health_changed`. |

## Mode-Locked Listener Lifetime (replaces "atomic group rebroadcast")

A `(client_name, listen_port)` listener is in exactly one mode for its
lifetime, fixed by the *first* rule that activates it:

| First rule's `sni_pattern` | Listener mode | Hot-path |
|---|---|---|
| `None` | **Legacy plain TCP** | No peek, no parse — byte-identical to v0.7 |
| `Some(pat)` | **SNI dispatch** | Peek + parse + `SniRoutingTable::lookup` |

Subsequent pushes are validated against the existing mode — see overlap table
below. The listener stays in its initial mode until the **last** rule on that
`(client, port)` is removed; the next first-push reopens the mode question.

This means we never have to flip an active listener between modes. No proto
change is needed: each `RuleUpdate` is still one rule, and the client just
adds/removes entries in the existing `SniRoutingTable` for that port. If the
group transitions from size 0 to 1 the listener is bound; from N to 0 it is
torn down.

### Overlap & uniqueness rules (replaces the old contradictory text)

When `ServerRuleStore::push` evaluates a TCP single-port candidate against
existing rules on `(client_name, candidate.listen_port)`:

| Existing on `(client, port)` | Candidate `sni_pattern` | Outcome |
|---|---|---|
| Empty | any | Accept (defines mode) |
| Single rule, `None` | `None` | 409 `conflict.duplicate_rule` (legacy slot taken) |
| Single rule, `None` | `Some(pat)` | 409 `conflict.legacy_to_sni_unsupported` — operator must remove first |
| ≥1 rule, all `Some(_)` | `Some(pat)` not in group | Accept (new SNI sibling) |
| ≥1 rule, all `Some(_)` | `Some(pat)` already in group | 409 `conflict.sni_route_duplicate` |
| ≥1 SNI rule + one `None` | `Some(pat)` not in group | Accept |
| ≥1 SNI rule + one `None` | `None` | 409 `conflict.sni_fallback_duplicate` |
| ≥1 SNI rule, no `None` | `None` | Accept (adds fallback) |
| Range rule on overlapping ports, same protocol | any | 409 `conflict.port_in_use` (existing v0.7 check) |

`legacy_to_sni_unsupported` is the only "unusual" error: it forces the
operator to remove the legacy plain-TCP rule first (with whatever connection
disruption that entails), then push SNI rules onto a freshly bound listener.

## User-Visible Behaviour

### Routing path (data plane, forward-client)

```
client TCP SYN
  ↓
accept on port P (one listener per (client, port) group)
  ↓
listener mode (set at first activation, immutable for lifetime)
  ├─ legacy plain → existing v0.7 path (byte-identical, no peek, no audit)
  └─ SNI dispatch → peek ClientHello (≤ 3 s, ≤ 64 KiB)
                    ├─ timeout / not TLS / malformed → reject + audit
                    └─ parse SNI (may be None)
                          ↓
                          SniRoutingTable::lookup(sni)
                            ├─ Exact hit
                            ├─ Wildcard hit (longest matching suffix; remainder no `.`)
                            ├─ Fallback (NULL slot) — used when sni is None or no match
                            └─ Miss → reject + audit
                          ↓
                          selected member rule_id → v0.7 target select + failover (unchanged)
                          ↓
                          connect upstream → write the buffered ClientHello bytes verbatim
                          ↓
                          bidirectional splice (unchanged)
```

### Control plane

- `Rule.sni_pattern: Option<String>` on the rule resource.
- Proto: `optional string sni_pattern = 11;` (next free slot after v1.4 field 10).
  Wire-compat test under `crates/forward-proto/tests/`.
- `POST /v1/rules` accepts the field; validation rejects:
  - non-TCP rule with `sni_pattern` set → 400 `validation.sni_on_unsupported_rule`
  - port-range rule (`listen_port_end IS NOT NULL`) with `sni_pattern` set →
    400 `validation.sni_on_unsupported_rule`
  - malformed pattern → 400 `validation.sni_pattern_malformed`. Grammar:
    - `exact.host.tld` — RFC 1035 hostname labels, ASCII only, ≤ 253 chars
    - or `*.suffix.tld` — `*` MUST be the first label and immediately
      followed by `.`; no `*` elsewhere; remainder follows hostname rules
    - IDN inputs MUST be Punycode (`xn--...`) by the caller; storage is
      lowercased
  - any conflict from the §Overlap table → 409 with the specific code.
- D9 capability gate: `sni_pattern.is_some()` to a client whose last
  `Hello.client_version < 0.9.0` → 422 `sni_unsupported_by_client`.
- CLI: `forward-server push-rule --sni <pattern>` (optional). Same validation.
- `list-rules --json` and `GET /v1/rules` include `sni_pattern` when set.
- Web UI rules page: new `SNI` column; new/edit form shows an optional
  `SNI Pattern` input only when Protocol = TCP and Port mode = Single, with
  helper text covering exact / wildcard / blank-fallback semantics.

### Failure handling — client tracing events (NOT audit ring)

All five events below are emitted as forward-client structured tracing
events with `target = "tls_sni"`. They do **not** flow into the SQLite
`audit` ring (D13 / R-AUDIT precedent at `audit.rs:16-31`). Operators
correlate via the structured log and the Prometheus counters below.

| Tracing event | Level | When |
|---|---|---|
| `tls.client_hello_timeout` | WARN | 3 s elapsed before a parseable ClientHello arrived |
| `tls.parse_failed` | WARN | Bytes are not TLS or ClientHello structure is malformed |
| `tls.no_sni` | INFO | ClientHello valid but no `server_name` extension; carries `fallback_used: bool` |
| `tls.sni_no_match` | WARN | Has SNI, no rule matches, no fallback present |
| `tls.sni_routed` | INFO | Successful routing on a SNI listener |

Legacy plain-TCP listeners emit no per-connection tracing events on the
`tls_sni` target (preserving v0.7's "control-plane changes + exceptions only"
audit budget).

### "None" Semantics — disambiguated

`Rule.sni_pattern = None` means different things depending on the listener
mode at the time the rule was pushed:

| Mode at push time | `None` rule means |
|---|---|
| Empty group → mode becomes legacy | **Legacy plain TCP forward** (same as v0.7); no peek, no parse |
| Existing SNI group | **TLS-only fallback** for valid TLS with no/unknown SNI; non-TLS bytes still rejected |

Mode-Locked Lifetime guarantees these two interpretations never coexist on a
live listener.

## Architecture

### forward-proto

`proto/forward.proto` Rule:
```proto
// Additive in v1.5 (spec 009-tls-sni-routing). Wire field number 11.
// Absent → plain TCP forward / TLS-only fallback (depending on listener mode).
// Present → host or `*.suffix` pattern; ASCII, lowercased, ≤ 253 chars.
optional string sni_pattern = 11;
```

`proto/forward.proto` RuleStats (D11a — hits only):
```proto
// Additive in v1.5 (spec 009-tls-sni-routing). Field numbers continue
// after v0.7's `target_failovers_total = 11` and `per_target = 12`.
// All three are monotonic; default-zero on the wire.
uint64 sni_route_exact_total    = 13;
uint64 sni_route_wildcard_total = 14;
uint64 sni_route_fallback_total = 15;
```

`proto/forward.proto` new SniListenerStats (D11b — listener-level events
that have no rule attribution):
```proto
// New in v1.5 (spec 009-tls-sni-routing). One per active SNI listener
// on the client. Carried alongside RuleStats in StatsReport.
message SniListenerStats {
  uint32 listen_port                       = 1;
  uint64 sni_route_miss_total              = 2;
  uint64 client_hello_parse_failures_total = 3;
}
```

`proto/forward.proto` StatsReport gains field 3:
```proto
message StatsReport {
  uint64 sent_at_unix_ms = 1;
  repeated RuleStats stats = 2;
  // Additive in v1.5; empty for clients with no SNI listener.
  repeated SniListenerStats sni_listener_stats = 3;
}
```

Wire-compat tests (`crates/forward-proto/tests/sni_wire_compat.rs`):
- v0.8 binary encoding round-trips through v0.9 deserialiser → all new
  fields absent / zero.
- v0.9 encoding with sni fields zero / `sni_listener_stats` empty is
  byte-identical to v0.8 for the same logical content.
- Round-trip: `Rule.sni_pattern = 11`; `RuleStats.sni_route_*_total =
  13/14/15`; `StatsReport.sni_listener_stats = 3` carrying `SniListenerStats`
  with fields 1/2/3.
- **Negative**: explicitly assert that nothing in this spec touches
  `RuleStats` field 11 (`target_failovers_total`) or 12 (`per_target`).

### forward-client (data plane)

```
crates/forward-client/src/forwarder/
├── mod.rs                  # ClientRule grows pub sni_pattern: Option<String>
├── sni/
│   ├── mod.rs              # pub use
│   ├── client_hello.rs     # parse(&[u8]) -> Result<ParseOutcome, ParseError>
│   ├── route_table.rs      # SniRoutingTable + lookup
│   ├── peek.rs             # async ClientHello peek (3 s / 64 KiB)
│   └── listener.rs         # SniListener (mode = SNI) — owns watch::Sender
├── port_groups.rs          # NEW: PortGroupManager — owns listener tasks per (client, port)
├── proxy.rs                # unchanged hot path; preread bytes plumbed in
└── stats.rs                # adds five SNI counters to per-rule stats
```

`PortGroupManager` is the single place that materialises rules into running
listeners. The control loop sends rule deltas to it. State per
`(client, listen_port)` group:
- `mode: Legacy | Sni`
- For legacy: the existing v0.7 forwarder task handle
- For SNI: a `tokio::sync::watch::Sender<Arc<SniRoutingTable>>`, the listener
  task handle, and the cumulative `SniListenerStats` (D11b) — incremented
  by the listener's accept handler and drained on the StatsReport tick.

The manager also keeps a **reverse index** `HashMap<RuleId, ListenPort>`
because `RuleUpdate(REMOVE)` carries only `rule_id` (per
`proto/forward.proto:179`). Without it, REMOVE can't find which group owns
the rule. Index is updated on every PUSH/REMOVE.

On each `RuleUpdate`:
1. **PUSH**: index `(client, candidate.listen_port)` → group.
   - Empty group → bind listener in the mode dictated by `sni_pattern`.
   - Non-empty same-mode group → add to membership; for SNI mode, build a
     fresh `SniRoutingTable` from current members and
     `watch::Sender::send_replace`. The accept loop's per-connection task
     borrows `Arc<SniRoutingTable>` once at accept time; in-flight
     connections are unaffected.
   - Mode-change attempt: refused at the server (D9 + overlap table) so the
     client should never see one; if it does, emit
     `event = "control.mode_change_attempt_rejected"` and answer with
     `RuleStatus { outcome = Failed, reason = "mode_change_unsupported" }`.
2. **REMOVE**: look up `rule_id` in the reverse index → group → drop member.
   - Group empty → cancel listener task, drain, drop the watch + reverse
     index entry.
   - Group non-empty SNI → rebuild table + `send_replace`.

`ClientRule` gains:
```rust
pub struct ClientRule {
    // ... existing fields
    pub sni_pattern: Option<String>,
}
```

### forward-server (control plane only)

- `crates/forward-server/src/rules.rs` — overlap check rewritten per the
  §Overlap table; `by_client_listen_start` becomes
  `BTreeMap<u16, Vec<RuleId>>` (no new dep). All callers updated.
- `crates/forward-server/src/store/migrations/V002__add_sni_pattern.sql` —
  schema migration (below).
- `crates/forward-server/src/operator/http.rs` — D9 capability gate; helper
  `version_at_least_0_9` next to `version_at_least_0_7`.
- `crates/forward-server/src/main.rs` — `push-rule --sni <pattern>` flag.
- `crates/forward-server/src/grpc/service.rs` — extend the existing
  `StatsReport` fold (around `:317`) to:
  - read the three new `RuleStats` SNI counters and update per-rule
    collectors with `client, rule, owner, result` labels;
  - read the new `StatsReport.sni_listener_stats` repeated field and update
    listener-level collectors with `client, port` labels.
- `crates/forward-server/src/metrics.rs` — register the new collectors,
  reusing the existing `client, rule, owner` label triple for hits and a
  new `client, port` pair for listener-level counters (no `rule_id` /
  `owner_user_id` labels — kept consistent with v0.7 conventions at
  `metrics.rs:156`).

### Component contracts (new code)

**`sni::client_hello::parse(bytes: &[u8]) -> Result<ParseOutcome, ParseError>`**
- Pure, no I/O. Tracks handshake-fragment reassembly across multiple TLS
  records (RFC 8446 §5.1).
- Outcomes: `Truncated`, `Ok(Some(host))`, `Ok(None)` (no SNI extension);
  errors `NotTls`, `Malformed`.

**`sni::route_table::SniRoutingTable`**
- `from_members(rules: &[&ClientRule]) -> Self` for one group.
- `lookup(sni: Option<&str>) -> SniMatch { Hit { rule_id, kind }, Miss }`.
- Wildcard match: store wildcard suffixes (the part after `*.`), sorted by
  suffix length descending. A `host` matches `*.suffix` iff
  `host.ends_with(".suffix")` AND
  `host[..host.len() - suffix.len() - 1]` contains no `.`.
- Comparison: lowercase `host` once on entry; storage is already lowercase.

**`sni::peek::read_client_hello`** — `tokio::time::timeout(3 s)` over a loop
that reads, accumulates into a `Vec<u8>` capped at 64 KiB, calls
`client_hello::parse`, exits on `Ok` or budget exhaustion.

**`sni::listener::SniListener`** — owns the bound `TcpListener`, the
`watch::Sender<Arc<SniRoutingTable>>`, and the cancellation token. On accept,
clones the watch receiver and reads `borrow()` once to pick a `rule_id`,
then dispatches into the existing `proxy::proxy`.

## Data Model & Schema

`Rule` (Rust, `crates/forward-server/src/rules.rs`):
```rust
pub struct Rule {
    // ... existing fields incl. listen_port, listen_port_end, protocol
    pub sni_pattern: Option<String>,
}
```

SQLite migration `V002__add_sni_pattern.sql` (additive column only):

```sql
ALTER TABLE rules ADD COLUMN sni_pattern TEXT;

-- Helper index for the operator API "list rules with SNI on port P" query.
CREATE INDEX rules_sni_lookup
    ON rules(listen_port, sni_pattern)
    WHERE sni_pattern IS NOT NULL;

-- NOTE: We deliberately do NOT add a UNIQUE constraint on
-- (listen_port, IFNULL(sni_pattern, '')). The `rules` table has no
-- client_name column today, and adding one is out-of-scope for v0.9.
-- Per-(client, listen_port) uniqueness is enforced authoritatively by
-- ServerRuleStore (in-memory, by_client_listen_start). The rules.json
-- → SQLite cutover never had a SQL UNIQUE either.
```

Schema-version handshake: supported range shifts from `[1,1]` to `[1,2]`. No
data migration (additive column).

## Observability

### Prometheus metrics (server-side, surfaced via existing `/metrics`)

Labels follow the v0.5+ convention used everywhere else in
`crates/forward-server/src/metrics.rs:156` — `client`, `rule`, `owner` for
per-rule, plus `result` where applicable. We deliberately do **not** invent
`rule_id` or `owner_user_id` labels.

| Metric | Type | Labels | Source |
|---|---|---|---|
| `forward_tls_sni_route_total` | counter | `client`, `rule`, `owner`, `result=exact|wildcard|fallback` | three `RuleStats.sni_route_*_total` fields, one fold per row |
| `forward_tls_sni_listener_miss_total` | counter | `client`, `port` | `SniListenerStats.sni_route_miss_total` |
| `forward_tls_client_hello_parse_failures_total` | counter | `client`, `port` | `SniListenerStats.client_hello_parse_failures_total` |
| `forward_tls_sni_routes_active` | gauge | — | `ServerRuleStore` count of rules with `sni_pattern.is_some()` (server-side, no client plumbing) |

Why the listener-level metrics use `port` (not `rule`): miss / parse-failure
happen before a rule is selected, so there is no honest `rule` label to
attach. `client` keeps cardinality bounded — one client × one listener port
per series.

`forward_tls_client_hello_peek_duration_seconds` (histogram, D12) is
**deferred** to v0.10+.

### Logs

forward-client emits all five `tls_sni` tracing events listed in §Failure
handling. forward-server does **not** mirror them into the SQLite `audit`
ring — that ring is reserved for operator allow/deny actions per
`audit.rs:16-31`. Operators correlate via the structured tracing log plus
the Prometheus counters above.

## Testing Strategy

Per Constitution Principle III, every contract surface and SC ships with
tests authored before implementation.

### Unit tests (in-source `#[cfg(test)] mod tests`)

- `sni/client_hello.rs`: real packet captures (TLS 1.0/1.1/1.2/1.3) under
  `crates/forward-client/tests/fixtures/tls/*.bin`; truncation at varied
  offsets; missing/empty SNI extension; oversize host; multiple `server_name`
  entries (first wins); incremental feed (Truncated → Ok); handshake message
  fragmented across two records.
- `sni/route_table.rs`: priority (Exact > Wildcard > Fallback); wildcard
  specificity (`*.foo.example.com` beats `*.example.com`); explicit single-
  label guard (`a.b.example.com` does NOT match `*.example.com`); miss
  without fallback; case insensitivity; rebuild from new member set.
- `port_groups.rs`: legacy → empty → SNI re-bind sequence; SNI add/remove
  member doesn't tear down listener; mode-change PUSH is rejected with
  `mode_change_unsupported`.

### Contract / integration tests

forward-server (`crates/forward-server/tests/`):
| File | Covers |
|---|---|
| `sni_rule_validation.rs` | UDP / range + sni → 400; malformed pattern → 400 |
| `sni_capability_gate.rs` | push-rule with sni to a v0.8 client → 422 (D9) |
| `sni_overlap_matrix.rs` | Every row of the §Overlap table |
| `sni_legacy_to_sni_unsupported.rs` | Active legacy rule + SNI candidate → 409 with the documented code |

forward-client (`crates/forward-client/tests/`):
| File | Covers |
|---|---|
| `sni_route_e2e_exact.rs` | Two SNI rules on `:443`, rustls clients land on the correct upstream |
| `sni_route_e2e_wildcard.rs` | `*.example.com` matches `foo.example.com`, not `example.com`, not `a.b.example.com` |
| `sni_route_fallback.rs` | No-SNI client → NULL fallback; without fallback → reset + `tls.no_sni` audit |
| `sni_route_timeout.rs` | TCP connect, no bytes for 3 s → reset + `tls.client_hello_timeout` |
| `sni_route_not_tls.rs` | Plain HTTP on the port → reset + `tls.parse_failed` |
| `sni_byte_passthrough.rs` | sha256 of upstream-received bytes equals client-sent bytes |
| `sni_hot_reload.rs` | In-flight connection unaffected when group members change |
| `sni_stats_emitted.rs` | After mixed traffic, `RuleStats` carries the expected counter values for fields 13/14/15 and `StatsReport.sni_listener_stats` carries non-zero miss / parse-failure counts |
| `sni_remove_by_rule_id.rs` | RuleUpdate(REMOVE) with only rule_id removes the right group member; reverse index stays consistent |
| `legacy_plain_tcp_unchanged.rs` | A non-SNI port on the same client is byte-identical to v0.7 (no peek path entered) |

forward-server end-to-end (`crates/forward-server/tests/`):
| File | Covers |
|---|---|
| `sni_metrics_surface.rs` | After a forward-client emits SNI counters, server `/metrics` exposes `forward_tls_sni_route_total{client,rule,owner,result}` and `forward_tls_sni_listener_miss_total{client,port}` |

forward-proto (`crates/forward-proto/tests/`):
| File | Covers |
|---|---|
| `sni_wire_compat.rs` | Field 11 on Rule, fields 11–15 on RuleStats; absent-field bytes identical to v0.8 |

### Benches (`crates/forward-client/benches/sni_route.rs`)

- `SniRoutingTable::lookup` ns/op at 100 / 1 000 / 10 000 routes (hit + miss).
- End-to-end TCP connect + handshake setup latency vs. v0.7 baseline; SNI
  ports allowed +5 ms (parse < 100 µs, balance is network); legacy plain
  ports must not enter the SNI code path (assert via tracing test target).

## Constitution Check (preview)

- **I. Auth invariants** — TLS + bearer token unchanged.
- **II. Single binary** — pure-Rust parser, **zero new deps**. `tokio::sync::watch`
  replaces ArcSwap; `Vec<RuleId>` replaces SmallVec; `parking_lot` not used.
- **III. Test-first** — every FR / SC has a failing test before implementation.
- **IV. Observability** — three per-rule SNI hit counters via `RuleStats`
  (fields 13/14/15) and a separate `SniListenerStats` for listener-level
  miss / parse-failure counters; both surface through the server's existing
  `/metrics` with `client/rule/owner/result` and `client/port` labels
  respectively. Data-plane events are tracing-only — they don't pollute the
  SQLite operator audit ring (D13). Histogram deferred.
- **V. byte-stable control plane** — proto fields 11 (Rule) and 11–15
  (RuleStats) are `optional` / default-zero; D9 prevents silent SNI
  activation on v0.8 clients; schema migration is purely additive.

## Risks & Mitigations

| Risk | Mitigation |
|---|---|
| Hand-rolled parser misses a corner case | Real-packet fixtures + fuzz harness in benches dir |
| TLS extension order drift breaks parsing | We only read `server_name` and skip everything else by length |
| `watch` rebuild stalls accept loop | Build the new table in the control task; `send_replace` is `O(1)` |
| Old client silently activates SNI rule as plain TCP | D9 capability gate (HTTP 422 before push) |
| Operator wants to convert legacy → SNI without disruption | Documented limitation; `legacy_to_sni_unsupported` error names the workflow |
| Validation false-positives reject legal hosts | Pattern grammar documented; reject reason names a single rule |
| `*.example.com` accidentally matching `a.b.example.com` | Explicit single-label remainder check + test |
| ClientHello legitimately spans records and exceeds early cap | Parser tracks handshake-fragment reassembly; cap is 64 KiB |
| Two clients want `:443 + api.example.com` simultaneously | Allowed: app-layer uniqueness is per-`(client_name, listen_port)`; SQL has no UNIQUE constraint |

## Open Questions

1. Should the `tls.sni_routed` tracing event include the matched
   `server_name` in clear text? (Privacy vs. observability.) Recommendation:
   yes — the value is already on the wire in clear, and operators need it
   for routing diagnostics.
2. Should v0.9 ship a `forward-client-bundle.json` field for SNI defaults
   (e.g. operator-side timeout override per client)? Recommendation: no —
   keep tunables server-side; revisit if real deployments ask.
3. Histogram deferral (D12) — confirm during `/speckit-specify` whether
   peek-duration is required for any SC; if so we add a separate stats
   channel.
4. Listener-level metric labels — `client, port` (chosen) keeps cardinality
   bounded but loses owner attribution for listener-level events. Acceptable
   because operator allow/deny on the *rule* is already audited;
   listener-level events are diagnostic. Confirm during speckit-specify.

Anything else surprising during plan-writing or implementation can be parked
here.
