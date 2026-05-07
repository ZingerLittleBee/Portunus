# forward-rs operator runbook (v0.1.0)

Day-1 install and day-2 operations for the
[`001-tcp-forward-mvp`](../specs/001-tcp-forward-mvp/spec.md) release.

## What you're deploying

| Component | Where | Purpose |
| --------- | ----- | ------- |
| `forward-server` | One control host | TLS-terminated gRPC for client control plane, loopback HTTP for operator commands, loopback `/metrics` for Prometheus |
| `forward-client` | Each edge host | Authenticates with bundle, accepts pushed rules, runs the TCP forwarders |
| `tokens.json` | `/var/lib/forward/` on server | Issued bearer tokens (mode 0600). Treat as secret. |
| `server.crt` + `server.key` | `/var/lib/forward/` on server | Self-signed TLS, auto-generated on first run. Key is mode 0600. |
| Client bundle | `/etc/forward/client.bundle.json` on each client | Server endpoint + cert pin + bearer token. Issued by `provision-client`. Treat as secret. |

Defaults follow the layout in `deploy/systemd/forward-server.service`
and `deploy/server.toml.example`. All non-loopback exposure is the gRPC
port (7443) only; operator HTTP and `/metrics` stay on `127.0.0.1`.

## Day-1: install via systemd (recommended)

On the **control host**:

```sh
# 1. Build (or copy a prebuilt binary in)
cargo build --release -p forward-server
sudo install -m 0755 target/release/forward-server /usr/local/bin/

# 2. Install unit + create user + write default config
sudo deploy/systemd/install.sh server

# 3. Edit /etc/forward/server.toml if you need a non-default endpoint
#    (e.g. bind control_listen to a specific interface)
sudo systemctl enable --now forward-server
journalctl -u forward-server -f
```

Wait for the `server.listening` event in the log, then provision your
first client (still on the control host):

```sh
sudo -u forward-server forward-server \
  --config-dir /var/lib/forward \
  provision-client edge-01 --out /tmp/edge-01.bundle.json
sudo cp /tmp/edge-01.bundle.json /tmp/edge-01.bundle.json   # for scp
```

`provision-client` is an offline operation against the same `tokens.json`
the server reads on startup. If the server is **already running**, prefer
the HTTP API instead (it keeps the in-memory cache in sync):

```sh
curl -sS -X POST -H 'content-type: application/json' \
  -d '{"name":"edge-01"}' \
  http://127.0.0.1:7080/v1/clients > /tmp/edge-01.bundle.json
```

Then on the **edge host**:

```sh
# 1. Build/copy binary
cargo build --release -p forward-client
sudo install -m 0755 target/release/forward-client /usr/local/bin/

# 2. Install unit + create user
sudo deploy/systemd/install.sh client

# 3. Land the bundle (received via scp)
sudo install -o root -g forward-client -m 0640 \
  edge-01.bundle.json /etc/forward/client.bundle.json

# 4. Start
sudo systemctl enable --now forward-client
journalctl -u forward-client -f
```

The first `control.connecting` log line should be followed by an INFO
event without `error=`; if you see `error="auth: token_not_found"`, the
bundle's token isn't in `tokens.json` — see Troubleshooting.

## Day-1: first rule

Back on the control host:

```sh
# Verify the client is connected
curl -s http://127.0.0.1:7080/v1/clients | jq
# → [{"client_name":"edge-01","connected":true,...}]

# Push a rule: edge-01:8080 → example.com:80
forward-server --config-dir /var/lib/forward \
  push-rule edge-01 8080 example.com:80
# → rule_id=0 state=active

# Verify on the edge host
curl -H "Host: example.com" http://127.0.0.1:8080/   # 200
```

For SC-001 timing reference: a fresh control host + edge host should go
from "no binaries on disk" to "first byte through a pushed rule" in
under 5 minutes. Real-Linux verification on Debian 13 measured 1.262 s
from `T0` to first byte (see `CHANGELOG.md`).

## Day-1: install via Docker (evaluation)

The provided compose is for local exploration, **not** production. Real
deployments should use systemd or your own orchestrator with proper
secret management.

```sh
docker compose -f deploy/docker/docker-compose.yml build server
docker compose -f deploy/docker/docker-compose.yml up -d server echo

# Provision a client (one-shot operator container)
docker compose -f deploy/docker/docker-compose.yml \
  run --rm operator provision-client edge-01 --out /shared/edge-01.bundle.json

# The bundle is now in the shared-bundles volume; copy it to the
# client host (or mount the same volume into the client container).
```

Distroless images deliberately ship without a shell. `docker exec` won't
work; for inspection use a sidecar:

```sh
docker run --rm --network container:forward-server \
  curlimages/curl -s 127.0.0.1:7081/metrics | grep forward_
```

## Day-2: operations

### Provision a new client

```sh
# Against a running server (preferred — no token-cache divergence):
curl -sS -X POST -H 'content-type: application/json' \
  -d '{"name":"edge-02"}' \
  http://127.0.0.1:7080/v1/clients > edge-02.bundle.json
```

### Revoke a client

```sh
forward-server --config-dir /var/lib/forward revoke edge-02
# Or via HTTP:
curl -sS -X POST http://127.0.0.1:7080/v1/clients/edge-02/revoke
```

Revocation is idempotent. The server immediately disconnects the client
(if connected) and rejects all future auth attempts with the revoked
token. The bundle file on the client side is now useless; delete it.

### Push / remove a rule

```sh
# Push: client edge-01, listen 8080, target example.com:80, TCP (default)
forward-server --config-dir /var/lib/forward \
  push-rule edge-01 8080 example.com:80

# List
forward-server --config-dir /var/lib/forward list-rules

# Remove
forward-server --config-dir /var/lib/forward remove-rule <rule_id>
```

`push-rule` waits up to its ack-timeout (default 2 s) for the client to
acknowledge `Active`. Failure modes: client offline, port already in use,
target unreachable from the client. See Troubleshooting.

### Managing port-range rules (v0.2.0+)

A range rule maps a contiguous listen-port window onto the same-offset
target window with one push:

```sh
# Listen 30000-30050 on edge-01, forward to upstream.local:30000-30050.
# The mapping is same-offset: listen 30007 → target 30007.
forward-server --config-dir /var/lib/forward \
  push-rule edge-01 30000-30050 upstream.local:30000-30050

# Per-port byte counters (range rules only):
forward-server --config-dir /var/lib/forward \
  rule-stats <rule_id> --per-port
```

Operational notes:

- **Cap.** `range_rule_max_ports` in `server.toml` (default `1024`)
  limits any single range. Pushing a larger range is rejected with HTTP
  400 `exceeds_cap`. Raising the cap requires raising
  `LimitNOFILE` in `forward-client.service` proportionally — each
  port consumes one TCP listener fd plus per-connection overhead.
- **Conflicts.** Overlapping ranges on the same client are rejected
  with HTTP 409 `port_in_use` and the offending port named in the
  message; the existing rule is unaffected. Different clients can
  bind the same port range without conflict.
- **All-or-nothing bind.** The client binds every port in the range
  before activating; if any single bind fails (port in use on the
  edge host, permission denied), all previously-bound listeners are
  released and the rule is reported `Failed` with the offending port.
- **Cardinality.** Prometheus collectors stay aggregate — one row per
  rule regardless of range size. `rule-stats --per-port` sources its
  per-port detail from a separate in-memory cache; that cache is
  not exported as Prometheus series. A 1024-port range adds one
  Prometheus row, not 1024.
- **Downgrade safety.** Range rules use additive proto fields; the
  v0.1.0 wire shape for single-port rules is byte-identical. A v0.2.0
  client paired with a v0.1.0 server still works for single-port
  rules.

### Replace the TLS cert (routable hostname)

The auto-generated cert SAN only covers `localhost`, `127.0.0.1`, and
the server's `hostname` output. To dial the server by a routable name
or public IP, replace the cert:

```sh
sudo systemctl stop forward-server

# Drop your operator-managed cert + key in place. Key MUST be mode 0600.
sudo install -o forward-server -g forward-server -m 0644 \
  my-cert.pem /var/lib/forward/server.crt
sudo install -o forward-server -g forward-server -m 0600 \
  my-key.pem /var/lib/forward/server.key

sudo systemctl start forward-server
```

**Re-issue every client bundle afterwards.** The bundle pins the cert by
SHA-256 fingerprint, so the old bundles won't validate the new cert:

```sh
# For each connected edge host:
forward-server --config-dir /var/lib/forward revoke edge-01
curl -sS -X POST -H 'content-type: application/json' \
  -d '{"name":"edge-01"}' \
  http://127.0.0.1:7080/v1/clients > edge-01.bundle.json
# scp + replace /etc/forward/client.bundle.json on the edge host
# systemctl restart forward-client
```

### Backup

What to preserve:

| Path | Why |
| ---- | --- |
| `/var/lib/forward/tokens.json` | Issued client tokens. Lose this and every client must be re-provisioned. |
| `/var/lib/forward/server.crt` + `server.key` | Same — without these, every bundle's cert pin will fail validation against a freshly auto-generated cert. |
| `/etc/forward/server.toml` | Operator config. |

A simple cron-driven `tar czf` snapshot of `/var/lib/forward` and
`/etc/forward` is sufficient. Restore is symmetric — stop the service,
unpack the archive, start the service.

## Observability

### Logs

JSON structured logs to stderr (captured by journald):

```sh
journalctl -u forward-server -f          # tail
journalctl -u forward-server --since "1 hour ago" | jq 'select(.fields.event)'
```

Key events:

| `event` | When | Who emits |
| ------- | ---- | --------- |
| `server.listening` | Server bound its three listeners | server |
| `client.connected` / `client.disconnected` | Client TLS handshake + Welcome ack succeeded/finished | server |
| `client.stats_report` | 5 s telemetry tick from a client | server |
| `audit.provision` / `audit.revoke` | Operator changed the token store | server |
| `audit.rule_push` (`outcome=sent` then `outcome=activated`) | Operator pushed a rule, client acked Active | server |
| `client.bundle_loaded` | Client read its bundle on startup | client |
| `control.connecting` / `control.terminal` | Client dialing/disconnect | client |

Sensitive field names (`token`, `secret`, `private_key`) are flagged by
the redaction layer with a counter; they should never appear in logs.

### Prometheus

`/metrics` exposes (loopback only):

| Metric | Labels | Meaning |
| ------ | ------ | ------- |
| `forward_clients_connected` | — | Currently-connected client count |
| `forward_auth_failures_total` | `reason` | Cumulative auth rejections |
| `forward_rule_bytes_in_total` | `client`, `rule` | Per-rule bytes received from upstream-of-proxy |
| `forward_rule_bytes_out_total` | `client`, `rule` | Per-rule bytes sent to target |
| `forward_rule_active_connections` | `client`, `rule` | Currently-open forwarded connections |

Counters with labels only materialise after the first observation.
`forward_auth_failures_total{reason="..."}` won't appear until at least
one bad-token attempt has hit the server.

### Rule-stats CLI

```sh
forward-server --config-dir /var/lib/forward rule-stats <rule_id>
# rule_id=0 client=edge-01 bytes_in=450 bytes_out=5052 active=0 updated_at=...
```

Numbers refresh on each 5 s `StatsReport` tick from the client.

## Troubleshooting

### Client logs `auth: token_not_found`

The bundle's token isn't in the server's `tokens.json`. Most common
causes:

1. **Bundle was issued offline against a different `--config-dir`.** Check
   that `forward-server provision-client` ran with the same `--config-dir`
   the running server uses (see `WorkingDirectory=` and `--config-dir` in
   the systemd unit, default `/var/lib/forward`).
2. **Server was started before the offline `provision-client` ran.** The
   server reads `tokens.json` once at startup. Either restart the server
   after offline provisioning, or use the HTTP API
   (`POST /v1/clients`) which mutates the live in-memory state.
3. **Token was revoked.** Re-issue with `provision-client` (or
   `POST /v1/clients`) and replace the bundle on the edge host.

### Client logs TLS handshake failure / cert mismatch

The bundle pins the server cert by SHA-256 fingerprint. If the cert
changed (auto-regenerated, or operator-replaced), every bundle becomes
invalid. Re-issue all bundles after any cert change.

If the cert is correct but the SAN doesn't include the address the
client is dialing, replace the cert with one whose SAN covers it (see
"Replace the TLS cert" above).

### `push-rule` returns `client_not_connected`

The named client has no active stream. Check:

```sh
curl -s http://127.0.0.1:7080/v1/clients | jq
```

If `connected=false`, look at the client host's `journalctl -u
forward-client` for the disconnect reason (auth failure, network, etc.).

### `push-rule` returns `port_in_use` / rule stays in `failed` state

The client refused to bind the listen port. Either another process owns
it, or a previous rule on the same port is still draining (default
30 s). `list-rules` shows current state; remove the conflicting rule
first.

### `/metrics` is missing some collectors

Label-bearing counters (`forward_rule_bytes_*`,
`forward_auth_failures_total`) only render after their first
observation. Push a rule and drive at least one byte through it before
expecting `forward_rule_bytes_in_total` to show up.

### Server won't start: `metrics_listen must bind to loopback`

You set `metrics_listen` (or `operator_http_listen`) to a non-loopback
address in `server.toml`. The server enforces this at startup per
Constitution Principle I — operator endpoints are unauthenticated and
trust shell access to the server host. Bind them to `127.0.0.1` and put
your scraper on the same host.

## Limitations (v0.1.0)

What this release does **not** do — listed explicitly so you don't go
looking for it:

- **No mTLS.** Authentication is TLS (server-only) + bearer token. The
  Constitution v2.0 deliberately replaced cert-based client auth with
  bearer tokens; tracked for future spec work.
- **No multi-operator / RBAC.** Anyone with shell access to the server
  host can provision clients and push rules. There's a single token
  store and no per-operator audit beyond the user that ran the systemd
  unit.
- **No domain-name forwarding, no UDP.** Each rule's target host
  resolves once at push time; subsequent DNS changes don't propagate
  until the rule is re-pushed. Only TCP is forwarded. Port-range
  rules ARE supported as of v0.2.0 — see "Managing port-range rules"
  above.
- **No hot reload of `server.toml`.** Config changes require a
  service restart.
- **No external secret management integration.** Bundles and tokens
  are plaintext on disk (mode 0600). Store them in your secret
  manager out-of-band if you need that posture.
