# Quickstart: TLS SNI Routing (v0.9)

**Feature**: 009-tls-sni-routing
**Phase**: 1 (Design & Contracts)
**Audience**: operators integrating v0.9 into an existing v0.8 deployment.

This walkthrough takes you from a fresh v0.9 install to two TLS-bearing
services sharing a single port (`:443`), plus a TLS-only fallback. It
finishes by demonstrating the legacy → SNI conversion workflow and
showing where to look on `/metrics` to verify everything is plumbed
correctly.

Total wall-clock time: ~10 minutes.

---

## 0. Prerequisites

- `forward-server` and `forward-client` binaries built from the v0.9
  branch.
- Two TCP-only HTTPS-style backends reachable from the forward-client
  (anything that returns bytes is fine — `nc -lk -p 8443 < banner.bin`
  works).
- `openssl` 3.x (for `s_client -servername`).
- An operator-API admin token (issued via `forward-server bootstrap-superadmin`
  per v0.5 RBAC; the v0.8 procedure is unchanged).
- Existing v0.8 `<data-dir>/state.db`, OR a fresh data dir — the v0.9
  binary auto-runs migration V002 on first boot.

---

## 1. Start the v0.9 server

```bash
forward-server serve \
    --data-dir /var/lib/portunus \
    --config-dir /etc/portunus
```

On first boot you will see (among other lines):

```text
INFO  store: applied migration V002 add_sni_pattern
INFO  serve: schema version 2 (range 1..=2)
```

If you ever need to roll back to v0.8 the column is additive — v0.8
binaries open a state.db migrated to V002 just fine, ignoring the new
column.

---

## 2. Connect a v0.9 client

```bash
forward-client run \
    --bundle /etc/portunus/edge-01.bundle.json
```

Confirm the server logged the version:

```bash
$ curl -sH "Authorization: Bearer $ADMIN" \
       https://forward-server.local/v1/clients | jq '.[0].client_version'
"0.9.0"
```

A v0.8 client connecting against the same server still works for plain
TCP rules — the SNI feature gates only kick in when you push a
`sni_pattern`.

---

## 3. Push two SNI rules on the same port

```bash
# Exact hostname → backend A
forward-server push-rule edge-01 443 10.0.1.5:8443 \
    --protocol tcp --sni api.example.com

# Single-label wildcard → backend B
forward-server push-rule edge-01 443 10.0.1.6:8443 \
    --protocol tcp --sni '*.web.example.com'
```

> Argument shape: `forward-server push-rule <client> <listen> [target]
> [--protocol tcp|udp] [--sni <pattern>] ...`. `<listen>` is a single
> port or `start-end` range; `[target]` is `host:port` (or `host:start-end`
> for ranges). The contract spec at `contracts/cli.md` uses
> `--client/--listen-port/--target` named flags — the implementation
> currently keeps the v0.1 positional shape. Both forms surface the
> same `--sni` flag.

Both pushes return immediately. The forward-client binds `:443` once
(the first push), then adds a second rule to the existing SNI listener
without rebinding (the second push). In-flight connections — there are
none yet, but verify under load — are not interrupted.

`list-rules` should show:

```text
$ forward-server list-rules --client edge-01
ID     CLIENT               PORT   TARGET                           SNI                      STATE
1      edge-01              443    10.0.1.5:8443                    api.example.com          active
2      edge-01              443    10.0.1.6:8443                    *.web.example.com        active
```

---

## 4. Verify routing with `openssl s_client`

```bash
# Reaches backend A (10.0.1.5:8443)
openssl s_client -connect edge-01.public:443 -servername api.example.com -quiet

# Reaches backend B (10.0.1.6:8443) via wildcard
openssl s_client -connect edge-01.public:443 -servername anything.web.example.com -quiet

# Reaches backend B too — wildcard matches one label
openssl s_client -connect edge-01.public:443 -servername foo.web.example.com -quiet

# REJECTED — wildcard does not match an extra label
openssl s_client -connect edge-01.public:443 -servername a.b.web.example.com -quiet
# ⇒ TCP reset; structured event `tls.sni_no_match` on the client
```

Note that Portunus **does not present a TLS certificate** — it
forwards bytes. `openssl s_client` will see whatever cert the upstream
serves.

---

## 5. Add a TLS-only fallback (optional)

Need a destination for valid TLS connections with no SNI extension or
an SNI that matches nothing? Push a rule on the same port without
`--sni`:

```bash
forward-server push-rule edge-01 443 10.0.1.7:8443 --protocol tcp
```

The listener stays in SNI mode (it was first activated as SNI when you
pushed rule 1). The new rule slots in as the fallback.

```text
$ forward-server list-rules --client edge-01
ID     CLIENT               PORT   TARGET                           SNI                      STATE
1      edge-01              443    10.0.1.5:8443                    api.example.com          active
2      edge-01              443    10.0.1.6:8443                    *.web.example.com        active
3      edge-01              443    10.0.1.7:8443                    -                        active
```

A second `push-rule` without `--sni` on this port now fails with
`409 conflict.sni_fallback_duplicate`.

A v1.0 plain HTTP request on this port still fails — `tls.parse_failed`
is emitted; the listener requires a parseable ClientHello.

---

## 6. Legacy → SNI conversion workflow

If `:444` was already serving plain TCP and you want to add SNI to it,
the **operationally honest** workflow is:

```bash
# 1. Identify the existing legacy rule (filter the table for `:444`)
$ forward-server list-rules --client edge-01 | awk '$3 == 444'

# 2. Remove it (this drains existing connections per v0.7 semantics)
$ forward-server remove-rule <ID>

# 3. Push the new SNI rules. The listener re-binds in SNI mode.
$ forward-server push-rule edge-01 444 10.0.2.5:8443 \
    --protocol tcp --sni api.internal
```

Trying to skip step 2 returns:

```text
HTTP 409 conflict.legacy_to_sni_unsupported
detail: port 444 has an active plain-TCP rule;
        remove rule <ID> before pushing SNI rules.
```

This is a **deliberate design choice** (R-004). Online conversion
would require migrating an active accept loop between byte-passthrough
and ClientHello-peek modes, which we considered too risky for v0.9.

---

## 7. Verifying via `/metrics`

After running mixed traffic for a minute, scrape `/metrics`:

```bash
$ curl -s http://127.0.0.1:7090/metrics | grep ^forward_tls
forward_tls_sni_route_total{client="edge-01",owner="u-7",result="exact",rule="1"} 124
forward_tls_sni_route_total{client="edge-01",owner="u-7",result="wildcard",rule="2"} 41
forward_tls_sni_route_total{client="edge-01",owner="u-7",result="fallback",rule="3"} 8
forward_tls_sni_listener_miss_total{client="edge-01",port="443"} 2
forward_tls_sni_listener_parse_failures_total{client="edge-01",port="443"} 1
forward_tls_sni_routes_active 3
```

> The `/metrics` endpoint binds loopback-only and does NOT require
> bearer auth — it's the same v0.5+ surface. Default port is the one
> the server logged at boot (`server.metrics_listen` in
> `server.toml`); the snippet above uses the dev-default
> `127.0.0.1:7090`.

Reading the labels:

- `result=exact|wildcard|fallback` — how the connection was matched.
  No `result=miss` label exists by design (miss has no rule attribution).
- `client, rule, owner` — same triple v0.5+ uses for every per-rule
  series; one Grafana dashboard fits all.
- The two listener-level counters use `client, port` instead, since
  miss / parse-failure happen before any rule is selected.

---

## 8. What does NOT change

Worth calling out explicitly:

- `GET /v1/audit` is **unchanged**. Data-plane SNI events (timeouts,
  parse failures, no-match) flow through `tracing` only. The audit
  ring is still reserved for operator allow/deny actions.
- The forwarding hot path on legacy plain-TCP listeners is **byte-stable**
  with v0.8 — no peek, no parse, no allocation introduced.
- The wire protocol's `RuleUpdate` is **single-rule** as before. The
  v0.9 client groups SNI rules locally via `PortGroupManager`.
- Existing v0.8 rules in your database keep working with no change.

---

## 9. Rolling back

If you need to roll back to a v0.8 binary:

1. Stop the v0.9 server.
2. Drop any rule with a non-NULL `sni_pattern` (or accept that v0.8
   simply ignores the column).
3. Start v0.8. The schema column is additive; v0.8 reads the table
   ignoring the new column. The helper index is partial and harmless.

Tag your v0.9 deployment so the `state.db` provenance is auditable.

---

## 10. Where to read more

- [`spec.md`](./spec.md) — user stories, FRs, SCs.
- [`design.md`](./design.md) — full technical decisions + three rounds
  of code-review history.
- [`research.md`](./research.md) — every R-NNN decision with
  rationale and alternatives.
- [`data-model.md`](./data-model.md) — entities, schema delta, lookup
  algorithm.
- [`contracts/wire.md`](./contracts/wire.md) — proto field numbers.
- [`contracts/operator-api.md`](./contracts/operator-api.md) — HTTP
  shapes, error codes, metrics catalogue.
- [`contracts/cli.md`](./contracts/cli.md) — CLI shapes.
