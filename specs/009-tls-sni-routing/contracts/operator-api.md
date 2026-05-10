# Contract: Operator HTTP API — SNI Routing

**Feature**: 009-tls-sni-routing
**Phase**: 1 (Design & Contracts)
**Authoritative reference**: [`../research.md`](../research.md),
[`../design.md`](../design.md)

This contract enumerates every change to the operator HTTP surface
(`/v1/...`) and the metric collectors served on `/metrics`. Anything not
listed here is byte-stable with v0.8.

---

## 1. `POST /v1/rules` (extended)

### 1.1 Request body

The existing JSON body grows one optional field:

```jsonc
{
  "client": "edge-01",
  "protocol": "tcp",
  "listen_port": 443,
  "targets": [{ "host": "10.0.1.5", "port": 8443, "priority": 0 }],
  "sni_pattern": "*.example.com"   // NEW; optional; omitted when absent
}
```

Field semantics:

- `sni_pattern: string` — exact host or `*.suffix` per the grammar in
  [`../data-model.md`](../data-model.md) §V-3.
- Field MUST be omitted (not `null`) when absent, to keep the v0.8
  byte-stable envelope (D8).

### 1.2 Validation responses

| Condition | HTTP | `error.code` |
|---|---|---|
| `sni_pattern` set but `protocol` is `udp` | 400 | `validation.sni_on_unsupported_rule` |
| `sni_pattern` set but `listen_port_end` is set | 400 | `validation.sni_on_unsupported_rule` |
| `sni_pattern` malformed (grammar fail) | 400 | `validation.sni_pattern_malformed` |
| `(client, listen_port, sni_pattern)` collision | 409 | `conflict.sni_route_duplicate` |
| Two `NULL` fallbacks | 409 | `conflict.sni_fallback_duplicate` |
| Mode flip on live listener | 409 | `conflict.legacy_to_sni_unsupported` |
| Range rule covers `listen_port` (existing v0.7 check) | 409 | `conflict.port_in_use` |
| Capability gate — see §3 | 422 | `sni_unsupported_by_client` |

Every error body includes `error.code` (machine-readable) and
`error.detail` (human-readable). Existing v0.8 error shape preserved.

### 1.3 Success response

201 with the persisted rule. New optional field:

```jsonc
{
  "id": 42,
  "client": "edge-01",
  "protocol": "tcp",
  "listen_port": 443,
  "targets": [...],
  "sni_pattern": "*.example.com",  // present when set; omitted otherwise
  "owner_user_id": "u-7",
  "created_at": "2026-05-08T10:00:00Z"
}
```

---

## 2. `GET /v1/rules` (extended)

The list response includes `sni_pattern` per rule, omitted when absent.
No new query parameters in v0.9 (operators filter client-side; FR-027).

---

## 3. Capability gate — `sni_unsupported_by_client`

When `POST /v1/rules` carries `sni_pattern.is_some()` and the targeted
`client` last reported `Hello.client_version < 0.9.0`:

- HTTP 422
- Response body:
  ```jsonc
  {
    "error": {
      "code": "sni_unsupported_by_client",
      "detail": "client edge-01 reported version 0.8.0; SNI routing requires >= 0.9.0",
      "client_name": "edge-01",
      "client_version": "0.8.0"
    }
  }
  ```
- The rule is NOT persisted; no `RuleUpdate` is sent over the bidi gRPC
  channel.

The check sits at the same control point as v0.7's multi-target gate
(`crates/portunus-server/src/operator/http.rs:353`). A new helper
`version_at_least_0_9(v: &str) -> bool` lives next to the existing
`version_at_least_0_7`.

---

## 4. `GET /metrics` — additive collectors

Five new collectors are registered on the existing Prometheus registry:

| Collector | Type | Labels | Source |
|---|---|---|---|
| `portunus_tls_sni_route_total` | counter | `client, rule, owner, result=exact|wildcard|fallback` | `RuleStats.sni_route_*_total` (3 fields → 3 series per rule) |
| `portunus_tls_sni_listener_miss_total` | counter | `client, port` | `SniListenerStats.sni_route_miss_total` |
| `portunus_tls_client_hello_parse_failures_total` | counter | `client, port` | `SniListenerStats.client_hello_parse_failures_total` |
| `portunus_tls_sni_routes_active` | gauge | _none_ | `ServerRuleStore` count of rules with `sni_pattern.is_some()` |

Label conventions follow `crates/portunus-server/src/metrics.rs:156` —
`client, rule, owner` for per-rule (matching v0.5..v0.8), `client, port`
for per-listener (no `rule` / `owner` because listener-level events
have no rule attribution).

No existing series is renamed. No existing series is reused with new
semantics — in particular `portunus_audit_buffer_drops_total` is **not**
touched (audit ring is for operator events only; SNI events are
diagnostic, see `../design.md` D13).

---

## 5. Audit ring (`GET /v1/audit`) — UNCHANGED

The SQLite `audit` ring continues to record only operator allow/deny
events. Data-plane SNI events (`tls.client_hello_timeout`,
`tls.parse_failed`, `tls.no_sni`, `tls.sni_no_match`, `tls.sni_routed`)
flow through portunus-client `tracing` only and are observable via the
structured log + the Prometheus counters above.

---

## 6. CLI mirror

`portunus-server push-rule --sni <pattern>` is documented in
[`./cli.md`](./cli.md). Validation is identical to the HTTP API; the
flag is rejected at parse time when combined with `--port-range` or
`--protocol udp` so operators get a fast local error.

---

## 7. Contract test plan

Tests live in `crates/portunus-server/tests/`.

| File | Asserts |
|---|---|
| `sni_rule_validation.rs` | Each malformed input from §1.2 produces the listed status + error.code. |
| `sni_capability_gate.rs` | A v0.8 client connection followed by `POST /v1/rules` with `sni_pattern` set returns 422 `sni_unsupported_by_client`; no rule appears in `GET /v1/rules`; no `RuleUpdate` is observed on the bidi channel. |
| `sni_overlap_matrix.rs` | Each row of [`../data-model.md`](../data-model.md) Overlap matrix produces the documented outcome. |
| `sni_legacy_to_sni_unsupported.rs` | Legacy plain-TCP rule active → push of SNI sibling → 409 with the specific code; remove first, then push succeeds. |
| `sni_metrics_surface.rs` | After a portunus-client emits `RuleStats` fields 13/14/15 and a `SniListenerStats` entry, `GET /metrics` exposes `portunus_tls_sni_route_total{client,rule,owner,result}` and `portunus_tls_sni_listener_miss_total{client,port}` with the expected values. |
| `sni_audit_ring_isolation.rs` | After mixed traffic that fires every tracing event listed in §5, `GET /v1/audit` is unchanged (zero new entries). |
