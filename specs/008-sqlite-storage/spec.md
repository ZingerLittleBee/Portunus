# Feature Specification: Unified Embedded SQL Store for Server Persistent State

**Feature Branch**: `008-sqlite-storage`
**Created**: 2026-05-08
**Status**: Draft

## Clarifications

### Session 2026-05-08

- Q: Audit write durability window on crash — what is the maximum bounded loss between durable persistence and a process kill? → A: Typical ≤ 100 ms; under sustained burst load ≤ 1 s.
- Q: Backup compatibility policy across server versions? → A: Forward-compatible within the v0.x major — a backup produced by version X is restorable on any binary ≥ X via automatic schema migration on restore; older binaries refuse to read a newer backup.
- Q: Where does the server's persistent data file live, and how is the location selected? → A: A new `--data-dir` flag (independent from `--config-dir`) with resolution `--data-dir > $STATE_DIRECTORY > $XDG_STATE_HOME/forward-rs > $HOME/.local/state/forward-rs > ./forward-rs.state`. Production deployments are documented to point this at `/var/lib/forward-rs/`, integrated with systemd's `StateDirectory=forward-rs`. The data file inside it is named `state.db` and is not operator-renameable. The data-dir MUST refuse to operate on filesystems that cannot honour POSIX file locking and `fsync` (notably NFS and tmpfs).
- Q: Does the v0.8 work also adjust the client's `--bundle` flag, or stay strictly server-side? → A: The client gets one ergonomic addition: `--bundle` becomes optional with a documented search order (`--bundle > $FORWARD_CLIENT_BUNDLE > $XDG_CONFIG_HOME/forward-rs/client.bundle.json > $HOME/.config/forward-rs/client.bundle.json > ./client.bundle.json`). No new client-side persistent state is introduced; the client remains stateless to disk.

**Input**: User description: "v0.8: migrate all server-side persistent data
from JSON files + in-memory ring to a single embedded local SQL data store;
unify rules, RBAC (users/credentials/grants), client tokens, and audit log
into one file with multiple tables; provide schema migration and
backup/restore tooling; preserve single-binary deployment; ephemeral runtime
data stays in-process; auth seam, wire protocol, and forwarding hot path
unchanged. Clean-slate schema — no migration from existing JSON files since
the project is not yet deployed."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Audit Log Survives Server Restart (Priority: P1)

An operator is investigating an alert that fired overnight: a credential
was denied access to a rule. They open the audit view in the Web UI (or
hit `GET /v1/audit`) and expect to see the deny event. Today the
forward-server process restarts (planned upgrade, host reboot, or crash)
wipe the in-memory audit ring buffer, so any event before the most recent
restart is gone — the operator cannot complete the investigation. After
v0.8, audit events persist across restarts.

**Why this priority**: This is the single most user-visible failure of
the current persistence model. The Web UI v0.6 audit page exists
specifically so operators can investigate; today its useful window
shrinks to "since the last restart", which can be minutes during an
incident. Fixing this is the headline value of the v0.8 release.

**Independent Test**: Bootstrap a server, perform a sequence of
authenticated and denied operator requests, restart the server, then
query `/v1/audit` and confirm all prior events are still present in
newest-first order. Done in isolation from rules / RBAC migration work.

**Acceptance Scenarios**:

1. **Given** a fresh server with audit events accumulated over a session,
   **When** the server is stopped and restarted with no flags,
   **Then** all previously recorded audit events remain queryable via
   `GET /v1/audit` with the same shape and ordering as before the restart.
2. **Given** a server under sustained operator request load,
   **When** authenticated request handling latency is measured before
   and after audit-log writes are added to the persistent store,
   **Then** the additional latency on the operator request path is below
   a threshold that keeps the existing operator-API responsiveness
   indistinguishable to the user.
3. **Given** an operator filters `GET /v1/audit?outcome=deny`,
   **When** the request is served from the persistent store,
   **Then** the response shape is identical to the v0.6 / v0.7 contract
   (newest-first JSON array, same fields).

---

### User Story 2 - Unified, Atomic Control-Plane State (Priority: P1)

An operator deletes a user. That user owned three forwarding rules and
held two credentials. Today this requires three independent file writes
across `identity.json`, `tokens.json`, and `rules.json`; if the second
write fails (disk full, process killed mid-write, sigkill on host
reboot), the operator is left with inconsistent state — the user is
gone but their rules are still active, or the credentials still
authenticate. After v0.8, this multi-entity change either fully succeeds
or fully rolls back; the on-disk state never reflects a partial write.

**Why this priority**: This is correctness, not convenience. Today's
three-file model is a latent integrity bug; the only reason it has not
caused damage is that operators rarely chain these mutations. v0.8
collapses all four data domains (rules, users, credentials, grants,
client tokens) into one transactional store so the integrity guarantee
is structural, not behavioural.

**Independent Test**: Perform a multi-entity mutation (e.g., remove a
user with active rules and credentials), simulate a process kill in the
middle, restart, and verify the post-restart state is one of the two
valid endpoints (either fully pre-mutation or fully post-mutation), never
a hybrid.

**Acceptance Scenarios**:

1. **Given** a user with N rules and M credentials,
   **When** the operator removes the user,
   **Then** all N rules, M credentials, and the user record either all
   disappear together or all remain together — no intermediate state is
   ever observable.
2. **Given** the server crashes mid-way through a multi-entity mutation,
   **When** the server is restarted,
   **Then** the next read of any affected entity returns a consistent
   pre-mutation or post-mutation snapshot.
3. **Given** the existing `GET /v1/users`, `GET /v1/rules`, and
   `GET /v1/users/{id}` endpoints,
   **When** they are served by the new store,
   **Then** their JSON response contracts match the v0.7 shapes byte-for-
   byte for the fields already in the contract.

---

### User Story 3 - Backup and Restore as a Single Operation (Priority: P2)

An operator wants to take a snapshot of all server state before a risky
upgrade, or migrate the server from one host to another. Today the
operator must stop the server, copy three files (`rules.json`,
`identity.json`, `tokens.json`) at the same instant to avoid drift,
remember to also capture any cold-storage of audit (today there is none),
and then put them in the right place on the new host. After v0.8, a
single CLI command produces one self-contained backup artefact, and a
second CLI command restores it.

**Why this priority**: Backup is currently a documented manual ritual
and consistently mis-performed (audit is silently absent from any
backup). One-command backup makes disaster recovery practical and
removes a class of operator error.

**Independent Test**: Run the backup CLI on a populated server,
restore on a fresh installation, and verify all rules, users,
credentials, grants, tokens, and audit events are recoverable and
identical (modulo restore timestamps).

**Acceptance Scenarios**:

1. **Given** a populated server,
   **When** the operator runs the backup CLI to a target file,
   **Then** the resulting file is a single artefact containing all
   persistent state and is safe to copy with standard file tooling.
2. **Given** a backup file produced from server A,
   **When** the operator runs the restore CLI on a fresh installation
   of server B with the matching binary version,
   **Then** all rules, users, credentials, grants, tokens, and audit
   events are observable on server B in the same form they had on A.
3. **Given** a backup taken while the server is running,
   **When** the backup is restored,
   **Then** the restored state is internally consistent (does not
   reflect a half-applied mutation).

---

### User Story 4 - Historic Audit Query With Pagination and Time Range (Priority: P3)

An operator is preparing a quarterly compliance summary and needs to
report the count of `deny` audit events for a specific user across the
prior month. Today `/v1/audit` returns at most the last 1000 entries
unfiltered by time and the Web UI shows a flat newest-first list. After
v0.8, the same endpoint accepts pagination (limit + cursor / offset)
and a time-range filter, and the Web UI can scroll back through
historic data.

**Why this priority**: A nice-to-have that becomes natural once
historic data exists. Without persistent audit (Story 1), this
capability is meaningless; with it, the existing UI/API contract can
be extended without breaking earlier callers.

**Independent Test**: Generate audit events spread across multiple
hours / days, then call `/v1/audit` with `since`, `until`, and `limit`
parameters and verify the returned slice matches expectation; verify
omitting the new parameters returns the same response a v0.6 / v0.7
caller would have received (default newest-first slice, default cap).

**Acceptance Scenarios**:

1. **Given** audit events spanning a multi-day window,
   **When** the operator queries `GET /v1/audit?since=...&until=...&limit=N`,
   **Then** the response contains only events whose timestamps fall
   inside the window, ordered newest-first, capped at N.
2. **Given** a v0.6 / v0.7 client that does not pass the new
   parameters,
   **When** that client calls `GET /v1/audit?limit=100&outcome=deny`,
   **Then** the response shape and ordering are unchanged from the
   prior contract.
3. **Given** a result set larger than a single page,
   **When** the operator follows the documented pagination mechanism,
   **Then** every event in the time window is reachable across pages
   exactly once with no duplication or omission.

---

### Edge Cases

- **Data file missing on first start**: a fresh server with no prior
  state must self-initialise the persistent store and succeed on
  startup (no operator pre-step required).
- **Data directory on an unsupported filesystem**: when the resolved
  data-dir resides on NFS, tmpfs, or any filesystem that cannot
  honour POSIX advisory file locking or `fsync`, the server must
  refuse to start with a message that names the path, the detected
  filesystem type, and the recommended remedy (point `--data-dir`
  at a local writable filesystem). The server must NOT attempt to
  proceed with degraded durability semantics.
- **Data directory not yet created**: when the resolved data-dir
  does not exist, the server must create it with permissions that
  restrict access to the daemon user (no world or group read /
  write). When the daemon is started by systemd with
  `StateDirectory=forward-rs`, systemd's pre-creation must be
  honoured rather than overridden.
- **Data file present but corrupt**: the server must refuse to start
  with a clear, actionable error message that points at the file
  location and a recovery path (restore from backup), and must NOT
  silently re-initialise (which would erase state).
- **Data file locked by another process** (e.g., a second
  `forward-server` instance pointed at the same file): the second
  process must fail to start with a clear "store is in use" error,
  not corrupt the file or block indefinitely.
- **Schema version newer than the running binary**: the server must
  refuse to start, naming the on-disk schema version and the binary's
  supported range. Silent downgrades are forbidden.
- **Schema version older than the running binary**: a controlled
  forward migration runs at startup before the listener opens; if
  migration fails the server refuses to start and leaves the file
  untouched.
- **Disk full during a write**: the failing mutation rolls back; the
  store remains in its prior consistent state; the failure surfaces
  to the caller as a 5xx with a discoverable error.
- **Audit write under sustained operator-request load**: audit
  persistence MUST NOT block the operator request path beyond a small
  bounded latency. Sustained load that exceeds the sink's drain rate
  must drop oldest pending audit entries (with the existing
  drop counter incrementing) rather than back-pressure operator API
  callers.
- **Bootstrap superadmin on an empty store**: the existing one-time
  superadmin bootstrap path (CLI subcommand) must continue to work and
  must seed the new store atomically; partial bootstrap is not
  observable.
- **Operator-driven full reset**: an operator must have a documented
  command path to wipe the store (equivalent to the existing
  `rm rules.json` / `rm identity.json` ergonomics) without manually
  poking files. The reset path MUST also remove any
  implementation-managed sidecar files (e.g., write-ahead-log,
  shared-memory index) so that the next start observes a directory
  state indistinguishable from "never been initialised". Operators
  MUST NOT be required to identify or delete sidecar files
  themselves.
- **Restore onto a populated store**: the restore CLI must refuse to
  overwrite a non-empty store unless the operator passes an explicit
  confirmation flag.
- **Backup taken while the server is running**: the resulting backup
  must be a point-in-time consistent snapshot, not a smear across
  in-flight transactions.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The server MUST keep all persistent state in a single
  embedded local data file managed by the server process. The data
  domains in scope are: forwarding rules; operator users; operator
  credentials; operator grants; client authentication tokens; audit
  log entries; and a schema-version metadata table. No additional
  data domain is added by this feature. From the operator's view
  this is exactly one file at a known path; any storage-engine
  sidecar artefacts (write-ahead-log / shared-memory index files)
  are implementation-managed, are recreated as needed by the server
  on startup, and never need to be touched by the operator.
- **FR-002**: The server MUST NOT require any external database
  process, daemon, or service to be reachable in order to start, run,
  or shut down. The single-binary deployment posture established in
  v0.1.0 is preserved.
- **FR-003**: The on-disk file MUST be protected at filesystem level
  with the same permissions posture as today's secret-bearing files
  (owner-read/write only).
- **FR-004**: A multi-entity mutation initiated through any operator
  surface (CLI or HTTP API) MUST be applied atomically: either all
  affected rows reach the durable file together, or none do. There is
  no observable intermediate state across the rules, users,
  credentials, grants, and tokens domains.
- **FR-005**: Audit log entries previously written MUST remain
  queryable through `GET /v1/audit` after a server restart. Audit
  entries written before a clean shutdown are durable. On an abrupt
  process kill, the durability window MUST NOT exceed 100 ms in the
  typical (non-overload) case and MUST NOT exceed 1 s under sustained
  burst load; in either case, entries that pre-date the lost window
  MUST NOT be corrupted, reordered, or rendered unreadable.
- **FR-006**: The audit write path MUST NOT block the operator
  request path beyond a small bounded latency budget on the median
  case. Under sustained burst load that exceeds the audit sink's
  drain rate, the sink MUST drop oldest pending entries (incrementing
  a counter that operators can observe) rather than back-pressure the
  operator request pipeline.
- **FR-007**: `GET /v1/audit` MUST accept the existing
  `limit` and `outcome` query parameters with the same semantics as
  v0.6 / v0.7. It MUST additionally accept opt-in parameters that
  select a time window and request pagination beyond the existing
  default cap. Callers that do not pass the new parameters MUST
  receive a response shape identical to the v0.6 / v0.7 contract.
- **FR-008**: `GET /v1/rules`, `GET /v1/users`, `GET /v1/users/{id}`,
  `GET /v1/users/me`, `GET /v1/rules/{id}/stats`, and the existing
  SSE streams MUST return JSON shapes byte-for-byte identical to the
  v0.7 contract for the fields they already define. New fields MAY
  be added but no existing field is renamed, removed, or retyped.
- **FR-009**: The bearer-token authentication seam (the
  `auth_middleware` in the operator HTTP server and its
  control-plane client equivalent) MUST be the only point at which
  authentication is performed. The new store MUST sit behind that
  seam; no read or write path may bypass it.
- **FR-010**: The forwarding data plane (TCP and UDP per-connection
  / per-flow code paths) MUST NOT be modified by this feature. No
  per-byte or per-packet code path acquires a database handle or
  performs a database read.
- **FR-011**: The control-plane wire protocol between
  `forward-server` and `forward-client` MUST NOT add a breaking field
  in this feature. Additive proto fields are permitted only if a
  separate concern (out of this spec's scope) requires them.
- **FR-012**: The server MUST maintain a schema-version record in
  the data file. On startup, the server MUST compare the on-disk
  schema version against the version range its binary supports and
  MUST refuse to start when the on-disk version is newer than the
  binary's supported range. When the on-disk version is older but
  upgradable, a forward migration runs to completion before the
  listener opens; on migration failure, the server refuses to start
  and leaves the file untouched.
- **FR-013**: A backup CLI subcommand MUST produce a single
  self-contained artefact representing a point-in-time consistent
  snapshot of the server's persistent state. The artefact MUST embed
  the schema version of the producing binary so a restoring binary
  can decide whether it supports the artefact. The artefact MUST be
  copyable with standard filesystem tooling.
- **FR-014**: A restore CLI subcommand MUST accept any backup artefact
  whose embedded schema version is less than or equal to the running
  binary's supported schema version, and reconstruct the server's
  persistent state from it. When the backup's schema version is older,
  the restore path MUST run the same forward migration sequence used
  on a regular startup before the restored state becomes queryable;
  if migration fails, the running server's state MUST be left
  untouched. The restore command MUST refuse to read a backup whose
  schema version is newer than the binary supports, with a clear
  error naming both versions. The restore command MUST also refuse
  to overwrite a non-empty data file unless the operator passes an
  explicit confirmation flag.
- **FR-015**: A reset CLI subcommand (or equivalent flag on an
  existing subcommand) MUST allow the operator to wipe all persistent
  state without manually deleting files, preserving the
  "delete-and-start-fresh" ergonomics of the prior model.
- **FR-016**: Ephemeral runtime data MUST remain in-process and out
  of the persistent store. Specifically: per-target health (v0.7
  passive/active health state), runtime traffic counters, the
  Prometheus metrics registry, and the connected-clients table are
  not persisted by this feature.
- **FR-017**: The bootstrap of an initial superadmin on an empty
  store MUST succeed atomically: a partial bootstrap (user without
  credential, or vice versa) MUST NOT be observable on disk.
- **FR-018**: When two `forward-server` processes attempt to operate
  on the same data file at the same time, the second process MUST
  fail to start with a clear "store is in use" error and MUST NOT
  corrupt the file.
- **FR-019**: The server MUST accept a `--data-dir <path>` argument
  that selects the directory containing `state.db` and any
  implementation-managed sidecars. The argument MUST be independent
  of `--config-dir`. When `--data-dir` is not passed, the server
  MUST resolve the location in this order, taking the first that
  resolves: (1) `$STATE_DIRECTORY` (set by systemd's
  `StateDirectory=forward-rs`); (2) `$XDG_STATE_HOME/forward-rs`;
  (3) `$HOME/.local/state/forward-rs`; (4) `./forward-rs.state` as
  the cwd fallback. The resolved directory MUST be created if it
  does not exist, with permissions that restrict access to the
  daemon user (no world or group access). The data file inside it
  MUST be named `state.db` (operator-renameable filenames are not
  supported). On startup, if the resolved directory resides on a
  filesystem that does not support POSIX advisory file locking or
  `fsync` (e.g., common NFS configurations, tmpfs), the server MUST
  refuse to start with a clear, actionable error rather than risk
  silent data corruption.
- **FR-020**: The `forward-client` binary's `--bundle` flag MUST
  become optional. When `--bundle` is omitted, the client MUST
  resolve the bundle path in this order, taking the first that
  resolves to an existing readable file: (1) `$FORWARD_CLIENT_BUNDLE`
  environment variable; (2) `$XDG_CONFIG_HOME/forward-rs/client.bundle.json`;
  (3) `$HOME/.config/forward-rs/client.bundle.json`;
  (4) `./client.bundle.json`. When none resolve, the client MUST
  exit with a clear, actionable error naming all attempted paths.
  No client-side persistent state is introduced by this feature;
  the client remains stateless to disk other than reading the
  bundle.

### Key Entities *(include if feature involves data)*

- **Forwarding Rule**: an operator-pushed rule (`listen_port`
  / `listen_port_end`, `target_host`, `target_port` /
  `target_port_end`, `protocol`, `owner_user_id`, `targets[]` for
  multi-target, `health_check_interval_secs` for active probe).
  Same shape as v0.7; this feature does not change rule semantics.
- **Operator User**: a control-plane principal (`user_id`, `role`,
  `display_name`). Same shape as v0.5.
- **Operator Credential**: a hashed bearer credential bound to a
  user (`credential_id`, `user_id`, `status`, `issued_at`,
  `revoked_at`). Same shape as v0.5.
- **Operator Grant**: an authorisation record binding a user to a
  resource (`grant_id`, `user_id`, resource selector). Same shape
  as v0.5.
- **Client Token**: a hashed token authenticating a `forward-client`
  to the data-plane control channel (`client_name`, `token_hash`,
  `issued_at`, `revoked_at`). Same shape as v0.1 / v0.5.
- **Audit Entry**: one record per operator-API allow / deny outcome
  (`timestamp`, `user_id`, `outcome`, `action`, `resource`,
  request-correlation fields). Shape extends the v0.6 in-memory
  entry with whatever is required to support time-range and
  pagination queries.
- **Schema Version Record**: a single-row metadata entity recording
  the on-disk schema version applied so far.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: After a clean restart of `forward-server`, an operator
  can retrieve every audit event recorded before the restart through
  the same endpoint they used before the restart, with no events
  lost and no events reordered. Following an abrupt process kill,
  at most the entries from the last 100 ms (typical case) or 1 s
  (under sustained burst load) of operator activity are absent;
  every earlier entry is recoverable, ordered, and uncorrupted.
- **SC-002**: An operator can complete a full backup of server state
  to a single file, copy the file to another host, and restore the
  state on a fresh installation in under 5 minutes for a server
  carrying 10 000 audit entries, 100 rules, 50 users, and 50 client
  tokens — without losing any entity.
- **SC-003**: A multi-entity mutation that fails or is killed
  mid-flight leaves the server, on next start, in either the fully
  pre-mutation or fully post-mutation state — never a hybrid — in
  100% of repeated fault-injection runs.
- **SC-004**: Operator API request latency at the 50th and 99th
  percentile, measured under a representative operator workload,
  remains within 10% of the v0.7 baseline once persistent audit
  writes are wired in.
- **SC-005**: The operator UI's audit page can scroll back through
  any contiguous time window over the entire retained history with
  one full page of results returning to the user in under 2 seconds
  for a store carrying up to 100 000 audit entries.
- **SC-006**: A v0.7 operator-API client (CLI or Web UI) calling any
  of the unchanged endpoints (`GET /v1/rules`, `GET /v1/users`,
  `GET /v1/users/me`, `GET /v1/audit?limit=...&outcome=...`) against
  a v0.8 server receives a response that the v0.7 client parses
  without modification.
- **SC-007**: Forwarding-plane throughput and p99 latency under the
  v0.7 benchmark workload remain within 5% of the v0.7 numbers (the
  Constitution Principle II hot-path budget) on the v0.8 binary.

## Assumptions

- **Engine choice**: the embedded data store is SQLite. The choice
  was made by the operator at v0.8 spec time on the basis of
  "single-binary, no external process dependency, modest write
  volume". The Constitution `TODO(STORAGE_CHOICE)` records SQLite vs
  Postgres as the open decision; this spec closes it as SQLite. The
  detailed engine integration (driver, concurrency model,
  WAL / journal mode, fsync policy) is a plan-level concern, not a
  spec-level one.
- **Path conventions follow FHS / XDG**: the recommended deployment
  shape splits **admin-edited configuration** and **daemon-managed
  state** into two directories. For Linux system-service deployments
  the documented pairing is `--config-dir /etc/forward-rs` (holding
  `server.toml`, `server.crt`, `server.key`) and `--data-dir
  /var/lib/forward-rs` (holding `state.db`). For developer / user
  mode the same flags resolve to `$XDG_CONFIG_HOME/forward-rs` and
  `$XDG_STATE_HOME/forward-rs` respectively. The deployment
  documentation provides a systemd unit example using
  `StateDirectory=forward-rs` so operators do not have to manage
  `mkdir`, ownership, or mode bits themselves. This pairing matches
  the convention used by Grafana, Authelia, step-ca, Vaultwarden,
  Tailscale, mosquitto, syncthing, and other SQLite-backed daemons.
- **No data migration from existing files**: the project is not yet
  deployed in production. Existing `rules.json`, `tokens.json`,
  `identity.json` files in any developer environment are not
  imported by this feature; operators on a v0.7 environment are
  expected to re-bootstrap state on the v0.8 binary. This trade-off
  was made deliberately to avoid taking on a one-shot importer's
  test surface for a use case that does not exist in production.
- **Reset path mirrors the prior delete-files ergonomics**: a single
  CLI command produces the same outcome as today's manual file
  removal — empty store, ready for first-run bootstrap.
- **Downgrade path uses backup/restore, not in-place rollback**:
  schema migrations are forward-only. Rolling back to a prior server
  binary requires an operator restore from a backup taken on that
  prior binary (or earlier); a backup produced by a newer binary
  cannot be loaded by an older binary, by design — the older
  binary refuses with an explicit version-mismatch error.
- **Operator-managed retention**: the persistent audit table grows
  unbounded by default. v0.8 ships a CLI prune command but does not
  enforce automatic eviction; operators choose their own retention
  policy. The current Prometheus drop counter remains and tracks any
  in-memory burst-buffer drops separately from durable retention.
- **Single-process operator**: only one `forward-server` process
  operates on a given data file at a time. Multi-process or
  multi-host clustering is out of scope for v0.8; the file-locking
  guard in FR-018 is a defence-in-depth check, not a clustering
  enabler.
- **TLS material stays as files**: server TLS certificate and
  private key continue to be operator-managed files outside the
  data store. They are not "data" in the persistent-state sense.
- **`tokens.json` and `identity.json` retire as source-of-truth**:
  after v0.8 ships, the JSON-file persistence layer for these
  domains is removed from the codebase. The data file is the only
  source of truth. (Read-only legacy import tooling is explicitly
  not provided per the no-migration assumption above.) The
  pre-v0.8 paths (`<config-dir>/tokens.json`,
  `<config-dir>/identity.json`, `<config-dir>/rules.json`) are not
  read on startup, even if leftover files from a prior
  installation exist.
- **Client remains stateless to disk**: `forward-client` does not
  introduce any persistent file in v0.8. Per-target health,
  runtime stats, the resolver cache, and reconnect state stay
  in-memory. The bundle is read-only configuration material; it is
  never rewritten by the client. Consequently no `--data-dir`
  exists on the client side.
- **HTTP path versioning**: existing `/v1/*` endpoints stay on `v1`.
  Only additive query parameters are added; no `/v2/*` introduction
  is required by this feature.
- **Audit ingestion is async-decoupled from the operator request
  path**: a small bounded in-memory hand-off queue sits between the
  `auth_layer` emit sites and the durable writer. The durability
  contract (FR-005) only covers entries that have crossed into the
  durable writer; entries waiting in the in-memory hand-off queue at
  the moment of a process kill MAY be lost.
