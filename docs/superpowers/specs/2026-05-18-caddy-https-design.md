# Caddy HTTPS for the Server Install — Design

> Status: Draft
> Reference studied: `~/Documents/ServerBee/deploy/install.sh`
> (install_caddy / write_caddyfile / DNS precheck / setup_domain / cmd_domain)
> Supersedes the prior "No Caddy / HTTPS" non-goal in
> `2026-05-18-install-wizard-ux-design.md` — at explicit user request.

## Problem

The Portunus server's operator HTTP listener (Web UI + `/v1` operator
API + `/metrics`) is loopback-pinned (default `127.0.0.1:7080`). Reaching
the management UI from a browser today needs an SSH tunnel. Operators
want a real HTTPS URL.

## Goal

Optionally put **host Caddy** in front of the loopback operator HTTP,
obtaining an automatic Let's Encrypt certificate for an operator-owned
domain. The domain must already resolve to the server's public IP before
setup (DNS precheck enforces this).

## What Caddy fronts (decision)

`https://<domain>` → `reverse_proxy 127.0.0.1:<operator-http-port>`
(port = the port of `--operator-http-listen`, default `7080`).

- The operator HTTP listener stays loopback-bound; **host Caddy** is the
  only thing that reaches it (Caddy runs on the same host for both the
  binary+systemd and docker deploy forms — docker publishes
  `127.0.0.1:7080:7080`, so the loopback target is identical).
- The gRPC control plane (`:7443`) is **untouched** — it keeps its own
  TLS cert. Caddy is never placed in front of gRPC.
- Security note (documented in output): enabling a domain makes the Web
  UI publicly reachable over TLS; it remains protected by the existing
  operator bearer-token / login auth.

## Scope

- Server role only. Both deploy forms (host Caddy proxies loopback).
- New optional flag `--domain <fqdn>` (+ `--acme-email <addr>`,
  `--skip-dns-check`).
- Interactive server wizard: one extra optional prompt after the
  advertised endpoint — "HTTPS domain (blank = skip)".
- New verb `domain <fqdn>` to (re)configure Caddy for an existing
  install (mirrors ServerBee `cmd_domain`).
- `uninstall` removes the managed Caddy block (best-effort) and reloads
  Caddy; it does NOT uninstall the Caddy package.
- Summary block shows the domain line when set.

Non-goals: no Caddy in front of gRPC; no wildcard/DNS-01; no multi-domain;
no Caddy removal on uninstall beyond the managed block; no Windows.

## DNS precheck (user requirement)

Before any Caddy action, resolve the domain and require it to point at
this server's public IP:

1. `public_ip` = `detect_public_ip()` (existing helper; public probe →
   NIC → loopback).
2. `dns_a` = `getent ahostsv4 <domain>` (fallback `dig +short A`).
3. If `dns_a` does not contain `public_ip`:
   - interactive: print the exact A record to add, then re-check on
     Enter / abort on Ctrl-C;
   - non-interactive: `die` with guidance.
4. `--skip-dns-check` bypasses (for operators terminating TLS elsewhere
   or split-horizon DNS). `PORTUNUS_SKIP_IP_PROBE` still applies to the
   public-IP side.

## Caddy install (per ServerBee)

`ensure_caddy()`:

- already on PATH ⇒ skip.
- Debian/Ubuntu (`ID`/`ID_LIKE` ~ debian): add Cloudsmith stable apt
  repo + key, `apt-get install -y caddy`.
- Fedora/RHEL/CentOS (`ID_LIKE` ~ rhel/fedora): `dnf`/`yum` COPR
  `@caddy/caddy`, install caddy.
- else: `die` with the manual Caddyfile snippet to add by hand.
- Port check: if `:80`/`:443` is held by a non-caddy listener ⇒ `die`
  with guidance.

## Caddyfile (idempotent managed block)

File `/etc/caddy/Caddyfile`. Back up once per change to
`Caddyfile.portunus.<ts>.bak`. Maintain exactly one delimited block:

```
# >>> portunus >>>
<domain> {
    reverse_proxy 127.0.0.1:<op_http_port>
}
# <<< portunus <<<
```

- If `--acme-email` given and the file has no global `email`, prepend a
  global options block `{ email <addr> }`.
- Re-running `domain` replaces the block in place (delete old delimited
  block, append fresh) so the domain/port can change without clobbering
  other sites in the user's Caddyfile.
- `uninstall` deletes the delimited block (and the global block only if
  we added it and it is now unused — keep simple: leave global email).

## setup flow

`setup_caddy_domain()` (called by the `domain` verb, by `--domain` on
server install, and by the wizard when a domain is entered):

1. validate FQDN (`valid_fqdn`), require server role.
2. DNS precheck (above) unless `--skip-dns-check`.
3. `ensure_caddy`.
4. `write_caddy_block`.
5. `systemctl enable --now caddy` + `systemctl reload caddy`
   (fallback: `caddy reload --config <file>`; if no systemd, print
   manual run command).
6. Verify: poll `https://<domain>/` for HTTP 200 (SPA index) up to ~12×5s
   to allow ACME issuance; on timeout, **warn** (not fatal) with
   `journalctl -u caddy` + DNS-propagation hint.
7. Print the HTTPS URL and the public-exposure security note.

`--dry-run` prints the plan (domain, reverse-proxy target, Caddyfile
path, DNS precheck intent) and performs zero network / zero writes
(preserves the global dry-run invariant).

## Interface details

- Globals: `DOMAIN=""`, `ACME_EMAIL=""`, `SKIP_DNS_CHECK="no"`.
- Flags: `--domain <fqdn>`, `--acme-email <addr>`, `--skip-dns-check`.
- Verb: `domain <fqdn>` → `lifecycle_domain` (reuses meta to find the
  operator-http port + deploy form; server only).
- Server install (`dispatch_verb` install branch): after meta write, if
  `DOMAIN` set, `setup_caddy_domain`.
- Wizard server branch: after advertised endpoint, prompt
  `ask_domain` (blank = skip). If non-blank, validate + stash in
  `DOMAIN`; summary shows it; setup runs as part of install.
- Meta: record `domain=<fqdn>` when configured (so `status` / `upgrade`
  can show / re-assert it).
- Test seams: `--valid-fqdn <s>` (exit 0/1), `--render-caddy <domain>
  <port>` (print the managed block, no write).

## i18n

New EN/ZH keys (parity enforced by existing coverage test):
`ask_domain`, `dns_check`, `dns_mismatch`, `dns_help`, `caddy_installing`,
`caddy_done`, `caddy_verify`, `caddy_verify_warn`, `https_ready`,
`https_public_note`, `sum_domain`, `bad_domain`. ZH uses the natural
phrasing established in the latest i18n polish.

## Testing

Network-free harness (`scripts/install.test.sh`):
- `--valid-fqdn`: accepts `serverbee-test.900040.xyz`, rejects
  `no_dot`, `bad host`, empty, `-leading.com`.
- `--render-caddy serverbee-test.900040.xyz 7080` contains
  `reverse_proxy 127.0.0.1:7080` and the `# >>> portunus >>>` markers.
- `--domain` accepted in the server dry-run plan; client + `--domain`
  ⇒ error (server only).
- `domain` verb `--dry-run` writes nothing, exits 0.
- EN/ZH parity for the new keys; shellcheck clean.

VPS smoke (covers every point), domain `serverbee-test.900040.xyz`
(already A-record → `207.241.173.217`):
- DNS precheck passes for the real domain; a bogus domain ⇒ guided
  failure; `--skip-dns-check` bypasses.
- server binary install `--domain serverbee-test.900040.xyz` ⇒ Caddy
  installed, Caddyfile block written, `caddy` active, `https://<domain>/`
  returns 200 with a valid Let's Encrypt cert; meta has `domain=`.
- `domain` verb re-point to a different port; block replaced in place,
  other Caddyfile content preserved.
- server docker install `--domain …` ⇒ same end state (Caddy proxies
  `127.0.0.1:7080` → container).
- interactive wizard server: entering the domain at the new prompt
  yields the same result; blank skips Caddy entirely.
- `uninstall` removes the managed block and reloads Caddy; HTTPS stops;
  Caddy package remains; full host cleanup at the end.
- Regression: server install with NO domain is byte-identical to before
  (no Caddy touched); `--dry-run` performs zero network/writes.

## Risks

- ACME issuance latency / rate limits: verification is a warning, not
  fatal; Let's Encrypt staging not used (real domain provided for test).
- Existing user Caddyfile: edits are confined to the delimited block
  with a timestamped backup each change.
- Port 80/443 already in use: detected up front with a clear error.
- Reverses a prior architectural non-goal: documented here and in the
  output security note; operator auth still gates the now-public UI.
