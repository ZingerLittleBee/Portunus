# Changelog

All notable changes to `Portunus` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.3.0] — 2026-05-13

Linux data-plane release. Single-flow uncapped TCP throughput on the
v1.2.0 reference bench host doubles (9,954 → 21,922 Mbit/s; 2.20×).
Operator surface, wire protocol, SQLite schema, and Web UI are
unchanged.

### Added

- **Linux TCP zero-copy fast path** — on Linux hosts the
  `portunus-client` data plane now uses `splice(2)` for TCP
  forwarding when a rule has no bandwidth caps and no per-owner
  bandwidth cap. The optimization is applied automatically with no
  rule-level configuration; the byte stream, half-close semantics,
  per-rule counters, and Prometheus metrics are identical to the
  previous userspace path. macOS and Windows builds are unchanged
  (the fast-path code is `#[cfg(target_os = "linux")]`-gated out
  entirely). SNI-routed (v0.9) and PROXY-protocol (v0.10) rules
  also benefit — the optimization kicks in once the prelude phase
  completes. Rules with `bandwidth_in_bps` / `bandwidth_out_bps`
  (per-rule or per-owner) continue on the userspace path so v0.11
  rate-limit semantics are preserved exactly.
- **`PORTUNUS_DISABLE_SPLICE` env variable** — set on the
  `portunus-client` environment to force every connection to the
  userspace path. Intended for diagnostic and bench-comparison use
  only; not advertised in `--help` or operator configuration. See
  `docs/operations/troubleshooting.mdx` for guidance.
- **Owner-cap "Add" dialog in Web UI** — Operators can now set a
  per-owner `concurrent_connections` / bandwidth cap on a client
  before that owner pushes their first rule. The Owner quotas tab
  previously only listed owners who already had rules, hiding the
  PUT-before-rule code path the backend has always supported.

### Changed

- **`make dev` / `make ui` / `make webui-build`** auto-install
  `webui/node_modules` on first use via a file-target gated on
  `pnpm-lock.yaml`. Fresh clones and post-`make clean` runs no
  longer fail with `vite: not found`.

### Fixed

- **SNI dispatcher silently dropped owner / rule rate-limit
  handles.** Rules with `sni_pattern` routed through
  `PortGroupManager` lost their limiter handles at `GroupMember` /
  `SniRuleSlot` construction; the SNI accept loop then passed four
  `None`s into the proxy. Capped SNI rules ran uncapped end-to-end.
  Limiters are now threaded through
  `GroupMember → SniRuleSlot → proxy_with_preread_and_prelude`,
  and a `try_acquire_layered` cascade in
  `sni::listener::handle_accept` mirrors the legacy and failover
  accept paths.
- **Legacy single-target HTTP push hard-coded
  `rule.owner_id: None` on the wire.** Rules created via
  `POST /v1/rules` with the v0.6 `target_host` / `target_port`
  shape lost owner-cap enforcement on the client — the client
  only installs an `OwnerRateLimitHandle` when the wire
  `owner_id` is non-empty. Now mirrors `push_rule_multi_target`
  and always emits `rule.owner_user_id`.

## [1.2.0] — 2026-05-13

UX-focused release. Operators can now recover from a lost credential
bundle, surface OS-specific install commands directly from the Web
UI, and keep the clients list uncluttered. CSRF works out of the box
on Docker / loopback / LAN deployments without any config knob.

### Added

- **Re-issue credential** — client detail page now offers an
  in-place credential rotation that invalidates the old bearer
  token, disconnects any live forwarder under the same name, and
  hands back a fresh bundle. Recovers from "I navigated away
  before saving the bundle" without having to revoke + provision
  with a different name.
- **Permanent client deletion** — `DELETE /v1/clients/{name}` and a
  matching Web UI action purge already-revoked rows from the
  store, freeing the name for re-provisioning. Refuses to touch
  still-active clients (`409 client_not_revoked`).
- **Hide-revoked filter** — clients list defaults to showing only
  active clients; a "Show revoked (N)" toggle exposes history when
  needed.
- **In-UI install commands** — after issuing or re-issuing a
  client, the Web UI renders OS-specific, copy-pasteable
  installation steps (Docker, Linux/systemd, manual run). The
  credential bundle is base64-embedded into each command so no
  separate file transfer is required.
- **`make dev` workflow** — top-level Makefile runs the backend
  and Vite UI together with hot reload, auto-bootstrapping the
  superadmin account and a temporary Web UI password on first
  launch.

### Changed

- **CSRF default policy** — cookie-authenticated writes now pass
  same-origin verification (`Origin` vs `Host`) when
  `operator_http_public_origin` is not configured. This matches
  Grafana / Caddy / Gitea and works zero-config for
  `localhost` / loopback / LAN / proxy-preserved `Host` setups.
  Setting `operator_http_public_origin` still hard-locks writes to
  one declared origin for reverse-proxy lockdown scenarios.
- **Bundle warning copy** clarifies that the bearer token is
  surfaced exactly once and the server stores only its hash;
  navigating away discards the plaintext.

### Fixed

- **Docker `--operator-http-listen 0.0.0.0:7080` no longer breaks
  the Web UI** — the previous CSRF middleware compared the request
  `Origin` against `http://{listen}`, which produced a guaranteed
  mismatch any time the listen address was `0.0.0.0`.

## [1.1.0] — 2026-05-11

Adds local password authentication for the operator Web UI. The data
plane and bearer-token operator CLI are unchanged; this release is
purely additive on the operator surface so existing bearer-only
automations keep working byte-for-byte.

### Added

- **Local password authentication** — operators can now sign in to the
  Web UI with a username + password instead of pasting a bearer token.
  Backed by Argon2 password hashing, a new SQLite schema for password
  state, and forced first-login password changes. Bearer tokens remain
  the canonical machine credential for the operator HTTP API.
- **Web session cookies** — successful UI logins mint a server-stored
  session cookie; the operator API accepts either a bearer header or a
  valid session cookie, with bearer taking precedence when both are
  present.
- **First-run onboarding flow** — `portunus-server serve` prints a
  rotating setup token (30 min TTL, regenerated each start until
  onboarding completes). The Web UI accepts that token to create the
  first superadmin account and password.
- **Login throttling** — failed login attempts and onboarding-token
  attempts are rate-limited per subject / client IP with burst-aware
  lockout windows.
- **Operator public origin config** — new `operator_public_origin`
  setting lets the server validate `Origin` / `Host` on cookie-bearing
  requests when the Web UI is fronted by a reverse proxy.
- **Local auth recovery CLI** — `reset-password`, `onboarding-token`
  and related break-glass subcommands open the SQLite store directly
  for offline recovery while the server is stopped.
- **Password management UI** — Web UI surfaces password change, forced
  reset, and per-user password state alongside the existing
  Credentials page.

### Changed

- **Web UI API client** uses cookie sessions by default and falls back
  to bearer when no session is present; identity is cleared
  client-side on session expiry instead of silently 401'ing.
- **Operator API auth layer** rejects malformed session cookies before
  falling through to bearer validation, so a tampered cookie no longer
  shadows a valid bearer header.

### Fixed

- **Docker server image** rebuilt against a glibc compatible with the
  distroless runtime, fixing the `GLIBC_2.38 not found` startup crash
  reported in #1.
- **Docker compose** now publishes the Web UI port so the operator
  loopback listener is reachable from the host.
- **Web session verification race** — concurrent verify + rotate on
  the same session no longer briefly returns 401.
- **Password reset audit trail** preserves the actor/subject/reason
  fields end-to-end through the reset path.
- **Onboarding throttle** keys on client IP rather than the (always
  empty) subject, so a single attacker IP cannot brute-force the setup
  token by rotating subjects.

### Versioning

- Wire / REST / SQLite-schema surfaces from v1.0.0 are unchanged for
  data-plane consumers. The operator HTTP surface gains the
  password / onboarding / session endpoints additively; pre-v1.1
  clients that only send `Authorization: Bearer` continue to work
  without modification.

## [1.0.0] — 2026-05-10

First stable release. Cuts the v0.x development line and freezes the
wire / REST / SQLite-schema surfaces inherited from v0.11. No new
data-plane features — the jump to 1.0 is a stability commitment, not a
new specification round.

### Added

- **User documentation site** (`docs/`) — comprehensive user-facing
  guide built on a Vite scaffold, covering install, RBAC, multi-target
  failover, SNI routing, PROXY protocol, rate-limit / QoS, web UI, and
  the operator HTTP API. Landing page surfaces feature highlights and
  the v0.10 performance report.
- **Documentation i18n** — Simplified Chinese translation of the full
  documentation set, served under a `/$lang` URL prefix.
- **Operator analysis scripts** (`analyze_commits.py`, `check_prs.py`,
  `check_work_hours.py`) — filter commits and PRs by holidays and work
  hours; intended for retrospectives and contributor reporting, not
  shipped in the server binary.

### Changed

- **TCP copy buffer sizing** — `bidirectional_copy` buffers retuned
  against the v0.10 perf harness; throughput report regenerated and
  added to the docs site.

### Fixed

- **`portunus-client` shutdown** — `run` futures now await the
  cancellation token before returning, so `Drop` no longer races the
  drain timeout. Regression tests cover forwarding behaviour past the
  drain deadline.
- **Rate-limit hot-update** — rule-cap mutations now reach the live
  `Arc<RuleRateLimiter>` instead of stranding on the stale config snap-
  shot; owner-quota mutations are propagated to all active rules under
  that owner.
- **Web UI rule state parsing** — payloads with optional fields parse
  robustly across server versions; rule editor burst help copy now
  matches the API validation envelope (`[rate/100, rate*60]`).

### Versioning

- Wire / REST / SQLite-schema range unchanged from v0.11
  (schema-version `[1,4]`). v1.x will add fields additively per the
  capability-gate discipline established in v0.5+.

## [0.11.0] — 2026-05-09

### Added (011-rate-limiting-qos)

Per-rule and per-owner connection rate limiting / QoS. Each cap is
independently optional; absent fields preserve v0.10 behaviour
byte-for-byte. Token-bucket implementation is hand-rolled — zero new
workspace deps.

- **Per-rule caps** on `Rule.rate_limit`:
  - `bandwidth_in_bps` / `bandwidth_out_bps` — token-bucket throttle
    on the bidirectional copy loop. Cumulative throttle wall-clock
    time per direction surfaces as
    `rate_limit_throttle_seconds_total{direction}`. Connection is
    never closed by the limiter — the read or write half parks until
    the next refill.
  - `new_connections_per_sec` — TCP accept-then-RST or UDP first-
    packet drop on rate exhaustion. Listener-pause was rejected so a
    capped rule never penalises another rule sharing a v0.7/v0.9 SNI
    listener.
  - `concurrent_connections` — atomic `fetch_add` then cap check on
    accept (TCP) or first-packet (UDP). RAII guard releases the slot
    on close; soft-cap overshoot of ±1 under concurrent accepts is
    closed before any byte flows.
  - Each cap has an optional sibling `*_burst` field; absent →
    `burst = 1 × rate`. Server validation clamps to
    `[rate/100, rate*60]` and rejects negative or zero rates.
- **Per-owner ceilings** (per-RBAC-owner within a portunus-client)
  bind **before** per-rule caps; rejects carry distinct
  `owner_*` reasons (`OwnerConnRate`, `OwnerConnConcurrent`,
  `OwnerUdpFlowRate`). REST surface:
  `/v1/clients/{id}/owners/{owner_id}/rate-limit`.
- **Capability gate** — pushing `rate_limit` (or any owner-cap
  mutation) to a pre-v0.11 client returns HTTP 422
  `rate_limit_unsupported_by_client` before any rule activates
  anywhere.
- **Hot-reload** — cap mutations swap the rule's
  `Arc<RuleRateLimiter>` while preserving `tokens` and
  `last_refill_micros` carryover so a raise doesn't mint a free
  burst and a lower doesn't strand the pool. A concurrent cap
  lowered below the live count drains gracefully (no forcible
  close).
- **CLI** — `portunus-server push-rule` accepts the four cap flags
  plus their three burst-override siblings
  (`--bandwidth-in-bps`, `--bandwidth-out-bps`,
  `--new-connections-per-sec`, `--concurrent-connections`,
  and matching `--*-burst`); `list-rules` human output gains a
  compact `CAPS` column. New `portunus-server owner-cap
  {list|get|set|delete}` subcommand family manages per-owner
  ceilings against the operator HTTP API; pre-flight rejects an
  empty `set` envelope with exit 3 +
  `validation.rate_limit_no_caps_provided` so operators don't
  round-trip a 400.
- **Web UI** — rule editor gains a "Quality of service" section
  (cap inputs, burst overrides folded behind an "Advanced"
  disclosure); rules table gains a compact `Caps` column; client
  detail page gains an `Owner quotas` tab.
- **Observability** — three new Prometheus collectors:
  `portunus_rate_limit_reject_total{client,rule,owner,reason}`,
  `portunus_rate_limit_throttle_seconds_total{client,rule,owner,direction}`,
  `portunus_rate_limit_active_connections{client,rule,owner}`.
  `owner` label is empty for per-rule rejects, populated for owner-
  scoped rejects. Data-plane reject/throttle events are tracing-only;
  they do NOT enter the SQLite operator audit ring (mirrors v0.9 D13).

### Wire (additive only — draft)

- `Rule.rate_limit = 12` (`RateLimit` message: four optional caps +
  three optional `*_burst` overrides).
- `RuleStats.rate_limit = 16` (`RateLimitStats` message: per-reason
  reject totals, throttle micros per direction, active-connection
  gauge).
- `StatsReport.owner_rate_limit_stats = 4` (repeated
  `OwnerRateLimitStats { owner_id, stats }`).
- New server-push variant `OwnerRateLimitUpdate { client_id,
  owner_id, action: SET | REMOVE, rate_limit }` on the existing
  control stream.
- New enums `RateLimitRejectReason` (6 values: `ConnConcurrent`,
  `ConnRate`, `UdpFlowRate`, `OwnerConcurrent`, `OwnerConnRate`,
  `OwnerUdpFlowRate`) and `OwnerRateLimitAction`.

A v0.10 client connected to a v0.11 server sees no behavioural
difference (server gates pushes via the capability check); a v0.11
client connected to a v0.10 server transparently omits the new fields
under proto3 default-stripping. Schema-version range shifts
`[1,3] → [1,4]` via additive SQL migration V005 (nullable cap
columns on `rules` plus a new `rate_limit_owner` table).

### No breaking changes

- v0.10 PROXY-protocol prelude + SNI peek-duration histogram, v0.9
  SNI routing, v0.8 SQLite storage, v0.7 multi-target failover, v0.6
  management UI, v0.5 RBAC, v0.4 UDP, v0.3 DNS resolution, v0.2
  port-range rules, and v0.1 TCP MVP all behave unchanged on
  uncapped rules.

## [0.9.0] — 2026-05-09

TLS SNI routing release (009). A single TCP listen port (typically
443) can fan out to different upstream targets based on the TLS
hostname in the client's ClientHello. Portunus remains a pure L4
byte-passthrough — never decrypts, terminates, or re-encrypts TLS.
Implementation lives in the data plane on `portunus-client` (peek +
parse + route) and in additive control-plane fields on
`portunus-server`. Auth seam, credential hashing, persistence layer,
and forwarding hot-path layout are byte-stable for v0.8 callers.
Zero new workspace deps.

Spec / plan: `specs/009-tls-sni-routing/`.

### Added (009-tls-sni-routing)

- **SNI routing on a single TCP listener** — `Rule.sni_pattern`
  accepts an exact host (`api.example.com`) or single-label wildcard
  (`*.example.com`). Multiple rules can share the same listen port
  with distinct SNI selectors; the client peeks the ClientHello,
  parses the SNI extension, and forwards the connection to the
  upstream of the matching rule. The peek buffer is replayed to the
  upstream byte-for-byte (no TLS termination at any point).
- **TLS-only fallback** — a rule with `sni_pattern = NULL` on a port
  that already runs in SNI mode catches valid TLS connections whose
  SNI is missing or unmatched. Without a fallback, unmatched
  connections are dropped without ever reaching a backend (FR-035).
- **CLI**: `portunus-server push-rule --sni <PATTERN>` adds the
  selector. Pre-API rejections (UDP rules, port-range rules,
  malformed grammar) exit 2 with `validation.sni_on_unsupported_rule`
  / `validation.sni_pattern_malformed`. `list-rules` human output
  carries an `SNI` column (`-` for legacy rules).
- **Operator HTTP API**: `POST /v1/rules` accepts `sni_pattern`;
  `GET /v1/rules` echoes it when present (omitted for legacy rules
  via `#[serde(skip_serializing_if = "Option::is_none")]`).
- **Capability gate** — pushing a rule with `sni_pattern` to a v0.8
  or earlier client returns HTTP 422
  `sni_unsupported_by_client` before any rule activates (FR-018).
- **Mode-locked listener lifetime** (R-004) — a `(client, listen_port)`
  group's mode (legacy plain-TCP vs SNI dispatch) is fixed for its
  lifetime. Cross-mode pushes are refused with HTTP 409
  `conflict.legacy_to_sni_unsupported`. Operators must remove the
  existing rule first.
- **Observability** — four new Prometheus collectors:
  `portunus_tls_sni_route_total{client,rule,owner,result}`,
  `portunus_tls_sni_listener_miss_total{client,port}`,
  `portunus_tls_sni_listener_parse_failures_total{client,port}`,
  `portunus_tls_sni_routes_active`. Five structured tracing events
  with `target = "tls_sni"` cover client_hello timeout, parse
  failure, no-SNI fallback, SNI no-match, and successful routing.
  Data-plane events do NOT enter the SQLite operator audit ring
  (D13 / FR-035).

### Wire (additive only)

- `Rule.sni_pattern = 11`
- `RuleStats.sni_route_exact_total = 13`
- `RuleStats.sni_route_wildcard_total = 14`
- `RuleStats.sni_route_fallback_total = 15`
- `StatsReport.sni_listener_stats = 3` (new
  `SniListenerStats { listen_port, sni_route_miss_total,
  client_hello_parse_failures_total }`)

A v0.8 client connected to a v0.9 server sees no behavioural
difference; a v0.9 client connected to a v0.8 server transparently
omits the new fields under proto3 default-stripping. Schema-version
range shifts `[1,1] → [1,2]` via additive SQL migration V002 (one
new nullable column on `rules`, one helper partial index).

### No breaking changes

- v0.8 plain-TCP rules forward byte-for-byte through the existing
  v0.7 hot path — never enter the SNI peek code (gated structurally
  in `control.rs::routes_via_sni`, verified by
  `t063_legacy_tcp_emits_no_tls_sni_events`).
- v0.7 multi-target failover, v0.6 management UI, v0.5 RBAC, v0.4
  UDP, v0.3 DNS resolution, v0.2 port-range rules, and v0.1 TCP MVP
  all behave unchanged.

## [0.8.0] — 2026-05-08

SQLite-storage release (008). Collapses every server-side persistent
JSON file (`tokens.json`, `identity.json`, `rules.json`) and the
in-memory audit ring buffer into one embedded SQLite database at
`<data-dir>/state.db`. Single-binary deployment unchanged; closes the
constitution-level `TODO(STORAGE_CHOICE)`. Forwarding hot path (TCP /
UDP fast paths, wire protocol) and the auth seam are untouched.

### Added (008-sqlite-storage)

- **SQLite store** — bundled `rusqlite` (no system libsqlite3 dependency)
  with WAL mode and `BEGIN IMMEDIATE` write transactions; `r2d2` pool
  fronting every read; refinery-managed migrations. Schema lives under
  `crates/portunus-server/migrations/` and is verified at startup.
- **`--data-dir` flag** — separate from `--config-dir` so operators can
  move state to a dedicated volume. Defaults to
  `$XDG_STATE_HOME/portunus` (or `$HOME/.local/state/portunus`) per
  FHS.
- **`backup` subcommand** — `portunus-server backup --out <PATH>` runs
  the SQLite Online Backup API (`rusqlite::backup::Backup`) so snapshots
  are WAL-aware and consistent without quiescing writers. Refuses to
  overwrite an existing destination.
- **`restore` subcommand** — `portunus-server restore --in <PATH>
  [--force]` validates the SQLite header magic, refuses to clobber a
  non-empty data dir without `--force`, copies the artefact in, and runs
  the regular schema-version handshake. Backups whose schema is newer
  than the binary's target version exit `78` with event
  `startup.schema_version_too_new` (FR-014).
- **`reset` subcommand** — `portunus-server reset --confirm` removes the
  state DB plus its sidecar `-wal` / `-shm` files. Without `--confirm`
  prints a dry-run summary. Refuses to operate on a path that doesn't
  start with the SQLite header — protects against typo'd `--data-dir`
  (R-011).
- **Audit envelope mode** — `GET /v1/audit?since=<RFC3339>&until=<RFC3339>
  &cursor=<base64>&limit=N` returns `{ entries, next_cursor?, count }`
  instead of the v0.7 array root, enabling cursor-based historic
  scroll-back over the durable audit table. v0.7 callers (no
  `since` / `until` / `cursor`) keep getting the array root unchanged —
  byte-stable per FR-008.
- **`audit prune` subcommand** — `portunus-server audit prune --before
  <RFC3339> [--dry-run]` deletes audit rows older than the cutoff under
  `BEGIN IMMEDIATE` then runs `PRAGMA incremental_vacuum`; `--dry-run`
  reports the count without mutating.
- **`portunus-client --bundle` is now optional (FR-020)** — when omitted
  the client searches `$PORTUNUS_CLIENT_BUNDLE` →
  `$XDG_CONFIG_HOME/portunus/client.bundle.json` →
  `$HOME/.config/portunus/client.bundle.json` →
  `./client.bundle.json` and exits `1` listing every attempted path
  when none resolve.
- **Web UI audit page** — adds a "Load earlier" button that pages
  through history via the new envelope cursor. Live tail (`limit=100`,
  no extra params) keeps the existing 5-second poll.

### Changed

- **State persistence** — every former JSON file is now a SQLite table:
  `users`, `credentials`, `grants`, `rule_targets`, `audit`. Trait
  seams (`OperatorIdentityStore`, `OperatorTokenStore`) are unchanged
  so v0.7 call sites keep compiling.
- **Audit retention** — durable storage replaces the in-memory ring;
  size is bounded by the new prune CLI rather than a fixed in-memory
  cap. The `audit` page live tail is unaffected.
- **Grant lookup** — moved into a single SQL `SELECT` joined on
  `credentials`, indexed on `(client_id, port, protocol)` to remain
  timing-independent of user presence vs absence (Constitution
  Principle V; verified by `grant_lookup_timing` integration test, 5×
  ratio assertion).

### Removed

- `crates/portunus-auth/src/file_store.rs` and
  `crates/portunus-auth/src/operator_store.rs` — JSON persistence is
  retired. The cross-cutting types (`IdentityStoreError`,
  `UserRemoveSummary`, `ProvisionedClient`) live in
  `portunus-auth::store_types`.
- `tokens.json` / `identity.json` / `rules.json` artefacts. Cold start
  on a clean checkout writes only `state.db`.

### Migration

This is a clean-slate schema — there is no upgrade path from a
deployment that wrote v0.7 JSON files. The project is not yet deployed
in production, so v0.7 → v0.8 expects a fresh `--data-dir`.

## [0.7.0] — 2026-05-08

Multi-target failover release (007). Extends a forwarding rule from a
single `(target_host, target_port)` to an ordered list of targets with
priority-ordered failover and per-target client-side health tracking.
Single-target rules stay byte-identical to v0.6.0 — multi-target lives
in a separate code path entered via `match targets.len()` at rule
activation, so existing TCP/UDP throughput is unaffected.

### Added (007-multi-target-failover)

- **Multi-target rules** — `Rule.targets[]` (length 1..=8) with
  per-target `(host, port, priority)`. Lower priority number = higher
  preference. Operators push via `portunus-server push-rule
  --target host:port[@priority]` (repeatable), `--targets-json '[...]'`,
  the new `targets[]` field on `POST /v1/rules`, or the Web UI rule push
  form's "Add another target" button.
- **Per-target health tracking** — passive failure detection (3
  consecutive connect failures within 30 s flips a target to Failed; 2
  consecutive successes flip it back). Optional active probe per rule
  via `health_check_interval_secs` (1..=3600). All client-side and
  in-memory; ephemeral across client restarts.
- **`portunus_rule_target_failovers_total{client, rule}`** — Prometheus
  counter for Healthy↔Failed transitions per rule. Per-target byte
  counters surface only on demand via `rule-stats --per-target`,
  `GET /v1/rules/{id}/stats?per_target=true`, the SSE stats stream with
  `?per_target=true`, and the Web UI rule detail page — never as
  default `/metrics` series, to keep cardinality bounded.
- **Web UI rule detail Targets section** — health badges (Healthy /
  Degraded / Failed), last-failure / last-success timestamps,
  per-target byte counters that update on the existing 5 s SSE cadence.

### Changed (007-multi-target-failover)

- **Wire protocol v1.4** — additive proto3 fields on `Rule` (`targets =
  9`, `health_check_interval_secs = 10`) and `RuleStats`
  (`target_failovers_total = 11`, `per_target = 12`); new `Target` and
  `PerTargetStats` messages. v0.6.0 readers/writers continue to work
  unchanged for single-target rules. Multi-target push to a v0.6.0
  client is rejected at the HTTP layer with `422
  multi_target_unsupported_by_client`.
- **`rules.json` persistence** — read path tolerates v0.6.0
  single-target rules and promotes them to a one-element `targets` list
  in memory. Write path uses the back-compat encoding (single-target
  rules emit legacy fields; multi-target rules emit the new `targets[]`
  field).

## [0.6.0] — 2026-05-08

Operator Web UI release. Closes the long-deferred `TODO(WEB_UI)` from
the constitution. The forwarding data plane (TCP + UDP + DNS resolver +
port-range rules) is **byte-identical** to v0.5.0; this release adds a
React + Vite SPA embedded into `portunus-server` via `rust-embed`, plus
two additive operator HTTP endpoints (`GET /v1/audit`, `GET
/v1/rules/{id}/stats/stream`) and a small `GET /v1/users/me`
projection. RBAC, identity store, audit-log emit sites, and every
existing CLI subcommand are unchanged.

### Added (006-management-web-ui)

- **Operator Web UI** — a single-page React + Vite + TypeScript app
  embedded into `portunus-server` via `rust-embed` and served at the
  existing operator HTTP listener's root path `/`. Login by pasting a
  bearer token (kept in `sessionStorage` only). Covers users,
  credentials, grants, rules (with per-rule live stats over SSE),
  clients, audit log, raw `/metrics`, theme + language settings.
  Single-binary distribution preserved — no Node runtime on the host.
- **`GET /v1/users/me`** — returns `{ user_id, role, display_name }`
  for the caller. Used by the SPA's `<AuthGate>` to probe the cached
  bearer once and decide what to render.
- **`GET /v1/audit?limit=N&outcome=allow|deny`** (superadmin-only) —
  reads the new in-memory audit ring buffer (capacity 1000) populated
  by every `auth_layer` allow/deny emit site. Newest-first JSON array.
  Server-side `outcome` filter mirrors the SPA's client-side dropdown
  to keep responses small for bandwidth-constrained tabs.
- **`GET /v1/rules/{id}/stats/stream`** (SSE, ownership-checked) —
  text/event-stream of `RuleStatsSnapshot` events fed by a per-rule
  `tokio::sync::broadcast`. Subscribers cost O(rules) not
  O(rules × subscribers); slow consumers receive `Lagged` and are
  logged once per minute per rule, never blocking fast subscribers.
- **`portunus_audit_buffer_drops_total`** Prometheus counter — bumped
  whenever the audit ring drops an entry on overflow.
- **`GET /v1/metrics`** (superadmin-only) — same Prometheus payload as
  the standalone `metrics_listen` endpoint, but RBAC-gated so the
  embedded SPA (loaded same-origin from `operator_http_listen`) can
  render the dashboard gauges and `/metrics` page without crossing
  listeners. The standalone scraper-facing endpoint is unchanged
  (Prometheus continues scraping it without bearer tokens).
- **English + 简体中文 i18n** for every UI string, with a coverage
  unit test that fails CI if a key drifts between bundles.

### Build & tooling

- New top-level `webui/` Vite project (sibling of `crates/`). Build
  with `pnpm install --frozen-lockfile && pnpm build`; output lands in
  `webui/dist/` and is consumed by `cargo build -p portunus-server`
  via the new `crates/portunus-server/build.rs` gate. Backend-only
  iteration: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server`
  emits a stub UI so `rust-embed` always has something to embed —
  release pipelines never set this env var.
- Bundle-size budget: gzipped main chunk ≤ 500 KB, enforced by
  `size-limit` at `pnpm build` time.

### Fixed

- **Self-rotation 401 race** in `useRotateCredential` — the post-mutation
  cache invalidation refetched the credentials list with the
  now-revoked old bearer, bouncing the operator to `/login` before the
  one-shot issuance modal could render the new token. The hook no
  longer auto-invalidates; `UserDetail.tsx` swaps the new bearer into
  `sessionStorage` first, then invalidates. The non-self path (a
  superadmin rotating someone else's credential) is unaffected.

### Constitution

- Closes the long-deferred `TODO(WEB_UI)` from constitution v1.x. No
  data-plane code paths touched (Principle II); SPA never bypasses the
  auth_layer (Principle I); both new endpoints get contract tests
  before implementation (Principle III); audit buffer never carries
  raw bearer tokens (Principle IV); RBAC isolation enforced server-
  side; the SPA renders whatever the server returns (Principle V).

## [0.5.0] — 2026-05-07

Multi-user RBAC for the operator API. The forwarding data plane (TCP +
UDP + DNS resolver + port-range) is **byte-identical** to v0.4.0; this
release adds an operator-side authorisation layer above it.

### Added

- **Multi-user identity store** (spec `005-multi-user-rbac`). Identity
  state lives in `<config_dir>/identity.json` (mode 0600, atomic-write
  JSON, schema v1) alongside the existing `tokens.json`. Three entity
  kinds:
  - `User` — id (lower-snake `[a-z][a-z0-9_-]{0,31}`, reserved `_`-prefix
    rejected through the public constructor), role (`superadmin` /
    `user`), display name, optional `disabled` flag.
  - `Credential` — blake3-hashed bearer token, optional label, status
    (`active` or `revoked` with timestamp), `last_used_at`.
  - `Grant` — per-user authorisation triple `{client, listen-port range,
    protocol set}`. `client` is either a named `ClientName` or wildcard
    `"*"`. Closed-set matching: a single grant must cover the entire
    requested listen range (R-004); rules straddling two grants are
    rejected.
- **RBAC enforcement** (FR-001..FR-014). Every operator HTTP request
  flows through one auth-layer seam (Constitution I): bootstrap-required
  503 → bearer extract → identity verify → audit log. Read-side
  filtering: `GET /v1/rules` projects only the caller's rules to
  non-superadmin users; superadmins additionally accept an
  `?owner=<user_id>` filter. `GET /v1/rules/{id}/stats` and
  `DELETE /v1/rules/{id}` enforce ownership before any work, returning
  403 `not_owner` to non-owners. Every rule response carries a new
  `owner` field stamped at push time (FR-014).
- **`bootstrap-superadmin` CLI subcommand** mints a `_superadmin` user
  + credential and prints the raw token to stdout EXACTLY ONCE.
  Idempotent on subsequent runs — refuses to bootstrap twice with a
  non-zero exit. Companion `gen-token` subcommand prints a fresh
  URL-safe-base64 token for out-of-band seeding.
- **`operator_token = "<token>"` server.toml shortcut**. On first start
  with no superadmin present, auto-bootstraps a reserved `_legacy`
  superadmin keyed to the configured token. Idempotent — leaving the
  line in across restarts is safe.
- **New HTTP endpoints** (all superadmin-only unless noted):
  `/v1/users` (POST/GET), `/v1/users/{id}` (GET/DELETE),
  `/v1/users/{id}/credentials` (POST/GET) — POST + the rotate/revoke
  variants are also accessible to the credential's own user
  (`not_owner` 403 otherwise),
  `/v1/users/{id}/credentials/{cred_id}` (DELETE),
  `/v1/users/{id}/credentials/{cred_id}/rotate` (POST),
  `/v1/grants` (POST/GET), `/v1/grants/{id}` (DELETE).
- **CLI surface** mirrors the HTTP API — new subcommands `user-add`,
  `user-list`, `user-get`, `user-remove`, `credential-issue`,
  `credential-list`, `credential-revoke`, `credential-rotate`,
  `grant-add`, `grant-list`, `grant-revoke`. All take the operator
  token from `PORTUNUS_OPERATOR_TOKEN` env (exit 4 if missing) and
  surface `RbacError` codes via the operator-api.md exit table.
- **Audit logging**. Every operator request emits one structured
  `event = "operator.allow"` (INFO) or `"operator.deny"` (WARN) line
  carrying `actor`, `method`, `path`, `outcome`, and (on deny) the
  `RbacError::code()` reason. **Raw bearer tokens NEVER reach the audit
  code path** (Constitution IV) — the audit emitter takes only the
  post-verify `OperatorIdentity`; the existing `RedactionLayer`
  continues to scrub legacy field names.
- **SIGHUP reload** (Linux + macOS). On `SIGHUP` the operator-store
  reloads `identity.json` from disk; on validation failure the prior
  in-memory snapshot is kept and one structured log line is emitted
  either way.

### Changed

- **Operator HTTP API now requires `Authorization: Bearer <token>`**.
  Pre-v0.5 unauthenticated callers get 401 `unauthenticated`. The data
  plane (gRPC client tokens, forwarding hot path) is unaffected.
- **Cascade ordering on user-removal / grant-revoke** (R-006): identity
  flush commits FIRST, then dependent rules are removed, so a crash
  mid-cascade leaves a coherent identity state. Last-superadmin
  protection refuses the removal that would orphan the cluster.
- **Per-rule Prometheus collectors gain an `owner` label**.
  `portunus_rule_bytes_in_total`, `_bytes_out_total`,
  `_active_connections`, `_dns_failures_total`, `_active_flows`,
  `_udp_datagrams_in_total`, `_udp_datagrams_out_total`,
  `_flows_dropped_overflow_total` all bump from `{client, rule}` to
  `{client, rule, owner}`. Cardinality budget unchanged: still one
  row per live rule (R-005). New
  `portunus_operator_requests_total{outcome, reason}` counter rolls up
  every operator HTTP request — `outcome` ∈ {allow, deny}; `reason`
  is `"ok"` on allow or the static `RbacError::code()` string on deny
  (bounded label set, R-009).
- **Rule responses across HTTP and CLI gain an `owner` field**
  (FR-014, byte-superset of v0.4.0).

### Migration notes

- **Fresh deploy** — set `operator_token = "<random-256-bit-token>"`
  in `server.toml` before first start. The server auto-bootstraps a
  `_legacy` superadmin on that token. Rotate to a real `_superadmin`
  user via `bootstrap-superadmin --name <display>` once a human is
  ready to take ownership.
- **Upgrade from v0.4.0** — the `tokens.json` client store carries
  over byte-identical; gRPC client tokens are unchanged. To unblock
  the operator path, either run `bootstrap-superadmin` once OR add
  `operator_token` to `server.toml`. Existing v0.4.0 CLI scripts that
  call `portunus-server push-rule` etc. need `PORTUNUS_OPERATOR_TOKEN`
  set in the environment; HTTP integrations need an `Authorization`
  header on every request.
- **Downgrade v0.5 → v0.4** — `identity.json` becomes inert;
  `operator_token` is silently ignored; auth-layer middleware is
  absent so every operator request is once again unauthenticated.
  RBAC state is lost (re-bootstrap required on a future v0.5+
  upgrade). Forwarding rules and `tokens.json` survive the round-trip
  unchanged.

### Verified

- **SC-001 (< 60 s onboarding from fresh deploy)** — covered by
  `crates/portunus-e2e/tests/rbac_smoke.rs::rbac_walkthrough_happy_and_violation_paths`,
  which mirrors `quickstart.md` § 1–7 (bootstrap → user-add →
  credential-issue → grant-add → push-rule → cascade → remove). Full
  walkthrough completes in **< 1 s** on a developer-class macOS host.
- **SC-003 (100 % violation rejection)** — covered by
  `portunus-server/tests/rbac_push_rule.rs` (6 violation paths) +
  `portunus-server/tests/http_grants_contract.rs` + the §3.1 block of
  `rbac_smoke.rs` (port_outside_grant / protocol_not_granted /
  client_not_granted each return 403 with the matching code).
- **SC-004 (legacy operator workflow byte-identical)** — the entire
  v0.4 e2e suite (`portunus-e2e/tests/{happy_path,udp_smoke,dns_smoke,
  range_smoke,scale,restart_recovery,…}`) passes verbatim under the
  v0.5 router after the test fixture writes `operator_token` to
  `server.toml`. Wire shape: rule responses carry the same fields
  plus `owner`, no removals.
- **SC-005 (revoke-grant cascade < 5 s)** — covered by
  `portunus-server/tests/http_grants_contract.rs::delete_grant_returns_grant_id_and_no_rules`
  + the rbac_smoke.rs §6 block. The cascade is in-process (one
  `RwLock::write`) — observed wall-clock is sub-millisecond.
- **SC-006 (restart roundtrip preserves identity)** — covered by
  `portunus-server/tests/identity_persistence.rs::full_round_trip_users_credentials_grants`:
  write `identity.json`, reopen via `FileOperatorStore::open`, all
  users / credentials / grants survive byte-identical.
- **SC-007 (operator CLI answers "who pushed X")** — covered by
  `portunus-server/tests/rbac_read_filtering.rs` (4 tests) +
  `portunus-server/tests/http_users_contract.rs`. Every rule response
  carries `owner`; `GET /v1/rules?owner=<id>` filters server-side.
- **R-005 (Prometheus cardinality budget preserved)** —
  `portunus-server/tests/rbac_metric_cardinality.rs` drives 5 rules ×
  3 owners through `RuleStatsCache::observe` with 3 observations per
  rule and asserts exactly 5 rows per per-rule collector. Owner
  label is verified to thread through end-to-end.
- **Constitution I (single-seam auth)** — `auth_middleware` in
  `portunus-server/src/operator/auth_layer.rs` is the **only**
  call site of `OperatorAuthenticator::verify`; the entire
  `/v1/*` router is mounted behind one `route_layer`. Verified by
  grep + `portunus-server/tests/legacy_no_auth_rejected.rs` (every
  unauthenticated request 401's).
- **Constitution II (data-plane untouched)** — `forwarder/proxy.rs`,
  `forwarder/udp/`, and `resolver/` are byte-identical to v0.4.0.
  The full v0.4 criterion suite (`data_plane`, `udp_data_plane`,
  `dns_resolver`) re-runs without modification.
- **Constitution IV (no raw tokens in logs)** —
  `portunus-server/tests/audit_log_redaction.rs` injects a known
  bearer through the auth-layer and asserts the captured tracing
  output never contains the token bytes; only the post-verify
  `actor` / `role` / `outcome` fields appear.

## [0.4.0] — 2026-05-07

UDP forwarding (additive on top of v0.3.0).

### Added

- **UDP forwarding** (spec `004-udp-forward`). Operator flips
  `--protocol udp` (CLI) / `"protocol": "udp"` (HTTP) on `push-rule` to
  activate a UDP rule. Each end-user `(addr, port)` gets its own
  kernel-allocated upstream `UdpSocket`, providing NAT-style return-path
  isolation — the kernel's source-port selection demuxes replies for
  free, so the proxy never tracks return-paths in userspace. UDP and
  TCP rules coexist on the same port (the conflict check now keys on
  protocol). Range rules and DNS-name targets work for UDP too: each
  port in a range spawns its own listener task with an independent flow
  table, all sharing the parent rule's `RuleStats` for aggregate
  roll-up; DNS targets reuse the v0.3.0 `LiveResolver` so cache +
  single-flight + IPv4-first preference + `dns_failures` semantics
  carry over verbatim. Per-flow state is reaped after
  `udp_flow_idle_secs` (server.toml, default 60s, range 30..=300);
  per-rule cap `udp_max_flows_per_rule` (default 1024, range
  1..=65535) bounds resource use under sustained churn — overflow drops
  increment the new `portunus_rule_flows_dropped_overflow_total`
  counter rather than evicting existing flows. Both knobs flow to the
  client over Welcome; v0.3.0 servers (no UDP fields) leave the client
  on the documented compile-time defaults.
- **Per-rule UDP collectors**: `portunus_rule_udp_datagrams_in_total`,
  `portunus_rule_udp_datagrams_out_total`, `portunus_rule_active_flows`,
  `portunus_rule_flows_dropped_overflow_total` (one row per rule —
  per-port detail stays out of `/metrics` to preserve the cardinality
  budget). `rule-stats` surfaces a `protocol` field plus the
  UDP-specific counters. The `--per-port` view extends to UDP range
  rules with per-port `datagrams_in/out` columns.

### Changed

- TCP hot path is **byte-identical** to v0.3.0 — `proxy.rs` is
  untouched, every existing TCP test passes (Constitution II / FR-010).
- `Hello.supported_protocols` gates UDP rules: pushing UDP at a v0.3.0
  client returns `unsupported_protocol` (HTTP 422 / exit 3).

### Verified

- **SC-002 (UDP datagram throughput)** — criterion bench
  `udp_data_plane.single_flow_throughput` in
  `crates/portunus-client/benches/udp_data_plane.rs` reports a median
  of **~51 µs** per full datagram round-trip (send + proxy fwd +
  echo + proxy back + recv = 4 datagram hops per iteration). At
  ~19.4k round-trips/s that is **~78k datagrams/s** through the
  proxy — comfortably above the 50,000 dgrams/s SC-002 floor.
- **SC-003 (per-flow isolation)** —
  `udp_listener_two_sources_isolated_replies` unit test in
  `crates/portunus-client/src/forwarder/udp/mod.rs` and the gated
  1000-source stress test `test_udp_us1_thousand_source_isolation`
  (cargo test --ignored) prove kernel-side per-flow upstream sockets
  give NAT-style isolation with zero misroutes.
- **SC-004 (UDP cardinality budget)** —
  `metrics::tests::active_flows_cardinality_is_one_row_per_rule`
  (asserts ≤ N rows for each of the 4 UDP collectors after observing
  N rules) and end-to-end test `test_udp_us3_metric_cardinality` in
  `crates/portunus-e2e/tests/udp_smoke.rs`. A 10-port UDP range with
  traffic on 3 ports produces exactly **1** row of
  `portunus_rule_udp_datagrams_in_total{rule=…}` (NOT 10).
- **SC-001 (push → first byte budget)** — `test_udp_us1_happy_path`
  reports wall-clock **~6.6 s** from server spawn through
  provision-client → push-rule → first datagram round-trip on a
  developer-class macOS host. Far under the 60 s budget.
- **Constitution II (TCP hot-path inspection)** — `forwarder/proxy.rs`
  is untouched in this release. The full v0.3.0 TCP test suite passes
  unmodified; the TCP data-plane criterion baseline is unchanged.

## [0.3.0] — 2026-05-07

Domain-name forwarding targets (additive on top of v0.2.0).

### Added

- **Domain-name forwarding targets** (spec `003-domain-name-forward`).
  The target host in any push-rule invocation may now be a DNS name
  (e.g. `api.example.com:443`) instead of an IP literal. Resolution
  happens lazily on first connect through `hickory-resolver` reading
  `/etc/resolv.conf`; results cache per the resolver-reported TTL
  clamped to `[5 s, 5 min]`. On refresh failure the rule stays Active
  and the last-known answer continues serving for up to 30 s of grace
  (RFC 8767 stale-while-error), then a fresh attempt is allowed every
  3 s (`negative_cache_retry`). Per-rule single-flight (FR-012)
  collapses concurrent first-connects to ONE upstream resolver call.
  Multi-A/AAAA fallback (FR-006) tries each returned address in
  family-preference order, so a single dead IP doesn't fail the
  connection. Address-family preference defaults to IPv4-first;
  operators flip per-rule with the new `--prefer-ipv6 / preferIpv6=true`
  flag (CLI + HTTP).
- **`portunus_rule_dns_failures_total{client,rule}`** per-rule
  monotonic counter on `/metrics` (one row per rule — SC-006
  cardinality budget preserved; the row is removed alongside
  `rule_active_connections` on `remove-rule`). Surfaced in `rule-stats`
  as a `dns_failures` field (always present, 0 for IP-target rules).

### Changed

- The hot path stays byte-identical for IP-literal targets (FR-010):
  the resolver layer short-circuits when `target_host` parses as an
  `IpAddr` and goes straight to `TcpStream::connect`.

### Verified

- **SC-004 (cache-hit hot path)** — criterion bench
  `dns_resolver_cache_hit` in
  `crates/portunus-client/benches/dns_resolver.rs` reports a median of
  **~75 ns** per warm-cache lookup (one async-mutex acquire +
  HashMap get + Vec clone). Three orders of magnitude under the
  loopback `connect()` budget, so adding a DNS rule does not regress
  the per-connection path.
- **FR-012 (single-flight under burst)** — criterion bench
  `dns_resolver_singleflight_100x` spawns 100 concurrent first-connects
  to the same unresolved hostname and asserts the resolver is invoked
  exactly **1** time; reported median wakeup latency ≈ **1.4 ms** for
  the full 100-task burst. Bench panics on any regression to >1 call.
- **SC-006 (per-rule metric cardinality)** —
  `metrics::tests::dns_failures_cardinality_is_one_row_per_rule` and
  end-to-end test `test_dns_us4_metric_cardinality` in
  `crates/portunus-e2e/tests/dns_smoke.rs`. Driving 6 failed connections
  through 2 rules pointing at `broken.invalid` produces exactly 2 rows
  of `portunus_rule_dns_failures_total`, each with value 3. Removing a
  rule drops the corresponding row.
- **Constitution II (hot-path inspection)** — IP-literal targets bypass
  the resolver entirely at
  `crates/portunus-client/src/resolver/mod.rs` (`connect_target`'s
  `IpAddr::from_str` short-circuit). The data-plane criterion baseline
  (v0.1.0 numbers) is unchanged for IP-only rules; the regression
  gate at `.github/workflows/bench.yml` continues to enforce ±25 %.

## [0.2.0] — 2026-05-07

### Added

- **Port-range forwarding rules** (additive, spec
  `002-port-range-forward`). Operators can now push a single forwarding
  rule that maps a contiguous listen-port range to a same-offset
  contiguous target-port range on one upstream host. The wire,
  persistence, HTTP, and CLI surfaces extend additively: existing
  single-port rules behave unchanged; range rules add optional
  `listen_port_end` / `target_port_end` fields. New server config
  `range_rule_max_ports` (default `1024`) caps any single range. New
  CLI flag `rule-stats <id> --per-port` exposes per-port counters
  on-demand (not via Prometheus — cardinality budget preserved).
  Range conflicts reuse the v1 `port_in_use` error code with the
  offending port named in the message.

### Verified

- **SC-001 (100-port range, fresh deploy)** — ran the recipe in
  `specs/002-port-range-forward/quickstart.md` § "Verifying SC-001 on
  a fresh host pair" against a Debian 13 (trixie) x86_64 host, glibc
  2.41, kernel 6.12.74, with both `portunus-server` and `portunus-client`
  on the same box talking loopback. Numbers (median of 3 fresh runs):
  - **Total wall clock** (server start → bundle issue → client connect
    → push 100-port range → traffic round-trip on 3 sample ports):
    **0.93 s** — well under the 5-minute SC-001 budget (≈300×).
  - **Range-push wall clock** (just the `push-rule edge-01
    30000-30099 127.0.0.1:41000-41099` invocation): **18 ms** — sub-second
    per quickstart prediction; the bind fan-out across 100 OS-assigned
    ports is comfortably linear.
  - **`list-rules`** returns one entry for the 100-port range
    (range collapses, FR-006).
  - **SC-002** — `/metrics` exposes exactly **1** row of
    `portunus_rule_bytes_in_total{rule="…"}` for the 100-port rule.
    Per-port detail surfaces only via the `?per_port=true` HTTP query,
    which returns a 100-element `per_port` array.

## [0.1.0] — 2026-05-06

Initial MVP release of the `001-tcp-forward-mvp` feature. Two binaries
(`portunus-server` and `portunus-client`) implementing the three user stories
from the spec end-to-end.

### Added

- **TLS + bearer-token auth** (Constitution Principle I, v2.0). Server
  generates a self-signed leaf cert on first run; the client pins it via
  SHA-256 fingerprint baked into the credential bundle. Bearer tokens are
  random 256-bit secrets stored in `tokens.json` (mode 0600). All identity
  decisions flow through `portunus_auth::Authenticator::verify` —
  `ClientIdentity` is the single source of truth used by every server
  handler.
- **Operator surface** (US1 + US2): CLI subcommands `provision-client`,
  `revoke`, `list-clients`, `push-rule`, `remove-rule`, `list-rules`,
  `rule-stats`. Loopback HTTP API `/v1/clients`, `/v1/rules`,
  `/v1/rules/{id}/stats` mirror the CLI for live operations against a
  running server.
- **Forwarding data plane** (US2): TCP rule push with `Pending → Active`
  state machine, 1 s ack target verified by integration test, deterministic
  drain on rule remove.
- **Observability** (US3): per-rule byte + active-connection counters
  reported every 5 s via gRPC `StatsReport`; cached server-side and exposed
  through `rule-stats` and Prometheus `/metrics` (loopback-only).
  Collectors: `portunus_clients_connected`,
  `portunus_auth_failures_total{reason}`,
  `portunus_rule_bytes_in_total{client,rule}`,
  `portunus_rule_bytes_out_total{client,rule}`,
  `portunus_rule_active_connections{client,rule}`.
- **Structured logs**: JSON layer enabled by default, `request_id`
  propagated through `RuleUpdate`/`RuleStatus`, redaction layer flags any
  log call referencing field names matching `token|secret|private_key`.
- **Graceful shutdown**: SIGINT/SIGTERM trigger drain; in-flight forwarded
  connections honour `--shutdown-drain-timeout-secs` (default 30 s) before
  the kernel reaps remaining sockets.

### Performance baseline

Baseline captured on macOS via the criterion harness in
`crates/portunus-client/benches/data_plane.rs`. Numbers are loopback,
single-rule, one bidirectional connection. The next hot-path-touching
spec is expected to wire CI regression gates against these:

| Workload                            | Median   | Throughput  |
| ----------------------------------- | -------- | ----------- |
| 64 KiB echo round-trip (throughput) | ~103 µs  | ~0.59 GiB/s |
| 1 MiB echo round-trip (throughput)  | ~817 µs  | ~1.19 GiB/s |
| 1-byte RTT through proxy (latency)  | ~44.9 µs |             |

Raw measurements live at
`crates/portunus-client/benches/baselines/v0.1.0.json` and the criterion
working dir at `target/criterion/.../v0.1.0/`. Re-capture with:

```sh
cargo bench -p portunus-client --bench data_plane -- --save-baseline v0.1.0
```

### SC-001 verification

Two passes of `quickstart.md`:

**1. Local-loopback (macOS, single host):** end-to-end in 8.1 s
post-build. The 6 s spike before `/metrics` reflects one StatsReport
tick at the default 5 s `--stats-report-interval-secs`. Hash equality
and the `rule-stats` / `/metrics` byte counters all matched the
104 857 600 byte payload.

**2. Real Linux host (Debian 13 trixie, x86_64, musl static binaries
cross-compiled from macOS via `cargo zigbuild`):** time-from-zero to
first byte through a pushed rule (`8080 → example.com:80`) measured
**1.262 s** post-binaries-on-disk:

| Step                            | t since T0 |
| ------------------------------- | ---------- |
| `server.listening`              | 0.224 s    |
| `POST /v1/clients` provisioned  | 0.473 s    |
| Client TLS connect + Welcome    | 0.968 s    |
| Rule push → Active              | 1.026 s    |
| First byte through proxy (200)  | 1.262 s    |

After driving 5×`curl` through the rule and waiting one StatsReport
tick: `bytes_in=450, bytes_out=5052` from `rule-stats`, and the same
numbers materialised on `/metrics` under
`portunus_rule_bytes_{in,out}_total{client="edge-01",rule="0"}`.
Both well under the 300 s SC-001 target.

### Out of scope (deferred)

- mTLS (Constitution v2.0.0 deliberately replaced cert-based client auth
  with bearer tokens). Tracked under future spec work.
- UDP forwarding, port-range rules, domain-name forwarding.
- Multi-user / RBAC (current design is single-operator with shell access
  to the server host).

[0.5.0]: https://github.com/ZingerLittleBee/Portunus/releases/tag/v0.5.0
[0.4.0]: https://github.com/ZingerLittleBee/Portunus/releases/tag/v0.4.0
[0.3.0]: https://github.com/ZingerLittleBee/Portunus/releases/tag/v0.3.0
[0.2.0]: https://github.com/ZingerLittleBee/Portunus/releases/tag/v0.2.0
[0.1.0]: https://github.com/Portunus/Portunus/releases/tag/v0.1.0
