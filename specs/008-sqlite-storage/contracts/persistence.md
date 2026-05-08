# Contract — Persistence Layer

Authoritative definition of:

1. The on-disk layout under `<data-dir>/`.
2. The schema-version handshake at boot, restore, and migration.
3. The backup file format and the rules an honest restore loop must
   honour.
4. Failure-mode protocol (corrupt file, schema-too-new, unsupported
   filesystem, lock contention).

Implementations on either side of this contract — server boot path,
backup CLI, restore CLI, future tooling that reads `state.db`
directly — MUST follow these rules. Test artefacts under
`crates/forward-server/tests/` and `crates/forward-auth/tests/` are
the verification points.

---

## 1. On-disk layout

For every running server with the spec's scope:

```
<data-dir>/
├── state.db          REQUIRED — main SQLite file
├── state.db-wal      OPTIONAL — present iff WAL has uncheckpointed frames
├── state.db-shm      OPTIONAL — present iff a process is currently open on state.db
└── state.db-journal  ABSENT in WAL mode (kept here only for documentation)
```

- `state.db-wal` and `state.db-shm` are SQLite-managed sidecars. They
  are NEVER written or read by application code; they appear and
  disappear under SQLite's control.
- After a clean shutdown the WAL is checkpointed and `state.db-wal`
  may be empty (size 0) but its presence is benign.
- `state.db-shm` exists for the lifetime of any open process; it is
  recreated on next open. Operators MUST NOT delete it while the
  server is running.
- The data-dir SHALL contain ONLY the four entries above. The
  presence of unrelated files MUST NOT fail the boot but MUST be
  ignored.

---

## 2. Schema-version handshake

The server MUST run this sequence at every startup (also at restore
completion):

```
1. open(state.db) with PRAGMA journal_mode=WAL, synchronous=NORMAL,
   foreign_keys=ON, busy_timeout=5000.
2. Read MAX(version) from schema_migrations.
3. Compute target_version = max(version baked into binary).
4. Switch on (read, target):
     read == None              → fresh DB → run all migrations 1..target.
     read == target            → no-op.
     read < target             → run forward migrations (read+1)..target
                                 in a single transaction. On failure: roll
                                 back, leave DB at `read`, refuse to start.
     read > target             → REFUSE TO START: log
                                 `event=startup.schema_version_too_new
                                  on_disk=<read> binary_supports=<target>`,
                                 exit 78 (EX_CONFIG).
5. Listener opens.
```

Migration runner uses `refinery`. Migrations are forward-only and
embedded in the binary (no external migration files).

The same sequence runs at the end of `restore` (after the file copy)
so a backup taken at version N is auto-upgraded if loaded by a binary
at version M ≥ N.

---

## 3. Backup file format

A backup artefact is a **vanilla SQLite database file** produced by
`sqlite3_backup_*` (via `rusqlite::backup::Backup`). It is bit-for-bit
identical to a quiesced `state.db` of the producing version.

- The artefact carries its own `schema_migrations` table, which is the
  schema-version envelope FR-013 requires. No additional metadata
  wrapper is needed.
- Format compatibility: any SQLite ≥ 3.0 can open the file
  read-only. Restore semantics (running the migration sequence on the
  restored file) are the v0.8 server's responsibility; third-party
  tools that just want to read may use any standard SQLite client.
- Filename convention (informational, not enforced): the backup CLI
  defaults to `forward-state-<RFC3339>.db` if `--out` names a
  directory rather than a file.

The backup CLI MUST:

1. Open `state.db` read-only at the running server's data-dir (a
   live server is allowed to keep running; SQLite's online-backup
   protocol cooperates with WAL writers).
2. Open the destination path as a fresh empty SQLite DB.
3. Run `Backup::run(-1)` (full copy in one call) — the API guarantees
   point-in-time consistency.
4. Close both handles before returning success.

The restore CLI MUST:

1. Refuse if `<data-dir>/state.db` is non-empty unless `--force` is
   passed.
2. Open the supplied artefact read-only.
3. Open the destination as a fresh empty DB at the standard path.
4. Run `Backup::run(-1)` source → destination.
5. Re-open the destination via the regular boot path (PRAGMA setup +
   schema-version handshake). On migration failure, restore is
   aborted and the destination file is removed (rename-to-temp and
   delete) so the operator is left in a known-empty state.

---

## 4. Failure-mode protocol

| Condition | Detected at | Server response |
|-----------|-------------|------------------|
| `state.db` missing, dir exists | boot | Run all migrations on a fresh DB; emit `event=startup.fresh_store` |
| `state.db` corrupt (`SQLITE_NOTADB`, header check fails) | boot, first PRAGMA | Refuse to start; log `event=startup.store_corrupt path=...`; exit 78. Recovery hint references `restore --in <backup>` |
| schema-too-new (read > target) | boot, post-PRAGMA | Refuse to start (see §2 step 4). Exit 78 |
| schema-too-old, migration in flight fails | boot, mid-migration | Roll back the failing migration in its own transaction; leave DB at the prior version; refuse to start; exit 78 |
| Filesystem unsupported (NFS / tmpfs / ramfs) | boot, after `--data-dir` resolution, BEFORE opening `state.db` | Refuse to start; log `event=startup.unsupported_filesystem path=... fs=<class>`; exit 78. Detail in `R-008`/`FR-019` |
| Another process holds the writer lock | boot, on first PRAGMA | Refuse to start; log `event=startup.store_in_use path=...`; exit 75 (EX_TEMPFAIL); operator can retry after stopping the other instance |
| Disk full mid-transaction | runtime | Map to `ForwardError::Internal`; transaction rolls back; surface 5xx with structured error to operator API caller; do not exit |
| Audit hand-off queue full | runtime | Drop oldest pending entry; increment `forward_audit_buffer_drops_total`; do NOT back-pressure operator path (FR-006) |

Exit codes follow `sysexits.h` (`EX_CONFIG=78`, `EX_TEMPFAIL=75`,
`EX_OK=0`).

---

## 5. PRAGMA contract

Every connection acquired from the pool MUST start with this exact
PRAGMA set (enforced via `r2d2_sqlite::SqliteConnectionManager::with_init`):

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 5000;     -- ms
PRAGMA temp_store = MEMORY;
PRAGMA cache_size = -8000;      -- 8 MB per connection
```

Tests MUST verify (a) all six PRAGMAs are set on fresh connections,
and (b) deviating from these defaults via configuration is not
exposed to operators in v0.8 (the values are intentionally not
operator-tunable).

---

## 6. Backwards-compat probes

At boot the server SHALL look in `<config-dir>` for any of:

- `tokens.json`
- `identity.json`
- `rules.json`

For each file present, emit:

```
event = "startup.legacy_persistence_file_ignored"
path  = "<absolute path>"
hint  = "Pre-v0.8 file; not loaded. Run `forward-server reset` to clean."
```

The server MUST NOT read these files. The presence of any of them
MUST NOT fail the boot. Tests cover the warn-and-ignore path
explicitly.
