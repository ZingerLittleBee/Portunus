# Implementation Plan: TLS SNI-Based Routing for Forwarded Connections

**Branch**: `009-tls-sni-routing` | **Date**: 2026-05-08 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/009-tls-sni-routing/spec.md`

## Summary

v0.9 lets a single forward-client TCP listener fan out to different
upstreams based on the TLS hostname (SNI) the client requests in its
ClientHello. forward-rs stays a pure L4 byte-passthrough — never decrypts,
terminates, or re-encrypts TLS. The implementation lives entirely in the
data plane on `forward-client` (peek + parse + route) and in additive
control-plane fields on `forward-server`; the auth seam, credential
hashing, persistence layer, and forwarding hot-path layout are
**byte-stable** for v0.8 callers.

A new optional `Rule.sni_pattern` (proto field 11) selects between an
exact hostname, a `*.suffix` single-level wildcard, or absent (the
fallback / legacy slot). `(client, single TCP port)` listeners are
**mode-locked** for their lifetime: legacy plain-TCP or SNI dispatch is
fixed by the first activated rule. Online conversion is forbidden — the
operator removes the legacy rule first, then pushes SNI rules. This
guarantees the accept loop never has to flip its peek/no-peek behaviour
mid-flight, eliminating the need for a new `RuleGroup*` wire message
(D8 / FR-025). Rule deltas continue to flow one rule at a time over the
existing bidi gRPC stream.

A v0.7-style client-version capability gate (D9 / FR-024) refuses any
`sni_pattern.is_some()` push to a forward-client whose declared version
is below `0.9.0`, returning `422 sni_unsupported_by_client` before any
rule activates — so a v0.8 client cannot silently "downgrade" a SNI rule
into a plain TCP forward.

Per-rule SNI hit counters (exact / wildcard / fallback) ride on
`RuleStats` at proto field numbers 13/14/15 (11/12 are taken by v0.7
multi-target failover). Listener-level miss / parse-failure counters
have no rule attribution and ride on a new `SniListenerStats` message
attached to `StatsReport.sni_listener_stats = 3`. Both surface through
the server's existing `/metrics` endpoint with the v0.5+ label
convention (`client, rule, owner` for per-rule, `client, port` for
per-listener). Data-plane events flow only through structured tracing
+ Prometheus — they do **not** enter the SQLite operator audit ring,
which remains reserved for operator allow/deny actions (D13).

The SQLite store gains exactly one additive column (`rules.sni_pattern
TEXT`) and one helper index. There is **no SQL UNIQUE** on
`(listen_port, sni_pattern)` because the `rules` table has no
`client_name` column today; per-`(client, listen_port)` uniqueness is
enforced authoritatively by `ServerRuleStore` in memory, matching how
v0.7 already handles client-scoped uniqueness for rule shape. Schema
version handshake range shifts `[1,1] → [1,2]`.

All technical decisions were reached through three rounds of code review
captured in `design.md` (commits `a556570 → 0142e43`); this plan and
its Phase 0/1 artifacts treat that document as authoritative.

## Technical Context

**Language/Version**: Rust 1.88 (workspace MSRV pinned by `tonic`'s own MSRV — `Cargo.toml:rust-version`)

**Primary Dependencies**:
- New: **none** (zero new workspace deps — see R-006). `tokio::sync::watch` replaces ArcSwap; `Vec<RuleId>` replaces SmallVec; the ClientHello parser is hand-rolled (~150 LOC, no `tls-parser` / `rustls`-internals dependency) — R-001, R-002.
- Retained: `tokio`, `tokio-rustls`, `tonic 0.14` (control plane), `prost` (proto codegen), `axum` (operator HTTP), `clap`, `serde`/`serde_json`, `prometheus`, `rusqlite` + `r2d2` + `refinery` (v0.8 store), `tracing`.
- Re-touched (no deps added): `crates/forward-proto/proto/forward.proto` (additive fields), `crates/forward-server/src/store/migrations/V002__add_sni_pattern.sql` (new migration), `crates/forward-server/src/rules.rs` (overlap rules), `crates/forward-client/src/forwarder/` (new `sni/` module + `port_groups.rs`).

**Storage**: SQLite (v0.8 store unchanged). Migration V002 adds one nullable column to `rules` plus one partial index. Schema version range `[1,1] → [1,2]`. No new tables, no data backfill (additive only — R-003).

**Testing**: `cargo test` workspace-wide; tiered as today —
- **Unit**: in-source `#[cfg(test)] mod tests` for the ClientHello parser (real-pcap fixtures TLS 1.0/1.1/1.2/1.3, fragment reassembly, malformed inputs) and `SniRoutingTable::lookup` (priority + single-label wildcard guard + case insensitivity).
- **Contract**: per-crate `tests/` for the operator API field, the CLI `--sni` flag, the wire-compat round-trip (proto fields 11 / 13 / 14 / 15 / `SniListenerStats`), the capability gate (HTTP 422), and every row of the §Overlap matrix in `spec.md`.
- **Integration**: `crates/forward-client/tests/sni_route_e2e_*.rs` against rustls clients on real loopback sockets — exact / wildcard / fallback / timeout / not-TLS / byte-passthrough / hot reload / remove-by-rule-id.
- **Cross-crate end-to-end**: `crates/forward-server/tests/sni_metrics_surface.rs` asserts the server `/metrics` exposes the new collectors after a forward-client emits them.
- **Bench**: `crates/forward-client/benches/sni_route.rs` (criterion) for `SniRoutingTable::lookup` + connection-setup-latency vs. v0.7 baseline. Existing `crates/forward-client/benches/data_plane.rs` continues to gate the legacy-port byte-stability budget.

**Target Platform**: Linux x86_64 + aarch64 (primary); macOS for development. Windows out of scope.

**Project Type**: Cargo workspace, six crates (`forward-server`, `forward-client`, `forward-auth`, `forward-core`, `forward-proto`, `forward-e2e`). v0.9 changes are concentrated in `forward-client` (data plane) and `forward-server` (control plane). `forward-auth` and `forward-core` are not touched.

**Performance Goals**:
- SNI listener connection-setup latency p99 within +5 ms of v0.7 plain-TCP baseline (SC-003) — peek parse must stay below 100 µs at p99 so the budget is dominated by network time.
- `SniRoutingTable::lookup` p99 < 100 µs at 100 routes (SC-006); informally must scale to 10 000 routes without an order-of-magnitude regression.
- Legacy plain-TCP listeners byte-identical to v0.8 (SC-002, SC-004) — verified by the existing data-plane bench plus a tracing-target assertion that no SNI module is entered.
- Control-plane HTTP latency: no measurable regression (rule overlap check is constant-time per existing rule on the candidate's port).

**Constraints**:
- Zero new workspace deps (R-006 — Constitution II "single binary, minimal surface").
- No allocation in the per-byte forwarding path on legacy ports (Constitution II); SNI listeners are allowed up to one buffer growth + one Arc clone per connection — bounded.
- ClientHello peek hard caps: 3 s wall clock + 64 KiB cumulative bytes (FR-009 / FR-013).
- v0.8 wire byte-stable when `sni_pattern = None` and `SniListenerStats` empty (SC-002 / SC-004).
- Mode-locked listener invariant — runtime mode flip is forbidden by both server-side overlap rules and a client-side defensive `RuleStatus { reason = "mode_change_unsupported" }` (FR-014, FR-015).

**Scale/Scope**:
- Per listener: typical ≤ 100 SNI routes, hard limit ≤ 10 000 (matches the current rule cap; route-table rebuild is allocations + O(N) sort, runs in the control task not the accept loop).
- Per client: SNI listeners scale to the v0.7 ceiling on listeners (no new limit).
- Wire field cost: 1 new `Rule` field + 3 new `RuleStats` fields + 1 new `StatsReport` field + 1 new top-level message (`SniListenerStats`); all `optional` / default-zero so v0.8 binaries decode and re-emit byte-identically when no SNI is in play.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution version: `2.0.1` (TLS + bearer token; data-plane userspace; SQLite at `<data-dir>/state.db`).

| Principle | Status | Justification |
|---|---|---|
| **I. Security by Default** | ✅ | Auth seam unchanged. SNI routing operates strictly post-authentication on the control plane (rule push) and on the data plane is L4 only — never reads past the ClientHello, never derives a credential from the SNI value. The capability gate (FR-024) sits behind the existing operator-API auth middleware. The new wire fields are scalar values (`string`, `uint64`); no new crypto, no new TLS path. Server certificate / client bearer-token model untouched. |
| **II. Performance Is a Feature** | ✅ (with bench gate) | Legacy plain-TCP listener path is byte-stable and gated by `crates/forward-client/benches/data_plane.rs` (Constitution II hot-path budget). SNI listener path is allowed up to +5 ms p99 setup latency relative to v0.7 (SC-003) and ≤ 100 µs p99 lookup (SC-006); both gated by `crates/forward-client/benches/sni_route.rs`. Zero new workspace deps (R-006) keeps the binary footprint flat. The accept loop never blocks on rebuild — the routing table is built in the control task and swapped via `tokio::sync::watch::send_replace` (D7). |
| **III. Test-First Discipline** | ✅ | TDD applies to every new path: parser unit tests + lookup unit tests + the §Overlap matrix contract test + e2e integration tests + capability-gate contract test + wire-compat round-trip + the negative wire-compat assertion that fields 11/12 of `RuleStats` are NOT touched (HIGH-1 from round-3 review). Real loopback sockets in integration tests; no mocks for the TLS path. |
| **IV. Observability & Operability** | ✅ | Three new per-rule counters and two new per-listener counters surfaced through the existing `/metrics` endpoint with the v0.5+ `client, rule, owner, result` and `client, port` label conventions (FR-034). Listener-level metrics use `port` because miss / parse-failure have no honest rule attribution (HIGH-2 from round-3). Data-plane events stay in `tracing` only — they do NOT pollute the SQLite operator audit ring (D13 / FR-035). Graceful reload preserved: routing table swap is non-blocking; in-flight connections keep their `Arc<SniRoutingTable>` snapshot. Drain on listener teardown reuses the existing v0.7 cancellation-token machinery. |
| **V. Multi-Tenant Isolation** | ✅ | SNI uniqueness is enforced per `(client_name, listen_port)` in `ServerRuleStore` (D10 / FR-021). Two clients can each own their own listener on `:443 + api.example.com` without seeing each other's traffic, error messages, or counter cardinality. RBAC `grants` are unchanged — the new field is part of an existing rule resource that is already grant-scoped. Listener-level metrics carry `client` so tenant attribution is preserved. |

**Constitution gate (initial): PASS.** No Complexity Tracking entries.

**Constitution gate (post-Phase 1, after `research.md`, `data-model.md`, `contracts/*`, `quickstart.md` written): PASS.** No new violations surfaced from the Phase 1 design:

- Principle I: `contracts/operator-api.md` and `contracts/wire.md` confirm auth envelope unchanged; `contracts/wire.md` enumerates exactly which proto field numbers are introduced (Rule.11, RuleStats.13/14/15, StatsReport.3, plus `SniListenerStats` as a new top-level message) and asserts fields 11/12 of `RuleStats` are NOT touched — pinned by `crates/forward-proto/tests/sni_wire_compat.rs`.
- Principle II: `contracts/wire.md` documents the byte-stable invariant when `sni_pattern = None` and `sni_listener_stats` is empty; `data-model.md` shows the single additive SQL column with no implicit default change.
- Principle III: every contract surface in `contracts/*` carries an enumerated test plan (parser fixtures, overlap matrix, capability gate, wire round-trip, e2e exact/wildcard/fallback/timeout/not-TLS/passthrough/hot-reload/remove-by-rule-id, metrics surface).
- Principle IV: `contracts/operator-api.md` lists all five new metric series (with labels) and the five new tracing event names; the existing `forward_audit_buffer_drops_total` is NOT reused — these counters are diagnostic, not audit.
- Principle V: `data-model.md` documents that `(client_name, listen_port, sni_pattern)` uniqueness is in-memory in `ServerRuleStore`, NOT a SQL constraint, with the rationale that SQL would either be too loose (global per port — would break two-client SNI sharing) or require a schema-impacting `client_name` column on `rules` that is out of scope for v0.9.

## Project Structure

### Documentation (this feature)

```text
specs/009-tls-sni-routing/
├── plan.md                                 # this file
├── spec.md                                 # /speckit-specify output
├── design.md                               # brainstorm artifact (committed before /speckit-specify)
├── research.md                             # Phase 0 — R-001..R-XX decisions
├── data-model.md                           # Phase 1 — entities, schema delta, in-memory routing table
├── quickstart.md                           # Phase 1 — operator walkthrough (push two SNI rules, verify with openssl s_client)
├── contracts/
│   ├── operator-api.md                     # POST /v1/rules + GET /v1/rules + capability-gate response shapes
│   ├── wire.md                             # proto field numbers 11 / 13/14/15 / SniListenerStats / StatsReport.3 + the byte-stable invariant
│   └── cli.md                              # forward-server push-rule --sni <pattern>
├── checklists/
│   └── requirements.md                     # /speckit-specify quality checklist
└── tasks.md                                # /speckit-tasks output (NOT created here)
```

### Source Code (repository root)

```text
crates/
├── forward-proto/
│   ├── proto/
│   │   └── forward.proto                   # ★ adds Rule.sni_pattern = 11; RuleStats.sni_route_*_total = 13/14/15;
│   │                                        #   new SniListenerStats { listen_port, sni_route_miss_total,
│   │                                        #   client_hello_parse_failures_total }; StatsReport.sni_listener_stats = 3
│   └── tests/
│       └── sni_wire_compat.rs              # NEW — round-trip + negative assertion that fields 11/12 of RuleStats are untouched
│
├── forward-server/
│   ├── src/
│   │   ├── rules.rs                        # ★ overlap check rewritten per §Overlap matrix; by_client_listen_start
│   │   │                                    #   becomes BTreeMap<u16, Vec<RuleId>>; new conflict codes
│   │   ├── store/migrations/
│   │   │   └── V002__add_sni_pattern.sql   # NEW — additive column + partial helper index
│   │   ├── operator/
│   │   │   ├── http.rs                     # ★ adds version_at_least_0_9; capability gate before push
│   │   │   └── audit.rs                    # unchanged (data-plane SNI events deliberately bypass this)
│   │   ├── grpc/service.rs                 # ★ extends StatsReport fold to ingest sni_route_*_total + sni_listener_stats
│   │   ├── metrics.rs                      # ★ registers forward_tls_sni_route_total{client,rule,owner,result},
│   │   │                                    #   forward_tls_sni_listener_miss_total{client,port},
│   │   │                                    #   forward_tls_client_hello_parse_failures_total{client,port},
│   │   │                                    #   forward_tls_sni_routes_active (gauge)
│   │   └── main.rs                         # ★ adds `push-rule --sni <pattern>` flag with same validation as the API
│   ├── tests/
│   │   ├── sni_rule_validation.rs          # NEW — UDP/range + sni → 400; malformed → 400
│   │   ├── sni_capability_gate.rs          # NEW — push w/ sni to v0.8 client → 422
│   │   ├── sni_overlap_matrix.rs           # NEW — every row of the §Overlap matrix
│   │   ├── sni_legacy_to_sni_unsupported.rs # NEW — active legacy + SNI candidate → 409
│   │   └── sni_metrics_surface.rs          # NEW — /metrics shows forward_tls_sni_*
│   └── benches/
│       └── operator_api.rs                 # existing; assert no regression on the rule-push path
│
├── forward-client/
│   ├── src/
│   │   ├── forwarder/
│   │   │   ├── mod.rs                      # ★ ClientRule.sni_pattern: Option<String>
│   │   │   ├── sni/
│   │   │   │   ├── mod.rs                  # NEW — pub use
│   │   │   │   ├── client_hello.rs         # NEW — hand-rolled parser, fragment-reassembly aware
│   │   │   │   ├── route_table.rs          # NEW — SniRoutingTable + lookup
│   │   │   │   ├── peek.rs                 # NEW — async ClientHello peek (3 s / 64 KiB)
│   │   │   │   └── listener.rs             # NEW — SniListener (mode = SNI), owns watch::Sender
│   │   │   ├── proxy.rs                    # ★ extended to accept a preread byte buffer for upstream replay
│   │   │   └── stats.rs                    # ★ adds three per-rule SNI counters; aggregates SniListenerStats
│   │   ├── port_groups.rs                  # NEW — PortGroupManager; mode-locked listener lifetime; rule_id reverse index
│   │   └── control.rs                      # ★ routes RuleUpdate(PUSH|REMOVE) into PortGroupManager
│   ├── tests/
│   │   ├── fixtures/tls/                   # real captured ClientHello bytes — TLS 1.0..1.3 + fragmented
│   │   ├── sni_route_e2e_exact.rs          # NEW — two SNI rules → distinct upstreams
│   │   ├── sni_route_e2e_wildcard.rs       # NEW — *.x matches y.x, not x, not a.b.x
│   │   ├── sni_route_fallback.rs           # NEW — no-SNI client + fallback rule; without fallback → reset
│   │   ├── sni_route_timeout.rs            # NEW — 3 s no-bytes → reset + tls.client_hello_timeout
│   │   ├── sni_route_not_tls.rs            # NEW — plain HTTP → reset + tls.parse_failed
│   │   ├── sni_byte_passthrough.rs         # NEW — sha256(upstream) == sha256(client)
│   │   ├── sni_hot_reload.rs               # NEW — in-flight conn unaffected on rule add/remove
│   │   ├── sni_remove_by_rule_id.rs        # NEW — REMOVE finds the right group via reverse index
│   │   ├── sni_stats_emitted.rs            # NEW — RuleStats fields 13/14/15 + StatsReport.sni_listener_stats populated
│   │   └── legacy_plain_tcp_unchanged.rs   # NEW — non-SNI port byte-identical to v0.7 (no peek code path entered)
│   └── benches/
│       └── sni_route.rs                    # NEW — lookup ns/op + setup-latency vs v0.7 baseline
│
├── forward-auth/                           # NOT TOUCHED
├── forward-core/                           # NOT TOUCHED
└── forward-e2e/                            # NOT TOUCHED (per-crate tests cover the surface)
```

**Structure Decision**: Six-crate Cargo workspace inherited from v0.7+. v0.9
introduces no new crate. The data-plane work is concentrated under
`crates/forward-client/src/forwarder/sni/` and `crates/forward-client/src/port_groups.rs`;
the control-plane work threads through `crates/forward-server/src/rules.rs`,
`store/migrations/`, `operator/http.rs`, `grpc/service.rs`, `metrics.rs`, and
`main.rs`. The proto changes (one Rule field, three RuleStats fields, one new
top-level message, one StatsReport field) live in
`crates/forward-proto/proto/forward.proto`.

## Complexity Tracking

> No Constitution Check violations. Table left empty intentionally.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|---|---|---|
| _none_ | _n/a_ | _n/a_ |
