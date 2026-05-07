# Phase 0 Research: Domain-name forwarding targets

Resolves the technology choices and design patterns the plan leaves
open for the resolver layer. Each section below states a **Decision**,
the **Rationale** behind it, and **Alternatives considered** so future
amendments can re-litigate from a known starting point.

---

## R-001 — Async DNS client library

**Decision**: Use `hickory-resolver` (formerly `trust-dns-resolver`)
with the `tokio-runtime` and `system-config` features.

**Rationale**:

- Need TTL out of the resolver answer (FR-003 clamps `min(TTL, ceiling)`,
  caches accordingly). `tokio::net::lookup_host` and the libc
  `getaddrinfo` it wraps **do not expose TTL** — the caller only sees
  the resolved addresses. Building TTL-aware caching on top of
  `getaddrinfo` would either require ignoring TTL (= violates FR-003)
  or shelling out to a second resolver tool.
- `hickory-resolver` reads `/etc/resolv.conf` (and the macOS / Windows
  equivalents via `system-config` feature), parses it the same way
  `getaddrinfo` does, and returns the resolver-reported TTL on every
  record. This satisfies FR-003 without giving up the OS resolver
  source (Assumptions: "client uses the host operating system's
  configured resolver").
- Pure Rust, no `libc` dependency for the resolution path itself —
  static musl builds keep working unchanged.
- Active maintenance (renamed from `trust-dns-resolver` to
  `hickory-resolver` in 2024; new releases tracked).
- License (MIT/Apache-2.0) compatible with the workspace.

**Alternatives considered**:

- **`tokio::net::lookup_host`** — rejected: no TTL in the returned
  iterator; would force a second non-resolver layer to keep TTL state.
- **`async-std-resolver`** — rejected: pulls `async-std`, the workspace
  is exclusively Tokio (Constitution: "Tokio. Custom executors require
  constitutional amendment").
- **`domain` crate (NLnet Labs)** — rejected: lower-level (you assemble
  your own resolver from primitives); useful if we needed DNSSEC or
  custom protocols, but spec § Assumptions explicitly puts DNSSEC out
  of scope.
- **Spawning `dig`/`getent` subprocesses** — rejected on operability
  grounds (per-conn fork cost, error-handling spaghetti, Linux-only
  for `getent`).

**Cargo features to enable**: `tokio-runtime`, `system-config`. Avoid
`dns-over-https` and `dns-over-tls` — spec § OOS explicitly defers
those.

---

## R-002 — Caching + single-flight coalescing pattern

**Decision**: One process-wide `Cache = Arc<Mutex<HashMap<Hostname,
CacheEntry>>>` keyed by the *hostname text only* (not by hostname +
port — the port travels separately); `CacheEntry::Pending` holds an
`Arc<Notify>` so concurrent waiters block on a single in-flight
resolver call (single-flight). On query completion the entry
transitions to `Resolved { addrs, expiry }` (or `Failed { until }`
during the stale-while-error window), and notified waiters race to
read the answer.

**Rationale**:

- FR-012 mandates that concurrent connection attempts to one DNS-name
  rule MUST coalesce to one in-flight resolver query (no thundering
  herd). The `Pending → Notify` pattern is the standard async
  single-flight idiom (cf. Go's `singleflight.Group`, Tokio's
  `OnceCell`).
- Keying by hostname (not by `(hostname, port)` or by `RuleId`) means
  two rules pointing at the same hostname share one cache entry —
  matches operator intuition ("if I push two rules to
  api.example.com, I expect one DNS query, not two") and keeps
  resolver QPS proportional to *unique hostnames in flight*, not
  rule count. SC-005 measures rule-count, so this is conservative
  in our favor.
- Single `Mutex` (not `RwLock`) is the right primitive: critical
  section is microseconds (look up, check expiry, return clone) and
  the `Notify` does the actual blocking. Read contention isn't the
  bottleneck.
- FR-005's stale-while-error grace fits naturally: when fresh
  resolution fails, we transition to `Resolved + StaleAfterFailedRefresh
  { stale_addrs, fail_grace_until }` and serve the stale addrs until
  the grace expires, then transition to `Failed { until: now + small
  retry backoff }`.

**Alternatives considered**:

- **`moka` crate (TTL cache)** — rejected: doesn't compose with
  single-flight semantics out of the box; we'd still need to layer
  `Notify` on top, defeating the simplification.
- **Per-rule cache instead of process-wide** — rejected: violates the
  intuition above (two rules to same name = two queries) and inflates
  resolver QPS unnecessarily.
- **`tokio::sync::OnceCell` per name** — rejected: `OnceCell` doesn't
  re-fire when the value expires; we'd need to wrap it in another
  cache layer to handle TTL, ending up exactly where Decision lands.

---

## R-003 — Address-family preference + multi-A failover

**Decision**: After the resolver returns the answer set, the resolver
layer (a) splits addresses into A-list and AAAA-list, (b) places the
preferred-family list first per the rule's `prefer_ipv6` flag (default:
A-first), (c) keeps the resolver-returned order within each family,
(d) hands the connect-loop the concatenated list to dial in order with
a 3 s per-attempt timeout (Assumptions). On per-attempt connect failure
(timeout, ECONNREFUSED, EHOSTUNREACH) the loop advances to the next
address; only when the list is exhausted does the connection report
`dns_resolution_failed` and bump the per-rule failure counter.

**Rationale**:

- Q3 of `/speckit-clarify` settled on "try in order on dial failure"
  (FR-006 + spec § Clarifications). This is the simplest correct
  implementation: linear walk, no parallel-dial budget, no
  RFC-8305 happy-eyeballs interleaving.
- The 3 s per-attempt timeout fits inside SC-003's 3 s user-facing
  failure budget on the typical case (one address tried). In the
  worst case (5 addresses returned, all timing out), the user waits
  ~15 s — but spec § Assumptions and SC-003 only commit to 3 s for
  the common-case fully-broken scenario (NXDOMAIN, where the
  resolver itself answers fast). When the *resolver* answers but
  every *upstream IP* is dead, we honor FR-006's "try them all"
  promise even if it exceeds SC-003. We document this trade-off
  explicitly in the contract docs so operators don't expect 3 s for
  5-address rotation outages.
- `prefer_ipv6 = false` (default) gives v4-only addresses if the
  hostname resolves to both A and AAAA — matches spec § Clarifications
  Q1 and the most common dual-stack-but-not-clean-v6-transit
  deployment.

**Alternatives considered**:

- **Parallel dial (RFC 8305 happy-eyeballs)** — rejected: Q3
  explicitly chose serial. Parallel dial halves the worst case but
  doubles the connect-side load on the upstream and complicates
  cancellation; not worth it for a forwarder where the upstream is
  expected to be the operator's own service.
- **First-only fail-fast** — rejected: Q3 ruled this out; rolling
  upstream restarts would cause user-visible failures.
- **OS-level happy-eyeballs (`tokio::net::TcpSocket::connect` with
  `SO_REUSEADDR`)** — rejected: not how Tokio's `TcpStream::connect`
  works, and we already have an explicit address list from the
  resolver — no need to delegate.

---

## R-004 — Cache eviction

**Decision**: No proactive eviction. Cache entries live until their
hostname is no longer referenced by any active rule, OR a fresh
connection through any rule re-fires resolution past expiry. On rule
removal, the cache entry is **not** explicitly evicted — it ages out
naturally on next access. The cache size is bounded by the count of
unique hostnames across all active rules on the client, which has the
same operator-intuitive bound as today's rule count.

**Rationale**:

- The only correctness-critical eviction signal is TTL expiry, which
  the cache enforces on `get_or_resolve` (lazy). No background sweeper
  needed.
- Cache size scales with *unique hostnames in active rules* — a
  100-rule client at the SC-005 scale has at most 100 entries, each
  ≤ a few dozen bytes (hostname text + 1–4 SocketAddr + timestamps).
  Total cache footprint: kilobytes, not megabytes. No memory
  pressure.
- Avoiding active eviction also means rule removal doesn't have to
  cross-check resolver state — keeps the rule store and the resolver
  store loosely coupled.

**Alternatives considered**:

- **LRU with size cap** — rejected: solves a problem (unbounded
  growth) the bound on rule count already prevents.
- **Active TTL sweeper task** — rejected: extra background task to
  watch + reap, while lazy eviction on read achieves the same with
  zero overhead when the rule isn't being driven.

---

## R-005 — Hostname validator (RFC 1123 strict)

**Decision**: A pure-function validator in `forward-core/src/hostname.rs`,
zero dependencies, returning `Result<Hostname, HostnameError>`. The
validator enforces:

- Total length 1..=253 octets (excluding optional trailing dot).
- Each `.`-separated label 1..=63 octets.
- Each label matches `[A-Za-z0-9-]+` (i.e. ASCII alphanumeric and
  hyphen only).
- No label starts or ends with `-`.
- Trailing dot allowed (FQDN form) but normalized away on store.
- The all-numeric form (e.g. `127`, `192168001001`) is **rejected**
  to disambiguate from IP-literal classification; if the input parses
  as an IPv4 or bracketed IPv6 literal, it's classified as IP and
  never reaches the hostname validator.

**Rationale**:

- Q2 of `/speckit-clarify` pinned strict RFC 1123 (rejecting
  underscores, IDN unicode, SRV-style names, whitespace, length
  overruns). FR-001 in the spec is the contract.
- Pure function = trivially unit-testable, no async, no I/O.
- No `regex` crate dependency for this — the rules are simple
  enough to express as iterator chains, saving a transitive dep.

**Alternatives considered**:

- **`url` crate's host parser** — rejected: it accepts more than RFC
  1123 (IDN, percent-encoded, etc.); we want a stricter contract
  than URL-parsing tolerance.
- **`hickory-proto::rr::domain::Name::from_ascii`** — rejected:
  requires the resolver dep at the server (which doesn't need it).
  Hostname validation lives in `forward-core` so server-side push
  validation has no resolver dep.

---

## R-006 — Where the resolver layer plugs into the proxy

**Decision**: Define a trait `Resolver` in
`forward-client/src/resolver/mod.rs`:

```rust
#[async_trait]
trait Resolver: Send + Sync + 'static {
    async fn connect_target(
        &self,
        target: &Target,                  // IpAddr | Hostname (already classified)
        port: u16,
        prefer_ipv6: bool,
    ) -> Result<TcpStream, ResolverError>;
}
```

The proxy (`forwarder/proxy.rs`) calls `self.resolver.connect_target(...)`
instead of `TcpStream::connect(format!("{host}:{port}"))`. For IP-literal
targets, the trait short-circuits straight to `TcpStream::connect((ip,
port))` with **zero** cache or resolver involvement (Constitution II:
"IP-literal rules MUST NOT regress vs v0.2.0").

The forwarder constructs one `LiveResolver` per `forward-client` process
(lifetime = client), not per rule — so the cache is shared across all
rules on the client (R-002).

**Rationale**:

- One trait, two implementations (`LiveResolver` for production,
  `MockResolver` for tests) cleanly separates the resolver-layer
  unit tests from real DNS dependence.
- Putting the connect call *inside* the trait (instead of returning
  `Vec<SocketAddr>` and connecting outside) lets the resolver itself
  own the multi-A fallback loop, the per-attempt timeout, and the
  stats-bumping on failure — keeping the proxy's connect path
  honest and free of resolver-shaped logic.
- The `Target` enum (already classified by hostname syntax in
  `forward-core`) means the resolver doesn't re-parse the input on
  every call — classification happens once at rule-store load.

**Alternatives considered**:

- **Function-style `resolve(name) -> Vec<SocketAddr>`** — rejected:
  splits the multi-A failover loop between resolver and proxy,
  makes the dns_failures_total counter awkward to bump.
- **Resolver per rule** — rejected: violates R-002's "share cache
  across rules with the same hostname".

---

## R-007 — Wire compatibility for `prefer_ipv6`

**Decision**: Add `optional bool prefer_ipv6 = 8` to the existing
`Rule` proto message. Field number 8 follows v0.2.0's used range
(2,3,4,6,7 — see `proto/forward.proto`). proto3 `optional` means
absent decodes as `None`, which the server treats as the default
(IPv4-first). A v0.2.0-shaped `Rule` (no field 8 on the wire) MUST
encode/decode byte-identically — verified by a contract test in
`forward-proto/tests/dns_wire_compat.rs`.

**Rationale**:

- Single-bit per-rule preference (Q1 outcome) maps cleanly to one
  optional bool. No new message, no new oneof.
- proto3 `optional` (Rust: `Option<bool>`) is the standard
  forward/backward-compat shape; v0.2.0 servers ignore the unknown
  field per proto3's "unknown fields preserved" semantics.
- Persistence (rules.json equivalent) gains the same optional field
  with serde's `#[serde(default, skip_serializing_if = "Option::is_none")]`
  so v0.2.0-on-disk rules load unchanged.

**Alternatives considered**:

- **Repeated `address_family_priority` enum** — rejected: more
  flexible than needed, two-element list (A, AAAA) is the only
  meaningful content. YAGNI.
- **New message `Rule.dns_options`** — rejected: over-engineered
  for one bool.

---

## R-008 — Metric naming and label cardinality

**Decision**: New collector
`forward_rule_dns_failures_total{client, rule}`, type `IntCounterVec`,
joining the existing per-rule family (`forward_rule_bytes_in_total`,
etc.). Same label set, same cardinality contract: one row per
`(client, rule)` pair. **No `reason` label** — the failure-mode
classifier (NXDOMAIN, SERVFAIL, timeout, …) is captured in the
structured log event `rule.dns_failed.reason` for diagnosis but not
exploded into Prometheus rows.

**Rationale**:

- SC-006 commits to "one row per rule that has ever attempted
  resolution"; adding `reason` would multiply rows by failure-mode
  count. Logs are the right place for that detail (Constitution IV
  on operability + the v0.2.0 budget pattern).
- Matching the existing metric family's labels means existing alert
  templates and dashboards can adopt the new counter with one-line
  diff (operator ergonomics).

**Alternatives considered**:

- **`forward_rule_dns_failures_by_reason{client, rule, reason}`** —
  rejected as above.
- **Histogram of resolution latency** — deferred. The ≤ 1 query per
  cache window load (SC-005) means resolver-latency histograms would
  be sparse and noisy at small volumes. Revisit if a deployment
  actually reports a need.

---

## R-009 — Test scaffolding for DNS

**Decision**: Three layers of test plant.

1. **Pure-Rust mock resolver** (`MockResolver`) implementing the
   `Resolver` trait, driven by the test: returns canned answers, can
   be told to fail, advances time via an injected `Clock`. Drives
   all unit tests for the resolver/cache/single-flight code.
2. **Hosts-file override** for `forward-e2e` integration tests:
   write a temp `/etc/hosts`-style mapping during test setup
   (`HostnameTo::Local("dual.test", "127.0.0.1")`) and either point
   `hickory-resolver` at it via `--hosts-file` config, or use a
   localhost-pinned mini-resolver (also via hickory) bound to a
   loopback port and pointed at by a custom `ResolverConfig`. This
   keeps the e2e test hermetic — no actual DNS over the network.
3. **One smoke test against a real, well-known resolver target** in
   the SC-001 verification recipe (manual or CI-on-demand) — e.g.
   `cloudflare.com` — to prove the production-shape configuration
   actually works end-to-end. Not part of `cargo test`.

**Rationale**:

- Layer 1 = fast, deterministic, no DNS dep — covers the bulk of
  the logic.
- Layer 2 = real `hickory-resolver` exercising the production code
  path, but with a controlled hostname space — verifies the
  trait-impl wiring without depending on the public DNS.
- Layer 3 = catches "we wired the OS resolver wrong" — a class of
  bug invisible to the first two layers.

**Alternatives considered**:

- **CI-side dnsmasq sidecar** — rejected: more infrastructure than
  Layer 2 needs; hickory's per-test config plus a temp hosts file
  is enough.
- **Skip mock resolver, only e2e** — rejected: would couple unit
  test runtime to network I/O; brittle.
