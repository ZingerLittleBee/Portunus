# Implementation Plan: Domain-name forwarding targets

**Branch**: `003-domain-name-forward` | **Date**: 2026-05-07 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/003-domain-name-forward/spec.md`

## Summary

Allow a forwarding rule's `target_host` to be a DNS name (RFC 1123
strict hostname) in addition to today's IP literal. The client gains a
small **resolver layer** sitting between the proxy hot path and the
existing `TcpStream::connect`: on the first connection through a
DNS-target rule it issues one resolver query, caches the answer for
the resolver-reported TTL clamped to 5 s..5 min, single-flight
coalesces concurrent in-flight queries, applies the rule's
address-family preference (IPv4 default, per-rule `prefer_ipv6`
opt-in), tries resolved addresses in order on dial failure
(happy-eyeballs-style), and on resolution failure fails the triggering
end-user connection while leaving the rule **Active** so it
auto-recovers when DNS comes back.

The wire / persistence / HTTP / CLI surfaces evolve **additively**:

- `Rule.target_host` is already a `string`; today it must be an IP, after
  this change it can be either an IP or a DNS name. **No new wire field
  is required** for the basic feature — every existing v0.2.0 client and
  server can already round-trip a hostname-bearing rule byte-for-byte.
- One new optional field `Rule.prefer_ipv6 = 8` carries the address-family
  opt-in. Absent (or `false`) preserves today's IPv4-first behavior.
- One new metric `forward_rule_dns_failures_total{client,rule}` joins the
  existing per-rule counter family — same cardinality budget as v0.2.0
  (one row per rule, never per attempt or per resolved address).

The forwarder hot path (`accept → copy_bidirectional`) is unchanged for
both IP-target rules (which skip the resolver entirely) and warm-cache
DNS-target rules (which add one hashmap lookup + atomic on the cache
path). Rule push, persistence, and gRPC remain blocking-free of DNS
state — resolution is **lazy, per-connection, client-local**.

## Technical Context

**Language/Version**: Rust 1.88 (constitution-pinned MSRV via `tonic`).
**Primary Dependencies**: existing — `tokio`, `tonic` 0.14, `prost`,
                          `rustls`, `prometheus`, `axum`. **New**:
                          `hickory-resolver` (formerly `trust-dns-resolver`),
                          the de-facto pure-Rust async DNS client. Reads
                          `/etc/resolv.conf` natively, exposes record
                          TTLs (which `tokio::net::lookup_host` does not),
                          plays nicely with Tokio. See `research.md` for
                          alternatives evaluated.
**Storage**: existing rules persistence layer (`forward-server`'s
             rules.json equivalent); new optional `prefer_ipv6` field
             defaults to absent so v0.2.0 rule files load unmodified
             (FR-009 / FR-010).
**Testing**: `cargo test` per crate (unit + integration); `forward-e2e`
             integration crate exercises server + client + real
             sockets; new resolver layer has its own unit tests using
             a mock `Resolve` trait so DNS behavior is testable
             without depending on a live resolver.
**Target Platform**: Linux primary (musl static binary), macOS for
                     development. No Windows.
**Project Type**: Multi-crate Cargo workspace (unchanged): `forward-core`,
                  `forward-proto`, `forward-auth`, `forward-server`,
                  `forward-client`, `forward-e2e`. No frontend.
**Performance Goals**:
  - **Cache hit** path adds **no observable latency** vs the v0.2.0
    IP-literal path (SC-004). Must show this in a criterion bench
    before/after.
  - **Resolver QPS** under steady-state traffic stays at or below
    **one query per rule per cache window** (SC-005) — i.e. the
    resolver is not a per-connection dependency.
  - The forwarder hot path (per-byte `copy_bidirectional`) MUST
    NOT regress vs v0.2.0 (Constitution II).
**Constraints**:
  - DNS-name validation at rule push: strict RFC 1123 hostname
    (FR-001), enforced server-side at the operator API and CLI.
  - Cache: floor 5 s, ceiling 5 min (FR-003), stale-while-error
    grace 30 s (FR-005).
  - Resolution timeout: 3 s per attempt (Assumptions). Fits inside
    the 3 s SC-003 user-visible failure budget.
  - Single-flight: concurrent connection attempts to one DNS name
    coalesce to one in-flight resolver query (FR-012).
  - Prometheus cardinality: one row per rule for the new
    `forward_rule_dns_failures_total` (SC-006).
  - DNS resolution MUST NOT block the accept loop (FR-012) or any
    unrelated rule's traffic.
**Scale/Scope**: A client running the worst case in scope (100 rules,
                 mixed IP and DNS targets) issues ≤ 100 resolver
                 queries per cache window (SC-005). With ceiling
                 5 min that's ≤ 0.33 qps to the OS resolver — well
                 under any sane resolver's per-client rate.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Pass? | Notes |
|---|---|---|
| I. Security by Default (TLS + bearer token, no plaintext) | ✅ | No control-plane transport changes. Hostnames travel in the existing `Rule.target_host` field over the same TLS+bearer-token channel; new optional `prefer_ipv6` field is informational. **No new credentials, no new auth surface.** The DNS resolver is a *client-local* concern — no DNS traffic crosses the control plane. The trust boundary explicitly remains at the OS resolver (spec § Assumptions); DNS spoofing of forwarding targets is the same threat model the operator already accepts when using IP literals on the same network. Tokens, certs, hostnames, and resolved IPs are never logged in the same record. |
| II. Performance Is a Feature | ✅ | Forwarder hot path unchanged: per-byte `copy_bidirectional` does not change. Per-connection setup adds (a) one cache hashmap lookup + atomic on cache hit, (b) one resolver call on cache miss (amortized to ≤ 1/cache-window — SC-005). IP-literal rules skip the resolver entirely (zero cost). Will ship two criterion benches under `forward-client/benches/dns_resolver.rs`: cache-hit latency (must be ≪ network connect cost) and miss+single-flight coalescing (must show N concurrent attempts trigger 1 query). The existing `data_plane.rs` bench provides the regression gate for the proxy hot path itself. |
| III. Test-First Discipline | ✅ | (a) Wire byte-compat: a `Rule` carrying only the v0.2.0 fields (no `prefer_ipv6`) MUST encode/decode byte-identical to v0.2.0 — captured by `forward-proto/tests/dns_wire_compat.rs` (new). (b) Resolver layer: unit tests against a `MockResolver` driven by the test (returns canned answers, drives TTL forward via injected clock) — covers cache hit/miss/expire/stale-grace/single-flight/multi-A failover/family-preference. (c) End-to-end: `forward-e2e/tests/dns_smoke.rs` (new) wires a deliberately-failing DNS name through a real client and asserts the rule stays Active, the connection fails fast, and the metric increments. (d) Server validation: hostname syntax tests in `forward-core` covering RFC 1123 happy/sad cases. |
| IV. Observability & Operability | ✅ | One new metric `forward_rule_dns_failures_total{client,rule}` follows the existing `forward_rule_bytes_in_total{client,rule}` shape — same labels, same cardinality (one row per rule, SC-006). Audit logs gain a `rule.dns_resolved` event (one per successful resolution, includes rule_id + chosen address + TTL applied) and a `rule.dns_failed` event (one per failed connect-attempt-due-to-DNS, includes rule_id + reason classifier). Hostname strings are NOT redacted (they are operator-supplied, not user secrets). Resolved addresses are NOT logged at INFO unless DEBUG; we don't want a hot loop logging every resolution. |
| V. Multi-Tenant Isolation | ✅ | Hostnames are a per-rule property, owned by one `(client_name, rule_id)` pair just like target_port. Resolution caches are scoped to the `forward-client` process — no cross-tenant cache, no shared resolver-result table, no shared dial state. Per-tenant policy (when it lands in a future spec) gets enforced at push time on the hostname's resolved family/range or on the literal hostname text — same enforcement seam as today's port-range check. |

**Gate result**: PASS. No constitutional violations; nothing to track in
the Complexity Tracking table.

**Post-Phase-1 re-check**: Re-evaluated after `data-model.md`,
`contracts/forward.proto`, `contracts/operator-api.md`, and
`contracts/persistence.md` landed. No new constraints surfaced —
`prefer_ipv6` is one optional bool on the existing `Rule` (R-007),
the new metric stays one-row-per-rule (R-008), the resolver layer
lives client-side only (R-006). PASS, gate clear for `/speckit-tasks`.

## Project Structure

### Documentation (this feature)

```text
specs/003-domain-name-forward/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/
│   ├── forward.proto    # Phase 1: additive proto diff (overlay vs v0.2.0)
│   ├── operator-api.md  # Phase 1: HTTP + CLI surface deltas
│   └── persistence.md   # Phase 1: rules persistence schema deltas
├── checklists/
│   └── requirements.md  # Already produced by /speckit-specify
└── tasks.md             # /speckit-tasks output (not created here)
```

### Source Code (repository root)

```text
proto/
└── forward.proto                         # add optional bool prefer_ipv6 = 8 on Rule

crates/
├── forward-core/src/
│   ├── hostname.rs                       # NEW: RFC 1123 strict-hostname validator + Target enum (Ip|Dns)
│   └── lib.rs                            # re-export
├── forward-proto/                        # codegen consumes the additive .proto
│   └── tests/dns_wire_compat.rs          # NEW: a v0.2.0-shaped Rule encodes byte-identically
├── forward-server/src/
│   ├── rules.rs                          # extend Rule deserialization to accept hostname; reject malformed at push
│   ├── operator/
│   │   ├── cli.rs                        # accept hostname + --prefer-ipv6 flag on push-rule
│   │   ├── http.rs                       # accept + return optional prefer_ipv6 field
│   │   └── rule_cli.rs                   # parse target spec accepting hostname syntax
│   └── metrics.rs                        # NEW counter: forward_rule_dns_failures_total
├── forward-client/src/
│   ├── resolver/                         # NEW module
│   │   ├── mod.rs                        # Resolver trait + LiveResolver (hickory-backed)
│   │   ├── cache.rs                      # TTL-clamped cache + single-flight coalescing
│   │   ├── target.rs                     # Target = IpAddr | Hostname; per-rule preference
│   │   └── tests/                        # unit tests against MockResolver + injected Clock
│   ├── forwarder/proxy.rs                # call resolver.connect_target(rule_target) instead of TcpStream::connect(host:port)
│   ├── forwarder/mod.rs                  # ClientRule grows resolved-target handle; dns failures bump per-rule counter
│   └── benches/dns_resolver.rs           # NEW: cache-hit + coalescing benches
├── forward-e2e/tests/
│   └── dns_smoke.rs                      # NEW: real client, hostname target, US1+US2+US3+US4 acceptance
└── forward-server/src/serve.rs           # wire new metric into the registry (one-line)
```

**Structure Decision**: same multi-crate workspace as v0.1.0 / v0.2.0;
the only structural addition is the `forward-client/src/resolver/`
module — kept inside the client crate because resolution is purely a
client-side concern and never crosses the control plane (Constitution
Principle I rationale).

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

(none)
