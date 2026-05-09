# Phase 0 — Research: Rate Limiting & QoS

**Feature**: 011-rate-limiting-qos
**Date**: 2026-05-09

## R-001 — Hand-rolled token bucket, no `governor` crate

**Decision**: Implement `{rate, burst}` token buckets in
`forward-client` using `tokio::time::Instant` plus `AtomicU64` for the
bucket level. No new workspace dependency.

**Rationale**:
- The repo's standing rule (Constitution II + v0.9 / v0.10 precedent) is
  zero new workspace deps unless the value is overwhelming.
- The math we need is small: lazy refill on observe, monotonic clock,
  one CAS per packet on capped flows.
- A custom implementation lets the cap-update path swap an
  `Arc<RateLimitConfig>` pointer without re-creating any locked state.

**Alternatives considered**:
- `governor` crate. Rejected: introduces `nonzero_ext`, `quanta`,
  optional `dashmap`, and its scope-manager APIs are richer than we
  need.
- Per-connection sleep-until-refill loops without an explicit bucket.
  Rejected: hard to share state across the multiple TCP copy
  half-loops (read and write) on a single connection and impossible
  to apply per-rule aggregate.

## R-002 — Per-rule limiter is one struct holding 4 buckets

**Decision**: Each capped rule owns one `RuleRateLimiter` containing up
to four optional `TokenBucket`s
(`bandwidth_in`, `bandwidth_out`, `new_connections_per_sec`,
`concurrent_connections`) plus an `AtomicU64` active-connection
counter. A bucket is `None` when its cap is unset.

**Rationale**:
- Keeps the no-cap fast path branch-free: if `RuleRateLimiter` is
  `None`, the limiter call sites compile away.
- Bucket lookup is a flat field access, no hash map.
- Hot-reload swaps the `Arc<RuleRateLimiter>` so in-flight work either
  observes the old or new config atomically — never a torn read.

**Alternatives considered**:
- One shared `HashMap<RuleId, Bucket>` keyed by dimension. Rejected:
  introduces global lock and complicates the no-cap path.

## R-003 — Per-owner limiter mirrors the per-rule shape

**Decision**: A separate `OwnerRateLimiter` of identical shape lives
in a `HashMap<OwnerId, Arc<OwnerRateLimiter>>` on `forward-client`,
populated from the rule-push channel and from a new
`OwnerRateLimitUpdate` server-message variant. Each capped flow
consults the per-owner limiter **before** the per-rule limiter (Q1,
FR-013).

**Rationale**:
- Per-owner ceilings naturally compose with per-rule via short-circuit
  evaluation (deny by owner if owner-bucket fails; otherwise check
  rule).
- Owner aggregation is naturally many-rule → one-bucket; a separate
  scope manager keeps the per-rule limiter from leaking owner state.

**Alternatives considered**:
- Inline owner caps as duplicated fields on every rule. Rejected:
  inconsistent state across same-owner rules and explosive
  per-rule wire size.
- Server-side owner enforcement. Rejected: data plane lives on
  `forward-client` and the server has no per-packet visibility.

## R-004 — Wire field tag assignments

**Decision**:
- `Rule.rate_limit = 12` (next free after v0.9's `sni_pattern = 11`).
- `RuleStats.rate_limit = 16` (next free after v0.9's
  `sni_route_*_total = 13/14/15`).
- `StatsReport.owner_rate_limit_stats = 4` (next free after v0.9's
  `sni_listener_stats = 3`).
- New top-level message `RateLimit` carries
  `{rate, burst}` pairs for four dimensions, each pair optional.
- New top-level message `RateLimitStats` carries the per-scope reject
  counts (by reason), throttle-seconds totals (by direction), and
  active-connection gauges.
- New `OwnerRateLimitStats` carries `(owner_id, RateLimitStats)`.
- New server-side push `OwnerRateLimitUpdate` carried in
  `ServerMessage.payload` next free oneof tag (currently 3 after
  v0.9 / v0.10 — TBD at proto edit time, additive).

**Rationale**:
- Conforms to v0.9 / v0.10 additive-tag convention.
- Single `RateLimit` sub-message keeps `Rule` clean and lets a future
  per-target sub-cap reuse the same shape if scope changes.
- Reject reasons are encoded as a proto enum so the `_total` counter
  can carry a structured `reason` label without string interning.

**Alternatives considered**:
- Inlining all eight optional fields directly on `Rule`. Rejected:
  bloats `Rule` and obscures intent.

## R-005 — SQLite migration V005 is purely additive

**Decision**: Migration `V005__add_rate_limit_columns.sql` adds eight
nullable columns to `rules`
(`rl_bandwidth_in_bps`, `rl_bandwidth_out_bps`,
`rl_new_connections_per_sec`, `rl_concurrent_connections`, plus four
companion `rl_burst_*`) and creates one new table
`rate_limit_owner(client_name TEXT, owner_id TEXT, …same eight columns…,
PRIMARY KEY (client_name, owner_id))`. Schema-version range shifts
`[1,3] → [1,4]`.

**Rationale**:
- v0.10's V004 already established that adding nullable columns to
  `rules` is the right shape for additive cap fields.
- Owner caps need a separate table because their primary key
  `(client_name, owner_id)` is not a function of any single rule.

**Alternatives considered**:
- One table keyed by `(client_name, owner_id, rule_id?)` with a
  nullable rule_id. Rejected: confuses sub-cap semantics that we
  explicitly defer.

## R-006 — Capability gate follows existing version-gate pattern

**Decision**: The server refuses any rule push whose `Rule.rate_limit`
is set, and any owner-cap mutation aimed at a client whose last
reported `Hello.client_version` is below `0.11.0`. Returns
`422 rate_limit_unsupported_by_client` consistent with v0.9
`sni_unsupported_by_client` and v0.10
`proxy_protocol_unsupported_by_client`.

**Rationale**:
- Pre-v0.11 clients would silently ignore unknown fields and run at
  effectively no cap, violating the operator-visible contract.

## R-007 — Concurrent-connection accounting is one atomic per accept/close

**Decision**: Each `RuleRateLimiter` and `OwnerRateLimiter` carries an
`AtomicU64 active_connections`. TCP accept attempts do
`fetch_add(1, AcqRel)`; if the post-increment exceeds the cap, the
limiter immediately closes the socket with RST and `fetch_sub(1)`s.
Connection close paths always `fetch_sub(1)`.

**Rationale**:
- A single hot-path CAS per accept beats any shared lock.
- Slight over-shoot (briefly N+1) is acceptable for a soft cap; the
  spec's exactness target (SC-002) is met because we close the over-
  shoot before any bytes flow.

**Alternatives considered**:
- Compare-and-swap loop that retries until below cap. Rejected:
  unnecessary; fetch-add-then-undo is simpler and equally correct.

## R-008 — Hot-reload swaps `Arc<RateLimitConfig>`, never resets buckets

**Decision**: A cap update is a `parking_lot::RwLock<Arc<...>>` swap
of the rule's or owner's `RateLimitConfig`. Token buckets keep their
existing `tokens` and `last_refill` fields; only `rate` and `burst`
are read freshly on the next refill tick. Concurrent-cap lower under
live count: `fetch_add` keeps succeeding for in-flight closes' decrement
path; new accepts see the new (lower) cap and reject as normal. No
forcible close (Q4, FR-011).

**Rationale**:
- Resetting the bucket on update would create a free-for-all burst on
  cap raise and a stall on cap lower.
- The graceful-drain semantics fall out for free if we never close
  in-flight connections from the limiter.

**Alternatives considered**:
- Recreate the bucket on update. Rejected for the reasons above.

## R-009 — UDP first-packet enforcement before NAT bind

**Decision**: For UDP rules with a flow-rate or concurrent-flow cap,
the `forward-client` UDP path consults the limiter on the first
packet of a new flow, **before** v0.4 NAT binding. A reject path is a
silent drop of the packet and an increment of
`rate_limit_reject_total{reason="udp_flow_rate" | "owner_udp_flow_rate"}`.
Packets on already-bound flows pass through bandwidth caps but are
never blocked by flow-rate caps.

**Rationale**:
- v0.4 already gated NAT bind through a per-rule check; the rate
  limiter slots in cleanly at the same call site.
- A flow-rate cap is meaningful only on new flows.

## R-010 — Bandwidth throttle blocks the copy half-loop

**Decision**: The bidirectional copy loop on each TCP connection
acquires `n_bytes` of tokens from the rule and (if present) owner
bucket before forwarding. When the bucket is empty, the loop awaits
`tokio::time::sleep(deficit / rate)`. The sleep timer is the only
backpressure mechanism; the connection is never closed by the
limiter.

**Rationale**:
- Reuses the existing copy loop without restructuring it.
- Cumulative `tokio::time::sleep` time on the read side is exactly
  what `rate_limit_throttle_seconds_total{direction="in"}` reports.

**Alternatives considered**:
- Pause the read by stopping the underlying `AsyncRead`. Rejected:
  more invasive and harder to get right under Tokio cancellation.

## R-011 — Burst override is one optional `burst_*` companion field

**Decision**: For each cap field `X`, a sibling `X_burst` field carries
the override. Absent → default `burst = 1 × rate`. Server validation
clamps to `burst ≥ rate / 100` (avoid degenerate sub-1ms refill) and
`burst ≤ rate × 60` (avoid effectively-no-cap one-minute bursts).

**Rationale**:
- Implements Q2 minimally — common UI shows only `rate`, advanced
  operators see the override under a disclosure.
- Clamping protects operators from typos and the limiter from
  overflow.

**Alternatives considered**:
- A single `burst_seconds` global override. Rejected: removes
  per-cap tunability for mixed-purpose rules.

## R-012 — Metrics labels & cardinality

**Decision**:
- `rate_limit_reject_total{client, rule, owner, reason}` — counter, 6
  reason values.
- `rate_limit_throttle_seconds_total{client, rule, owner, direction}` —
  counter, 2 direction values.
- `rate_limit_active_connections{client, rule, owner}` — gauge.
- `rate_limit_owner_active_connections{client, owner}` — gauge.

The `owner` label is empty string for the per-rule reasons that have no
owner attribution (e.g., `conn_concurrent`); the per-owner reasons
always carry a non-empty `owner`.

**Rationale**:
- Mirrors v0.5+ label conventions (`client`, `rule`, `owner`).
- Cardinality envelope is `rules × owners × 6` reject-series and
  `rules × 2 + owners × 2` throttle/gauge series — same order of
  magnitude as v0.10's metrics surface.

## R-013 — Per-owner cap REST API path layout

**Decision**: `POST | PUT | GET | DELETE
/v1/clients/{client_id}/owners/{owner_id}/rate-limit` carries the
owner-cap envelope. The `client_id` segment matches the existing
client-resource path; `owner_id` is the stable v0.5 RBAC owner ID.

**Rationale**:
- Implements Q5 — `(client, owner)` is the natural primary key, so
  the URL matches the storage shape directly.
- Adjacent to existing client resources, easy to discover from CLI
  listings.

## R-014 — Web UI placement mirrors the API

**Decision**:
- Rules table gains a "Caps" column showing a compact summary
  (e.g., `1 MB/s in · 100 conn`) for each rule.
- Rule editor gains four optional cap inputs in a new "Quality of
  service" section. Burst overrides are hidden behind an "Advanced"
  disclosure.
- Client detail page gains a new "Owner quotas" tab next to RBAC,
  listing every owner with rules on this client and their per-owner
  cap envelope (read + edit + delete).
- Per-owner reject / throttle counters render under the same tab in
  a "Last 5 min" / "Last 1 h" pair.

**Rationale**:
- Implements Q5 — operator finds owner caps next to RBAC membership,
  which is the constraint they parameterise.
- Hidden burst inputs implement Q2 — keeps simple-case UI clean.

## R-015 — Hot-path zero-allocation invariant

**Decision**: Token-bucket refill, acquire, and active-conn accounting
do not allocate. The limiter call sites take all inputs by value and
write nothing on the heap. `Arc::clone` of a `RateLimitConfig` is the
only refcount manipulation and happens once per accept, not per byte.

**Rationale**:
- Constitution II requires zero-allocation steady state for hot path
  changes.
- Bench gate (SC-004) measures regression on no-cap and capped paths
  and would fail otherwise.
