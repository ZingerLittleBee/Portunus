# On-Disk Persistence Contract

**Feature**: 001-tcp-forward-mvp

The MVP persists three things on the server side: the token store, the TLS
certificate, and the TLS private key. This document fixes their on-disk
layout so future versions of forward-rs can reason about migration.

---

## Layout

```
<config_dir>/
├── server.toml          # config (operator-managed)
├── server.crt           # TLS leaf cert (PEM)
├── server.key           # TLS private key (PEM, mode 0600)
└── tokens.json          # token store (mode 0600)
```

`<config_dir>` defaults to:
- `$XDG_CONFIG_HOME/forward-rs` if set, else `$HOME/.config/forward-rs` on Linux/macOS
- overridable via `--config-dir <path>` flag on `forward-server`.

The directory is created on first launch with mode `0700`.

---

## tokens.json

```json
{
  "version": 1,
  "tokens": [
    {
      "client_name": "edge-01",
      "token_hash": "f5e7c2a1...64hex...",
      "issued_at": "2026-05-06T12:00:00Z",
      "revoked_at": null
    },
    {
      "client_name": "edge-02",
      "token_hash": "9a3b...",
      "issued_at": "2026-05-06T12:05:00Z",
      "revoked_at": "2026-05-06T13:00:00Z"
    }
  ]
}
```

**Invariants**:
- `version` is `1` for this MVP. The server refuses to load a file with
  an unknown `version` (forward-compatible: never silently upgrade).
- `client_name` values are unique within `tokens` (enforced by the
  loader; a duplicate refuses to load and the server exits with a clear
  error message). Q2's `client_already_exists` is enforced at write time
  via the same uniqueness check.
- `token_hash` is `blake3(token)` hex-encoded (lowercase, 64 chars).
- `revoked_at` is `null` or RFC3339 UTC timestamp.
- Revoked records are **kept**, not deleted (audit trail).

**Write protocol** (every mutation):
1. Read current file contents into memory.
2. Apply the mutation in-memory.
3. Serialize to a temp file `tokens.json.tmp.<pid>.<random>` in the same directory.
4. `fsync` the temp file.
5. `rename(2)` temp file over `tokens.json` (atomic on Linux/macOS within the same FS).
6. `fsync` the parent directory.

If any step fails, the temp file is removed and the original `tokens.json`
remains untouched. The mutation is reported to the operator as failed.

---

## server.crt / server.key

Standard PEM. `server.key` is mode `0600`; the server refuses to start if
the mode is more permissive (defence against accidental world-readable
keys).

On first launch, if either file is absent, the server generates a fresh
self-signed cert using `rcgen` 0.13:
- ECDSA P-256 key.
- CN: hostname of the server (or `forward-rs-server` if hostname lookup
  fails).
- 10-year validity.
- No SANs (pinning by leaf fingerprint makes name validation moot).

The operator may replace either file with their own. Restart picks them
up. Pinned clients break if the leaf cert's SHA-256 fingerprint changes —
they will need re-provisioning (or the operator can update the bundle's
`server_cert_sha256` out of band).

---

## server.toml

Operator-managed; not written by forward-rs except by an explicit
`forward-server init` command (out of MVP scope, but the file may be
hand-written). See `data-model.md` `ServerConfig` for the field set.

---

## Migration model

- A file with `"version": N` where `N > current_supported` is a hard
  failure. The server prints "config schema vN is newer than this binary
  supports (max v1)" and exits.
- A file with `"version": N` where `N < current_supported` triggers an
  in-place migration: the server reads the old shape, writes the new
  shape via the same atomic-write protocol, and continues. Migrations are
  documented per release.
- For MVP there are no migrations to apply (only `v1` exists).
