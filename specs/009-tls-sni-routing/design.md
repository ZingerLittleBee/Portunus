# Design: TLS SNI Routing

**Feature Branch (proposed)**: `009-tls-sni-routing`
**Created**: 2026-05-08 (revised after code review)
**Status**: Brainstorm output — pending user review, then `/speckit-specify` or direct `writing-plans`
**Predecessor**: v0.8.0 (`008-sqlite-storage`, merged 2026-05-08)

## Goal

Add Server Name Indication (SNI) based routing to forward-rs so a single TCP listen
port (typically 443) can fan out to different upstream targets based on the TLS
hostname the client sends in its `ClientHello`. forward-rs remains a pure L4
byte-passthrough — it never decrypts, never terminates TLS, never re-encrypts.

## Non-Goals

- TLS termination, reverse-proxy, HTTP-aware routing — tracked in the post-v0.8
  L7 backlog, out of scope for v0.9.
- QUIC / HTTP/3 (UDP) SNI routing — different parser path, deferred.
- SNI on TCP port-range rules — see D5 in §Decisions; relaxable later.
- Connection rate limiting / QoS, PROXY protocol — separate backlog items.

## Where This Feature Lives (corrected after code review)

The data plane (listener bind, accept loop, proxy / splice) lives in
**forward-client** (`crates/forward-client/src/forwarder/`), driven by
`ClientRule`s pushed over the bidi gRPC stream. The server only stores rules,
exposes them via the operator API, and pushes them to clients. Therefore:

- forward-client gets the new SNI peek + routing modules.
- forward-proto's `Rule` message gets `optional string sni_pattern = 11;`.
- forward-server gets schema/validation/RBAC/CLI/Web-UI changes and a
  client-version capability gate (mirrors the v0.7 multi-target precedent at
  `crates/forward-server/src/operator/http.rs:353`).

## Decisions

| # | Topic | Choice | Rationale |
|---|---|---|---|
| D1 | Match model | Exact host **+** single-level wildcard `*.example.com` **+** NULL fallback | Aligns with TLS SAN semantics; minimal hot-path overhead. |
| D2 | Match priority | Exact → Wildcard (longest matching suffix wins; remainder must be a single label, i.e. contains no `.`) → Fallback (NULL) → Miss | Standard "more specific wins"; explicit single-label rule prevents `a.b.example.com` from accidentally matching `*.example.com`. |
| D3 | ClientHello parser | Hand-rolled, ~150 LOC, **zero new deps** | Small audit surface; only `server_name` extension consumed. |
| D4 | Read budget | 3 s timeout **and** 64 KiB cap per ClientHello peek | ClientHello can span multiple TLS records (each record ≤ 16 384 bytes); 64 KiB gives generous headroom for PQ-hybrid groups while still bounding slowloris. |
| D5 | Rule scope | TCP single-port only | UDP rejected (no TLS); port range rejected by validation. May relax later. |
| D6 | Failure semantics | Timeout → reject; non-TLS → reject; no SNI → fallback else reject; no match → fallback else reject. | Reject by default; fallback is opt-in. |
| D7 | Hot reload | `ArcSwap<SniRoutingTable>` per listener; full table rebuild on every rule change in the listener's group | Routes ≪ connections; simpler than incremental update. |
| D8 | API compat | New `sni_pattern: Option<String>` field; serde `skip_serializing_if = Option::is_none`; proto field `optional string sni_pattern = 11;` | byte-stable for v0.8 clients that never send the field. |
| D9 | Client capability gate | Refuse `push-rule` with `sni_pattern.is_some()` to a client whose last `Hello.client_version < 0.9.0`; HTTP 422 `sni_unsupported_by_client` | Without the gate, an old client would drop the unknown proto field and activate a plain TCP forward, silently breaking `api.example.com:443 → all-TLS-into-first-target`. Mirrors R-007 (v0.7 multi-target gate). |

## Rule Lifecycle Change: SNI Route Groups

The current `ServerRuleStore` (`crates/forward-server/src/rules.rs:394`) rejects
two TCP rules sharing a listen port — same-protocol overlap raises `PortInUse`.
SNI routing **requires** multiple TCP rules on the same `(client, listen_port)`
when each carries a distinct `sni_pattern`.

We introduce a **route-group** semantics layered on top of the existing rule
table — without inventing a new top-level resource:

- A *route group* is the set of rules sharing
  `(client_name, protocol = tcp, listen_port, listen_port_end IS NULL)`.
- Within one group, rules MUST have pairwise-distinct `sni_pattern`
  (`NULL` is one slot; each non-null pattern is one slot).
- Across groups (different ports / clients), rules are independent.
- A group of size 1 with `sni_pattern = NULL` is a **legacy plain TCP rule** —
  byte-identical to v0.7 behaviour (no peek, no parse, no SNI dispatch).
- A group of size ≥ 2 (or size 1 with non-null pattern) is a **SNI listener** —
  one accept loop, all members share the listener via the `SniRoutingTable`.

### Rule store changes (forward-server)

`ServerRuleStore::push` overlap check (`rules.rs:394-411`) becomes:

```text
// existing rules on same client, overlapping listen range, same protocol
//   ─ if both ranges are single-port AND both have sni_pattern set with
//     distinct values  → ALLOWED (same SNI group, distinct routes)
//   ─ otherwise (range, missing sni, or duplicate sni)               → PortInUse / SniRouteDuplicate
```

`by_client_listen_start: BTreeMap<u16, RuleId>` cannot hold multiple values
per port; replace with `BTreeMap<u16, SmallVec<[RuleId; 4]>>` (or
`HashMap<(ClientName, u16), Vec<RuleId>>`). Lookups are still O(log n) on
port; group enumeration becomes O(group size).

The control loop notifies the client of the entire group on every group
mutation: a single push or remove rebroadcasts the resulting member set so
the client can rebuild its `SniRoutingTable` for that listener atomically.

### forward-client lifecycle change

Today `ClientRule` → one `forwarder::ClientRule` task → one listener. After
v0.9 the client groups inbound `ClientRule`s by
`(protocol = tcp, listen_range = single port, any member has sni_pattern)`:

- Group activation = bind once, build `SniRoutingTable` from group members,
  start one accept loop with the SNI dispatch path.
- Group mutation (rule add / remove / update inside the group) triggers an
  `ArcSwap` of the `SniRoutingTable`. The listener stays bound; in-flight
  proxies keep their original `rule_id` and target list.
- Group becomes empty → cancel listener (existing drain semantics apply
  unchanged).

A standalone `ClientRule` with `sni_pattern = None` and no SNI siblings
remains a v0.7-shape forwarder — no peek, no parse.

## User-Visible Behaviour

### Routing path (data plane, forward-client)

```
client TCP SYN
  ↓
accept on port P (one listener per group)
  ↓
listener mode?
  ├─ legacy plain (group size 1, sni_pattern = NULL) → existing v0.7 path
  │   (byte-identical, no peek, no audit on accept)
  └─ SNI dispatch (group size ≥ 1 and any member has sni_pattern)
      ↓
      peek ClientHello (≤ 3 s, ≤ 64 KiB)
        ├─ timeout / not TLS / malformed → reject + audit
        └─ parse SNI (may be None)
              ↓
              SniRoutingTable::lookup(sni)
                ├─ Exact hit
                ├─ Wildcard hit (longest matching suffix; remainder has no `.`)
                ├─ Fallback (NULL slot) — used when sni is None or no match
                └─ Miss → reject + audit (`tls.no_sni` or `tls.sni_no_match`)
              ↓
              selected member rule_id → v0.7 target select + failover (unchanged)
              ↓
              connect upstream → write the buffered ClientHello bytes verbatim
              ↓
              bidirectional splice (unchanged)
```

### Control plane

- `Rule.sni_pattern: Option<String>` — new optional field on the rule resource.
- Proto: `optional string sni_pattern = 11;` (next free slot after v1.4's
  field 10). Wire-compat test added under `crates/forward-proto/tests/`.
- `POST /v1/rules` accepts the field; validation rejects:
  - non-TCP rule with `sni_pattern` set → 400 `validation.sni_on_unsupported_rule`
  - port-range rule (`listen_port_end IS NOT NULL`) with `sni_pattern` set →
    400 `validation.sni_on_unsupported_rule`
  - malformed pattern → 400 `validation.sni_pattern_malformed`. Grammar:
    - `exact.host.tld` — RFC 1035 hostname labels, ASCII only, ≤ 253 chars
    - or `*.suffix.tld` — `*` MUST be the first label and immediately
      followed by `.`; no `*` elsewhere; remainder follows hostname rules
    - IDN inputs MUST be Punycode (`xn--...`) by the caller; storage is lowercased
  - duplicate `(client_name, listen_port, sni_pattern)` (NULL counted) → 409
    `conflict.sni_route_duplicate`
- D9 capability gate: `sni_pattern.is_some()` push to a client whose last
  `Hello.client_version` is < 0.9.0 → 422 `sni_unsupported_by_client`.
- CLI: `forward-server push-rule --sni <pattern>` (optional). Same validation.
- `list-rules --json` and `GET /v1/rules` include `sni_pattern` when set.
- Web UI rules page: new `SNI` column; new/edit form shows an optional
  `SNI Pattern` input only when Protocol = TCP and Port mode = Single, with
  helper text covering exact / wildcard / blank-fallback semantics.

### Failure handling (audit events)

| Event | When |
|---|---|
| `tls.client_hello_timeout` | 3 s elapsed before a parseable ClientHello arrived |
| `tls.parse_failed` | Bytes are not TLS or ClientHello structure is malformed |
| `tls.no_sni` | ClientHello valid but no `server_name` extension; carries `fallback_used: bool` |
| `tls.sni_no_match` | Has SNI, no rule matches, no fallback present |
| `tls.sni_routed` | Successful routing on a SNI listener |

`tls.sni_routed` is the only per-connection audit event introduced; legacy
plain-TCP listeners continue to emit zero per-connection events. The four
failure events are exceptional and follow v0.7's "audit only control-plane
changes + exceptions" rule.

### "None" Semantics — disambiguated

`Rule.sni_pattern = None` means different things depending on the route group:

| Group composition | `None` rule means | Listener behaviour |
|---|---|---|
| Single rule, all `None` | **Legacy plain TCP forward** (same as v0.7) | No peek, no parse — byte-identical to v0.7 |
| ≥ 1 SNI rule + a `None` sibling | **SNI fallback** for *valid TLS* with no/unknown SNI | Peek + parse always; non-TLS bytes still get rejected |
| ≥ 1 SNI rule, no `None` sibling | (no fallback) | Peek + parse; no-SNI / no-match → reject |

This means a `None` rule on a port that *also* has SNI rules does **not**
silently become a plain-TCP catch-all; the listener still demands a parseable
ClientHello.

## Architecture (corrected crate placement)

### forward-proto

`proto/forward.proto` Rule:
```proto
// Additive in v1.5 (spec 009-tls-sni-routing). Wire field number 11.
// Absent → plain TCP forward / SNI fallback (depending on route group).
// Present → host or `*.suffix` pattern; ASCII, lowercased, ≤ 253 chars.
optional string sni_pattern = 11;
```

Wire-compat test (`crates/forward-proto/tests/sni_wire_compat.rs`):
- v0.8 binary encoding round-trips through v0.9 deserialiser → field absent.
- v0.9 encoding with field omitted is byte-identical to v0.8 encoding for
  the same Rule.
- v0.9 encoding with field set is rejected by v0.8 deserialiser only via the
  D9 server-side gate; on-wire it is silently dropped.

### forward-client (data plane)

```
crates/forward-client/src/forwarder/
├── mod.rs                  # ClientRule grows pub sni_pattern: Option<String>
├── sni/
│   ├── mod.rs              # pub use
│   ├── client_hello.rs     # parse(&[u8]) -> Result<Option<ServerName>, ParseError>
│   ├── route_table.rs      # SniRoutingTable + lookup
│   ├── peek.rs             # async ClientHello peek (3 s / 64 KiB)
│   └── listener.rs         # group-aware listener: bind once, dispatch per accept
├── proxy.rs                # unchanged hot path; preread bytes plumbed in
└── ...
```

`ClientRule` gains:
```rust
pub struct ClientRule {
    // ... existing fields
    pub sni_pattern: Option<String>,
}
```

### forward-server (control plane only)

- `crates/forward-server/src/rules.rs` — overlap check + index data structure
  changes (see §Rule Lifecycle Change).
- `crates/forward-server/src/store/migrations/V002__add_sni_pattern.sql` —
  schema migration (below).
- `crates/forward-server/src/operator/http.rs` — D9 capability gate; helper
  `version_at_least_0_9` next to existing `version_at_least_0_7`.
- `crates/forward-server/src/main.rs` — `push-rule --sni <pattern>` CLI flag.

### Component contracts (new code)

**`sni::client_hello::parse(bytes: &[u8]) -> Result<ParseOutcome, ParseError>`**
- Pure function, no I/O. Handshake-message-level: tracks the current handshake
  fragment across multiple TLS records (handshake messages can be fragmented
  per RFC 8446 §5.1).
- Outcomes: `Truncated` (need more bytes), `Ok(Some(host))`, `Ok(None)` (no
  SNI extension); errors `NotTls`, `Malformed`.

**`sni::route_table::SniRoutingTable`**
- `from_members(rules: &[&ClientRule]) -> Self` for one group.
- `lookup(sni: Option<&str>) -> SniMatch { Hit { rule_id, kind }, Miss }`.
- Wildcard match: store wildcard suffixes (the part after `*.`), sort by
  suffix length descending. A candidate `host` matches `*.suffix` iff
  `host.ends_with(".suffix")` AND the prefix `host[..host.len() - suffix.len() - 1]`
  contains no `.`.
- Comparison: lowercase `host` once on entry; storage is already lowercase.

**`sni::peek::read_client_hello`** — `tokio::time::timeout(3 s)` over a loop
that reads, accumulates into a `Vec<u8>` capped at 64 KiB, calls
`client_hello::parse`, and exits on `Ok` or budget exhaustion.

**`sni::listener`** — owns the bound `TcpListener`, the `Arc<ArcSwap<SniRoutingTable>>`,
and the cancellation token. Dispatches each accepted stream into the existing
`proxy::proxy` after picking a `rule_id`.

## Data Model & Schema

`Rule` (Rust, server-side `crates/forward-server/src/rules.rs`):
```rust
pub struct Rule {
    // ... existing fields incl. listen_port, listen_port_end, protocol
    pub sni_pattern: Option<String>,
}
```

SQLite migration `V002__add_sni_pattern.sql` (using actual v0.8 columns):

```sql
ALTER TABLE rules ADD COLUMN sni_pattern TEXT;

-- Within a SNI route group (same client + single TCP port), each pattern
-- (NULL counts) appears at most once. We don't have a client column on
-- `rules` today (rules carry owner_user_id, not client_name) — uniqueness
-- by client is enforced in-memory by ServerRuleStore; the SQL constraint
-- guards against logical duplicates per single-port TCP rule.
CREATE UNIQUE INDEX rules_port_sni_uniq
    ON rules(listen_port, IFNULL(sni_pattern, ''))
    WHERE protocol = 'tcp' AND listen_port_end IS NULL;

CREATE INDEX rules_sni_lookup
    ON rules(listen_port, sni_pattern)
    WHERE sni_pattern IS NOT NULL;
```

> Open item for `/speckit-specify`: the rules table today has no `client_name`
> column — rule → client mapping is held by `ServerRuleStore`'s in-memory
> index. If we want SQL-level uniqueness scoped to client we need to add a
> `client_name` column or accept that the SQL UNIQUE is a tighter "global per
> port" constraint. Recommendation: keep SQL UNIQUE global per port (matches
> the existing `rules_listen_idx` granularity) and let the application layer
> raise a friendlier error.

Schema-version handshake (v0.8): supported range shifts from `[1,1]` to
`[1,2]`. No data migration (additive column).

## Observability

### Prometheus metrics (new)

| Metric | Type | Labels |
|---|---|---|
| `forward_tls_sni_route_total` | counter | `port`, `result=exact|wildcard|fallback|miss` |
| `forward_tls_client_hello_parse_failures_total` | counter | `port`, `reason=timeout|not_tls|truncated|malformed` |
| `forward_tls_client_hello_peek_duration_seconds` | histogram | `port` (buckets 0.001 / 0.01 / 0.1 / 1 / 3) |
| `forward_tls_sni_routes_active` | gauge | — |

Exported by forward-client's existing metrics endpoint; forward-server only
mirrors the totals if the operator scrapes via the management plane.

### Logs

`tracing` events with `target = "tls_sni"`, INFO for routed connections, WARN
for parse failures and unmatched SNI. Names mirror the audit events.

## Testing Strategy

Per Constitution Principle III, every contract surface and SC ships with tests
authored before implementation.

### Unit tests (in-source `#[cfg(test)] mod tests`)

- `sni/client_hello.rs`: real packet captures (TLS 1.0/1.1/1.2/1.3) under
  `crates/forward-client/tests/fixtures/tls/*.bin`; truncation at varied
  offsets; missing/empty SNI extension; oversize host; multiple `server_name`
  entries (first wins); incremental feed (Truncated → Ok); handshake message
  fragmented across two records.
- `sni/route_table.rs`: priority (Exact > Wildcard > Fallback); wildcard
  specificity (`*.foo.example.com` beats `*.example.com`); explicit single-label
  guard (`a.b.example.com` does NOT match `*.example.com`); miss without
  fallback; case insensitivity; ArcSwap rebuild safety.

### Contract / integration tests

forward-server (`crates/forward-server/tests/`):
| File | Covers |
|---|---|
| `sni_rule_validation.rs` | UDP / range + sni → 400; malformed pattern → 400; duplicate `(port, sni)` → 409 |
| `sni_capability_gate.rs` | push-rule with sni to a v0.8 client → 422 `sni_unsupported_by_client` (D9) |
| `sni_route_group_overlap.rs` | Two TCP rules same port distinct sni → accepted; same sni → 409; one with sni one without → accepted (None = fallback) |

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
| `sni_metrics.rs` | Mixed traffic produces the expected `forward_tls_sni_route_total{result=...}` distribution |
| `legacy_plain_tcp_unchanged.rs` | A non-SNI port on the same client is byte-identical to v0.7 (no peek path entered) |

forward-proto (`crates/forward-proto/tests/`):
| File | Covers |
|---|---|
| `sni_wire_compat.rs` | Field 11 round-trip; absent-field bytes identical to v0.8 encoding |

### Benches (`crates/forward-client/benches/sni_route.rs`)

- `SniRoutingTable::lookup` ns/op at 100 / 1 000 / 10 000 routes (hit + miss).
- End-to-end TCP connect + handshake setup latency vs. v0.7 baseline; SNI
  ports allowed +5 ms (parse < 100 µs, balance is network), legacy plain
  ports must not enter the SNI code path (assert via tracing test target).

## Constitution Check (preview)

- **I. Auth invariants** — TLS + bearer token unchanged. SNI is L4 routing,
  no auth seam touched.
- **II. Single binary** — pure-Rust parser, zero new deps.
- **III. Test-first** — every FR / SC has a failing test before implementation.
- **IV. Observability** — new metrics + audit events listed; legacy plain
  ports retain zero per-connection audit cost.
- **V. byte-stable control plane** — proto field 11 is `optional` so v0.8
  bytes are unchanged; D9 prevents silently activating SNI rules on v0.8
  clients; schema migration is additive.

## Risks & Mitigations

| Risk | Mitigation |
|---|---|
| Hand-rolled parser misses a corner case | Real-packet fixtures + fuzz harness in benches dir |
| TLS extension order drift breaks parsing | We only read `server_name` and skip everything else by length |
| ArcSwap rebuild stalls accept loop | Build new table off the accept task; ArcSwap is `O(1)` swap |
| Old client silently activates SNI rule as plain TCP | D9 capability gate (HTTP 422 before push) |
| Validation false-positives reject legal hosts | Pattern grammar documented; reject reason names a single rule |
| Operator forgets a fallback and locks out non-SNI clients | Web UI helper text and CLI docs make NULL fallback discoverable |
| `*.example.com` accidentally matching `a.b.example.com` | Explicit single-label remainder check in `route_table::lookup`; test covers it |
| ClientHello legitimately spans records and exceeds early cap | Parser tracks handshake-fragment reassembly; cap raised to 64 KiB |

## Open Questions

1. SQL-level uniqueness scope (per port, vs per `(client, port)`) — see Data
   Model note. Recommendation: keep SQL global per port; let the app layer
   raise a friendlier error scoped by client.
2. Should the `tls.sni_routed` audit event include the matched `server_name`
   in clear text? (Privacy vs. observability.) Recommendation: yes — it is
   already on the wire in clear, and operators need it for routing audits.

Anything else surprising during plan-writing or implementation can be parked
here.
