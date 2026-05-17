# Advertised Endpoint: Runtime-Configurable (SQLite + Web UI)

Date: 2026-05-17
Status: Approved (design)

## Problem

The `host:port` baked into newly-issued client credential bundles
(`portunus://{endpoint}/enroll?...`) is captured **once at server
startup** from the CLI flag `--advertised-endpoint`. On Railway the
flag is bridged from the `PORTUNUS_ADVERTISED_ENDPOINT` env var by
`deploy/railway/start-server.sh`.

Consequences:

- Changing the advertised endpoint requires a restart / redeploy.
- The value is invisible to the operator (no Web UI surface).
- `PORTUNUS_ADVERTISED_ENDPOINT` is **not** a real env var the binary
  reads; it only exists as a manual flag bridge in the Railway script.

The advertised endpoint is the **gRPC control-plane** `host:port`
(`control_listen`, default `7443`) — a *different* listener from the
operator HTTP / Web UI port. On split-domain platforms (Railway: HTTP
public domain ≠ TCP Proxy host:port) it cannot be reliably derived from
the browser URL, so an explicit operator-set override is required.

## Goals

- Operator can view and set the advertised endpoint from the Web UI
  without a restart; the value persists in SQLite.
- Sensible zero-config default for simple/local deployments
  (auto-derive from the browser request host + the server's resolved
  control-plane port).
- Preserve headless / offline bundle issuance (CLI flag) — the offline
  `enroll-client` path has no HTTP request to derive from.
- The full value is a `host:port` (Railway TCP Proxy maps the internal
  `7443` to a different public port; host-only is insufficient).

## Non-Goals

- No change to the client. The enrolled bundle already carries the
  endpoint (and pinned cert) atomically; `portunus-client` reads it
  once at startup. Out of scope.
- No change to the bundle wire/URI format.
- No new Prometheus metrics.

## Resolution Order (Approach B — layered fallback, no seeding)

At **issuance time**, the advertised endpoint is resolved in this
order:

1. **SQLite operator-set value** (`server_settings.advertised_endpoint`,
   non-empty) — highest priority. This is what a Railway operator sets
   once in the Web UI to the TCP-Proxy `host:port`.
2. **Startup seed** — CLI flag `--advertised-endpoint`, optionally
   bound to clap `env = "PORTUNUS_ADVERTISED_ENDPOINT"`. Held in
   `AppState` as `Option<String>`.
3. **Auto-derive** — hostname from the operator HTTP request `Host`
   header + the server's startup-resolved `control_listen` port. When
   no request host is available, fall back to the existing
   `127.0.0.1:{control_port}` logic.

SQLite stays empty by default; the Web UI writes the override; clearing
it in the UI reverts to (2)/(3). Env keeps working for headless/offline
(just lower priority than an explicit UI override). Rejected
alternatives: Approach A (CLI/env first-run seeding into SQLite — adds
ordering ambiguity when env is permanently set on Railway) and
Approach C (drop CLI/env entirely — breaks offline `enroll-client`,
which has no request to derive from).

## Design

### 1. Storage layer

New migration `V010__add_server_settings.sql`: a singleton config table.

```sql
CREATE TABLE server_settings (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    advertised_endpoint TEXT
) STRICT;

INSERT INTO server_settings (id, advertised_endpoint) VALUES (1, NULL);
```

New `store/settings_store.rs`:

- `get_advertised_endpoint() -> Result<Option<String>, StoreError>`
- `set_advertised_endpoint(Option<String>) -> Result<(), StoreError>`
  — validates `host:port` shape before write; empty / `None` clears the
  override.

### 2. Resolution & issuance

- `AppState`: remove the eagerly-fixed `server_endpoint: String`.
  Add `advertised_seed: Option<String>` (the startup CLI/env value)
  and a handle to the settings store.
- New `resolve_advertised_endpoint(state, req_host: Option<&str>)
  -> String` implementing the order above. Port for auto-derive comes
  from the server's resolved `control_listen` port (captured at bind
  time in `serve.rs`), **not** the browser port.
- Issuance sites:
  - `operator/cli.rs` `enrollment_uri()` — Web UI / HTTP "create
    client" path. Thread the request `Host` header in so step 3 can
    auto-derive.
  - `grpc/enrollment.rs` — gRPC inbound; no HTTP Host. Steps
    1 → 2 → existing loopback fallback only (no browser derive).
  - `build_offline_state` (offline `enroll-client`) — steps
    1 (if SQLite already has a value) → 2 → existing fallback. No
    browser derive.

### 3. Web UI

- Settings page: a "Client connect address / Advertised endpoint"
  field showing the **effective value and its source** (operator-set /
  startup flag / auto-derived), editable; saving writes SQLite,
  clearing reverts to auto-derive.
- New operator HTTP endpoints `GET` / `PUT
  /v1/settings/advertised-endpoint`, with `host:port` validation,
  reusing the existing operator-API auth + CSRF middleware.
- Placeholder/help text states explicitly: on platforms where the HTTP
  domain differs from the TCP path (e.g. Railway), the operator must
  set the TCP-Proxy `host:port` here.

### 4. Client — unchanged

`portunus-client` reads the endpoint from the enrollment bundle once at
startup (current behavior). The bundle still carries pinned cert +
endpoint together. No code change.

## Testing

- `settings_store` unit tests: get/set round-trip, `host:port`
  validation (reject host-only, reject garbage), clear → `None`.
- Migration test: `V010` applies cleanly; singleton `id=1` row present;
  `store_schema_handshake` updated for the new table.
- `resolve_advertised_endpoint` unit tests covering each precedence
  branch: SQLite set; seed only; Host header derive (host + control
  port); no host → loopback fallback.
- Operator HTTP integration test: `PUT` then `GET` round-trip; CSRF
  rejection on missing token; validation rejection on bad value.
- Regression: `cargo test --workspace` green; existing enrollment /
  bundle tests updated for the resolver indirection.

## Migration / Compatibility

- Existing deployments: `server_settings.advertised_endpoint` is `NULL`
  → behavior is identical to today (falls through to CLI/env seed, then
  loopback). No operator action required.
- `deploy/railway/start-server.sh` keeps working unchanged (flag still
  honored as the seed). Operators may optionally stop setting the env
  and instead set the value once in the Web UI.
- Optionally bind clap `env = "PORTUNUS_ADVERTISED_ENDPOINT"` so the
  binary reads the env directly; the Railway script's flag bridge then
  becomes redundant but harmless.
