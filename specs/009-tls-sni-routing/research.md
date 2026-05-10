# Phase 0 — Research: TLS SNI Routing

**Feature**: 009-tls-sni-routing
**Date**: 2026-05-08
**Authoritative reference**: [`design.md`](./design.md) (commits a556570 → 0142e43)

This document closes every "NEEDS CLARIFICATION" implicit in the spec by
recording the decisions, the rationale, and the alternatives considered.
Decisions are numbered `R-NNN` so later artifacts (`data-model.md`,
`contracts/*`, `tasks.md`) can cite them.

---

## R-001 — ClientHello parser implementation

**Decision**: Hand-roll a pure-Rust parser (~150 LOC) at
`crates/portunus-client/src/forwarder/sni/client_hello.rs`. Read only what
is needed to extract the `server_name` extension; skip everything else by
length. Tracks handshake-message reassembly across multiple TLS records
(RFC 8446 §5.1).

**Rationale**:
- Constitution II: zero new workspace deps keeps the binary footprint flat.
- Audit surface is tiny — one parser, one extension, one purpose.
- Existing crates (`tls-parser`, `rustls`) pull in a much larger surface
  (full handshake state, certificate chains, key shares) that we do not
  need and that would couple our build to upstream PQ-hybrid changes.

**Alternatives considered**:
- `tls-parser` crate — full nom-based parser; rejected for size and dep
  weight.
- Re-using `rustls`'s ClientHello reader — not exposed as public API; we
  would depend on an internal module.
- Regex / byte-scan for `server_name` — fragile; TLS 1.3 ClientHello
  layout makes a structural parser the only correct path (RFC 8446
  §4.1.2 + §4.2).

---

## R-002 — Hot-reload primitive (replaces ArcSwap)

**Decision**: `tokio::sync::watch::Sender<Arc<SniRoutingTable>>` per
listener. The control task builds a fresh routing table on every group
mutation and calls `send_replace`. Per-connection accept handlers read the
current value via `borrow().clone()` once and own the `Arc` for the
connection's lifetime.

**Rationale**:
- `tokio` is already a workspace dep; no new crate needed.
- `watch::Sender::send_replace` is `O(1)` and lock-free for readers.
- An `Arc` snapshot is cheap (one atomic increment).
- Builds satisfy Constitution II (no allocation in steady-state byte
  forwarding — table rebuild happens in the control task, not on accept).

**Alternatives considered**:
- `arc_swap` crate — first design used it; rejected in round-2 review
  for being a new workspace dep with no precedent in the project.
- `Arc<RwLock<Arc<…>>>` — extra lock contention on the hot path; rejected.
- `tokio::sync::Mutex` — write would block reads; rejected.

---

## R-003 — Storage delta

**Decision**: One additive `ALTER TABLE rules ADD COLUMN sni_pattern TEXT`,
plus one helper `CREATE INDEX rules_sni_lookup ON rules(listen_port,
sni_pattern) WHERE sni_pattern IS NOT NULL`. Migration filename:
`V002__add_sni_pattern.sql`. Schema-version handshake range shifts
`[1,1] → [1,2]`. No SQL `UNIQUE` constraint on
`(listen_port, IFNULL(sni_pattern, ''))`.

**Rationale**:
- `rules` has no `client_name` column today (rules carry `owner_user_id`,
  not the client identity that runs the rule). Adding such a column is
  out of scope for v0.9.
- A *global* `UNIQUE` per port would forbid two clients from each owning
  `:443 + api.example.com` — a legitimate multi-tenant pattern (FR-021).
- `ServerRuleStore::by_client_listen_start` already provides per-client
  indexing; uniqueness can be enforced authoritatively in memory, with
  a friendlier error than a generic SQL constraint violation.
- The helper index is partial (only non-NULL `sni_pattern`) so the legacy
  scan plan is unaffected.

**Alternatives considered**:
- Add `client_name TEXT` to `rules` so a SQL UNIQUE can be tight — rejected
  for scope creep into v0.9 (would require migrating in-memory state too).
- Drop the helper index — rejected; the operator API "list rules with SNI"
  query needs it for predictable plan stability as audit history grows.

---

## R-004 — Listener mode lifetime (mode-locked)

**Decision**: A `(client_name, listen_port)` listener is in **exactly one
mode** for its lifetime — legacy plain-TCP or SNI dispatch — fixed by the
first rule pushed onto the port. Online conversion is **forbidden**: a
push that would change a live listener's mode is refused server-side
with `409 conflict.legacy_to_sni_unsupported`. The operator must remove
the existing rule first (which drains its connections), then push the
new shape onto a freshly bound listener.

**Rationale**:
- Eliminates the need to migrate the accept loop's read semantics
  (no peek → peek) on a live socket.
- Removes the need for an atomic "rebroadcast entire group" wire message
  (HIGH-2 from round-2 review).
- Operationally honest: SNI requires a peek phase that is fundamentally
  different from byte-pass-through; surfacing that at push time avoids
  silent behaviour change for in-flight connections.

**Alternatives considered**:
- Allow live mode flip — would require atomic group rebroadcast, a
  new `RuleGroupUpdate` proto message, and tearing down + rebinding the
  socket while clients are connected. Disproportionate complexity.
- Forbid `None` ever from being a sibling of SNI rules — would lose the
  "TLS-only fallback" semantic that operators legitimately want.

---

## R-005 — Wire compatibility envelope

**Decision**:
- `Rule.sni_pattern = 11` (`optional string`, proto3 optional).
- `RuleStats.sni_route_exact_total = 13`, `sni_route_wildcard_total = 14`,
  `sni_route_fallback_total = 15` (uint64; default-zero).
- New `SniListenerStats { listen_port = 1; sni_route_miss_total = 2;
  client_hello_parse_failures_total = 3; }`.
- `StatsReport.sni_listener_stats = 3` (repeated `SniListenerStats`).
- `RuleUpdate` stays single-rule (no group-update message added).

**Rationale**:
- `RuleStats` fields 11 (`target_failovers_total`) and 12 (`per_target`)
  are already claimed by v0.7 (HIGH-1 from round-3 review). Picking 13+
  avoids any wire collision and makes the negative wire-compat assertion
  trivial.
- Miss / parse-failure events have no rule attribution — forcing them
  onto `RuleStats` would require a fake `rule_id`. A separate
  `SniListenerStats` keyed by `listen_port` is honest and keeps
  Prometheus labels clean (HIGH-2 from round-3 review).
- Mode-locked lifetime (R-004) makes the single-rule `RuleUpdate` shape
  sufficient — no need for `RuleGroupUpdate`.

**Alternatives considered**:
- Per-result Prometheus labels via a single counter on `RuleStats` —
  would still require a fake rule attribution for miss / parse-failure;
  rejected.
- Bumping the proto major version — gratuitous; the changes are strictly
  additive, v0.8 readers ignore unknown fields.

---

## R-006 — Zero-new-deps invariant

**Decision**: Ship v0.9 without adding any workspace dependency. All new
data-plane plumbing uses `tokio::sync::watch` (existing), `Vec` /
`HashMap` / `BTreeMap` (std), and a hand-rolled ClientHello parser.

**Rationale**:
- Aligns with Constitution II (single-binary, minimal surface).
- Aligns with the v0.8 audit history — `rusqlite` was the last new
  workspace dep, motivated by a clear architectural need; SNI parsing
  has no comparable need.
- Reduces supply-chain risk for a feature that sits on the network path.

**Alternatives considered**:
- Add `arc_swap` for hot-reload — rejected per R-002.
- Add `smallvec` for the per-port rule list — rejected; lists are
  bounded ≤ 8 (typical) and a `Vec<RuleId>` allocation per port is
  one-time.
- Add a TLS parsing crate — rejected per R-001.

---

## R-007 — Capability gate (client-version)

**Decision**: portunus-server's operator API refuses any
`POST /v1/rules` whose body has `sni_pattern.is_some()` if the targeted
portunus-client's last `Hello.client_version` is below `0.9.0`. Response:
`HTTP 422 sni_unsupported_by_client { client_name, client_version }`.
The check sits next to the v0.7 multi-target check at
`crates/portunus-server/src/operator/http.rs:353` (function
`version_at_least_0_7` — we add `version_at_least_0_9` alongside).

**Rationale**:
- v0.8 portunus-client decodes proto3 in the standard way: unknown fields
  are dropped on decode. Without the gate it would activate the rule
  as a plain TCP forward, silently losing SNI dispatch.
- Mirrors the v0.7 multi-target gate (R-007 from spec 007), so the
  operator UX is consistent.
- The error code names the missing capability so the operator can decide
  whether to upgrade the affected client.

**Alternatives considered**:
- Force a hard wire break — would require a major version bump and
  break v0.8 plain-TCP rules; rejected.
- Trust the client to refuse the rule — possible (FR-015's defensive
  `mode_change_unsupported`) but moves the failure to runtime; the gate
  catches it at push time, before any byte forwards.

---

## R-008 — Wildcard match grammar

**Decision**: Pattern grammar is one of:
- `exact.host.tld` — RFC 1035 hostname labels, ASCII only, ≤ 253 chars,
  lowercased.
- `*.suffix.tld` — `*` MUST be the first label and immediately followed
  by `.`; no `*` elsewhere; remainder follows hostname rules and MUST
  contain at least two labels.

A candidate `host` matches `*.suffix` iff
`host.ends_with(".suffix")` AND
`host[..host.len() - suffix.len() - 1]` contains no `.`.

Match priority: Exact > Wildcard (longest matching `suffix` wins) >
Fallback (NULL slot) > Miss.

**Rationale**:
- Aligns with TLS certificate SAN wildcard semantics (single-level only).
- The explicit single-label remainder check prevents `a.b.example.com`
  from matching `*.example.com` (MEDIUM 7 from round-1 review).
- Longest-suffix-wins gives operators a natural way to layer wildcards
  (`*.team.example.com` beats `*.example.com`).
- IDN labels arrive on the wire as Punycode (`xn--…`); byte-comparison
  after lowercasing is sufficient — no Unicode normalisation library.

**Alternatives considered**:
- Multi-level wildcards (`*.*.example.com`) — rejected; semantics are
  undefined in TLS practice and confuse operators.
- Regex patterns — rejected; ReDoS surface and operational complexity
  are out of proportion.
- "First match wins" instead of longest-suffix-wins — rejected; would
  make the order-of-insertion meaningful, which is hostile to declarative
  rule lists.

---

## R-009 — Read budget (timeout + size)

**Decision**: 3 second wall-clock timeout on the ClientHello peek, OR a
cumulative 64 KiB cap, whichever fires first. On either limit the
connection is closed and a tracing event is emitted.

**Rationale**:
- Real ClientHellos including PQ-hybrid key shares (X25519+ML-KEM) fit
  comfortably under 16 KiB per record; 64 KiB allows multi-record
  fragmented handshake messages (RFC 8446 §5.1) without legitimate
  rejection (MEDIUM 6 from round-2 review).
- 3 s is a practical floor for a slowloris-style attack window without
  hurting legitimate clients on high-RTT networks.
- Both limits emit structured events keyed by listen_port + peer_addr,
  giving operators enough fields for triage without a packet capture.

**Alternatives considered**:
- 16 KiB (one TLS record max) — too tight for fragmented handshakes
  (MEDIUM 6).
- No size cap, only a timeout — risks unbounded buffer growth under
  attack; rejected.
- Configurable budget — out of scope for v0.9; defaults are conservative
  enough for now.

---

## R-010 — Data-plane events surface (tracing only)

**Decision**: All five SNI events (`tls.client_hello_timeout`,
`tls.parse_failed`, `tls.no_sni`, `tls.sni_no_match`, `tls.sni_routed`)
are emitted **only** as portunus-client `tracing` events with
`target = "tls_sni"`. They do **NOT** flow into the SQLite `audit` ring,
do **NOT** generate a `ClientMessage` to the server, and do **NOT** add
any new wire envelope.

**Rationale**:
- `crates/portunus-server/src/operator/audit.rs:16-31` reserves the
  audit ring for operator allow/deny actions and explicitly documents
  that high-frequency client-side events stay in the structured tracing
  log (the v0.7 precedent: `rule.target.health_changed`).
- Routing decisions are diagnostic, not auditable security actions.
- Forcing them through `ClientMessage` would expand the wire surface
  (MEDIUM 1 from round-3 review).

**Alternatives considered**:
- Stream them as `ClientEvent` over the bidi gRPC channel — rejected;
  expands the wire surface and adds backpressure questions.
- Co-locate them in the `audit` ring — rejected per the precedent above
  (would either evict legitimate operator events under attack-style
  bursts, or balloon ring memory).

---

## R-011 — Listener-level metric labels

**Decision**: Per-rule SNI metrics use the existing v0.5+ label triple
`(client, rule, owner)` plus `result ∈ {exact, wildcard, fallback}`.
Listener-level metrics use `(client, port)` — no `rule`, no `owner`.

**Rationale**:
- Listener-level events have no rule attribution — they happen before
  rule selection. Inventing a `rule = "_listener"` label would confuse
  operators (MEDIUM 3 from round-3 review).
- `(client, port)` keeps cardinality bounded — at most one series per
  listener.
- Reusing the established `client, rule, owner` triple keeps
  Prometheus-side queries consistent across v0.5..v0.9.

**Alternatives considered**:
- Drop `client` from listener-level — rejected; multi-client deployments
  need tenant attribution.
- Add `owner` from "the rule that would have matched" — rejected;
  there is no such rule (the event fires precisely because none matched).

---

## R-012 — Test fixtures source

**Decision**: Capture real ClientHello bytes from
`openssl s_client -connect localhost:N -servername host` against a
local `openssl s_server` for TLS 1.0 / 1.1 / 1.2 / 1.3 (one binary
fixture per version, plus one fragmented capture). Store under
`crates/portunus-client/tests/fixtures/tls/*.bin`. Fragmented capture is
synthesised by truncating + concatenating record-layer headers around a
real ClientHello.

**Rationale**:
- Real captures catch extension-order quirks no synthetic generator
  thinks of.
- Binary fixtures are byte-stable across runs and easy to diff.
- File size is trivial (≈ 1 KiB per fixture).

**Alternatives considered**:
- Generate fixtures with `rustls` at test time — rejected; couples our
  test to upstream behaviour and re-introduces the dep we deliberately
  excluded (R-001).
- Inline the fixtures as `&'static [u8]` arrays — rejected; binary diffs
  are easier to review when the bytes live in their own files.

---

## R-013 — Quickstart workflow

**Decision**: `quickstart.md` walks an operator through:
1. Bootstrap a v0.9 server + a v0.9 client.
2. Push two SNI rules: `:443 → backend-a` (sni `api.example.com`),
   `:443 → backend-b` (sni `*.web.example.com`).
3. Verify with `openssl s_client -connect server:443 -servername …`
   that traffic reaches the correct backend.
4. Add a fallback rule (no `sni_pattern`) for non-SNI clients.
5. Demonstrate the legacy → SNI conversion workflow (remove first, then
   re-push).
6. Show the relevant `/metrics` series after running the above.

**Rationale**: Mirrors v0.8's quickstart shape — concrete commands,
expected output, and a final "what to expect on `/metrics`" so operators
can integrate with their monitoring without re-reading the spec.

---

## R-014 — Data-dir / config-dir interaction

**Decision**: No new flags. SNI routing piggybacks on the v0.8
`<data-dir>/state.db` for persistence and on `<config-dir>` for
operator credentials. No new file is introduced.

**Rationale**: Constitution II (single-binary deployment posture
unchanged); no operator-visible config layout change.

---

## R-015 — Minimal-ClientHello assumptions

**Decision**: The peek path treats only the *first* TLS handshake message
of the connection. If a client sends `HelloRequest` + `ClientHello`
(legacy 1.2 renegotiation kick-off), the `HelloRequest` is malformed in
this position and the peek closes with `tls.parse_failed`. Post-handshake
renegotiations are between the client and the upstream — once we are
splicing, we never inspect bytes again.

**Rationale**: TLS 1.3 forbids renegotiation; TLS 1.2 renegotiation is
extremely rare in practice and sending a `HelloRequest` *as the first
record* is malformed. The simpler invariant ("first handshake message
must be ClientHello") avoids parser state explosion.

**Alternatives considered**:
- Tolerate `HelloRequest` and parse the next record as ClientHello —
  rejected; parser state machine grows by ≈ 50 LOC for a vanishingly
  rare case.

---

## Summary of resolved unknowns

Every "NEEDS CLARIFICATION" in `plan.md`'s Technical Context has been
resolved by the decisions above:

| Field | Resolution |
|---|---|
| Language/Version | Rust 1.88 (R-006) |
| Primary Dependencies | Zero new (R-001, R-002, R-006) |
| Storage | v0.8 SQLite + V002 additive migration (R-003) |
| Testing | tiered cargo test (Phase 0 fixtures R-012) |
| Target Platform | Linux primary, macOS dev (inherited) |
| Project Type | Cargo workspace, six crates (inherited) |
| Performance Goals | SC-003 / SC-006 budgets (plan.md) |
| Constraints | Mode-locked lifetime R-004; budgets R-009 |
| Scale/Scope | 100 typical / 10 000 max routes per listener (plan.md) |

Phase 0 complete — proceed to Phase 1 (`data-model.md`, `contracts/`,
`quickstart.md`).
