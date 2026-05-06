# Changelog

All notable changes to `forward-rs` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

### SC-001 dry-run

Local-loopback walkthrough of `quickstart.md` (two terminals on macOS,
single host) completed end-to-end in **8.1 seconds** post-build:

| Step                                 | Δ vs prev |
| ------------------------------------ | --------- |
| `serve` start → `server.listening`   | 0.16 s    |
| `provision-client` via HTTP API      | 0.05 s    |
| Client TLS connect + Welcome         | 0.72 s    |
| Rule push → Active                   | 0.32 s    |
| 100 MB `/dev/urandom` payload prep   | 0.54 s    |
| Stream 100 MB through proxy          | 0.19 s    |
| `/metrics` reports 100 MB cumulative | 6.05 s    |
| `remove-rule` returns 204            | 0.03 s    |

The 6 s spike before `/metrics` reflects one StatsReport tick at the
default 5 s `--stats-report-interval-secs`. Hash equality and the
`rule-stats` / `/metrics` byte counters all matched the 104 857 600 byte
input. Two-real-Linux-hosts validation (T068) remains pending; the
loopback timing strongly suggests the < 5 min SC-001 target will hold
once the cross-host hop is added.

### Out of scope (deferred)

- mTLS (Constitution v2.0.0 deliberately replaced cert-based client auth
  with bearer tokens). Tracked under future spec work.
- UDP forwarding, port-range rules, domain-name forwarding.
- Multi-user / RBAC (current design is single-operator with shell access
  to the server host).

[0.1.0]: https://github.com/forward-rs/forward-rs/releases/tag/v0.1.0
