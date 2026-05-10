# Phase 1 — Data Model

**Feature**: 001-tcp-forward-mvp
**Date**: 2026-05-06

This document defines every entity that appears in the MVP, its fields, its
validation rules (mapped back to spec FRs), its lifecycle, and where it
lives at runtime (in-memory vs persisted).

---

## ClientName (newtype)

`ClientName(String)` — the operator-supplied name for a client.

| Field | Type | Validation |
|---|---|---|
| inner | `String` | 1–63 chars, `[a-z0-9](-?[a-z0-9])*` (DNS label shape, lowercase). Rejected on `provision-client` if invalid. |

Used as the primary key in the token store and as the identifier in every
log/audit/metric label. Newtype enforces validation at construction.

---

## RuleId (newtype)

`RuleId(u64)` — server-assigned monotonically increasing rule identifier.

Visible in operator output, logs, audit records, and metrics labels.
Stable for the lifetime of the rule (does not change on `failed → active`
re-attempts — but per Q4, those don't happen automatically).

Generated server-side by an `AtomicU64` counter; not persisted (rules
themselves aren't persisted, so neither is the counter — a server restart
resets to 0, which is safe because rules also reset).

---

## RequestId (newtype)

`RequestId(Ulid)` — server-assigned per-operator-action correlation ID.
Threaded through every log line that touches the action, including the
client's response logs. Used to satisfy Constitution IV's correlation-ID
requirement.

---

## Token (transient)

The plaintext bearer token. Lives only:
- in `OsRng → 32 bytes → base64url-encoded String` for ~1 ms during
  `provision-client`,
- in the credential bundle returned to the operator,
- in the client's config / memory while connecting,
- as the `Authorization: Bearer <token>` metadata on the gRPC request.

**Never persisted in plaintext.** Constitution I + FR-005.

Format: 43 ASCII chars (URL-safe base64 of 32 random bytes, no padding).

---

## TokenRecord (persisted)

Stored on the server as one entry per `provision-client`.

| Field | Type | Notes |
|---|---|---|
| `client_name` | `ClientName` | Primary key. Uniqueness enforced; second `provision` of same name fails with `client_already_exists` (FR-004). |
| `token_hash` | `[u8; 32]` (hex string in JSON) | `blake3(token)`. Verification = compute hash of presented token, constant-time compare to this. |
| `issued_at` | RFC3339 timestamp | When `provision-client` was called. |
| `revoked_at` | `Option<RFC3339 timestamp>` | `Some` when operator runs `revoke`. Revoked records are NOT deleted — kept for audit. |

**Storage**: `<config_dir>/tokens.json`, schema:
```json
{
  "version": 1,
  "tokens": [
    {
      "client_name": "edge-01",
      "token_hash": "f5e7…",
      "issued_at": "2026-05-06T12:00:00Z",
      "revoked_at": null
    }
  ]
}
```
Atomic write (tmp file + `rename`). File is mode `0600`. See
`contracts/persistence.md`.

---

## CredentialBundle (one-shot output)

What `provision-client` writes to disk for the operator to transfer to the
target machine. Stored at `<output_dir>/<client_name>.bundle.json`:

| Field | Type | Notes |
|---|---|---|
| `version` | `u32` | Schema version (starts at 1). |
| `client_name` | `ClientName` | |
| `server_endpoint` | `String` | `<host>:<port>` for the gRPC control listener. |
| `server_cert_sha256` | hex string (64 chars) | SHA-256 of the server leaf cert DER. Used by the client's `pinned_verifier`. |
| `token` | `String` | Plaintext bearer token. **Sensitive — readable on-disk only by the operator's UNIX user (mode 0600).** |

The operator copies this file (or its contents) to the target machine
through whatever channel they trust (SSH, copy-paste, internal secret
store). Portunus is unopinionated about that channel.

---

## ConnectedClient (in-memory)

Tracked only while the client's gRPC stream is open.

| Field | Type | Notes |
|---|---|---|
| `client_name` | `ClientName` | From the authenticated `ClientIdentity`. |
| `remote_addr` | `SocketAddr` | From the gRPC transport. |
| `connected_at` | `Instant` + `RFC3339` for output | |
| `cancel_token` | `CancellationToken` | Drops the stream + downstream rules on cancel. |
| `outbound_tx` | `mpsc::Sender<RuleUpdate>` | Server → client push channel. |

Stored in `Arc<RwLock<HashMap<ClientName, ConnectedClient>>>` on the
server. Bounded by ≤100 entries (SC-004a).

State transitions:
- `disconnected → connected` on successful auth (Tonic interceptor sets
  the entry, service handler upserts the channel).
- `connected → disconnected` on stream close, transport error, or operator
  `revoke` (revoke triggers `cancel_token.cancel()` which closes the
  stream).

---

## Rule (in-memory, server-side authoritative)

| Field | Type | Notes |
|---|---|---|
| `id` | `RuleId` | Generated on `push-rule`. |
| `client_name` | `ClientName` | Owner client (must be `connected` at push time, FR-014). |
| `listen_port` | `u16` | 1–65535. |
| `target_host` | `String` | Hostname or IP literal. Resolved on each inbound connection (intentionally — no caching in MVP). |
| `target_port` | `u16` | |
| `protocol` | `Protocol` | Enum `{ TCP }` — only TCP in MVP. |
| `state` | `RuleState` | See state machine below. |
| `created_at` | `RFC3339` | |
| `last_state_change_at` | `RFC3339` | |

**State machine** (resolves Q4):
```
                 push-rule
       (none) ────────────► Pending
                              │
                  client      │ activation
                  acks        │ result
                              ▼
              ┌───────── Active ◄──── (no auto-retry)
              │           │
              │  remove   │ remove
              ▼           ▼
            Removed     Removed
              ▲
              │ remove
              │
              Failed(reason)
                 ▲
                 │ activation
                 │ failure
                 │
              Pending  (initial pending state on push)
```

Transition rules:
- `Pending → Active`: client returned `RuleStatus { activated = true }`.
- `Pending → Failed(reason)`: client returned `RuleStatus { activated = false, reason }`.
- `Active → Removed`: operator `remove-rule`. Triggers `cancel_token` on the listener; drain → forced close.
- `Failed → Removed`: operator `remove-rule`. No cleanup needed (nothing was running).
- `Failed → Pending`: **forbidden in MVP** (no auto-retry). Operator must `remove` then `push` a fresh rule (which gets a new `RuleId`).
- A `push-rule` whose `(client_name, listen_port)` collides with any existing rule in state `Active` OR `Failed` returns `port_in_use` to the operator (FR-012 last clause). This means `failed` rules block reuse of their listen_port until removed.

**Storage**: in-memory in `Arc<RwLock<HashMap<RuleId, Rule>>>` plus a
secondary index `HashMap<(ClientName, u16), RuleId>` for the collision
check. Not persisted (spec Assumption); lost on server restart.

---

## RuleStats (in-memory, client-side authoritative)

Held by the client for each `Active` rule; reported to the server on
demand (`stats-query`) and periodically (every 5 s by default — see
quickstart) so the server's metrics endpoint can stay current.

| Field | Type | Notes |
|---|---|---|
| `rule_id` | `RuleId` | |
| `bytes_in` | `AtomicU64` | Inbound→outbound direction (the original requester's bytes). |
| `bytes_out` | `AtomicU64` | Outbound→inbound direction. |
| `active_connections` | `AtomicU32` | Current count, incremented on accept, decremented on close. |

Mirrored on the server side as `RuleStatsCache` keyed by `RuleId`, updated
on every `StatsReport` from the client.

---

## AuditEvent (log record, not stored)

Emitted by `tracing` with the structured fields below. NOT separately
persisted in MVP — the operator's log collector (journalctl, file rotation,
etc.) is the source of truth.

| Field | Type | Required |
|---|---|---|
| `timestamp` | RFC3339 | yes (set by `tracing-subscriber`) |
| `level` | `INFO` for success, `WARN` for failure, `ERROR` for transport-level | yes |
| `event` | `audit.provision \| audit.revoke \| audit.rule_push \| audit.rule_remove \| auth.failure \| client.connected \| client.disconnected \| rule.activated \| rule.failed \| rule.removed` | yes |
| `request_id` | `RequestId` | when applicable (operator-initiated actions) |
| `client_name` | `ClientName` | when applicable |
| `rule_id` | `RuleId` | when applicable |
| `reason` | `String` | for `*.failure` and `rule.failed` events |
| `outcome` | `success \| failure` | for `audit.*` events |

---

## ClientIdentity (in-memory, request-scoped)

Returned by the `Authenticator` trait (`forward-auth`):

```rust
pub struct ClientIdentity {
    pub client_name: ClientName,
    // Future: pub tenant_id: TenantId,  // V. Multi-Tenant Isolation
}
```

Inserted into Tonic request extensions by the auth interceptor; every
service handler reads it via `req.extensions().get::<ClientIdentity>()`.
Constitution V's preservation requirement is satisfied by carrying
identity through this struct rather than re-deriving it inside handlers.

---

## Configuration

### ServerConfig (persisted as `<config_dir>/server.toml`)

| Field | Type | Default | Notes |
|---|---|---|---|
| `control_listen` | `SocketAddr` | `0.0.0.0:7443` | gRPC TLS listener. |
| `operator_http_listen` | `SocketAddr` | `127.0.0.1:7080` | Loopback only. |
| `metrics_listen` | `SocketAddr` | `127.0.0.1:7081` | Loopback only. |
| `tls_cert_path` | `PathBuf` | `<config_dir>/server.crt` | |
| `tls_key_path` | `PathBuf` | `<config_dir>/server.key` | |
| `token_store_path` | `PathBuf` | `<config_dir>/tokens.json` | |
| `shutdown_drain_timeout_secs` | `u64` | `30` | FR-020. |
| `log_format` | `json \| compact` | `json` | |

### ClientConfig (`<client_config_dir>/client.toml` OR command-line flags)

| Field | Type | Default | Notes |
|---|---|---|---|
| `bundle_path` | `PathBuf` | required | Path to the `.bundle.json` from `provision-client`. |
| `reconnect_initial_delay_ms` | `u64` | `500` | Backoff base. |
| `reconnect_max_delay_secs` | `u64` | `30` | Backoff cap. |
| `shutdown_drain_timeout_secs` | `u64` | `30` | FR-020. |
| `log_format` | `json \| compact` | `json` | |
| `stats_report_interval_secs` | `u64` | `5` | How often to push `StatsReport` to server. |
