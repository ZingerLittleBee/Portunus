<!-- SPECKIT START -->
Active feature: `011-rate-limiting-qos` on branch `011-rate-limiting-qos`.
v0.11 adds per-rule and per-owner connection rate limiting / QoS:
bandwidth (bytes/sec, both directions), new-connection rate (TCP
conn/sec or UDP flow/sec), and concurrent connection / flow count.
Each cap is independently optional; absent fields preserve v0.10
behaviour byte-for-byte. Bandwidth caps throttle in-flight flows via
a token bucket; connection-rate and concurrent caps reject new
connections (TCP RST after accept; UDP packet drop before NAT bind).
The rate limiter never closes existing connections — including under
hot-reload that lowers a concurrent cap below the live count
(graceful drain). Token-bucket implementation is hand-rolled; zero
new workspace deps.

Key invariants:
- "Per-client" cap is per-RBAC-owner within a portunus-client (Q1).
  Cap envelope keyed `(client, owner)`. Node-level aggregate caps
  are explicitly out of scope for v0.11.
- Wire fields are additive: `Rule.rate_limit = 12`,
  `RuleStats.rate_limit = 16`, `StatsReport.owner_rate_limit_stats = 4`,
  new server-push variant `OwnerRateLimitUpdate`. New messages
  `RateLimit`, `RateLimitStats`, `OwnerRateLimitStats`, enums
  `RateLimitRejectReason` (6 values) and `OwnerRateLimitAction`.
- Capability gate: `rate_limit` push (or any owner-cap mutation)
  to a pre-v0.11 client → `422 rate_limit_unsupported_by_client`
  before any rule activates anywhere.
- Per-owner ceiling binds **before** per-rule cap (FR-013); rejects
  carry distinct `owner_*` reasons (FR-014).
- Reject path: TCP accept-then-RST (Q3) — listener-pause was rejected
  because v0.7/v0.9 share listeners across rules.
- Burst defaults to `1 × rate`; optional per-cap `*_burst` field
  overrides (Q2). UI hides burst behind an "Advanced" disclosure.
- Hot-reload swaps `Arc<RateLimitConfig>`; concurrent cap lowered
  below live count drains gracefully (Q4) — no forced close.
- Per-owner cap REST path: `/v1/clients/{id}/owners/{owner_id}/rate-limit`
  (Q5). Web UI surfaces it as an "Owner quotas" tab on client detail.
- Data-plane reject/throttle events are tracing-only — they do NOT
  enter the SQLite operator audit ring (mirrors v0.9 D13 / v0.10
  invariant).
- SQLite migration V005 adds nullable cap columns to `rules` plus a
  new `rate_limit_owner` table; schema-version range
  `[1,3] → [1,4]`.

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/011-rate-limiting-qos/plan.md`
- Supporting artifacts in the same directory: `spec.md`,
  `research.md` (R-001..R-015 decisions), `data-model.md`,
  `contracts/wire.md`, `contracts/operator-api.md`, `quickstart.md`,
  `checklists/requirements.md`.

Inherited baselines (do not re-derive):
- v0.10.0 — `specs/010-proxy-protocol-and-peek-histogram/plan.md`.
  Per-target PROXY v1/v2 prelude + SNI ClientHello peek-duration
  histogram. v0.11 schema gains migration V005; rate-limit fields are
  additive on top of v0.10's `Target.proxy_protocol = 4`.
- v0.9.0 — `specs/009-tls-sni-routing/plan.md`. Per-listener SNI
  dispatch. v0.11 listener-pause rejected to avoid penalising other
  rules sharing v0.9 SNI listeners (Q3 rationale).
- v0.8.0 — `specs/008-sqlite-storage/plan.md`. Server persistent state
  unified into one embedded SQLite at `<data-dir>/state.db`. v0.11
  schema-version range shifts `[1,3] → [1,4]`.
- v0.7.0 — `specs/007-multi-target-failover/plan.md`. Multi-target
  rules. v0.11 caps apply at rule aggregate (per-target sub-caps
  out of scope).
- v0.6.0 — `specs/006-management-web-ui/plan.md`. React+Vite SPA
  embedded via rust-embed; v0.11 adds a `Caps` column on the rules
  page, a QoS section in the rule editor (with hidden burst overrides),
  and a new `Owner quotas` tab on the client detail page.
- v0.5.0 — `specs/005-multi-user-rbac/plan.md`. RBAC owner is the
  tenant boundary v0.11's per-owner caps key on. Metric labels
  follow v0.5+ conventions.
- v0.4.0 — `specs/004-udp-forward/plan.md`. UDP first-packet enforcement
  before NAT bind; flow-rate cap meaningful only on new flows.
- v0.3.0 — `specs/003-domain-name-forward/plan.md`. DNS resolver
  unchanged.
- v0.2.0 — `specs/002-port-range-forward/plan.md` (range rules).
- v0.1.0 — `specs/001-tcp-forward-mvp/plan.md` (TCP forwarding MVP).

Project-wide governance: `.specify/memory/constitution.md` (currently v2.0.2 —
TLS + bearer token, NOT mTLS; SQLite as bundled persistence).
<!-- SPECKIT END -->
