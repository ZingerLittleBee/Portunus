# install.sh interactive UX redesign

Date: 2026-06-16
Status: approved (decisions delegated to the implementer via `/goal`)
Scope: `scripts/install.sh` (+ `scripts/install.test.sh`). No change to the
Rust workspace, the wire protocol, or the persisted install metadata format.

## Problem

`scripts/install.sh` is a 1549-line POSIX-sh lifecycle manager. It works, but
its **interactive** experience is unfriendly:

1. The first decision is jargon: the user must pick `client` / `server` /
   `standalone` with no guidance on what they mean.
2. One menu mixes first-install with six management verbs; a first-timer sees
   uninstall/upgrade/config before they have anything installed.
3. `-h/--help` is a single unreadable mega-line; the ~20 CI/test seams
   (`--meta-write`, `--render-caddy`, `--detect-init`, …) are mixed in with the
   handful of flags a user actually needs.
4. Heavy, system-touching steps (install Caddy, write `/etc/caddy/Caddyfile`,
   probe the public IP, DNS pre-check, create system users) happen with little
   up-front explanation.

## Goal

Rework the interactive layer to be **task-oriented** while keeping the script a
single self-contained POSIX-sh file, and keep **unattended / non-interactive**
operation a first-class path (flags drive a fully silent install).

## Locked decisions

| # | Decision | Resolution |
|---|----------|------------|
| 1 | Direction | Keep one script; rework interaction to be task-oriented; hide CI seams; layered help. |
| 2 | Unattended | Hard requirement. Enough flags / `--yes` / no TTY ⇒ fully silent auto-execute, zero prompts. |
| 3 | Role | Intent mapping: ask the *scenario*, infer `standalone`/`server`/`client`. User need not know the words. |
| 4 | Entry | Smart routing: installed ⇒ manage menu; not installed ⇒ install wizard. A unified main menu is always reachable. |
| 5 | Heavy ops | Smart recommendation + step-by-step confirmation, each with a one-line "what this will do". |

## Architecture: one resolve→execute pipeline, two front-ends

The CPU of the script is unchanged. We keep the existing **executor** and
**lifecycle** functions (`install_binary`, `install_docker`, `svc *`,
`setup_caddy_domain`, `lifecycle_*`, `dispatch_verb`, the init-system
abstraction, i18n core, meta store) exactly as-is — they are battle-tested and
pinned by the dry-run/seam tests.

We rework only two layers:

- **Input front-ends** — flags (`parse_args`) and the interactive wizard both
  populate the *same* global "intent" variables (`ROLE`, `DEPLOY`, `VERSION`,
  `DATA_DIR`, `OP_HTTP_LISTEN`, `ADVERTISED`, `DOMAIN`, `ACME_EMAIL`,
  `CONFIG_PATH`, `ENROLL_URI`, `NO_SERVICE`). The wizard is a *gap-filler*: it
  only asks for what flags did not provide. `main()` already routes to the
  wizard only when there is no actionable verb/role and a TTY is present, so
  unattended is the default whenever inputs are complete or `--yes` / no-TTY.
- **Presentation** — the menus, the role question, the install summary, and the
  help text.

Grafted-on idea (best part of the "wizard builds a command" approach): the
install summary prints the **equivalent non-interactive command**, so the
wizard teaches the flags and yields a copy-pasteable CI invocation, and the two
paths are provably the same executor.

## New interactive contract

### Entry (no verb, no role, interactive TTY): `run_menu`

Smart routing chooses the first screen by install state
(`current_meta_file` probe):

- **Not installed → install screen** = the intent question itself:

  ```
  What do you want to do with Portunus?
    [1] Forward ports/traffic on THIS machine (no control plane)      → standalone
    [2] Run a control panel to manage many forwarding nodes (Web UI)  → server
    [3] Connect THIS machine to an existing control panel             → client
    [m] More options (manage / upgrade / uninstall)
    [0] Exit
  ```

- **Installed → manage screen**:

  ```
  Portunus — installed: <role> <version>
    [1] Status   [2] Service (start/stop/restart)   [3] Upgrade
    [4] Config   [5] Uninstall
    [6] Install another instance / role
    [m] Main menu        [0] Exit
  ```

- **`[m]` → main menu** = the existing unified 7-verb menu (Install / Uninstall
  / Upgrade / Status / Service / Config / Env / Exit). This is the always-there
  escape hatch.

Navigation is held in a parent-scope `MENU_SCREEN` variable; each action still
runs in a `( … )` subshell so a `die()` inside one action never kills the
session (preserves the existing P1#2 invariant).

### Wizard (per scenario)

After the intent maps to a role, the wizard gap-fills:

- **server**: deploy (recommended default), then the HTTPS step (see below),
  else advertised-endpoint with detected-IP default; data-dir / op-http keep
  their defaults silently.
- **client**: deploy, then optionally an **enroll URI** (blank = configure
  later). New key `ask_enroll`.
- **standalone**: deploy; reminds that a config file must be authored (the
  binary exits without it) — no auto-seeding.

### Heavy-op confirmation (server HTTPS)

The domain step is reframed to state its side effects before asking, default No:

```
Set up an HTTPS domain for the Web UI?
  This installs Caddy, writes /etc/caddy/Caddyfile, and DNS-prechecks this
  host's public IP. [y/N]
```

Answering yes then asks for the FQDN (and optional ACME email). Blank/No skips
Caddy entirely (unchanged downstream behavior).

### Install summary + equivalent command + single confirm

`print_install_summary` is followed by:

```
Equivalent non-interactive command (copy into CI / docs):
  install.sh server --domain panel.example.com --acme-email ops@example.com --yes
```

built by a new pure function `build_equiv_cmd()` that emits only non-default
flags and redacts the enroll secret (`--enroll '<your-enroll-uri>'`). Then the
existing single `confirm` gate runs.

## Layered help

- `-h` / `--help`: short, grouped, readable usage — common install commands,
  management verbs, and the handful of user-facing options. Exits 0.
- `--help-all`: the complete flag surface plus the CI/test seams.

All seams stay functional and reachable; they are merely removed from the
default help. Back-compat no-ops (`--systemd`) are retained.

## i18n additions (zh + en, parity enforced by the test)

New keys: `intent_title`, `intent_standalone`, `intent_server`, `intent_client`,
`intent_more`, `ask_enroll`, `ask_setup_https`, `equiv_cmd`, `manage_title`,
`manage_status`, `manage_service`, `manage_upgrade`, `manage_config`,
`manage_uninstall`, `manage_install_another`, `nav_main`. Each is added to the
`I18N_KEYS` manifest and to both the `zh:` and `*:` arms of `t()`. The
`%`→`%s`-only convention is preserved.

## Test strategy

`scripts/install.test.sh` is the safety net (network-free, runs the script
under `$SH`).

- **Unchanged / must stay green**: every dry-run, seam, validation, i18n-parity,
  enroll-redaction, Caddy-render, and drop-in test. These pin the executor
  contract this refactor does not touch.
- **Rewritten**: the interactive menu/wizard tests (current lines ~90–164) are
  updated to the new contract — intent question text, smart routing (clean env
  ⇒ install screen; seeded meta ⇒ manage screen), `[m]` reaching the main menu,
  the equivalent-command line in the summary, and `--help` / `--help-all`.
- **Added**: assertions for `build_equiv_cmd` output, intent→role mapping, and
  EN/ZH parity of the new keys.

Verification gates: full suite under `bash` and a POSIX shell (`dash`/`sh`),
`shellcheck -s sh -S warning`, and a dry-run smoke for each role.

## Out of scope

No change to: the wire protocol, persisted `.install-meta` format/keys, the
download/verify path, the init-system drivers, Docker compose generation, or the
Caddy block format. Metric labels, RBAC, and the Web UI are untouched.
