# Changelog

All notable changes to `forward-rs` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **UDP forwarding** (additive, spec `004-udp-forward`). Operator
  flips `--protocol udp` (CLI) / `"protocol": "udp"` (HTTP) on
  `push-rule` to activate a UDP rule. Each end-user `(addr, port)`
  gets its own kernel-allocated upstream `UdpSocket`, providing
  NAT-style return-path isolation — the kernel's source-port
  selection demuxes replies for free, so the proxy never tracks
  return-paths in userspace. UDP and TCP rules coexist on the same
  port (the conflict check now keys on protocol). Range rules and
  DNS-name targets work for UDP too: each port in a range spawns
  its own listener task with an independent flow table, all sharing
  the parent rule's `RuleStats` for aggregate roll-up; DNS targets
  reuse the v0.3.0 `LiveResolver` so cache + single-flight +
  IPv4-first preference + `dns_failures` semantics carry over
  verbatim. Per-flow state is reaped after `udp_flow_idle_secs`
  (server.toml, default 60s, range 30..=300); per-rule cap
  `udp_max_flows_per_rule` (default 1024, range 1..=65535) bounds
  resource use under sustained churn — overflow drops increment the
  new `forward_rule_flows_dropped_overflow_total` counter rather
  than evicting existing flows. Both knobs flow to the client over
  Welcome; v0.3.0 servers (no UDP fields) leave the client on the
  documented compile-time defaults. The TCP hot path is
  byte-identical to v0.3.0 — `proxy.rs` is untouched, every existing
  TCP test passes (Constitution Principle II / FR-010). New
  Prometheus collectors observe the per-rule cardinality budget
  (one row per rule across `forward_rule_udp_datagrams_in_total`,
  `forward_rule_udp_datagrams_out_total`, `forward_rule_active_flows`,
  and `forward_rule_flows_dropped_overflow_total`), and `rule-stats`
  surfaces a `protocol` field plus the UDP-specific counters
  (datagrams_in/out, active_flows, flows_dropped_overflow). The
  `--per-port` view extends to UDP range rules with per-port
  `datagrams_in/out` columns.
- **Domain-name forwarding targets** (additive, spec
  `003-domain-name-forward`). The target host in any push-rule
  invocation may now be a DNS name (e.g. `api.example.com:443`) instead
  of an IP literal. Resolution happens lazily on first connect through
  `hickory-resolver` reading `/etc/resolv.conf`; results cache per the
  resolver-reported TTL clamped to `[5 s, 5 min]`. On refresh failure
  the rule stays Active and the last-known answer continues serving for
  up to 30 s of grace (RFC 8767 stale-while-error), then a fresh
  attempt is allowed every 3 s (`negative_cache_retry`). Per-rule
  single-flight (FR-012) collapses concurrent first-connects to ONE
  upstream resolver call. Multi-A/AAAA fallback (FR-006) tries each
  returned address in family-preference order, so a single dead IP
  doesn't fail the connection. Address-family preference defaults to
  IPv4-first; operators flip per-rule with the new
  `--prefer-ipv6 / preferIpv6=true` flag (CLI + HTTP). DNS resolution
  failures surface as a per-rule counter in `rule-stats` and as
  `forward_rule_dns_failures_total{client,rule}` on `/metrics` (one
  row per rule — SC-006 cardinality budget preserved; the row is
  removed alongside `rule_active_connections` on `remove-rule`). The
  hot path stays byte-identical for IP-literal targets (FR-010): the
  resolver layer short-circuits when `target_host` parses as an
  `IpAddr` and goes straight to `TcpStream::connect`.

### Verified

- **SC-002 (UDP datagram throughput)** — criterion bench
  `udp_data_plane.single_flow_throughput` in
  `crates/forward-client/benches/udp_data_plane.rs` reports a median
  of **~51 µs** per full datagram round-trip (send + proxy fwd +
  echo + proxy back + recv = 4 datagram hops per iteration). At
  ~19.4k round-trips/s that is **~78k datagrams/s** through the
  proxy — comfortably above the 50,000 dgrams/s SC-002 floor.
- **SC-003 (per-flow isolation)** — `udp_listener_two_sources_isolated_replies`
  unit test in `crates/forward-client/src/forwarder/udp/mod.rs` and
  the gated 1000-source stress test
  `test_udp_us1_thousand_source_isolation` (cargo test --ignored)
  prove kernel-side per-flow upstream sockets give NAT-style
  isolation with zero misroutes.
- **SC-004 (UDP cardinality budget)** — covered by
  `metrics::tests::active_flows_cardinality_is_one_row_per_rule`
  (asserts ≤ N rows for each of the 4 UDP collectors after
  observing N rules) and end-to-end test `test_udp_us3_metric_cardinality`
  in `crates/forward-e2e/tests/udp_smoke.rs`. A 10-port UDP range
  with traffic on 3 ports produces exactly **1** row of
  `forward_rule_udp_datagrams_in_total{rule=…}` (NOT 10).
- **SC-001 (push → first byte budget)** — `test_udp_us1_happy_path`
  reports wall-clock **~6.6 s** from server spawn through
  provision-client → push-rule → first datagram round-trip on a
  developer-class macOS host. Far under the 60 s budget; see the
  test's wait_for ack timeouts (`Some(3)`) for the per-step caps.
- **Constitution II (TCP hot-path inspection)** — `forwarder/proxy.rs`
  is untouched in this release. The full v0.3.0 TCP test suite
  (5 dns_smoke + 50 forwarder unit + 70 server unit) passes
  unmodified; the TCP data-plane criterion baseline is unchanged.

- **SC-004 (cache-hit hot path)** — criterion bench
  `dns_resolver_cache_hit` in `crates/forward-client/benches/dns_resolver.rs`
  reports a median of **~75 ns** per warm-cache lookup
  (one async-mutex acquire + HashMap get + Vec clone). Three orders of
  magnitude under the loopback `connect()` budget, so adding a DNS
  rule does not regress the per-connection path.
- **FR-012 (single-flight under burst)** — criterion bench
  `dns_resolver_singleflight_100x` spawns 100 concurrent first-connects
  to the same unresolved hostname and asserts the resolver is invoked
  exactly **1** time; reported median wakeup latency ≈ **1.4 ms** for
  the full 100-task burst. Bench panics on any regression to >1 call.
- **SC-006 (per-rule metric cardinality)** — covered by
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
  (`v0.1.0` numbers above) is unchanged for IP-only rules; the regression
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

[0.1.0]: https://github.com/forward-rs/forward-rs/releases/tag/v0.1.0
