# Persistence Contract: identity.json

**Feature**: 005-multi-user-rbac
**Phase**: 1 (design)

This document fixes the on-disk schema and write protocol for the
operator-identity store. The schema is **versioned**; loaders refuse
unknown versions to keep forward-compat predictable. The write
protocol mirrors `forward-auth::file_store::FileTokenStore` verbatim
so operators have one mental model for both stores.

## File location

- Default: `<config_dir>/identity.json`, where `config_dir` is the
  directory holding `server.toml` (typically
  `/etc/forward-server/`).
- Override: `operator_store_path = "/some/path/identity.json"` in
  `server.toml`.
- Permissions: created `0600` by the server. Operators MUST keep this
  file mode; group/world-readable would expose all credential hashes
  (still not the raw tokens, but the hashes are sensitive enough to
  treat as secrets).

## Schema (version 1)

The file is a single JSON object. Keys are lower_snake_case to match
existing v0.4 persisted shapes (`rules.json` precedent in spec 001).

```json
{
  "version": 1,
  "users": [
    {
      "id": "_superadmin",
      "display_name": "Built-in superadmin",
      "role": "superadmin",
      "created_at": "2026-05-07T10:00:00Z",
      "disabled": false
    },
    {
      "id": "alice",
      "display_name": "Alice — payments team",
      "role": "user",
      "created_at": "2026-05-07T11:00:00Z",
      "disabled": false
    }
  ],
  "credentials": [
    {
      "id": "01HEXY7Z2K8R0M3N9P4QHEXY...",
      "user_id": "_superadmin",
      "token_hash": "<64 lowercase hex chars (blake3 of the raw token)>",
      "label": "bootstrap",
      "created_at": "2026-05-07T10:00:00Z",
      "last_used_at": "2026-05-07T10:05:32Z",
      "status": "active"
    },
    {
      "id": "01HFXY7Z2K8R0M3N9P4QHFXY...",
      "user_id": "alice",
      "token_hash": "<64 lowercase hex chars>",
      "label": "ci runner #3",
      "created_at": "2026-05-07T11:05:00Z",
      "last_used_at": null,
      "status": "active"
    },
    {
      "id": "01HABC7Z2K8R0M3N9P4QHABC...",
      "user_id": "alice",
      "token_hash": "<64 lowercase hex chars>",
      "label": "old laptop",
      "created_at": "2026-05-01T08:00:00Z",
      "last_used_at": "2026-05-04T14:22:01Z",
      "status": { "revoked": { "revoked_at": "2026-05-07T11:00:00Z" } }
    }
  ],
  "grants": [
    {
      "id": "01HFGH7Z2K8R0M3N9P4QHFGH...",
      "user_id": "alice",
      "client": "client-a",
      "listen_port_start": 30000,
      "listen_port_end": 30010,
      "protocols": ["tcp"],
      "note": "payments staging",
      "created_at": "2026-05-07T11:10:00Z"
    }
  ]
}
```

### Field types

- `version`: positive integer. Loader recognises `1` only in v0.5.0.
- `id` (User): string, regex `^[a-z][a-z0-9_-]{0,31}$` for
  user-issued; reserved IDs (`_superadmin`, `_superadmin_legacy`)
  begin with `_`.
- `id` (Credential / Grant): string, ULID-like 26-char Crockford
  base32 from `ulid::Ulid::new()` — chosen over UUID-v4 for sortability
  in the JSON (operators can grep credentials by recency without
  parsing timestamps). The on-wire format is opaque to operators.
- `token_hash`: 64 lowercase hex chars. Same encoding as
  `forward-auth::file_store`'s `TokenRecordWire::token_hash`.
- `created_at`, `last_used_at`, `revoked_at`: RFC 3339 / ISO 8601 UTC,
  with `Z` suffix.
- `last_used_at`: nullable. `null` until the credential's first
  successful verification.
- `status`: either the string `"active"` or the object form
  `{ "revoked": { "revoked_at": "..." } }`. Untagged-enum
  serialisation matches `RuleState` in v0.4 for stylistic
  consistency.
- `client` (Grant): either a `ClientName`-shaped string or the literal
  `"*"`.
- `listen_port_start` / `listen_port_end`: u16, `1..=65535`,
  start ≤ end.
- `protocols`: non-empty array of `"tcp"` and/or `"udp"`. Order does
  not matter; the in-memory representation is a set.
- `disabled` (User): bool. Defaults to `false` if absent.
- `note` (Grant), `label` (Credential): nullable string, ≤ 128 / 64
  chars.

### Invariants enforced at load time

The loader validates the document and refuses to start the server on
any of the following:

- Unknown `version`.
- Duplicate `User.id`.
- Duplicate `Credential.id`.
- Duplicate `Grant.id`.
- `Credential.user_id` not present in `users`.
- `Grant.user_id` not present in `users`.
- `Credential.token_hash` not 64 hex chars.
- `Grant.listen_port_start > listen_port_end`.
- `Grant.protocols` empty.
- Two credentials with the same `token_hash` (collision attack
  detection — practically impossible at 256 bits, but cheap to
  check).

Failure mode: server startup exits non-zero with
`identity_store_corrupt: <reason>` printed to stderr; the server
does NOT auto-repair. Operators restore from backup or re-run
`bootstrap-superadmin` against an empty file.

## Atomic write protocol

Verbatim from `forward-auth::file_store::FileTokenStore::flush_locked`.
Reproduced here for the contract.

Given target path `P = identity.json`:

1. Acquire the in-memory `RwLock` write guard.
2. Mutate the in-memory snapshot.
3. Serialize snapshot to bytes via `serde_json::to_vec_pretty` (pretty
   for human inspection during incident response).
4. Open `P.tmp` with `O_CREAT | O_TRUNC | O_WRONLY`, mode `0600`, in
   the same directory as `P` (so `rename(2)` is intra-FS).
5. `write_all(bytes)` then `file.sync_all()` (`fsync` on the file).
6. `rename(P.tmp, P)`.
7. Open the parent directory and `sync_all()` (`fsync` on the
   directory) to durably commit the rename.
8. Drop the write guard.

Failure handling:

- Step 4 fails (disk full, permission) → in-memory state is rolled
  back by re-reading from `P` before the next operation. The
  caller's HTTP handler returns `500 internal_error` with
  `code = "store_write_failed"`.
- Step 5 fails after partial write → `P.tmp` is orphaned. The next
  startup ignores it (only `P` is loaded); the orphan is cleaned up
  on the next successful flush (which truncates and re-creates).
- Step 6 fails → same as step 5; `P` is unchanged.
- Step 7 fails → `P` is on-disk but the parent isn't synced. A power
  loss in this window may revert the rename on some filesystems.
  The next operator action will redo the write. Acceptable.

## Concurrency

- One `forward-server` process per host (Constitution Tech
  Constraints), so file locking across processes is unnecessary.
- In-process: a single `RwLock` over the in-memory snapshot. Reads
  (auth verification on every operator request) take the read lock;
  writes (user/credential/grant CRUD) take the write lock and the
  flush is serialised by the lock.
- The store does NOT support inotify-style hot reload of external
  edits. SIGHUP triggers an explicit reload (R-009); editing
  `identity.json` while the server is running without sending SIGHUP
  is unsupported and may be silently overwritten by the next write.

## SIGHUP reload

On SIGHUP:

1. Acquire write lock (briefly blocks new auth checks; in-flight
   requests already past their auth check are unaffected).
2. Re-read `P` from disk; validate per § Invariants.
3. Replace the in-memory snapshot.
4. Drop write lock.
5. Emit one INFO log: `event = "operator.store_reloaded"`,
   `users = N`, `credentials = M`, `grants = K`.

If validation fails, the in-memory snapshot is **kept**, the reload
is aborted, and one WARN log: `event = "operator.store_reload_failed"`,
`reason = "<validation error>"`. The server continues serving from
the previous snapshot. Operators see the WARN and fix the file.

This is consistent with Constitution IV's "graceful reload" — bad
config does NOT take the server down.

## Backup & restore

- Backup: copy `identity.json` while the server is running. Atomic
  rename guarantees the copy sees a consistent snapshot (it sees
  either the pre-write or post-write state, never a partial). No
  freeze required.
- Restore: stop the server, replace `identity.json`, start the
  server. The next operator request authenticates against the
  restored state. Tokens revoked between backup and restore become
  valid again — operators MUST rotate any credential they care about
  after a restore.

## What is NOT in the store

- **Forwarding rules**. Rules remain in-memory per R-003. A future
  feature may add `rules.json` parallel to this design.
- **Audit log**. Audit lines emit through the existing tracing/JSON
  pipeline (R-008). Persistent audit storage is a future feature.
- **Client→server tokens** (`forward-auth::file_store`). That store
  remains a separate file (`tokens.json` or whatever
  `tokens_path` says in `server.toml`). Two stores, two locks, two
  files — see data-model.md § "Mapping to existing v0.4.0 types".

## Migration from v0.4.0

A v0.4.0 deployment has no `identity.json`. v0.5.0 startup behavior:

- If `operator_token` is set in `server.toml`: mint
  `_superadmin` user + credential at first start (writing
  `identity.json` for the first time).
- Else: server starts, but `/v1/*` returns `503 bootstrap_required`
  until `bootstrap-superadmin` runs.
- The data-plane (gRPC client channel + the existing client token
  store) is unaffected. Forwarding continues without operator
  intervention.

Downgrade from v0.5.0 → v0.4.0: the v0.4.0 server ignores
`identity.json` (it doesn't read the file). All operator requests
revert to v0.4.0's unauthenticated behavior. **No data loss**, but
the security posture reverts; this is documented in
`docs/runbook.md` as "v0.5 → v0.4 downgrade is permitted but
discouraged".

## Future schema versions (deferred)

- `version: 2` (hypothetical): add per-user token-issuance rate limit
  fields, nested grant ACLs, etc. The migration path is the schema's
  loader-side transform documented at the time. v0.5.0 does NOT
  pre-allocate fields for future use; YAGNI.
