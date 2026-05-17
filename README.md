# Portunus

Port-based TCP and UDP forwarding service with a control-plane server,
edge client, and operator surface.

A `portunus-server` runs on a control host. Edge hosts run `portunus-client`,
authenticate over TLS + bearer token, and accept rule pushes from the
operator. Each rule binds a listener on the client (TCP `accept` loop or
UDP `recv_from` loop, per the rule's `protocol`) and forwards traffic to
a configured `host:port` target. Per-rule byte / connection / datagram
metrics flow back to the server every 5 seconds and are surfaced both
via `rule-stats` (operator CLI / HTTP) and Prometheus (`/metrics`,
loopback-only).

This repository is the v1.0.0 release. v1.0.0 is the first stable
Portunus release, preserving the wire / REST / SQLite-schema surfaces
from v0.11 and publishing release binaries plus GHCR Docker images.
The release notes and performance baseline are in
[`CHANGELOG.md`](CHANGELOG.md).

## Status

- Stable release (v1.0.0) — see [CHANGELOG](CHANGELOG.md).
- Rust 1.88, edition 2024, workspace of six crates (`portunus-proto`,
  `portunus-core`, `portunus-auth`, `portunus-server`, `portunus-client`,
  `portunus-e2e`).
- Auth model: TLS + bearer token. Cert-based client auth (mTLS) was
  deliberately removed in Constitution v2.0; see `.specify/memory/constitution.md`.

## Install

The fastest install is the one-line script (detects OS/arch, verifies
the release checksum):

```sh
# Edge host
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client
# Control plane host
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- server
```

Docker Compose is also supported; published images default to `:latest`:

```sh
docker pull ghcr.io/zingerlittlebee/portunus-server:latest
docker pull ghcr.io/zingerlittlebee/portunus-client:latest
```

Pin a release tag such as `:1.0.0` when you need a fully repeatable deploy.
See the Docker Compose guide in
[`docs/content/docs/deployment/docker.mdx`](docs/content/docs/deployment/docker.mdx).

Release binaries are published at
[`github.com/ZingerLittleBee/Portunus/releases/tag/v1.0.0`](https://github.com/ZingerLittleBee/Portunus/releases/tag/v1.0.0).

Source builds require Rust 1.88+ stable. `protoc` is vendored via
`prost-build`.

```sh
cargo build --release -p portunus-server -p portunus-client
# →  target/release/portunus-server
#    target/release/portunus-client
```

## Basic flow

```sh
# Operator (Host A) — bootstrap the superadmin operator account (v0.5.0+).
# Prints the bearer token EXACTLY ONCE — capture it now.
./target/release/portunus-server --data-dir ./srv bootstrap-superadmin --name ops
# →  superadmin user_id=_superadmin token=<paste-into-PORTUNUS_OPERATOR_TOKEN>

# Every operator subcommand below reads PORTUNUS_OPERATOR_TOKEN from env.
export PORTUNUS_OPERATOR_TOKEN=<paste-token-here>

# Operator — create a one-time enrollment command for the edge host.
./target/release/portunus-server --data-dir ./srv \
  enroll-client edge-01 --ttl-secs 600
# → portunus-client enroll 'portunus://host:7443/enroll?...'

# Host A — start the server (state.db + TLS material auto-generated)
./target/release/portunus-server --data-dir ./srv serve

# Host B — redeem the enrollment URI (writes the bundle), then start
./target/release/portunus-client enroll 'portunus://host:7443/enroll?...' --out ./client.bundle.json
./target/release/portunus-client --bundle ./client.bundle.json

# Operator — push a rule (8080 on edge-01 → example.com:80)
./target/release/portunus-server push-rule edge-01 8080 example.com:80

# Operator — push a port-range rule (30000-30050 → upstream.local:30000-30050)
./target/release/portunus-server push-rule edge-01 30000-30050 upstream.local:30000-30050

# Operator — push a UDP rule (v0.4.0+)
./target/release/portunus-server push-rule edge-01 6000 upstream.local:9999 --protocol udp

# UDP and TCP rules can coexist on the same port — the kernel demuxes by protocol
./target/release/portunus-server push-rule edge-01 6000 upstream.local:9999  # TCP:6000

# Operator — observe traffic
./target/release/portunus-server rule-stats <rule_id>
./target/release/portunus-server rule-stats <rule_id> --per-port  # range rules only
curl -s 127.0.0.1:7081/metrics | grep portunus_rule_bytes
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
`portunus_rule_dns_failures_total{client,rule}` in `/metrics`.

```sh
# DNS target — resolves api.example.com on first connect, caches TTL
./target/release/portunus-server push-rule edge-01 8443 api.example.com:443

# Same target, prefer IPv6 (AAAA-first; falls back to A if no AAAA)
./target/release/portunus-server push-rule edge-01 8444 api.example.com:443 --prefer-ipv6
```

IP-target rules from v0.2.0 keep their byte-identical hot path —
the resolver layer is short-circuited entirely.

## Multi-user RBAC (v0.5.0,
[`005-multi-user-rbac`](specs/005-multi-user-rbac/quickstart.md))

The operator API is now bearer-authed (`Authorization: Bearer <token>`
on every `/v1/*` request). Persistent state lives in
`<data-dir>/state.db`, and the optional config override file is
`<data-dir>/server.toml`. Two bootstrap paths:

```sh
# Path A — interactive single-shot bootstrap (recommended).
./target/release/portunus-server --data-dir ./srv bootstrap-superadmin --name ops
# Path B — server.toml shortcut. Add this once, restart, then remove.
#   operator_token = "<43-char URL-safe-base64 token>"
./target/release/portunus-server gen-token  # ← prints a fresh token to stdout
```

Add a constrained user, give them a credential, scope what they can push:

```sh
./target/release/portunus-server user-add alice --display-name Alice
./target/release/portunus-server credential-issue alice --label laptop
./target/release/portunus-server grant-add --user-id alice --client edge-01 \
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
PORTUNUS_OPERATOR_TOKEN=<alice's old token> \
  ./target/release/portunus-server credential-rotate alice <credential_id>
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

## Standalone forwarder (v1.4+)

`portunus-standalone` is a self-contained TCP/UDP forwarder driven by a
TOML file — no `portunus-server` required. It uses the same data-plane
code as `portunus-client`.

```sh
cargo build --release -p portunus-standalone
```

Minimal `portunus.toml`:

```toml
[[rule]]
name        = "ssh"
protocol    = "tcp"
listen_port = 2222
target      = "10.0.0.5:22"
```

Run or validate:

```sh
./target/release/portunus-standalone --config portunus.toml
./target/release/portunus-standalone --check --config portunus.toml  # exits 0 if valid
```

See [`docs/content/docs/operations/standalone.mdx`](docs/content/docs/operations/standalone.mdx)
for the full configuration reference, multi-target failover, PROXY
protocol, and systemd unit example.

## Deploy

Production scaffolding lives under [`deploy/`](deploy):

- [`deploy/systemd/`](deploy/systemd) — `portunus-server.service` and
  `portunus-client.service` with hardened defaults (`User=` + `ProtectSystem=`
  + `CapabilityBoundingSet=` etc.) plus an `install.sh` that creates the
  service users and lays out `/var/lib/portunus/` + `/etc/portunus/`.
- [`deploy/docker/`](deploy/docker) — `Dockerfile.server` and
  `Dockerfile.client` runtime images that copy prebuilt binaries into
  `distroless/cc:nonroot`, plus a local-only `docker-compose.yml` for
  kicking the tires.
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

`portunus-server` ships a single-page React UI on the operator HTTP
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
`cargo build --release -p portunus-server` so the bundle is embedded
into the binary at compile time. There is **no** runtime Node
dependency on the deployment host.

## Layout

```
crates/
  portunus-proto/    gRPC schema (control-plane) — generated by tonic-prost
  portunus-core/     IDs, errors, config, structured-log redaction layer
  portunus-auth/     Authenticator trait + FileTokenStore (mode 0600)
  portunus-server/   Control-plane binary: gRPC + operator HTTP + Prometheus
  portunus-client/   Edge binary: bidi gRPC stream + TCP forwarding listeners
  portunus-e2e/      Process-level integration tests
deploy/
  systemd/          portunus-{server,client}.service + install.sh
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
cargo bench -p portunus-client --bench data_plane -- --save-baseline v0.1.0
```

The criterion baseline lives at
`crates/portunus-client/benches/baselines/v0.1.0.json`. Re-running
`cargo bench` without `--save-baseline` compares against it.

CI runs a regression gate (`.github/workflows/bench.yml`) on PRs touching
the data-plane code: `scripts/bench_regression_gate.py` fails if any
benchmark's median is >25% slower than the committed baseline. When an
intentional perf change lands, recapture and commit the new numbers:

```sh
cargo bench -p portunus-client --bench data_plane -- --save-baseline v0.1.0
# regenerate the JSON summary (see CHANGELOG for the snippet that built it)
```

### Local multi-user demo

`make demo` stands up a complete, self-verifying multi-tenant
environment on loopback: it builds the binaries, starts the server,
starts the Vite Web UI on http://localhost:5173, creates N RBAC users
(each with its own grant + bearer token and an independent edge client),
pushes K real forwarding rules per user to local echo upstreams, runs a
real end-to-end TCP round-trip plus RBAC/cross-tenant checks, prints an
operator cheat-sheet (Web UI login, tokens, rule ids, listen ports, log
paths), then holds the environment open.

```sh
make demo                                         # 3 users × 2 rules, then hold open (Ctrl-C stops + cleans up)
make demo DEMO_ARGS="--users 5 --rules-per-user 3" # scale it
make demo DEMO_ARGS="--no-wait"                    # run + verify + exit (CI / quick regression)
make demo DEMO_ARGS="--keep"                       # reuse /tmp/portunus-demo, skip wipe/bootstrap
make demo DEMO_ARGS="--dry-run"                    # print the resolved topology only
```

Flags (forwarded to `scripts/demo.sh`): `--users N`,
`--rules-per-user K`, `--base-listen P` (default 18001), `--keep`,
`--disable-splice`, `--no-wait`, `--dry-run`. Once it prints
`demo ready`, log in at http://localhost:5173 with `_superadmin` and
the printed demo password (`portunus-demo-password` by default; override
with `PORTUNUS_DEMO_PASSWORD=...`), or exercise it by hand:

```sh
# data plane — bytes are forwarded through the edge client to the echo upstream
printf 'hello\n' | nc 127.0.0.1 18001

# monitoring — per-rule byte counters (token + rule id from the cheat-sheet)
curl -s -H "Authorization: Bearer <user-token>" \
  http://127.0.0.1:7080/v1/rules/<rule_id>/stats | jq
```

State lives in an isolated `/tmp/portunus-demo` (never touches the
`make dev` data dir). Stats refresh on the client's report interval
(~5 s), so a freshly sent payload takes a moment to show up.

## License

Dual-licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the
Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
