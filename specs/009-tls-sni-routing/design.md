# Design: TLS SNI Routing

**Feature Branch (proposed)**: `009-tls-sni-routing`
**Created**: 2026-05-08
**Status**: Brainstorm output — pending user review, then `/speckit-specify` or direct `writing-plans`
**Predecessor**: v0.8.0 (`008-sqlite-storage`, merged 2026-05-08)

## Goal

Add Server Name Indication (SNI) based routing to forward-rs so a single TCP listen
port (typically 443) can fan out to different upstream targets based on the TLS
hostname the client requests in its `ClientHello`. forward-rs remains a pure L4
byte-passthrough — it never decrypts, never terminates TLS, never re-encrypts.

## Non-Goals

- TLS termination, reverse-proxy, HTTP-aware routing — those are tracked in the
  post-v0.8 backlog as L7 work and are explicitly out of scope.
- QUIC / HTTP/3 (UDP) SNI routing — different parser path, deferred.
- SNI on TCP port-range rules — see §Decisions, may be relaxed later.
- Connection rate limiting / QoS, PROXY protocol — separate backlog items.

## Decisions

| # | Topic | Choice | Rationale |
|---|---|---|---|
| D1 | Match model | Exact host **+** single-level wildcard `*.example.com` **+** NULL fallback | Aligns with TLS SAN semantics; minimal hot-path overhead. |
| D2 | Match priority | Exact → Wildcard (longest suffix first) → Fallback (NULL) → Miss | Standard "more specific wins". |
| D3 | ClientHello parser | Hand-rolled, ~150 LOC, **zero new deps** | Small audit surface, only one extension (`server_name`) consumed. |
| D4 | Read budget | 3 s timeout **and** 16 KiB cap per ClientHello peek | TLS record max is 16 384 bytes; real-world ClientHello (incl. PQ-hybrid groups) fits well under that. Budget protects against slowloris. |
| D5 | Rule scope | TCP single-port only | UDP rejected (no TLS); port range rejected by validation. May relax later. |
| D6 | Failure semantics | Timeout → reject; non-TLS → reject; no SNI → fallback else reject; no match → fallback else reject. | Reject by default; fallback is opt-in via explicit NULL rule. |
| D7 | Hot reload | `ArcSwap<SniRoutingTable>`; full table rebuild on every rule change | Rules ≪ connections; simpler than incremental update. |
| D8 | API compat | New `sni_pattern: Option<String>` field; serde `skip_serializing_if = Option::is_none` | byte-stable for v0.8 clients that never send the field. |

## User-Visible Behaviour

### Routing path (data plane)

```
client TCP SYN
  ↓
accept on port P
  ↓
P in SNI routing set?
  ├─ no  → existing v0.7 path (unchanged byte-for-byte)
  └─ yes → peek ClientHello (≤ 3 s, ≤ 16 KiB)
            ├─ timeout / not TLS / malformed → reject + audit
            └─ parse SNI (may be None)
                  ↓
                lookup(port, sni)
                  ├─ Exact hit
                  ├─ Wildcard hit (longest suffix wins)
                  ├─ Fallback (NULL rule on this port) — used when sni is None or no match
                  └─ Miss → reject + audit
                  ↓
                selected Rule → v0.7 target select + failover (unchanged)
                  ↓
                connect upstream → write the buffered ClientHello bytes verbatim
                  ↓
                bidirectional splice (unchanged)
```

### Control plane

- `Rule.sni_pattern: Option<String>` — new optional field on the existing rule
  resource. `None` (or omitted) means the rule is a plain TCP forward / NULL
  fallback for its port.
- `POST /v1/rules` accepts the field; validation rejects:
  - non-TCP rules with `sni_pattern` set → 400 `validation.sni_on_unsupported_rule`
  - port-range rules with `sni_pattern` set → 400 `validation.sni_on_unsupported_rule`
  - malformed pattern → 400 `validation.sni_pattern_malformed`. Grammar:
    - `exact.host.tld` — RFC 1035 hostname labels, ASCII only, ≤ 253 chars
    - or `*.suffix.tld` — `*` MUST be the first label and immediately
      followed by `.`; no `*` elsewhere; remainder follows hostname rules
    - IDN inputs MUST be Punycode-encoded (`xn--...`) by the caller; storage
      is lowercased on write
  - duplicate `(local_port, sni_pattern)` (NULL counted) → 409
    `conflict.sni_route_duplicate`
- CLI: `forward-server push-rule --sni <pattern>` (optional). Same validation.
- `list-rules --json` and `GET /v1/rules` include `sni_pattern` when set.
- Web UI rules page: new `SNI` column in the list; in the new/edit form an
  optional `SNI Pattern` input appears when Protocol = TCP and Port mode =
  Single, with helper text describing exact / wildcard / blank.
- `forward-client` and the wire protocol are NOT touched.

### Failure handling (audit events)

| Event | When |
|---|---|
| `tls.client_hello_timeout` | 3 s elapsed before a parseable ClientHello arrived |
| `tls.parse_failed` | Bytes are not TLS or ClientHello structure is malformed |
| `tls.no_sni` | ClientHello valid but no `server_name` extension; carries `fallback_used: bool` |
| `tls.sni_no_match` | Has SNI, no rule matches, no fallback present |
| `tls.sni_routed` | Successful routing on a SNI port |

`tls.sni_routed` is the only per-connection audit event introduced; non-SNI
ports continue to emit zero per-connection events, preserving v0.7's
"audit only control-plane changes + exceptions" rule. The four failure
events above are by definition exceptional and follow that rule.

## Architecture

### New module tree (forward-server crate)

```
crates/forward-server/src/
├── tls/
│   ├── mod.rs              # pub use
│   ├── client_hello.rs     # parse(&[u8]) -> Result<Option<ServerName>, ParseError>
│   ├── sni_route.rs        # SniRoutingTable + lookup
│   └── peek.rs             # async ClientHello peek with timeout + size cap
└── forward/tcp.rs          # add SNI dispatch branch
```

### Component contracts

**`tls::client_hello::parse`** — pure function; returns `Truncated` to ask the
caller for more bytes, `NotTls` / `Malformed` for hard failures, `Ok(Some|None)`
on success. Only the `server_name` extension is read; everything else is
skipped without allocation.

**`tls::sni_route::SniRoutingTable`**
- Built from `&[Rule]`; per-port struct with `HashMap<host, rule_id>` for exact,
  `Vec<(suffix, rule_id)>` sorted by suffix length descending for wildcards,
  and `Option<rule_id>` for the NULL fallback.
- `lookup(port, sni: Option<&str>) -> SniMatch` returns the matched rule id and
  match kind, or Miss. Hot-path target.
- Input SNI from the wire and stored patterns are both lowercased before
  comparison. IDN labels arrive as Punycode in TLS so byte comparison after
  lowercasing is sufficient — no Unicode normalisation needed.

**`tls::peek::read_client_hello`** — owns one `tokio::time::timeout` + one
growing `Vec<u8>` (capped at 16 KiB) and re-invokes `client_hello::parse`
on each `read` until success or budget exhaustion. Returns the captured
buffer so the dispatcher can replay it to the upstream.

**`forward::tcp` dispatcher** — adds one branch:

```rust
if ctx.sni_table.has_port(port) {
    let (preread, sni) = peek::read_client_hello(&mut stream, 3s, 16 KiB).await?;
    let rule = ctx.sni_table.lookup(port, sni.as_deref()).into_rule_or_reject(audit)?;
    let upstream = select_target_with_failover(rule, ctx).await?;  // v0.7
    upstream.write_all(&preread).await?;
    splice_bidirectional(stream, upstream).await
} else {
    // v0.7 path, untouched
}
```

### Hot-reload

- `OperatorService::push_rule` / `remove_rule` rebuild a fresh
  `SniRoutingTable` after the SQLite transaction commits and `ArcSwap` it into
  the dispatcher context.
- In-flight connections keep their original `rule_id` and are unaffected.

## Data Model & Schema

`Rule` gains:

```rust
pub struct Rule {
    // ... existing fields
    pub sni_pattern: Option<String>,
}
```

SQLite migration `V002__add_sni_pattern.sql`:

```sql
ALTER TABLE rules ADD COLUMN sni_pattern TEXT;

-- One rule per (port, sni) — NULL counts as the fallback slot
CREATE UNIQUE INDEX rules_port_sni_uniq
    ON rules(local_port, IFNULL(sni_pattern, ''))
    WHERE protocol = 'tcp' AND port_mode = 'single';

CREATE INDEX rules_sni_lookup
    ON rules(local_port, sni_pattern)
    WHERE sni_pattern IS NOT NULL;
```

Server boot keeps the v0.8 schema-version handshake; only the supported range
shifts from `[1,1]` to `[1,2]`. No data migration needed (additive column).

## Observability

### Prometheus metrics (new)

| Metric | Type | Labels |
|---|---|---|
| `forward_tls_sni_route_total` | counter | `port`, `result=exact|wildcard|fallback|miss` |
| `forward_tls_client_hello_parse_failures_total` | counter | `port`, `reason=timeout|not_tls|truncated|malformed` |
| `forward_tls_client_hello_peek_duration_seconds` | histogram | `port` (buckets 0.001 / 0.01 / 0.1 / 1 / 3) |
| `forward_tls_sni_routes_active` | gauge | — |

No new per-target metrics: SNI selects which Rule, then v0.7's existing per-target
metrics carry on.

### Logs

`tracing` events with `target = "tls_sni"`, INFO for routed connections, WARN
for parse failures and unmatched SNI. Names mirror the audit events above.

## Testing Strategy

Per Constitution Principle III, every contract surface and SC ships with tests
authored before implementation.

### Unit tests (in-source `#[cfg(test)] mod tests`)

- `client_hello.rs`: real packet captures (TLS 1.0/1.1/1.2/1.3) under
  `crates/forward-server/tests/fixtures/tls/*.bin`; truncation at varied
  offsets; missing/empty SNI extension; oversize host; multiple
  `server_name` entries (first wins); incremental feed (Truncated → Ok).
- `sni_route.rs`: priority (Exact > Wildcard > Fallback); wildcard specificity
  (`*.foo.example.com` beats `*.example.com`); miss without fallback; case
  insensitivity; ArcSwap rebuild safety.

### Contract / integration tests (`crates/forward-server/tests/`)

| File | Covers |
|---|---|
| `sni_rule_validation.rs` | UDP / range + sni → 400; malformed pattern → 400; duplicate `(port, sni)` → 409 |
| `sni_route_e2e_exact.rs` | Two SNI rules on `:443`, rustls clients land on the correct upstream |
| `sni_route_e2e_wildcard.rs` | `*.example.com` matches `foo.example.com`, not `example.com` (single-level) |
| `sni_route_fallback.rs` | No-SNI client uses NULL fallback; without fallback the connection is reset + `tls.no_sni` audited |
| `sni_route_timeout.rs` | TCP connect, no bytes for 3 s → reset + `tls.client_hello_timeout` |
| `sni_route_not_tls.rs` | Plain HTTP on the port → reset + `tls.parse_failed` |
| `sni_byte_passthrough.rs` | sha256 of upstream-received bytes equals client-sent bytes |
| `sni_hot_reload.rs` | Existing connections unaffected when rule set changes |
| `sni_metrics.rs` | Mixed traffic produces the expected `forward_tls_sni_route_total{result=...}` distribution |
| `sni_v07_compat.rs` | A non-SNI port on the same server is byte-identical to v0.7 |
| `sni_audit_envelope.rs` | Audit event JSON includes `server_name` and `match_kind`; envelope is byte-stable |

### Benches (`crates/forward-server/benches/sni_route.rs`)

- `SniRoutingTable::lookup` ns/op at 100 / 1 000 / 10 000 rules (hit + miss).
- End-to-end TCP connect + handshake setup latency vs. v0.7 baseline; SNI
  ports allowed +5 ms (parse < 100 µs, balance is network), non-SNI ports
  must be byte-identical (no SNI branch entered).

## Constitution Check (preview)

- **I. Auth invariants** — TLS + bearer token unchanged. SNI is L4 routing,
  no auth seam touched.
- **II. Single binary** — pure-Rust parser, zero new deps. `tls-parser` crate
  considered and rejected (see D3).
- **III. Test-first** — every FR / SC has a failing test before its
  implementation; see §Testing.
- **IV. Observability** — new metrics + audit events listed; non-SNI ports
  retain zero per-connection audit cost.
- **V. byte-stable control plane** — `sni_pattern` is opt-in, omitted when
  None, schema migration is additive.

## Risks & Mitigations

| Risk | Mitigation |
|---|---|
| Hand-rolled parser misses a corner case | Real-packet fixtures + fuzz harness in benches dir |
| TLS extension order drift breaks parsing | We only read `server_name` and skip everything else by length |
| ArcSwap rebuild stalls accept loop | Build new table off-thread, ArcSwap is `O(1)` swap |
| Validation false-positives reject legal hosts | Pattern grammar is documented; reject reason names a single rule |
| Operator forgets a fallback and locks out non-SNI clients | Web UI helper text and CLI docs make NULL fallback discoverable |

## Open Questions

None — all defaults accepted during brainstorm. Anything surprising during
plan-writing or implementation can be parked back here.
