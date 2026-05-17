# Advertised Endpoint: Runtime-Configurable (SQLite + Web UI)

Date: 2026-05-17
Status: Approved (design, rev 2 — incorporates review findings)

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
- Preserve headless / offline bundle issuance (CLI flag / env) — the
  offline `enroll-client` path has no HTTP request to derive from.
- The full value is a `host:port` (Railway TCP Proxy maps the internal
  `7443` to a different public port; host-only is insufficient).
- A generated enrollment URI and the credential bundle later redeemed
  from it MUST carry the **same** endpoint.
- The endpoint host MUST be covered by the server leaf certificate SAN
  (the client uses it as the TLS verification domain).

## Non-Goals

- No change to the client. The enrolled bundle already carries the
  endpoint (and pinned cert) atomically; `portunus-client` reads it
  once at startup. Out of scope.
- No change to the bundle wire/URI format.
- No certificate reissue / hot-reload. Cert SAN coverage for a new
  host is an operator/deploy concern (e.g. Railway regenerates the
  self-signed cert at startup from `PORTUNUS_ADVERTISED_ENDPOINT`; a
  host change there means a redeploy). This design only *validates*
  coverage and rejects values it cannot honor.
- No IPv6-literal endpoints (see Validation). Rejected at write time so
  the client's existing host parser stays correct.
- No new Prometheus metrics.

## Core Model: Resolve-once-at-creation, replay-at-redeem

The original divergence risk: the enrollment URI is generated when the
operator *creates* an enrollment (Web UI / HTTP — request `Host`
available), but the bundle is produced later when the client *redeems*
by code over gRPC (`crates/portunus-server/src/grpc/enrollment.rs` —
**no** HTTP host). Resolving independently at each site lets a
browser-derived URI (`portunus://public.example:7443/...`) redeem into
a bundle pointing at `127.0.0.1:7443`.

**Fix:** resolve the endpoint exactly once, at enrollment **creation**,
persist it on the `client_enrollments` row, and have `redeem()` return
that immutable value. The gRPC enroll RPC writes the persisted value
into the bundle verbatim — it no longer reads `state.server_endpoint`.

## Resolution Order (at creation time)

Evaluated once when the enrollment row is created, then frozen:

1. **SQLite operator-set value** (`server_settings.advertised_endpoint`,
   non-empty) — highest priority. What a Railway operator sets once in
   the Web UI to the TCP-Proxy `host:port`.
2. **Startup seed** — CLI flag `--advertised-endpoint`. When the flag
   is absent, `main` falls back to an explicit
   `std::env::var("PORTUNUS_ADVERTISED_ENDPOINT")` read. (Not clap
   `#[arg(env = ...)]`: the workspace pins `clap = { features =
   ["derive"] }` — `Cargo.toml:74` — and `#[arg(env)]` would require
   enabling clap's `env` Cargo feature; a manual `std::env` read keeps
   the dependency surface unchanged. The Railway script's flag bridge
   becomes redundant but stays harmless.) Held in `AppState` as
   `Option<String>`.
3. **Auto-derive** — host parsed from the operator HTTP request `Host`
   header (contract below) + the server's startup-resolved
   `control_listen` port. Only available on the Web UI / HTTP creation
   path.
4. **Loopback fallback** — existing `127.0.0.1:{control_port}` logic
   (used by gRPC-initiated and offline creation that have no request
   host and no seed/override).

**`Host` header parsing contract (tier 3):** treat the header as an
HTTP authority. Strip an optional `:port` (the *browser* port, e.g.
`localhost:5173`, `ops.example.com:443`) and discard it — the
control-plane port always comes from the server's resolved
`control_listen`. Reject (→ skip tier 3) if the header contains a
scheme, path, `@` userinfo, whitespace/control chars, or is an IPv6
literal (`[...]`). The surviving bare host is then `host:{control
port}`. Examples: `Host: localhost:5173` → `localhost:7443`;
`Host: public.example:443` → `public.example:7443`.

**Covered vs. uncovered, by tier kind:**

- **Explicit operator config** (tier 1 SQLite override, tier 2
  CLI/env seed): if present but its host is **not** SAN-covered,
  resolution is a **hard error** (`ConfiguredEndpointNotCovered`),
  *not* a silent fall-through. Reason: an operator who set a value
  (or a seed left stale after cert rotation) must be told it is
  unusable, never silently downgraded to a "valid but wrong"
  lower-priority endpoint.
- **Implicit candidates** (tier 3 auto-derive, tier 4 loopback): if
  not SAN-covered, skip with a `warn` and fall through.

So precedence is: if tier 1 present → it must be covered or hard
error; else if tier 2 present → it must be covered or hard error;
else try tier 3, then tier 4, skipping uncovered ones. If nothing
usable remains, resolution **fails** with `NoSanCoveredCandidate`
(see §4). Creation never fabricates an unusable endpoint.

Rejected alternatives: Approach A (CLI/env first-run seeding into
SQLite — ordering ambiguity when env is permanently set on Railway);
Approach C (drop CLI/env entirely — breaks offline `enroll-client`,
which has no request to derive from); resolve-at-each-issuance-site
(the divergence Blocker above).

## Design

### 1. Storage layer

New migration `V010__add_server_settings.sql`:

```sql
CREATE TABLE server_settings (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    advertised_endpoint TEXT
) STRICT;

INSERT INTO server_settings (id, advertised_endpoint) VALUES (1, NULL);

ALTER TABLE client_enrollments ADD COLUMN advertised_endpoint TEXT;
```

- `server_settings`: singleton (`id = 1`) operator override.
- `client_enrollments.advertised_endpoint`: the endpoint frozen at
  creation. `NULL` only for rows created before this migration;
  `redeem()` of such a legacy row falls back to the loopback default
  (documented; legacy rows are short-lived enrollments and will expire).

New `store/settings_store.rs`:

- `get_advertised_endpoint() -> Result<Option<String>, StoreError>`
- `set_advertised_endpoint(Option<String>) -> Result<(), StoreError>`
  — validates authority grammar (below) before write; empty / `None`
  clears the override. SAN coverage is enforced by the HTTP handler
  (it owns the cert), not the store.

### 2. Validation (authority grammar)

The value is simultaneously a URI authority and the client's TLS
verification domain, so it is validated strictly at every write
(`set_advertised_endpoint` and the HTTP `PUT`):

- Exactly `host:port`. No scheme, path, query, fragment, userinfo,
  whitespace, or control characters.
- `host` is a DNS hostname (LDH labels, RFC 1123) **or** an IPv4
  dotted quad. **IPv6 literals are rejected** — this keeps the
  client's existing `extract_host` (`rsplit_once(':')`,
  `crates/portunus-client/src/control.rs:156`) correct without
  touching the client.
- `port` is a decimal `1..=65535`.
- Total length bound (e.g. ≤ 255) to avoid pathological input.

### 3. SAN coverage check

The client passes the endpoint host into TLS domain verification
against the pinned self-signed leaf cert
(`crates/portunus-client/src/control.rs:76`). An endpoint whose host
is absent from the leaf cert SAN produces bundles that cannot connect.

**Matching must mirror the client's TLS verifier, not literal SAN
membership.** The client connects via tonic 0.14 (`tls-aws-lc`) over
rustls/tokio-rustls, so the effective verifier is webpki. The coverage
helper MUST use webpki/rustls-equivalent name matching:

- DNS SAN entries: case-insensitive, ASCII-lowercased comparison.
- Wildcard DNS SAN (`*.example.com`): matches exactly one leftmost
  label (`a.example.com`), not multi-label (`a.b.example.com`) and not
  the bare apex (`example.com`) — webpki rules.
- IPv4 host (allowed by grammar) is matched against **IP** SAN
  entries, never DNS SAN entries. (IPv6 hosts are rejected at the
  grammar stage, so no IPv6 SAN logic is needed.)
- Prefer a rustls/webpki name-matching primitive over a hand-rolled
  comparator so semantics cannot drift from what the client accepts.

- Parse the leaf cert SAN once at startup; cache the structured SAN set
  on `AppState`.
- `PUT /v1/settings/advertised-endpoint`: after grammar validation,
  reject (HTTP 422, machine code `endpoint_not_in_cert_san`) if the
  host is not covered under the rules above. Error text tells the
  operator the cert must be reissued/redeployed to cover the host
  (out of scope here).
- Creation-time resolution applies the same coverage *predicate*, but
  the *consequence* of a miss depends on tier kind: explicit config
  (tier 1/2) → hard error; implicit (tier 3/4) → skip-and-fall-through.
  See "Covered vs. uncovered, by tier kind" in Resolution Order.

### 4. Resolution & issuance wiring

- `AppState`: remove the eagerly-fixed `server_endpoint: String`. Add
  `advertised_seed: Option<String>`, the parsed cert SAN set, and a
  settings-store handle.
- New `resolve_advertised_endpoint(state, req_host: Option<&str>)
  -> Result<ResolvedAdvertisedEndpoint, ResolveEndpointError>`
  implementing the ordered, SAN-filtered resolution. Port for
  auto-derive is the server's resolved `control_listen` port
  (captured at bind in `serve.rs`), not the browser port.
  `ResolvedAdvertisedEndpoint` carries the `host:port` string plus the
  winning `source` (override / seed / derived / loopback) for the UI.
  `ResolveEndpointError` variants:
  - `ConfiguredEndpointNotCovered { tier, host }` — an **explicit**
    candidate (tier 1 SQLite override or tier 2 CLI/env seed) is
    present but its host is not SAN-covered. Hard error; never
    downgraded to a lower tier.
  - `NoSanCoveredCandidate` — no explicit config present, and both
    auto-derive and loopback are absent/uncovered.
  **Error surfaces (both variants):**
  - Web UI / HTTP create-client → HTTP 422,
    `endpoint_not_in_cert_san`, body distinguishes the two variants
    and lists the candidate(s) tried with the reason each failed.
  - Offline `enroll-client` → non-zero exit with the same diagnostic.
  - The gRPC `enroll` redeem path never resolves (it replays the
    persisted row), so it cannot hit either error.
- `ClientEnrollmentStore::create` gains an `advertised_endpoint:
  String` input (resolved by the caller) and writes it to the new
  column. `CreatedEnrollment` / `redeem()` return it.
- Issuance sites:
  - `operator/cli.rs` `enrollment_uri()` / create flow — Web UI / HTTP
    "create client". Resolve with the request `Host`; the same value
    goes into the URI and the persisted row.
  - `grpc/enrollment.rs` — write `issued.advertised_endpoint` (the
    persisted value) into `CredentialBundle.server_endpoint`. Delete
    the `state.server_endpoint` read.
  - `build_offline_state` / offline `enroll-client` — resolve with
    `req_host = None` (steps 1 → 2 → 4), persist on the row.

### 5. Web UI

- Settings page: a "Client connect address / Advertised endpoint"
  field showing the **effective resolved value and its source**
  (operator-set / startup seed / auto-derived / loopback), editable;
  saving writes SQLite, clearing reverts to auto-derive.
- New operator HTTP endpoints `GET` / `PUT
  /v1/settings/advertised-endpoint` reusing existing operator-API auth
  + CSRF middleware.
  - **`GET` always returns 200** (it is a status/read endpoint, not a
    failure). Body:
    `{ override: string|null, effective: string|null,
       source: "override"|"seed"|"derived"|"loopback"|null,
       diagnostic: string|null }`.
    `override` is the raw SQLite value. `effective`/`source` are the
    resolution result *without* a request `Host` (server-side view);
    when resolution would error, `effective` and `source` are `null`
    and `diagnostic` carries the `ConfiguredEndpointNotCovered` /
    `NoSanCoveredCandidate` reason so the UI can render the problem
    inline rather than treating GET as broken.
  - **`PUT`** enforces grammar + SAN coverage; on failure returns
    422 with `endpoint_not_in_cert_san` (or the grammar error code),
    distinct enough that the UI can explain the cert requirement.
- Help text: on platforms where the HTTP domain differs from the TCP
  path (Railway), set the TCP-Proxy `host:port` here, and ensure the
  server cert covers that host.

### 6. Client — unchanged

`portunus-client` reads the endpoint from the enrollment bundle once
at startup. IPv6 rejection at write time keeps `extract_host` correct.
No code change.

## Testing

- `settings_store`: get/set round-trip; grammar validation (reject
  host-only, scheme, path, userinfo, control chars, bad/zero/65536
  port, IPv6 literal, over-length); clear → `None`.
- Migration: `V010` applies; `server_settings` singleton row present;
  `client_enrollments.advertised_endpoint` column added;
  `store_schema_handshake` updated.
- `Host` parsing contract: `Host: localhost:5173` → `localhost:7443`;
  `Host: public.example:443` → `public.example:7443`; bare
  `Host: public.example` → `public.example:7443`; reject (skip tier 3)
  for scheme/path/`@`userinfo/whitespace/IPv6-literal headers.
- `resolve_advertised_endpoint`: each precedence branch;
  **explicit-config not covered → `ConfiguredEndpointNotCovered`
  hard error, never downgraded** (e.g. stale SQLite override or
  CLI/env seed after cert rotation, even when loopback *is* covered);
  implicit tiers (auto-derive, loopback) skip-and-fall-through when
  uncovered; nothing usable → `NoSanCoveredCandidate`. Both map to
  422 `endpoint_not_in_cert_san` on the create-client HTTP path and
  non-zero exit offline.
- `GET /v1/settings/advertised-endpoint`: 200 with
  `effective`/`source` populated on success; 200 with
  `effective: null` + `diagnostic` when resolution would error
  (never a non-200).
- SAN matching (webpki parity): exact DNS hit; case-insensitive DNS
  hit; wildcard `*.example.com` matches `a.example.com` but rejects
  `a.b.example.com` and bare `example.com`; IPv4 host matches IP SAN
  only, not a same-text DNS SAN; miss → not covered.
- Enrollment round-trip (the Blocker-1 regression): create via HTTP
  with `Host: public.example` → URI host == persisted row endpoint;
  gRPC `redeem` returns a bundle whose `server_endpoint` equals the URI
  authority (no `127.0.0.1` divergence).
- Operator HTTP: `PUT` then `GET` round-trip; CSRF rejection;
  grammar 422; `endpoint_not_in_cert_san` 422.
- Regression: `cargo test --workspace` green; existing enrollment /
  bundle tests updated for the persisted-endpoint indirection.

## Migration / Compatibility

- Existing deployments: `server_settings.advertised_endpoint` is
  `NULL`; new enrollments resolve via seed → loopback exactly as
  today. No operator action required.
- Legacy `client_enrollments` rows (pre-`V010`) have
  `advertised_endpoint = NULL`; `redeem()` falls back to the loopback
  default for those. Acceptable: enrollments are short-TTL and expire.
- `deploy/railway/start-server.sh` keeps working unchanged. The binary
  now reads `PORTUNUS_ADVERTISED_ENDPOINT` directly via an explicit
  `std::env::var` fallback in `main` (no clap `env` Cargo feature
  added); the script's `--advertised-endpoint` bridge is redundant but
  harmless and may be simplified later (not in scope).
