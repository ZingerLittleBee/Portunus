# Phase 0 — Research & Decisions

**Feature**: 001-tcp-forward-mvp
**Date**: 2026-05-06

This document records the technical decisions resolved during planning, with
rationale and alternatives considered. Each decision answers an unknown that
appeared in the spec, the constitution's TODOs, or the plan's Technical
Context.

---

## Decision 1 — Wire Protocol: gRPC over TLS via Tonic

**Decision**: Use gRPC (HTTP/2) with bidirectional streaming, implemented via
`tonic` 0.12 + `prost` 0.13. Codegen through `tonic-build` invoked from
`forward-proto`'s `build.rs`. Wire schema in `proto/forward.proto`, versioned
with a top-level package `forward.v1`.

**Rationale**:
- Bidirectional streaming naturally models the long-lived control channel:
  one stream per connected client, server pushes `RuleUpdate` messages,
  client pushes `RuleStatus` and periodic `StatsReport` messages.
- protobuf gives us the versioning discipline the constitution requires
  ("breaking changes require a major-version protocol bump"). Adding
  optional fields is non-breaking; required-field changes force `v2`.
- Tonic interceptors map cleanly to bearer-token verification — one
  interceptor enforces auth before any service handler runs.
- Mature Rust ecosystem, audited TLS via `rustls`, well-tested under load.

**Alternatives considered**:
- *QUIC via `quinn`*: attractive (single connection, multiplexed streams,
  TLS 1.3 baked in). Rejected because we'd hand-roll RPC framing on top —
  more code surface in the security-sensitive control path. Reconsider if
  the data plane ever moves to QUIC for NAT traversal.
- *Custom length-prefixed framing + serde + JSON/CBOR/MsgPack*: fewest
  dependencies but no codegen, no schema registry, hand-rolled versioning.
  Saves a build-time `protoc` dependency at the cost of giving up the
  versioning discipline the constitution requires. Rejected.
- *WebSocket + JSON*: easiest to debug with `curl`/browser tools but no
  binary efficiency, no schema, and harder to add bidi streaming semantics
  cleanly. Rejected.

**Cost accepted**: build-time `protoc` dependency. Mitigated by using
`prost-build`'s bundled `protoc` (`PROTOC_NO_VENDOR=0` is the default behaviour
when `protoc` is absent on the build host) — keeps the developer-machine
prerequisite list small.

---

## Decision 2 — Token Storage: Atomic-Write JSON File

**Decision**: Token store is a JSON file at `<config_dir>/tokens.json`,
written via the temp-file + `rename(2)` pattern for atomicity. Schema in
`data-model.md`. No SQL store in MVP. Resolves Constitution
`TODO(STORAGE_CHOICE)` for this spec.

**Rationale**:
- ≤100 entries (SC-004a / spec Assumption); the entire file is tens of KB.
- Mutations happen only on `provision` and `revoke` — no hot-path concern.
- `rename(2)` on the same filesystem is atomic on Linux/macOS — readers
  always see either the old or the new full file, never a torn write.
- Zero added runtime dependency; one less thing to operate.
- Easy to inspect with `jq`, easy to back up with `cp`.

**Alternatives considered**:
- *SQLite via `rusqlite` or `sqlx`*: well-understood, but introduces a
  build dep (`libsqlite3` or bundled), a migration story, and brings
  schema-versioning concerns for ~100-row tables. Reconsider when the next
  spec adds rule persistence, audit-log persistence, or multi-tenant
  identity (probably ~spec 003 or 004).
- *`sled` / `redb`*: embedded KV stores. Solve a problem we don't have at
  this scale. Rejected.
- *Plain JSON without atomic write*: unacceptable — a crash mid-write
  would corrupt the trust store and lock out every client.

**Cost accepted**: when the next spec adds another persisted entity, we
will likely migrate to SQLite. The migration is bounded — read JSON,
populate `tokens` table once.

---

## Decision 3 — Server TLS Cert Material on Disk

**Decision**: Server TLS leaf certificate and private key are stored as PEM
files at `<config_dir>/server.crt` and `<config_dir>/server.key`. On first
launch, if neither file exists, the server generates a self-signed cert
(via `rcgen` 0.13) with a configurable CN and 10-year validity. Operator
may replace either file with their own (e.g., from a corporate CA);
Portunus does not care, only that `server.key` corresponds to
`server.crt`.

**Rationale**:
- The client pins by **leaf certificate fingerprint**, not chain. So
  whether the cert is self-signed or enterprise-issued is irrelevant to
  Portunus's trust model — pinning happens regardless.
- Persisting the cert means client-side fingerprint pins keep working
  across server restarts (FR-006a).
- PEM is the format every Rust TLS lib already speaks.

**Alternatives considered**:
- *In-memory ephemeral cert per server start*: would invalidate every
  pinned client on every restart. Rejected (violates FR-006a).
- *DER-encoded files*: marginally smaller but less human-debuggable.
  Rejected.

---

## Decision 4 — Token Hash: blake3 (Salt-less)

**Decision**: Tokens are hashed for storage with `blake3::hash` (32-byte
output, hex-encoded). No salt, no per-user pepper.

**Rationale**:
- Tokens are 32 bytes from `OsRng` (≥256 bits entropy, well above the
  ≥128 required by FR-001). At that entropy level a brute-force attack
  is computationally infeasible regardless of hash choice or salting.
- blake3 is fast (≥1 GB/s single-threaded), keyed if needed in future,
  and has no `getrandom`-style platform issues.
- Salt's purpose (defeating pre-computed rainbow tables for low-entropy
  inputs) does not apply to high-entropy random tokens.
- Argon2 / bcrypt would add CPU cost on every connection auth without
  benefit at this entropy level.

**Alternatives considered**:
- *SHA-256*: equally adequate, slightly slower, more conventional. Either
  works; blake3 chosen for speed and modernity.
- *Argon2id*: appropriate for low-entropy passwords, overkill (and slow)
  here.
- *Plaintext storage*: rejected — Constitution I forbids long-lived
  secrets in non-hashed form.

---

## Decision 5 — Server Certificate Pinning on the Client

**Decision**: Implement a custom `rustls::client::danger::ServerCertVerifier`
in `forward-client/src/pinned_verifier.rs`. The verifier:
1. Computes SHA-256 over the leaf certificate's DER bytes.
2. Compares to the fingerprint string from the credential bundle.
3. Returns `Ok(ServerCertVerified::assertion())` on match, otherwise
   `Err(rustls::Error::General("server_cert_mismatch"))`.
4. Performs *no other validation* (no chain check, no name check, no time
   check) — pinning supersedes them.

**Rationale**:
- Pinning by leaf fingerprint is the simplest model that gives strong
  identity binding without operating a CA.
- Bypassing chain/name/time validation is intentional: the operator may
  use a self-signed cert with no SANs and a 10-year validity. Pinning is
  the ground truth.
- The verifier is the only place that touches `rustls::danger::*` APIs;
  isolating it makes security review easy.

**Alternatives considered**:
- *Pin by SPKI (subject public key info) hash*: more cert-renewal-friendly
  (key can stay across cert reissue) but more complex to compute and
  explain. Reconsider when we add cert auto-renewal.
- *Trust on first use (TOFU) like SSH*: unacceptable for a control plane
  bootstrap-from-bundle flow; the bundle gives us the fingerprint
  out-of-band, so we should use it.

---

## Decision 6 — Data-Plane Bidirectional Copy

**Decision**: Use `tokio::io::copy_bidirectional` for proxying inbound
TCP socket ↔ outbound TCP socket. Wrap in `select!` against a per-rule
shutdown signal so rule removal can interrupt long-running copies after
the drain window.

**Rationale**:
- This is the canonical Tokio primitive; battle-tested in production
  reverse proxies (e.g., `tower-http`, custom proxies built on tokio).
- Internally uses ring buffers; on Linux with both ends being TCP, the
  kernel can use `splice(2)` for zero-copy. Aligns with Constitution II
  ("zero-allocation steady state").
- Avoids re-implementing back-pressure, half-close semantics, error
  propagation — all of which are subtle.

**Alternatives considered**:
- *Hand-rolled `tokio::io::copy` in two directions with `tokio::join!`*:
  doable but reproduces `copy_bidirectional`'s logic, including the
  edge cases (one direction errors while other is mid-flush). Rejected.
- *`splice(2)` directly via `nix`*: would skip even the userspace ring
  buffer. Premature — measure with the criterion bench first; consider in
  a future hot-path optimisation spec under the
  `TODO(KERNEL_OFFLOAD)` rubric.

---

## Decision 7 — Graceful Shutdown Pattern

**Decision**: Root a `tokio_util::sync::CancellationToken` at process
start. The signal handler (SIGINT/SIGTERM, `tokio::signal::unix`) fires
`token.cancel()`. Every long-running task obtains a `child_token` from
the root; the rule listener loop's accept future is `select!`-ed against
the token. After cancellation, the listener stops accepting new
connections immediately; in-flight `copy_bidirectional` futures continue
until `shutdown_drain_timeout` (default 30 s, FR-020), after which their
sockets are forcibly closed.

**Rationale**:
- `CancellationToken` is the idiomatic Tokio shutdown primitive.
- Per-rule child tokens let rule removal use the same pattern as global
  shutdown — same draining semantics, same code path.
- The 30 s default matches the spec; configurable per-process.

**Alternatives considered**:
- *Broadcast channel*: works but tokens are purpose-built and carry
  parent/child relationships natively.
- *Process-level abort*: violates the constitution (no graceful drain).

---

## Decision 8 — Operator Interface: CLI + Loopback HTTP

**Decision**: The `forward-server` binary exposes both a CLI subcommand
surface (`provision-client`, `revoke`, `list-clients`, `push-rule`,
`remove-rule`, `rule-stats`) and a small HTTP service bound to
`127.0.0.1` with the same operations, so future tooling (a web UI,
scripts) can talk to it programmatically without spawning subprocesses.
The CLI subcommands shell out to the HTTP service when one is running and
fall back to direct in-process execution when invoked stand-alone.

**Rationale**:
- CLI is what the spec primarily describes (FR-021).
- HTTP on loopback adds <100 LOC (axum) and unblocks future tooling
  without re-architecting.
- Authentication of the HTTP service in MVP = bind to loopback only +
  trust the local UNIX user (FR-022). No tokens, no CSRF, no session.

**Alternatives considered**:
- *CLI only*: simpler now, but every future tool either re-implements
  the operator surface or shells out. Rejected.
- *gRPC for operator interface*: would conflate the operator surface
  with the client-facing control plane. Rejected — different audiences,
  different auth models.

---

## Decision 9 — Metrics Endpoint (Prometheus, Loopback)

**Decision**: A separate HTTP endpoint at `127.0.0.1:<metrics_port>/metrics`
exposes Prometheus-format text. Collectors:
- `forward_clients_connected` (gauge) — current count
- `forward_rule_bytes_in_total{client,rule}` (counter)
- `forward_rule_bytes_out_total{client,rule}` (counter)
- `forward_rule_active_connections{client,rule}` (gauge)
- `forward_auth_failures_total{reason}` (counter)

**Rationale**:
- Constitution IV makes the metrics endpoint a MUST. The spec's claim of
  "no Prometheus in MVP" was misaligned; this plan corrects.
- Adding it now is cheap (~50 LOC + the `prometheus` crate) and prevents
  having to retrofit observability later.
- Loopback bind matches the operator-interface posture (no separate
  authn for metrics; trust local UNIX user).

**Alternatives considered**:
- *No metrics endpoint, structured logs only*: contradicts constitution.
  Rejected.
- *OpenTelemetry exporter*: heavier dependency footprint, more setup.
  Defer to a future spec when tracing across server↔client matters.

---

## Decision 10 — Reconnection Backoff on the Client

**Decision**: Exponential backoff with jitter, base 500 ms, factor 2, cap
30 s, full jitter (`backoff` crate or hand-rolled — small enough to roll).
Reset to base on any successful connect.

**Rationale**:
- Hits the spec's "bounded exponential backoff" (FR-008) cleanly.
- Full jitter spreads thundering-herd reconnects when many clients
  disconnect from a server that comes back.
- 30 s cap means the worst-case time-to-reconnect after a server outage
  is 30 s + the operator's restart time, well within human-operator
  expectations.

**Alternatives considered**:
- *Fixed delay*: causes thundering herd. Rejected.
- *No cap*: starvation risk after long outages. Rejected.

---

## Open items intentionally deferred to subsequent specs

- **`TODO(KERNEL_OFFLOAD)`**: not used in MVP (Decision 6 alternative).
  Revisit when criterion benches show userspace copy as the bottleneck for
  representative workloads.
- **`TODO(MTLS_REVISIT)`** (Constitution v2.0.0): revisit Principle I if
  a deployment requires regulated-industry compliance. Auth seam is in
  place (Decision implicit in Constitution Check I.8).
- **Persistent SQL store**: see Decision 2 alternatives.
- **Rule persistence**: deliberately deferred per spec Assumption.
- **Multi-tenancy**: deferred per spec Assumption; data shapes (Decision
  implicit in plan, Constitution Check V) preserve the option.
- **Token rotation automation**: spec Assumption — manual re-provision in
  MVP.
