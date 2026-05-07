# forward-rs

Port-based TCP and UDP forwarding service with a control-plane server,
edge client, and operator surface.

A `forward-server` runs on a control host. Edge hosts run `forward-client`,
authenticate over TLS + bearer token, and accept rule pushes from the
operator. Each rule binds a listener on the client (TCP `accept` loop or
UDP `recv_from` loop, per the rule's `protocol`) and forwards traffic to
a configured `host:port` target. Per-rule byte / connection / datagram
metrics flow back to the server every 5 seconds and are surfaced both
via `rule-stats` (operator CLI / HTTP) and Prometheus (`/metrics`,
loopback-only).

This repository is the v0.4.0 release. v0.4.0 adds UDP forwarding on
top of v0.3.0's DNS-target support and v0.2.0's port-range rules; the
TCP hot path is byte-identical to v0.3.0 (Constitution Principle II).
The release notes and performance baseline are in
[`CHANGELOG.md`](CHANGELOG.md).

## Status

- Initial release (v0.1.0) — see [CHANGELOG](CHANGELOG.md).
- Rust 1.88, edition 2024, workspace of six crates (`forward-proto`,
  `forward-core`, `forward-auth`, `forward-server`, `forward-client`,
  `forward-e2e`).
- Auth model: TLS + bearer token. Cert-based client auth (mTLS) was
  deliberately removed in Constitution v2.0; see `.specify/memory/constitution.md`.

## Install

Prerequisites: Rust 1.88+ stable. `protoc` is vendored via `prost-build`.

```sh
cargo build --release -p forward-server -p forward-client
# →  target/release/forward-server
#    target/release/forward-client
```

## Basic flow

```sh
# Host A — start the server (TLS material + token store auto-generated)
./target/release/forward-server --config-dir ./srv serve

# Operator (Host A) — bootstrap the superadmin operator account (v0.5.0+).
# Prints the bearer token EXACTLY ONCE — capture it now.
./target/release/forward-server --config-dir ./srv bootstrap-superadmin --name ops
# →  superadmin user_id=_superadmin token=<paste-into-FORWARD_OPERATOR_TOKEN>

# Every operator subcommand below reads FORWARD_OPERATOR_TOKEN from env.
export FORWARD_OPERATOR_TOKEN=<paste-token-here>

# Operator — provision a forwarding client and copy the bundle
./target/release/forward-server --config-dir ./srv \
  provision-client edge-01 --out ./edge-01.bundle.json

# Host B — start the client against the issued bundle
./target/release/forward-client --bundle ./edge-01.bundle.json

# Operator — push a rule (8080 on edge-01 → example.com:80)
./target/release/forward-server push-rule edge-01 8080 example.com:80

# Operator — push a port-range rule (30000-30050 → upstream.local:30000-30050)
./target/release/forward-server push-rule edge-01 30000-30050 upstream.local:30000-30050

# Operator — push a UDP rule (v0.4.0+)
./target/release/forward-server push-rule edge-01 6000 upstream.local:9999 --protocol udp

# UDP and TCP rules can coexist on the same port — the kernel demuxes by protocol
./target/release/forward-server push-rule edge-01 6000 upstream.local:9999  # TCP:6000

# Operator — observe traffic
./target/release/forward-server rule-stats <rule_id>
./target/release/forward-server rule-stats <rule_id> --per-port  # range rules only
curl -s 127.0.0.1:7081/metrics | grep forward_rule_bytes
```

Range rules (v0.2.0,
[`002-port-range-forward`](specs/002-port-range-forward/quickstart.md))
collapse a contiguous listen-port window onto the same-offset target
window with a single push: `30000-30050 → host:30000-30050` binds 51
ports atomically and forwards each port to its same-offset target. The
default cap is 1024 ports per range (`range_rule_max_ports` in
`server.toml`). Per-port byte counters surface only via `--per-port`,
so the Prometheus cardinality budget stays one row per rule regardless
of range size.

DNS-name targets (v0.3.0,
[`003-domain-name-forward`](specs/003-domain-name-forward/quickstart.md)):
the target host in any rule may now be a DNS name instead of an IP
literal. The client resolves on first connect, caches per the
resolver-reported TTL clamped to `[5 s, 5 min]`, and serves the last
known answer for up to 30 s of grace if a refresh fails — the rule
stays Active throughout, individual connections fail fast with a
classified reason. Default address-family is IPv4-first; pass
`--prefer-ipv6` to flip the order per rule. DNS failure rate is
exposed per rule both via `rule-stats` and as
`forward_rule_dns_failures_total{client,rule}` in `/metrics`.

```sh
# DNS target — resolves api.example.com on first connect, caches TTL
./target/release/forward-server push-rule edge-01 8443 api.example.com:443

# Same target, prefer IPv6 (AAAA-first; falls back to A if no AAAA)
./target/release/forward-server push-rule edge-01 8444 api.example.com:443 --prefer-ipv6
```

IP-target rules from v0.2.0 keep their byte-identical hot path —
the resolver layer is short-circuited entirely.

## Multi-user RBAC (v0.5.0,
[`005-multi-user-rbac`](specs/005-multi-user-rbac/quickstart.md))

The operator API is now bearer-authed (`Authorization: Bearer <token>`
on every `/v1/*` request). State lives in `<config-dir>/identity.json`
alongside the existing `tokens.json`. Two bootstrap paths:

```sh
# Path A — interactive single-shot bootstrap (recommended).
./target/release/forward-server --config-dir ./srv bootstrap-superadmin --name ops
# Path B — server.toml shortcut. Add this once, restart, then remove.
#   operator_token = "<43-char URL-safe-base64 token>"
./target/release/forward-server gen-token  # ← prints a fresh token to stdout
```

Add a constrained user, give them a credential, scope what they can push:

```sh
./target/release/forward-server user-add alice --display-name Alice
./target/release/forward-server credential-issue alice --label laptop
./target/release/forward-server grant-add --user-id alice --client edge-01 \
  --listen-port-start 30000 --listen-port-end 30050 --protocols tcp,udp
```

Now alice can `push-rule` only on `edge-01`, only ports `30000..=30050`,
only TCP or UDP. Outside that envelope she gets HTTP 403 with one of
`client_not_granted`, `port_outside_grant`, or `protocol_not_granted`.
A grant whose `--client *` matches any client; a grant whose
`--listen-port-end` equals `--listen-port-start` is a single-port grant.
Closed-set matching: a single grant must cover the entire requested
listen range — rules straddling two grants are rejected.

`GET /v1/rules` projects only the caller's owned rules to non-superadmin
users; superadmin gets `?owner=<user_id>` to filter by owner. Every
rule response carries an `owner` field stamped at push time. Audit
log: every operator request emits one structured `event =
"operator.allow"` (INFO) or `"operator.deny"` (WARN); raw bearer
tokens never reach the audit code path (Constitution Principle IV).

Self-service credential rotate: alice authenticates with her current
token and rotates it herself; the response carries a fresh token, the
old token then 401s on subsequent requests:

```sh
FORWARD_OPERATOR_TOKEN=<alice's old token> \
  ./target/release/forward-server credential-rotate alice <credential_id>
```

The data plane (gRPC client tokens, TCP/UDP forwarding hot path, DNS
resolver, range rules) is **byte-identical** to v0.4.0 — every existing
forwarding test passes verbatim under the v0.5 router after the test
fixtures add the bearer header. See
[`specs/005-multi-user-rbac/contracts/operator-api.md`](specs/005-multi-user-rbac/contracts/operator-api.md)
for the full HTTP surface and exit-code table.

The full step-by-step walkthrough — including key fingerprint pinning,
revocation, and the SC-001 5-minute target — is in
[`specs/001-tcp-forward-mvp/quickstart.md`](specs/001-tcp-forward-mvp/quickstart.md).

## Deploy

Production scaffolding lives under [`deploy/`](deploy):

- [`deploy/systemd/`](deploy/systemd) — `forward-server.service` and
  `forward-client.service` with hardened defaults (`User=` + `ProtectSystem=`
  + `CapabilityBoundingSet=` etc.) plus an `install.sh` that creates the
  service users and lays out `/etc/forward/` + `/var/lib/forward/`.
- [`deploy/docker/`](deploy/docker) — `Dockerfile.server` and
  `Dockerfile.client` (multi-stage `rust:1.88` → `distroless/cc:nonroot`)
  and a local-only `docker-compose.yml` for kicking the tires.
- [`deploy/server.toml.example`](deploy/server.toml.example) —
  documented sample config matching the systemd unit layout.

Day-1 install, day-2 operations (provision, revoke, replace cert,
backup), observability, and an honest list of v0.1.0 limitations are
in [`docs/runbook.md`](docs/runbook.md).

## Operator API

The CLI subcommands and the loopback HTTP API
(`http://127.0.0.1:7080/v1/...`) are documented in
[`specs/001-tcp-forward-mvp/contracts/operator-api.md`](specs/001-tcp-forward-mvp/contracts/operator-api.md).
Exit codes and HTTP status mappings are frozen at v1.

## Web UI

`forward-server` ships a single-page React UI on the operator HTTP
listener (loopback by default). Open the listener address in a modern
browser (Chrome / Firefox / Safari / Edge — latest two releases),
paste your operator bearer token at the login screen, and you get:

- Dashboard, Users, Credentials, Grants, Rules, Clients, Audit log,
  Metrics, Settings.
- Live per-rule stats over Server-Sent Events (5 s cadence; falls back
  to plain polling if SSE is blocked).
- English + 简体中文 (toggle in Settings; remembered across reloads).
- Light / dark / `prefers-color-scheme` themes.

The UI never stores tokens in `localStorage` or cookies — the bearer
lives in `sessionStorage` only and is flushed on browser close. Every
SPA request flows through the same `auth_layer` middleware the CLI
uses; tenants see only their own rules / credentials, superadmins see
everything.

Remote access stays an operator concern (the listener is loopback-pinned
at startup): SSH-tunnel `127.0.0.1:7080` from your workstation, or sit
the listener behind a reverse proxy that adds its own auth.

Build instructions for the SPA live in
[`webui/README.md`](webui/README.md). Release pipelines run
`pnpm install --frozen-lockfile && pnpm build` before
`cargo build --release -p forward-server` so the bundle is embedded
into the binary at compile time. There is **no** runtime Node
dependency on the deployment host.

## Layout

```
crates/
  forward-proto/    gRPC schema (control-plane) — generated by tonic-prost
  forward-core/     IDs, errors, config, structured-log redaction layer
  forward-auth/     Authenticator trait + FileTokenStore (mode 0600)
  forward-server/   Control-plane binary: gRPC + operator HTTP + Prometheus
  forward-client/   Edge binary: bidi gRPC stream + TCP forwarding listeners
  forward-e2e/      Process-level integration tests
deploy/
  systemd/          forward-{server,client}.service + install.sh
  docker/           Dockerfile.{server,client} + local docker-compose.yml
  server.toml.example
docs/
  runbook.md        Day-1 install, day-2 ops, troubleshooting
specs/001-tcp-forward-mvp/
  spec.md           User stories + acceptance criteria
  plan.md           Architecture, dependencies, technical context
  data-model.md     Entities and state machines
  contracts/        Wire formats (proto, operator-api, persistence)
  quickstart.md     Two-host walkthrough
  tasks.md          Implementation task list (drives /speckit-implement)
.specify/memory/constitution.md  Project principles (auth model, perf gates, etc.)
```

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo bench -p forward-client --bench data_plane -- --save-baseline v0.1.0
```

The criterion baseline lives at
`crates/forward-client/benches/baselines/v0.1.0.json`. Re-running
`cargo bench` without `--save-baseline` compares against it.

CI runs a regression gate (`.github/workflows/bench.yml`) on PRs touching
the data-plane code: `scripts/bench_regression_gate.py` fails if any
benchmark's median is >25% slower than the committed baseline. When an
intentional perf change lands, recapture and commit the new numbers:

```sh
cargo bench -p forward-client --bench data_plane -- --save-baseline v0.1.0
# regenerate the JSON summary (see CHANGELOG for the snippet that built it)
```

## License

Apache-2.0. See workspace `Cargo.toml`.
