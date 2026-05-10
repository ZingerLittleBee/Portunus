# Contract — CLI Surface Delta

The v0.8 CLI introduces 4 new server subcommands and 1 ergonomic
change on the client side. Existing subcommands keep their v0.7
spelling, flags, and exit codes byte-stable.

This contract is the source of truth for every CLI test under
`crates/forward-server/tests/cli_*.rs` and
`crates/forward-client/tests/bundle_search_path.rs`.

---

## `forward-server` — server-side CLI

### Top-level flag added

```
--data-dir <PATH>
    Directory containing the persistent state database (state.db
    plus SQLite-managed sidecars). Independent from --config-dir.

    Resolution order when omitted (first that resolves):
      1. $STATE_DIRECTORY                  (set by systemd's StateDirectory=)
      2. $XDG_STATE_HOME/portunus
      3. $HOME/.local/state/portunus
      4. ./portunus.state
```

The flag MUST be accepted by every subcommand that opens the store
(`serve`, `bootstrap-superadmin`, `provision-client`, `revoke`,
`list-clients`, `push-rule`, `remove-rule`, `list-rules`,
`rule-stats`, plus the new ones below). Subcommands that do not open
the store (e.g., `--version`) MUST NOT require it.

### New subcommand: `serve` — additive flags only

```
forward-server serve [--config-dir <DIR>] [--data-dir <DIR>] [other v0.7 flags]
```

No flag is renamed or removed. Operators with existing systemd units
keep working as long as they pass `--data-dir` (or rely on
`StateDirectory=`).

### New subcommand: `backup`

```
forward-server backup --out <PATH> [--data-dir <DIR>]

  --out <PATH>     Destination file path. If <PATH> is a directory,
                   the artefact is written as
                   <PATH>/forward-state-<RFC3339>.db inside it.
                   Refuses to overwrite an existing file.
```

Behaviour:

- Reads `state.db` via the SQLite Online Backup API while the server
  may continue running.
- Exits 0 on success, prints the absolute artefact path to stdout
  followed by a single line of `event=cli.backup_complete ...`
  structured-log to stderr.
- Exits 1 with a structured error if the data-dir is empty, the
  destination is unwritable, or the source is corrupt.

### New subcommand: `restore`

```
forward-server restore --in <PATH> [--data-dir <DIR>] [--force]

  --in <PATH>      Backup artefact produced by `backup`.
  --force          Permit overwriting a non-empty <data-dir>/state.db.
                   Without this flag, refuses if state.db is non-empty.
```

Behaviour:

1. Validate `<PATH>` exists and is a readable SQLite database.
2. Refuse if destination state.db is non-empty unless `--force`.
3. Copy via Online Backup API.
4. Run the standard schema-version handshake on the restored file
   (auto-applies forward migrations if the backup was older).
5. Exit 0 on success.

If the source backup's schema version is newer than the binary's
supported range, exit 78 with a clear version-mismatch message.

### New subcommand: `reset`

```
forward-server reset --confirm [--data-dir <DIR>]

  --confirm        Mandatory. Without it, prints the exact command the
                   operator must run, then exits 1.
```

Behaviour:

1. Refuses to operate if `state.db` does not look like a SQLite
   database (signature check) — protects against typo'd `--data-dir`.
2. Closes any pool/handle.
3. Removes `state.db`, `state.db-wal`, `state.db-shm` (rename-to-temp
   then unlink, with fallback).
4. Emits one structured log line summarising what was deleted.
5. Exit 0.

After `reset` the next `serve` start observes a fresh DB and runs
`bootstrap-superadmin` interactively (or via its own subcommand) is
expected next.

### New subcommand: `audit prune`

```
forward-server audit prune --before <RFC3339> [--data-dir <DIR>] [--dry-run]

  --before <RFC3339>   Delete audit entries with ts < <RFC3339>.
  --dry-run            Print the count of entries that WOULD be
                       deleted, do not modify the DB.
```

Behaviour:

- Acquires an immediate transaction, deletes the matching rows, runs
  `PRAGMA incremental_vacuum;` to reclaim file space.
- `--dry-run` returns the same count via `SELECT COUNT(*)` without
  modifying the DB.
- Exit 0 on success; non-zero on transaction failure.

This is the only path that mutates the `audit` table beyond
INSERTs.

### Subcommands unchanged in spelling and behaviour (storage swap is invisible)

- `bootstrap-superadmin` — atomic write through the new store
- `provision-client` — same
- `revoke` — same
- `list-clients` — same
- `push-rule` — same
- `remove-rule` — same
- `list-rules` — same
- `rule-stats` — same; per-target stats unchanged

A regression test snapshots stdout for each of these on a populated
store and asserts byte-stable output between v0.7 and v0.8 (modulo
seeded ULIDs and timestamps).

---

## `forward-client` — client-side CLI

### `--bundle` becomes optional

```
forward-client [--bundle <PATH>] [other v0.7 flags]

  --bundle <PATH>      Optional. Resolution order when omitted:
                         1. $FORWARD_CLIENT_BUNDLE
                         2. $XDG_CONFIG_HOME/portunus/client.bundle.json
                         3. $HOME/.config/portunus/client.bundle.json
                         4. ./client.bundle.json
                       When none resolve, exits 1 listing the paths
                       attempted.
```

No other change to client CLI surface. Tests cover all 4 fallback
paths and the explicit override.

---

## Exit-code conventions (reaffirmed)

| Code | Meaning | Examples |
|------|---------|----------|
| 0 | Success | All happy paths |
| 1 | Generic error | `--out` not writable; unknown `client_name` for `revoke` |
| 75 | EX_TEMPFAIL | Store in use by another process |
| 78 | EX_CONFIG | Schema-too-new; corrupt store; unsupported filesystem |

These are unchanged from v0.7; new subcommands follow the same map.
