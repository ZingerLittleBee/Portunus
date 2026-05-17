# Interactive `install.sh` — Design

> Status: Draft
> Reference studied: `/Users/zingerbee/Documents/ServerBee/deploy/install.sh`
> Replaces: `scripts/install.sh` (current 180-line POSIX downloader)

## Goal

Turn `scripts/install.sh` into a single self-contained **bash 4+**
full-lifecycle manager for Portunus — Install / Uninstall / Upgrade /
Status / Service / Config / Env — bilingual (en/zh), supporting both the
release-binary+systemd and Docker Compose deploy forms, while preserving
the existing non-interactive flag interface that CI, automation, and the
documented `curl … | …` one-liner depend on.

## Decisions (locked during brainstorming)

| Axis | Decision |
|------|----------|
| Scope | Full lifecycle manager (menu: Install/Uninstall/Upgrade/Status/Service/Config/Env/Exit) |
| Shell | bash 4+ (entry guard hard-errors below 4) |
| i18n | Bilingual en/zh, autodetect + first-run prompt, `PORTUNUS_LANG`/`--lang` override |
| Layout | Single self-contained file replacing `scripts/install.sh` (filename kept) |
| Deploy forms | Both binary+systemd **and** Docker Compose; subcommands dispatch by detected form |
| Server config | Centered on advertised-endpoint + common items (data-dir, operator-http-listen, version pin) |
| Piped TTY | Read from `/dev/tty` when piped so `curl … | bash` reaches the menu |
| Structure | One file, comment-banner sections, function-per-concern |

## Non-Goals (YAGNI)

- No Caddy / domain / HTTPS management (operator HTTP is loopback-pinned;
  the only TLS is the gRPC server cert — not an installer concern).
- No generic key=value config editor — only the scoped keys below.
- No Windows; no non-systemd init systems (warn + manual instructions,
  as the current script already does for `--systemd` on non-Linux).

## 1. File, contract, mode detection

- `scripts/install.sh`: `#!/usr/bin/env bash`, `set -euo pipefail`.
- **Entry guard:** if `${BASH_VERSINFO:-0} < 4`, print an error (incl.
  macOS `brew install bash` hint) and exit non-zero.
- **`curl | bash` self-path:** detect whether the script runs as a file
  or piped (mirror ServerBee's `BASH_SOURCE[0]` case) so subcommands can
  reference the right installer copy.
- **Mode dispatch:**
  - *Non-interactive* when any actionable verb/role/recognized flag is
    present, OR `--yes`, OR no usable TTY.
  - *Interactive* when no actionable args **and** a TTY is reachable:
    use stdin if it is a tty, else `/dev/tty` if readable; if neither,
    fall back to non-interactive with a help message.
- Single-URL contract preserved; filename unchanged. Documentation and
  `install.test.sh` change the pipe target from `sh` to `bash`.

## 2. CLI / subcommand surface

Verbs (each also a menu item):

| Verb | Form |
|------|------|
| `install <client\|server>` | guided or flag-driven |
| `uninstall [client\|server]` | confirm; `--purge` typed-confirm |
| `upgrade [client\|server]` | latest vs installed, reuse recorded config |
| `status` | meta + live probe |
| `service <start\|stop\|restart>` | dispatch by deploy form |
| `config <get\|set> [key] [value]` | scoped keys only |
| `env` | view/edit the scoped env (advertised-endpoint et al.) |

**Back-compat:** bare `client` / `server` ≡ `install <role>`. All
existing flags retained: `--version --bin-dir --systemd --yes
--dry-run -h/--help`. New flags: `--deploy <binary|docker>`,
`--advertised-endpoint <host:port>`, `--lang <en|zh>`, `--data-dir
<dir>`, `--operator-http-listen <addr>`, `--compose-dir <dir>`.

Scoped config/env keys (the **only** keys `config`/`env` accept):
`advertised-endpoint`, `data-dir`, `operator-http-listen`,
`version-pin`. Unknown keys are a hard error.

## 3. Deploy-form detection & install metadata

- On install, write a metadata file `.install-meta` (shell `key=value`):
  - binary+systemd: client → `/etc/portunus/.install-meta`; server →
    `/var/lib/portunus/.install-meta`.
  - Docker: `<compose-dir>/.install-meta`.
  - Fields: `role`, `deploy` (`binary`|`docker`), `version`, `bin_dir`
    or `compose_dir`, `unit` (if systemd), `advertised_endpoint_set`
    (bool), `lang`, `installed_at`, `installer_version`.
- Subcommands read the meta to choose `systemctl` vs `docker compose`.
  If no meta is found, probe: a `portunus-<role>` on PATH and/or a
  `/etc/systemd/system/portunus-<role>.service` ⇒ binary; a
  `compose.y*ml` with a portunus service ⇒ docker. Ambiguous/none ⇒
  prompt (interactive) or error with guidance (non-interactive).

## 4. Install flows

Common: role → deploy form → version (latest|pin) → paths → confirm.

- **client:**
  - binary: existing download+checksum+install logic verbatim; optional
    systemd unit (existing path), enrollment reminder; bundle path
    guidance.
  - docker: write client compose, image pull, enrollment reminder.
- **server:**
  - Prompt **advertised endpoint** (`host:port`): explain it is the
    public gRPC reach address, that the server cert SAN must cover the
    host, link `/docs/features/advertised-endpoint`; blank ⇒ auto
    (tier-3/4 at runtime).
  - Persist per form (never edit the shipped unit / base compose):
    - binary+systemd: systemd drop-in
      `/etc/systemd/system/portunus-server.service.d/10-portunus.conf`
      with `Environment=PORTUNUS_ADVERTISED_ENDPOINT=…` and, if set,
      `--data-dir` / `--operator-http-listen` reflected in the unit
      `ExecStart` override; `systemctl daemon-reload`.
    - docker: write/patch a sibling `.env` (compose `env_file`) with
      `PORTUNUS_ADVERTISED_ENDPOINT=…` plus the other scoped vars.
- Reuse existing download → checksum (sha256) → `install -m 0755`
  pipeline for the binary path unchanged.
- Plan preview: extend `print_plan` to cover deploy form, advertised
  endpoint, persistence target. **`--dry-run` short-circuits before any
  network call** (preserve the current invariant) and performs no
  filesystem writes. `--yes` skips prompts using flags/defaults.
- **Idempotent re-install:** if meta exists, offer
  upgrade/repair/reconfigure instead of blind overwrite.

## 5. Lifecycle subcommands

- **status:** meta + live probe (`systemctl is-active` /
  `docker compose ps`), installed vs resolved-latest version, the
  persisted advertised endpoint (note: authoritative value is
  runtime-resolved; this shows the configured seed/override input),
  Web UI URL. Read-only; no network unless `--check-latest`.
- **upgrade:** resolve latest, compare to meta `version`; if newer,
  confirm, re-run the install path **reusing recorded config** (paths,
  advertised endpoint, deploy form); systemd ⇒ restart unit; docker ⇒
  `pull` + `up -d`. No-op + message when already current.
- **service start|stop|restart:** dispatch by meta deploy form
  (`systemctl …` vs `docker compose …`).
- **uninstall:** confirm (default `[y/N]`). Removes binary + unit +
  drop-in, or `docker compose down`. `--purge` additionally removes
  data-dir / `state.db` / compose volume and requires a **typed**
  confirmation token (mirrors the repo's destructive-action posture).
  `--yes` bypasses the ordinary confirm but **not** the purge typed
  confirm.
- **config get|set / env:** scoped keys only. `get` prints the current
  persisted value (from drop-in/.env). `set` rewrites it and offers a
  restart. Grammar for `advertised-endpoint` is validated by the server
  at runtime; the installer does a light `host:port` sanity check and
  defers authoritative SAN/grammar validation to the server (documents
  the `endpoint_invalid` / `endpoint_not_in_cert_san` 422s and links the
  troubleshooting page).

## 6. i18n

- Two associative arrays `MSG_EN` / `MSG_ZH`; `t <key> [printf-args…]`
  resolves against the active table with `printf` `%s` substitution.
- Language resolution order: `--lang` / `PORTUNUS_LANG` → `LC_ALL`/
  `LANG` autodetect (`zh*` ⇒ zh, else en) → if interactive and still
  undetermined, first-run prompt `[1] English [2] 中文`; non-interactive
  defaults to `en`.
- Chosen language is persisted into `.install-meta` (`lang=`) and reused
  by later subcommands.
- **Invariant:** every key in `MSG_EN` must exist in `MSG_ZH` (enforced
  by a test).

## 7. Safety / robustness

- bash 4 guard; `set -euo pipefail`; per-path `need` tool checks
  (`curl`,`tar` always; `sha256sum`/`shasum` either; `systemctl` only
  for systemd; `docker` only for docker form).
- Destructive actions always confirm; `--purge` needs a typed token;
  `--yes` never bypasses the purge token.
- Every verb honors `--dry-run`: print the plan, perform zero network
  and zero filesystem mutation.
- Re-install detects existing meta and routes to upgrade/repair rather
  than overwriting silently.
- Privilege: reuse `maybe_sudo` for file installs; the docker form
  relies on docker permissions, not sudo file writes.
- Never edits the shipped systemd unit or a user's base compose file;
  all server config goes through the drop-in / `.env` sidecar.

## 8. Testing & docs

- Expand `scripts/install.test.sh` (run under `bash`, not `sh`):
  - Keep all existing non-interactive / `--dry-run` assertions.
  - Add: arg/verb parsing matrix; deploy-form detection from fixtures;
    i18n key-coverage (`MSG_EN` ⊆ `MSG_ZH` and vice-versa); each verb
    `--dry-run` produces no side effects; `.install-meta` write/read
    round-trip; interactive path driven by feeding a script to a
    substitute fd standing in for `/dev/tty`.
  - `shellcheck` clean (bash dialect).
- Docs: update `docs/content/docs/getting-started/installation.mdx`
  (en + zh) — `| sh` → `| bash`, document the menu, the subcommands,
  and the server advertised-endpoint prompt. Cross-link from
  `deployment/docker.mdx` and `deployment/railway.mdx`.

## Open risks

- Large single bash file: mitigated by strict section banners,
  function-per-concern, and the expanded test + shellcheck gate.
- `curl | bash` + `/dev/tty`: not available in some CI/container
  contexts; the non-interactive fallback path covers those and is the
  contract automation should use.
- Docker form variability (`docker compose` v2 vs legacy
  `docker-compose`): detect and prefer v2; error with guidance if only
  legacy is present.
