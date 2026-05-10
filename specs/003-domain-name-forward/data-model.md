# Phase 1 Data Model: Domain-name forwarding targets

Adds three concepts on top of the v0.2.0 model: a richer **Target**
shape (IP-or-hostname classification), a **per-rule preference flag**
for address family, and a **client-local resolution cache**. Field
numbers / on-disk shapes are documented in
`contracts/portunus.proto` and `contracts/persistence.md` — this file
captures the in-memory entities, their invariants, and how they
relate to the v0.2.0 entities.

---

## Entity: `Hostname` (new, in `portunus-core`)

Validated wrapper around an RFC 1123 strict hostname.

| Field   | Type     | Notes                                                                  |
|---------|----------|------------------------------------------------------------------------|
| (inner) | `String` | The validated hostname text, lowercase-normalized, trailing-dot stripped. |

**Validation rules (FR-001, R-005)**:

- Total length 1..=253 octets after stripping the optional trailing `.`.
- Each `.`-separated label 1..=63 octets.
- Each label matches `[A-Za-z0-9-]+`.
- No label starts or ends with `-`.
- The all-numeric form (e.g. `12345`) is rejected — such inputs are
  caught earlier by the IP-literal classifier.
- Comparison is **ASCII case-insensitive** (`example.com` and
  `Example.COM` are the same hostname for cache-key purposes); we
  normalize on construction so `==` and `Hash` work the obvious way.

**Invariants**:

- A `Hostname` value MUST satisfy every validation rule for its
  entire lifetime — `Hostname::new()` is the only constructor and
  it never returns `Ok` with a violating value.

---

## Entity: `Target` (new, in `portunus-core`)

Discriminated classification of a rule's target host string.

```text
enum Target {
    Ip(IpAddr),       // already-parsed IP literal (v4 or v6)
    Dns(Hostname),    // already-validated RFC 1123 hostname
}
```

**Construction**: `Target::parse(s: &str) -> Result<Target, TargetError>`
follows this disambiguation order (matches the spec § Edge Cases):

1. Try `s.parse::<Ipv4Addr>()` → `Target::Ip(V4(_))`.
2. Try `s.strip_prefix('[').and_then(strip_suffix(']')).parse::<Ipv6Addr>()`
   → `Target::Ip(V6(_))`. Bare (unbracketed) IPv6 in a host string with
   a port is rejected at push time.
3. Otherwise hand `s` to `Hostname::new()` → `Target::Dns(_)` or error.

**Wire / persistence**: serialized as the original string in
`Rule.target_host`. The classification is recomputed on load — the
server always re-validates inputs from disk for defense in depth.

---

## Entity: `Rule` (extended — additive on v0.2.0)

v0.2.0 fields (unchanged): `rule_id`, `listen_port`, `target_host`,
`target_port`, `listen_port_end?`, `target_port_end?`,
`request_id` (transient).

**New field**:

| Field         | Type           | Default | Notes                                                                                                                  |
|---------------|----------------|---------|------------------------------------------------------------------------------------------------------------------------|
| `prefer_ipv6` | `Option<bool>` | `None`  | When the resolver returns both A and AAAA records, prefer the AAAA list. Absent or `false` = default IPv4-first (FR-007 / spec § Clarifications Q1). |

**Invariants**:

- `prefer_ipv6 == Some(true)` is only meaningful when `Target` is
  `Target::Dns(_)`. Setting it on a `Target::Ip` rule is harmless
  (the resolver layer skips the family-preference path entirely),
  but the operator surface SHOULD still accept it without error so
  operators can build rules generically.
- `target_host` MUST classify cleanly via `Target::parse` at push
  time. Server-side push handler returns
  `OperatorError::InvalidTargetHost { reason }` on failure with
  `reason ∈ {"invalid_ip", "hostname_label_too_long", "hostname_invalid_char", …}`.

---

## Entity: `ResolutionCacheEntry` (new, in `portunus-client/src/resolver/cache.rs`)

Per-hostname state inside the process-wide resolver cache (R-002).

```text
enum CacheEntry {
    Pending {
        notify: Arc<Notify>,           // single-flight: waiters block here
        started_at: Instant,           // for resolution-timeout enforcement
    },
    Resolved {
        addrs: Vec<IpAddr>,            // resolver-returned, in resolver order
        expiry: Instant,               // = received_at + clamp(ttl, 5s..5min)
    },
    StaleAfterFailedRefresh {
        stale_addrs: Vec<IpAddr>,      // last successful answer, past TTL
        fail_grace_until: Instant,     // = expiry + 30s (FR-005)
    },
    Failed {
        retry_after: Instant,          // brief negative-cache window after grace expiry
        last_reason: ResolveFailReason,
    },
}
```

**Lifecycle transitions** (single-threaded under the cache mutex,
released across awaits):

```text
                    ┌──────────────────────────────────────────────────────┐
                    │                                                      │
   first lookup     │                                                      │
        │           v                                                      │
        v        Pending  ─── resolver Ok ──> Resolved  ─── now ≥ expiry ──┴── refresh fires
                    │                            │                              │
              resolver Err                  re-resolve on lookup                │
                    │                            │                              │
                    v                            v                              v
                  Failed   <── grace expired ── StaleAfterFailedRefresh <── refresh Err
```

**Invariants**:

- At most one `Pending` entry per hostname at any instant
  (single-flight, FR-012).
- A `Resolved` entry's `addrs` is non-empty (a resolver answer with
  zero usable addresses is treated as a `Failed` outcome).
- `StaleAfterFailedRefresh::stale_addrs` is always the most recent
  successful `Resolved::addrs` (we don't keep older history).
- `Failed::retry_after` is bounded (3 s suggested) so a flapping
  resolver does not cause a hot-loop of NXDOMAIN-then-retry queries.

**Lifetime**: the cache lives as long as the `portunus-client`
process. No cross-process sharing, no on-disk persistence — caches
are deliberately discarded across client restarts so a new process
re-validates DNS state on first traffic.

---

## Entity: `ResolverConfig` (new, in `portunus-client/src/resolver/mod.rs`)

Process-wide resolver constants for v0.3.0. All fields are
spec-fixed defaults — no CLI/config wire-up in this feature; the
struct exists so future work can swap defaults for operator-supplied
values without changing call sites.

| Field                       | Type             | Default | Notes                                                                                |
|-----------------------------|------------------|---------|--------------------------------------------------------------------------------------|
| `cache_floor`               | `Duration`       | `5 s`   | Lower clamp on resolver-reported TTL (FR-003). Spec-fixed in v0.3.0; future work may expose as a server-side default at rule install. |
| `cache_ceiling`             | `Duration`       | `5 min` | Upper clamp on resolver-reported TTL (FR-003). Spec-fixed in v0.3.0; future work may expose as a server-side default at rule install. |
| `stale_while_error_grace`   | `Duration`       | `30 s`  | Stale-while-error window past TTL when fresh resolution fails (FR-005). **Fixed spec budget**, not a runtime knob even in future work. |
| `attempt_timeout`           | `Duration`       | `3 s`   | Per-resolver-attempt timeout (Assumptions).                                          |
| `negative_cache_retry`      | `Duration`       | `3 s`   | After grace expiry, brief delay before next resolver attempt (R-002).                |
| `max_concurrent_resolves`   | `usize`          | `64`    | Cap on `Pending` entries to bound resolver-side load if many unique names go bad simultaneously. |

**Future-work seam (NOT built in v0.3.0)**: a later spec can deliver
floor/ceiling overrides via `portunus-server`-issued client config so
operators tune cache budgets per fleet without redeploying clients.
For v0.3.0 the defaults above are baked at compile time; any "operator
tunability" language in earlier drafts of this doc is deferred.

---

## Entity: `DnsFailureCounter` (new, in `portunus-client/src/forwarder/stats.rs`)

Per-rule monotonic count of end-user connections that failed because
of DNS resolution. Lives alongside the v0.2.0 counters
(`bytes_in`, `bytes_out`, `active_connections`).

| Field          | Type        | Notes                                                                                              |
|----------------|-------------|----------------------------------------------------------------------------------------------------|
| `value`        | `AtomicU64` | Bumped exactly once per end-user connection that failed because of `dns_resolution_failed` (FR-008). |
| `wire`         | `RuleStats.dns_failures = 6` (uint64) | Carried to the server on the existing 5 s `StatsReport` tick (R-008 / contracts/portunus.proto). |
| `Prometheus`   | `IntCounterVec({"client", "rule"})` | Server-side collector fed by the StatsReport accumulation; one row per rule on the metrics endpoint (SC-006). |

**Increment rule**: bumped *only* by the resolver layer on a
"final" DNS failure (NXDOMAIN, SERVFAIL, full multi-A exhaustion,
attempt timeout, …) that the end-user connection saw. Cache hits
that succeed do NOT bump it. Cache hits that succeed *during the
stale-while-error grace window* DO bump it (the underlying
fresh-refresh failed even though we served stale data) — FR-005
requires this so operators see the underlying problem in metrics
even while end users are unaffected.

---

## Relationships

```text
Rule
  ├── target_host : String  ─────► Target (parsed on load)
  │                                  │
  │                                  ├── Ip(IpAddr)  → resolver layer skipped
  │                                  └── Dns(Hostname)
  │                                        │
  │                                        ▼
  │                                  ResolutionCacheEntry  (process-wide, keyed by Hostname)
  │
  ├── prefer_ipv6 : Option<bool>  ─► influences address-family ordering at connect time
  │
  └── ── ── ── (rule's per-rule stats live here) ── ── ──┐
                                                         ▼
                                             { bytes_in, bytes_out,            }
                                             { active_connections,             }
                                             { dns_failures (NEW)              }
```

A `Hostname` is shared across N rules — that's the cache-key
coalescing. A `Rule` has at most one `Target` and at most one
`DnsFailureCounter`. `ResolverConfig` is process-singleton.
