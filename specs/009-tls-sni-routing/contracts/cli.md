# Contract: CLI — SNI Routing

**Feature**: 009-tls-sni-routing
**Phase**: 1 (Design & Contracts)

This contract defines the operator-facing `forward-server` CLI changes
introduced by v0.9. The CLI is a thin wrapper over the operator HTTP
API (see [`./operator-api.md`](./operator-api.md)); validation rules
match exactly.

---

## 1. `forward-server push-rule` (extended)

A new optional flag `--sni <PATTERN>` is added.

### 1.1 Synopsis

```text
forward-server push-rule \
    --client <NAME> \
    --protocol tcp \
    --listen-port <PORT> \
    --target <HOST:PORT> [--target ...] \
    [--sni <PATTERN>] \
    [--health-check-interval-secs <N>]
```

### 1.2 `--sni` semantics

- Optional. When omitted the rule is plain TCP / fallback (per the
  listener mode rules in [`../data-model.md`](../data-model.md)).
- When provided, MUST be one of:
  - An exact ASCII hostname (`api.example.com`)
  - A single-level wildcard (`*.example.com`)

### 1.3 Pre-API rejections (fast local errors)

The CLI rejects these combinations at parse time without contacting the
server:

| Combination | Exit | Stderr |
|---|---|---|
| `--sni` + `--protocol udp` | 2 | `--sni is only valid with --protocol tcp` |
| `--sni` + `--port-range` (or any range listen) | 2 | `--sni is only valid with single-port listeners` |
| `--sni` malformed (basic regex check) | 2 | `invalid --sni pattern: <reason>` |

Server-side conflicts (mode-flip, duplicate, capability-gate) come back
through the HTTP 4xx response and are surfaced as exit 1 with the
server's `error.detail`.

### 1.4 Examples

```bash
# Two SNI rules on :443 → two upstreams
forward-server push-rule --client edge-01 --protocol tcp --listen-port 443 \
    --target 10.0.1.5:8443 --sni api.example.com

forward-server push-rule --client edge-01 --protocol tcp --listen-port 443 \
    --target 10.0.1.6:8443 --sni '*.web.example.com'

# Optional fallback (catch-all for valid TLS without matching SNI)
forward-server push-rule --client edge-01 --protocol tcp --listen-port 443 \
    --target 10.0.1.7:8443
```

---

## 2. `forward-server list-rules --json` (extended)

Output JSON gains an optional `sni_pattern` per rule (omitted when
absent), matching the HTTP API. Human-readable output adds an `SNI`
column to the table; `—` is rendered when absent.

```bash
$ forward-server list-rules
ID  CLIENT   PROTO PORT  SNI                  TARGETS         STATE
42  edge-01  tcp   443   api.example.com      10.0.1.5:8443   Active
43  edge-01  tcp   443   *.web.example.com    10.0.1.6:8443   Active
44  edge-01  tcp   443   —                    10.0.1.7:8443   Active
```

---

## 3. `forward-server rule-stats` — UNCHANGED in shape

The v0.7 stats CLI is unchanged in v0.9. Operators read SNI counters
through `/metrics` (see `operator-api.md` §4). A future minor release
MAY thread the new counters into the CLI; out of scope for v0.9.

---

## 4. Help text

`forward-server push-rule --help` includes a new section:

```text
SNI routing (v0.9+):
  --sni <PATTERN>    Route TLS connections by Server Name Indication.
                     <PATTERN> is either an exact hostname (e.g.
                     'api.example.com') or a single-level wildcard
                     ('*.example.com'). Only valid with --protocol tcp
                     and a single --listen-port.

                     A port may host multiple SNI rules (each with a
                     distinct pattern) plus at most one rule without
                     --sni acting as the TLS-only fallback. To convert
                     an existing plain-TCP rule into a SNI listener,
                     remove it first, then push the new SNI rules.
```

---

## 5. Contract test plan

Tests live in `crates/forward-server/tests/cli/`.

| File | Asserts |
|---|---|
| `cli_push_rule_sni.rs` | `push-rule --sni api.example.com` succeeds; rule shows up in `list-rules --json` with the field. |
| `cli_push_rule_sni_validation.rs` | Each pre-API rejection in §1.3 exits 2 with the documented stderr. |
| `cli_push_rule_sni_server_error.rs` | A push that the server rejects (capability gate / overlap) exits 1 with the server's `error.detail` on stderr. |
| `cli_list_rules_sni_column.rs` | Human-readable output includes the SNI column; JSON output includes the field when present and omits it when absent. |
