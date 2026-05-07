# Changelog

All notable changes to `forward-rs` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
  token from `FORWARD_OPERATOR_TOKEN` env (exit 4 if missing) and
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
  `forward_rule_bytes_in_total`, `_bytes_out_total`,
  `_active_connections`, `_dns_failures_total`, `_active_flows`,
  `_udp_datagrams_in_total`, `_udp_datagrams_out_total`,
  `_flows_dropped_overflow_total` all bump from `{client, rule}` to
  `{client, rule, owner}`. Cardinality budget unchanged: still one
  row per live rule (R-005). New
  `forward_operator_requests_total{outcome, reason}` counter rolls up
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
  call `forward-server push-rule` etc. need `FORWARD_OPERATOR_TOKEN`
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
  `crates/forward-e2e/tests/rbac_smoke.rs::rbac_walkthrough_happy_and_violation_paths`,
  which mirrors `quickstart.md` § 1–7 (bootstrap → user-add →
  credential-issue → grant-add → push-rule → cascade → remove). Full
  walkthrough completes in **< 1 s** on a developer-class macOS host.
- **SC-003 (100 % violation rejection)** — covered by
  `forward-server/tests/rbac_push_rule.rs` (6 violation paths) +
  `forward-server/tests/http_grants_contract.rs` + the §3.1 block of
  `rbac_smoke.rs` (port_outside_grant / protocol_not_granted /
  client_not_granted each return 403 with the matching code).
- **SC-004 (legacy operator workflow byte-identical)** — the entire
  v0.4 e2e suite (`forward-e2e/tests/{happy_path,udp_smoke,dns_smoke,
  range_smoke,scale,restart_recovery,…}`) passes verbatim under the
  v0.5 router after the test fixture writes `operator_token` to
  `server.toml`. Wire shape: rule responses carry the same fields
  plus `owner`, no removals.
- **SC-005 (revoke-grant cascade < 5 s)** — covered by
  `forward-server/tests/http_grants_contract.rs::delete_grant_returns_grant_id_and_no_rules`
  + the rbac_smoke.rs §6 block. The cascade is in-process (one
  `RwLock::write`) — observed wall-clock is sub-millisecond.
- **SC-006 (restart roundtrip preserves identity)** — covered by
  `forward-server/tests/identity_persistence.rs::full_round_trip_users_credentials_grants`:
  write `identity.json`, reopen via `FileOperatorStore::open`, all
  users / credentials / grants survive byte-identical.
- **SC-007 (operator CLI answers "who pushed X")** — covered by
  `forward-server/tests/rbac_read_filtering.rs` (4 tests) +
  `forward-server/tests/http_users_contract.rs`. Every rule response
  carries `owner`; `GET /v1/rules?owner=<id>` filters server-side.
- **R-005 (Prometheus cardinality budget preserved)** —
  `forward-server/tests/rbac_metric_cardinality.rs` drives 5 rules ×
  3 owners through `RuleStatsCache::observe` with 3 observations per
  rule and asserts exactly 5 rows per per-rule collector. Owner
  label is verified to thread through end-to-end.
- **Constitution I (single-seam auth)** — `auth_middleware` in
  `forward-server/src/operator/auth_layer.rs` is the **only**
  call site of `OperatorAuthenticator::verify`; the entire
  `/v1/*` router is mounted behind one `route_layer`. Verified by
  grep + `forward-server/tests/legacy_no_auth_rejected.rs` (every
  unauthenticated request 401's).
- **Constitution II (data-plane untouched)** — `forwarder/proxy.rs`,
  `forwarder/udp/`, and `resolver/` are byte-identical to v0.4.0.
  The full v0.4 criterion suite (`data_plane`, `udp_data_plane`,
  `dns_resolver`) re-runs without modification.
- **Constitution IV (no raw tokens in logs)** —
  `forward-server/tests/audit_log_redaction.rs` injects a known
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
  increment the new `forward_rule_flows_dropped_overflow_total`
  counter rather than evicting existing flows. Both knobs flow to the
  client over Welcome; v0.3.0 servers (no UDP fields) leave the client
  on the documented compile-time defaults.
- **Per-rule UDP collectors**: `forward_rule_udp_datagrams_in_total`,
  `forward_rule_udp_datagrams_out_total`, `forward_rule_active_flows`,
  `forward_rule_flows_dropped_overflow_total` (one row per rule —
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
  `crates/forward-client/benches/udp_data_plane.rs` reports a median
  of **~51 µs** per full datagram round-trip (send + proxy fwd +
  echo + proxy back + recv = 4 datagram hops per iteration). At
  ~19.4k round-trips/s that is **~78k datagrams/s** through the
  proxy — comfortably above the 50,000 dgrams/s SC-002 floor.
- **SC-003 (per-flow isolation)** —
  `udp_listener_two_sources_isolated_replies` unit test in
  `crates/forward-client/src/forwarder/udp/mod.rs` and the gated
  1000-source stress test `test_udp_us1_thousand_source_isolation`
  (cargo test --ignored) prove kernel-side per-flow upstream sockets
  give NAT-style isolation with zero misroutes.
- **SC-004 (UDP cardinality budget)** —
  `metrics::tests::active_flows_cardinality_is_one_row_per_rule`
  (asserts ≤ N rows for each of the 4 UDP collectors after observing
  N rules) and end-to-end test `test_udp_us3_metric_cardinality` in
  `crates/forward-e2e/tests/udp_smoke.rs`. A 10-port UDP range with
  traffic on 3 ports produces exactly **1** row of
  `forward_rule_udp_datagrams_in_total{rule=…}` (NOT 10).
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
- **`forward_rule_dns_failures_total{client,rule}`** per-rule
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
  `crates/forward-client/benches/dns_resolver.rs` reports a median of
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
  `crates/forward-e2e/tests/dns_smoke.rs`. Driving 6 failed connections
  through 2 rules pointing at `broken.invalid` produces exactly 2 rows
  of `forward_rule_dns_failures_total`, each with value 3. Removing a
  rule drops the corresponding row.
- **Constitution II (hot-path inspection)** — IP-literal targets bypass
  the resolver entirely at
  `crates/forward-client/src/resolver/mod.rs` (`connect_target`'s
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
  2.41, kernel 6.12.74, with both `forward-server` and `forward-client`
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
    `forward_rule_bytes_in_total{rule="…"}` for the 100-port rule.
    Per-port detail surfaces only via the `?per_port=true` HTTP query,
    which returns a 100-element `per_port` array.

## [0.1.0] — 2026-05-06

Initial MVP release of the `001-tcp-forward-mvp` feature. Two binaries
(`forward-server` and `forward-client`) implementing the three user stories
from the spec end-to-end.

### Added

- **TLS + bearer-token auth** (Constitution Principle I, v2.0). Server
  generates a self-signed leaf cert on first run; the client pins it via
  SHA-256 fingerprint baked into the credential bundle. Bearer tokens are
  random 256-bit secrets stored in `tokens.json` (mode 0600). All identity
  decisions flow through `forward_auth::Authenticator::verify` —
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
  Collectors: `forward_clients_connected`,
  `forward_auth_failures_total{reason}`,
  `forward_rule_bytes_in_total{client,rule}`,
  `forward_rule_bytes_out_total{client,rule}`,
  `forward_rule_active_connections{client,rule}`.
- **Structured logs**: JSON layer enabled by default, `request_id`
  propagated through `RuleUpdate`/`RuleStatus`, redaction layer flags any
  log call referencing field names matching `token|secret|private_key`.
- **Graceful shutdown**: SIGINT/SIGTERM trigger drain; in-flight forwarded
  connections honour `--shutdown-drain-timeout-secs` (default 30 s) before
  the kernel reaps remaining sockets.

### Performance baseline

Baseline captured on macOS via the criterion harness in
`crates/forward-client/benches/data_plane.rs`. Numbers are loopback,
single-rule, one bidirectional connection. The next hot-path-touching
spec is expected to wire CI regression gates against these:

| Workload                            | Median   | Throughput  |
| ----------------------------------- | -------- | ----------- |
| 64 KiB echo round-trip (throughput) | ~103 µs  | ~0.59 GiB/s |
| 1 MiB echo round-trip (throughput)  | ~817 µs  | ~1.19 GiB/s |
| 1-byte RTT through proxy (latency)  | ~44.9 µs |             |

Raw measurements live at
`crates/forward-client/benches/baselines/v0.1.0.json` and the criterion
working dir at `target/criterion/.../v0.1.0/`. Re-capture with:

```sh
cargo bench -p forward-client --bench data_plane -- --save-baseline v0.1.0
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
`forward_rule_bytes_{in,out}_total{client="edge-01",rule="0"}`.
Both well under the 300 s SC-001 target.

### Out of scope (deferred)

- mTLS (Constitution v2.0.0 deliberately replaced cert-based client auth
  with bearer tokens). Tracked under future spec work.
- UDP forwarding, port-range rules, domain-name forwarding.
- Multi-user / RBAC (current design is single-operator with shell access
  to the server host).

[0.5.0]: https://github.com/ZingerLittleBee/forward-rs/releases/tag/v0.5.0
[0.4.0]: https://github.com/ZingerLittleBee/forward-rs/releases/tag/v0.4.0
[0.3.0]: https://github.com/ZingerLittleBee/forward-rs/releases/tag/v0.3.0
[0.2.0]: https://github.com/ZingerLittleBee/forward-rs/releases/tag/v0.2.0
[0.1.0]: https://github.com/forward-rs/forward-rs/releases/tag/v0.1.0
