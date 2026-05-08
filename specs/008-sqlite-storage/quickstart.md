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
#      /etc/forward-rs/        admin-edited config + TLS material
#      /var/lib/forward-rs/    daemon-managed state.db

sudo install -d -m 0700 -o forward-rs -g forward-rs /var/lib/forward-rs
sudo install -d -m 0750 -o forward-rs -g forward-rs /etc/forward-rs
```

A minimal systemd unit:

```ini
# /etc/systemd/system/forward-server.service
[Unit]
Description=forward-rs server
After=network.target

[Service]
User=forward-rs
Group=forward-rs
StateDirectory=forward-rs
StateDirectoryMode=0700
ExecStart=/usr/local/bin/forward-server serve \
    --config-dir /etc/forward-rs \
    --data-dir   /var/lib/forward-rs
Restart=on-failure
RestartSec=2

[Install]
WantedBy=multi-user.target
```

`StateDirectory=forward-rs` is the systemd-native way: systemd
auto-creates `/var/lib/forward-rs` with the right ownership and
exposes `$STATE_DIRECTORY=/var/lib/forward-rs` to the process.

Bootstrap the first superadmin (one-time):

```bash
sudo -u forward-rs forward-server \
    --config-dir /etc/forward-rs \
    --data-dir   /var/lib/forward-rs \
    bootstrap-superadmin --name 'Initial admin'
# Prints the freshly issued bearer token to stdout.
# Save it — it is shown only once.
```

Then enable and start:

```bash
sudo systemctl enable --now forward-server
sudo systemctl status forward-server
```

Smoke check:

```bash
curl -k -H "Authorization: Bearer $TOKEN" https://localhost:7443/v1/users/me
```

---

## 2. Dev workstation cold start

```bash
cargo build --release -p forward-server -p forward-client
```

Defaults are XDG-aligned, so no flags needed:

```bash
./target/release/forward-server bootstrap-superadmin --name 'Dev admin'
# Token printed once; copy it.

./target/release/forward-server serve
# Reads:
#   $XDG_CONFIG_HOME/forward-rs   (or ~/.config/forward-rs)
#   $XDG_STATE_HOME/forward-rs    (or ~/.local/state/forward-rs)
```

Confirm path layout:

```bash
ls -la ~/.local/state/forward-rs
# state.db
# state.db-shm   (present while server is running)
# state.db-wal   (present iff WAL frames pending)

ls -la ~/.config/forward-rs
# server.toml         (optional — auto-generated from defaults if absent)
# server.crt
# server.key
```

For a v0.7 dev install lurking in `~/.config/forward-rs` (or
wherever `--config-dir` pointed):

```bash
forward-server serve
# Logs:
#   event=startup.legacy_persistence_file_ignored path=~/.config/forward-rs/tokens.json
#   event=startup.legacy_persistence_file_ignored path=~/.config/forward-rs/identity.json
#   event=startup.legacy_persistence_file_ignored path=~/.config/forward-rs/rules.json
# Server proceeds with empty state.db; bootstrap-superadmin is now needed.
```

To clean up:

```bash
rm ~/.config/forward-rs/{tokens,identity,rules}.json
forward-server reset --confirm    # also wipes state.db if present
```

---

## 3. Backup / restore walkthrough

Take a backup while the server is running:

```bash
forward-server backup --out ~/forward-backup-$(date +%F).db
# /home/me/forward-backup-2026-05-08.db
# event=cli.backup_complete ...
```

Verify the artefact:

```bash
sqlite3 ~/forward-backup-2026-05-08.db \
    'SELECT version FROM schema_migrations ORDER BY version DESC LIMIT 1;'
```

Restore on a fresh host:

```bash
# Stop any local server.
forward-server reset --confirm

forward-server restore --in /tmp/forward-backup-2026-05-08.db
forward-server serve
# State is exactly what the source had; if the binary is newer than the
# backup's schema, forward migrations run automatically.
```

---

## 4. `forward-client` walkthrough

Provision a client (server side):

```bash
forward-server provision-client --client-name edge-1 \
    --advertised-endpoint forward.example.com:7443 \
    --out edge-1.bundle.json
```

Run the client (any of these works):

```bash
# Explicit
forward-client --bundle ~/edge-1.bundle.json

# Via env var
FORWARD_CLIENT_BUNDLE=~/edge-1.bundle.json forward-client

# Drop the bundle into XDG config and let resolution find it
mkdir -p ~/.config/forward-rs
mv edge-1.bundle.json ~/.config/forward-rs/client.bundle.json
forward-client
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
cargo test -p forward-server --test audit_persists_across_restart

# Multi-entity atomic mutation under SIGKILL:
cargo test -p forward-auth --test multi_entity_atomic

# Backup → restore roundtrip:
cargo test -p forward-server --test backup_restore_roundtrip

# CLI contract:
cargo test -p forward-server --test cli_backup
cargo test -p forward-server --test cli_restore
cargo test -p forward-server --test cli_reset
cargo test -p forward-server --test cli_audit_prune

# Operator API regression (proves v0.7 byte-stability):
cargo test -p forward-server --test operator_api_v07_compat
```

Run the new bench (criterion):

```bash
cargo bench -p forward-server --bench operator_api
# Produces target/criterion/... — compare against the saved v0.7
# baseline checked into specs/008-sqlite-storage/baselines/.
```

Regenerate fixtures (if you change a migration):

```bash
cargo run -p forward-server --bin forward-server -- \
    --data-dir tests/fixtures/seeded \
    bootstrap-superadmin --name Seed
# Then commit tests/fixtures/seeded/state.db.
```

---

## 7. Common pitfalls

- **Pointing `--data-dir` at NFS / tmpfs**: the server refuses to
  start with `event=startup.unsupported_filesystem`. Move the
  data-dir to a local writable filesystem.
- **Running two `forward-server serve` against the same data-dir**:
  the second exits with `event=startup.store_in_use` (exit 75).
  This is by design — clustering is out of scope for v0.8.
- **Backup taken on v0.9 and trying to restore on a v0.8 binary**:
  refused with `event=startup.schema_version_too_new`. Either keep
  the older backup or restore using the v0.9 binary.
- **`reset --confirm` does not wipe `--config-dir`**: it only
  removes state. Keep that in mind if you also want to clear the
  server cert / key.
