# Quickstart — 008 Unified SQLite Store

Three audiences:

1. **Operators** standing up v0.8 in production for the first time.
2. **Operators** moving from v0.7 (which never reached production —
   per the spec's no-migration assumption — but this section covers
   the dev-environment cleanup).
3. **Contributors** running the new test suite locally.

---

## 1. Production cold start (Linux, systemd)

```bash
# 0. Pick the standard FHS pair:
#      /etc/portunus/        admin-edited config + TLS material
#      /var/lib/portunus/    daemon-managed state.db

sudo install -d -m 0700 -o portunus -g portunus /var/lib/portunus
sudo install -d -m 0750 -o portunus -g portunus /etc/portunus
```

A minimal systemd unit:

```ini
# /etc/systemd/system/portunus-server.service
[Unit]
Description=Portunus server
After=network.target

[Service]
User=portunus
Group=portunus
StateDirectory=portunus
StateDirectoryMode=0700
ExecStart=/usr/local/bin/portunus-server serve \
    --config-dir /etc/portunus \
    --data-dir   /var/lib/portunus
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
```

`StateDirectory=portunus` is the systemd-native way: systemd
auto-creates `/var/lib/portunus` with the right ownership and
exposes `$STATE_DIRECTORY=/var/lib/portunus` to the process.

Bootstrap the first superadmin (one-time):

```bash
sudo -u portunus portunus-server \
    --config-dir /etc/portunus \
    --data-dir   /var/lib/portunus \
    bootstrap-superadmin --name 'Initial admin'
# Prints the freshly issued bearer token to stdout.
# Save it — it is shown only once.
```

Then enable and start:

```bash
sudo systemctl enable --now portunus-server
sudo systemctl status portunus-server
```

Smoke check:

```bash
curl -k -H "Authorization: Bearer $TOKEN" https://localhost:7443/v1/users/me
```

---

## 2. Dev workstation cold start

```bash
cargo build --release -p portunus-server -p portunus-client
```

Defaults are XDG-aligned, so no flags needed:

```bash
./target/release/portunus-server bootstrap-superadmin --name 'Dev admin'
# Token printed once; copy it.

./target/release/portunus-server serve
# Reads:
#   $XDG_CONFIG_HOME/portunus   (or ~/.config/portunus)
#   $XDG_STATE_HOME/portunus    (or ~/.local/state/portunus)
```

Confirm path layout:

```bash
ls -la ~/.local/state/portunus
# state.db
# state.db-shm   (present while server is running)
# state.db-wal   (present iff WAL frames pending)

ls -la ~/.config/portunus
# server.toml         (optional — auto-generated from defaults if absent)
# server.crt
# server.key
```

For a v0.7 dev install lurking in `~/.config/portunus` (or
wherever `--config-dir` pointed):

```bash
portunus-server serve
# Logs:
#   event=startup.legacy_persistence_file_ignored path=~/.config/portunus/tokens.json
#   event=startup.legacy_persistence_file_ignored path=~/.config/portunus/identity.json
#   event=startup.legacy_persistence_file_ignored path=~/.config/portunus/rules.json
# Server proceeds with empty state.db; bootstrap-superadmin is now needed.
```

To clean up:

```bash
rm ~/.config/portunus/{tokens,identity,rules}.json
portunus-server reset --confirm    # also wipes state.db if present
```

---

## 3. Backup / restore walkthrough

Take a backup while the server is running:

```bash
portunus-server backup --out ~/portunus-backup-$(date +%F).db
# /home/me/portunus-backup-2026-05-08.db
# event=cli.backup_complete ...
```

Verify the artefact:

```bash
sqlite3 ~/portunus-backup-2026-05-08.db \
    'SELECT version FROM schema_migrations ORDER BY version DESC LIMIT 1;'
```

Restore on a fresh host:

```bash
# Stop any local server.
portunus-server reset --confirm

portunus-server restore --in /tmp/portunus-backup-2026-05-08.db
portunus-server serve
# State is exactly what the source had; if the binary is newer than the
# backup's schema, forward migrations run automatically.
```

---

## 4. `portunus-client` walkthrough

Provision a client (server side):

```bash
portunus-server provision-client --client-name edge-1 \
    --advertised-endpoint forward.example.com:7443 \
    --out edge-1.bundle.json
```

Run the client (any of these works):

```bash
# Explicit
portunus-client --bundle ~/edge-1.bundle.json

# Via env var
PORTUNUS_CLIENT_BUNDLE=~/edge-1.bundle.json portunus-client

# Drop the bundle into XDG config and let resolution find it
mkdir -p ~/.config/portunus
mv edge-1.bundle.json ~/.config/portunus/client.bundle.json
portunus-client
```

If no path resolves the client exits 1 and lists every path it
tried, in order.

---

## 5. Audit page — historic query

```bash
# v0.7-shape (still works) — newest 100 deny events:
curl -H "Authorization: Bearer $TOKEN" \
     'https://server:7443/v1/audit?limit=100&outcome=deny'
# Returns a JSON array (root).

# v0.8 envelope — last 24 hours, paginate:
curl -H "Authorization: Bearer $TOKEN" \
     'https://server:7443/v1/audit?since=2026-05-07T15:00:00Z&limit=50'
# {"entries":[...], "next_cursor":"abc123"}

curl -H "Authorization: Bearer $TOKEN" \
     'https://server:7443/v1/audit?cursor=abc123&limit=50'
# next page
```

The Web UI's audit page is updated to use the envelope automatically;
no operator action needed.

---

## 6. Contributor recipes

Run the new test suites:

```bash
# Whole workspace — make sure v0.7 contracts still hold:
cargo test --workspace --release

# Audit-survives-restart integration:
cargo test -p portunus-server --test audit_persists_across_restart

# Multi-entity atomic mutation under SIGKILL:
cargo test -p portunus-auth --test multi_entity_atomic

# Backup → restore roundtrip:
cargo test -p portunus-server --test backup_restore_roundtrip

# CLI contract:
cargo test -p portunus-server --test cli_backup
cargo test -p portunus-server --test cli_restore
cargo test -p portunus-server --test cli_reset
cargo test -p portunus-server --test cli_audit_prune

# Operator API regression (proves v0.7 byte-stability):
cargo test -p portunus-server --test operator_api_v07_compat
```

Run the new bench (criterion):

```bash
cargo bench -p portunus-server --bench operator_api
# Produces target/criterion/... — compare against the saved v0.7
# baseline checked into specs/008-sqlite-storage/baselines/.
```

Regenerate fixtures (if you change a migration):

```bash
cargo run -p portunus-server --bin portunus-server -- \
    --data-dir tests/fixtures/seeded \
    bootstrap-superadmin --name Seed
# Then commit tests/fixtures/seeded/state.db.
```

---

## 7. Common pitfalls

- **Pointing `--data-dir` at NFS / tmpfs**: the server refuses to
  start with `event=startup.unsupported_filesystem`. Move the
  data-dir to a local writable filesystem.
- **Running two `portunus-server serve` against the same data-dir**:
  the second exits with `event=startup.store_in_use` (exit 75).
  This is by design — clustering is out of scope for v0.8.
- **Backup taken on v0.9 and trying to restore on a v0.8 binary**:
  refused with `event=startup.schema_version_too_new`. Either keep
  the older backup or restore using the v0.9 binary.
- **`reset --confirm` does not wipe `--config-dir`**: it only
  removes state. Keep that in mind if you also want to clear the
  server cert / key.
