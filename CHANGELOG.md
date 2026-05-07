# Changelog

All notable changes to `forward-rs` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Domain-name forwarding targets** (additive, spec
  `003-domain-name-forward`). _In progress — body filled at quickstart
  verification (T058)._

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
