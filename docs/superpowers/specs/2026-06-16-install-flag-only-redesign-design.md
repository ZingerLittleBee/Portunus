# install.sh flag-only redesign

**Date:** 2026-06-16
**Status:** Design — approved, pending implementation plan
**Scope:** `scripts/install.sh`, `scripts/install.test.sh`,
`docs/content/docs/cli/installer.mdx` (+ `zh` mirror)

## Problem

`scripts/install.sh` carries two layers that exist only to support
*interactive* use: a TTY-driven menu/wizard system and a self-contained
zh/en i18n table. Together they account for ~600 lines, a parity
manifest (`I18N_KEYS`), several test seams (`--menu-stdin`,
`--print-i18n*`), and a language cache on disk. The menu's "smart
routing" and the wizard's gap-filling prompts duplicate, in prose, the
exact behavior the flags already express, and every user-facing string
must be maintained twice (en + zh).

The installer's actual job — install/manage a role, drive a service
lifecycle, render config — is done by a stable executor layer that does
not need any of this. We want the installer to be a **pure flag-driven
CLI**: every action selected by arguments, every prompt replaced by a
flag or a deterministic default, all output in English, and the full
parameter surface documented with examples.

## Decisions (locked)

1. **No interactivity.** Remove the menu, the wizard, and every
   confirmation prompt. There are no TTY reads anywhere in the script.
2. **Flags are consent.** Invoking a verb *is* the confirmation.
   `uninstall` runs immediately; deleting data still requires the
   explicit `--purge` flag (but no typed "purge" challenge). `upgrade`
   runs immediately. `config set` applies the file change but does **not**
   restart by default — `--restart` opts into the restart.
3. **English only, clean break on removed flags.** Drop the i18n table
   entirely. Removed flags (`--lang`, `--reset-lang`, `--menu-stdin`,
   `--yes`, `--print-i18n`, `--print-i18n-keys`, `--systemd`) are *not*
   silently accepted — passing one is an `unknown argument` error.
4. **Docs i18n is independent.** The script is English-only, but the
   Chinese docs mirror (`docs/content/docs/zh/cli/installer.mdx`) is kept
   and rewritten in sync with the English page.

## What stays unchanged

The entire executor / lifecycle / rendering layer is kept verbatim:

- Install paths: `install_binary`, `install_docker`,
  `install_systemd_unit`, `write_compose_file`/`write_compose_env`,
  `place_client_bundle`, `apply_config_path`.
- Init abstraction: `detect_init`, the `svc` dispatcher, and the
  `systemd_*` / `openrc_*` / `none_*` operation families.
- Lifecycle verbs: `lifecycle_status`, `lifecycle_service`,
  `lifecycle_upgrade`, `lifecycle_uninstall`, `lifecycle_config`,
  `lifecycle_env`, `lifecycle_domain`, and `dispatch_verb`.
- HTTPS/Caddy: `setup_caddy_domain`, `ensure_caddy`, `write_caddy_block`,
  `render_caddy_block`, `caddy_reload`, `verify_https`.
- Config subsystem (hardened in v2.x): `valid_config_value`,
  `config_sudo`, `flag_value_from`, `config_key_flag`,
  `hydrate_binary_config`, `write_server_dropin`, `write_server_confd`,
  `render_dropin`/`render_confd`, `server_extra_args`,
  `valid_host_port`, `SCOPED_KEYS`.
- Meta store, download/verify path, platform/version detection, and all
  CI/test render seams (`--render-dropin`, `--render-caddy`,
  `--render-openrc`, `--render-confd`, `--render-config-dropin`,
  `--detect-init`, `--detect-ip`, `--detect-deploy`, `--meta-write`,
  `--meta-read`, `--resolve-meta`, `--effective-advertised`,
  `--valid-endpoint`, `--valid-fqdn`, `--valid-email`).

Constraint preserved: POSIX `sh` (dash/busybox-ash), `local` the only
non-POSIX builtin, `shellcheck -s sh -S warning` clean.

## Architecture after the change

A single, linear pipeline with no branch back into a UI:

```
main()
  parse_args            # flags -> intent globals; unknown flag => die
  [ -z VERB && -z ROLE ] => print_usage >&2; exit 2   # no menu fallback
  VERB defaulted to "install"
  platform/version detect, cross-flag guards (server-only, client-only)
  apply_advertised_default / apply_install_defaults
  --effective-advertised / --dry-run short-circuits (unchanged)
  dispatch_verb         # install | status | service | upgrade
                        # | config | env | uninstall | domain
```

`dispatch_verb` and every executor it calls are untouched. The redesign
only deletes the input layer (menu/wizard/prompts) and the output layer
(i18n), and rewires the handful of prompt call sites to flags/defaults.

### Input layer — removed

Functions deleted entirely: `is_interactive`, `ask`, `first_run_lang`,
`build_equiv_cmd`, `run_install_flow`, `reset_menu_state`, `pause`,
`read_menu`, `menu_service`, `menu_config`, `menu_screen_install`,
`menu_screen_manage`, `menu_screen_main`, `run_menu`, `read_tty`,
`confirm`. Globals deleted: `MENU_FORCE_STDIN`, `MENU_SCREEN`,
`REPLY_MENU`, `REPLY_TTY`, `ASSUME_YES`. If `print_install_summary` is
only referenced by the wizard, it is deleted too.

`main()` no-command behavior changes from "launch wizard on a TTY" to:
print usage to stderr and exit `2`.

### Output layer (i18n) — removed

Deleted: `t()` (the bilingual `case` table), `resolve_lang`,
`I18N_KEYS`, `LANG_CACHE`, and `LANG_CODE`. Every `t <key> [args…]` and
`$(t <key> …)` call site is replaced with the inline English string —
the text already present in the `en` arm of that key — emitted via
`printf`/`echo`. The `lang=` field is dropped from every `meta_write`
call (readers already ignore unknown keys, so old meta files stay
compatible).

### Prompt call sites — rewired

| Site | Before | After |
|------|--------|-------|
| `lifecycle_upgrade` | `confirm confirm_proceed` | run unconditionally |
| `lifecycle_uninstall` | `confirm confirm_uninstall` | run unconditionally |
| `lifecycle_uninstall` (purge) | typed-"purge" `read_tty` challenge | `--purge` flag only |
| `lifecycle_config` docker | `confirm restart_now` | `[ "$RESTART" = yes ]` |
| `lifecycle_config` binary | `confirm restart_now` | `[ "$RESTART" = yes ]` |
| `dns_points_here` | retry loop + `read_tty` | check once; mismatch ⇒ `die` (hint: `--skip-dns-check`) |

### New flag

- `--restart` — for `config set`, restart the service after writing the
  new value (default: do not restart; print a hint that a restart is
  needed to apply). Sets a `RESTART=yes` global; ignored by other verbs.

### Help / usage

- `print_usage` (`-h` / `--help`): one comprehensive reference — roles,
  manage verbs, every user-facing flag grouped by purpose, plus an
  **Examples** block. The "interactive wizard (recommended)" line and the
  layered "More: --help-all" pointer for *user* flags are removed.
- `--help-all`: `print_usage` plus the Automation / CI seams block
  (those seams remain, for tests and scripting). The `--print-i18n*`,
  `--menu-stdin`, and `--systemd` lines are removed from it.

## CLI surface (final)

```
install.sh <role> [options]      standalone | server | client
install.sh <verb> [options]      status | service start|stop|restart
                                 | upgrade | config get|set <key> [value]
                                 | env | uninstall [--purge]
                                 | domain <fqdn>
```

Options retained: `--version`, `--deploy binary|docker`, `--bin-dir`,
`--compose-dir`, `--enroll`, `--domain`, `--acme-email`,
`--skip-dns-check`, `--data-dir`, `--advertised-endpoint`,
`--operator-http-listen`, `--config`, `--no-service`, `--purge`,
`--dry-run`, plus the new `--restart`. Plus all CI/test seam flags.

Options removed (now `unknown argument`): `--yes`, `--lang`,
`--reset-lang`, `--menu-stdin`, `--print-i18n`, `--print-i18n-keys`,
`--systemd`.

## Error handling

- Unknown flag / unknown argument ⇒ `die` (exit 1), unchanged.
- No verb and no role ⇒ usage to stderr, exit 2.
- `config` on a non-server install ⇒ message to stderr, exit 2 (existing
  guard, now without i18n).
- Invalid `config set` value ⇒ message to stderr, exit 2 (existing
  `valid_config_value` guard).
- `domain` with DNS not pointing at this host and no `--skip-dns-check`
  ⇒ `die` (exit 1) — previously a prompt on a TTY.

## Testing

`scripts/install.test.sh` (bash harness, `TEST_SH=dash` for the POSIX
pass) is updated:

- **Remove:** all menu/wizard tests, `--menu-stdin`-driven flows, i18n
  parity / render-difference tests, `--print-i18n*` tests, and any
  `--yes`/confirm-path tests.
- **Keep:** dry-run role guards, every render seam, value-injection
  rejection, the systemd/openrc/docker config round-trips (via
  `PORTUNUS_TEST_CONFIG_ROOT`), client-reject, docker both-compose-files.
- **Add:**
  - no-argument invocation prints usage and exits non-zero (2).
  - each removed flag (`--yes`, `--lang`, `--menu-stdin`,
    `--print-i18n`, `--systemd`) now exits non-zero as unknown.
  - `config set … --restart` triggers the restart path; without it the
    value is written and no restart occurs.
  - `uninstall --purge` removes the data dir with no prompt;
    `uninstall` alone leaves data intact.

Gates that must stay green: `shellcheck -s sh -S warning
scripts/install.sh`, `bash scripts/install.test.sh`, `TEST_SH=dash bash
scripts/install.test.sh`.

## Documentation

Rewrite `docs/content/docs/cli/installer.mdx` and its
`docs/content/docs/zh/cli/installer.mdx` mirror into a complete,
flag-first reference:

- Remove the "two modes / interactive wizard / smart routing / equivalent
  command / language" sections — they describe behavior that no longer
  exists.
- One table per verb/role documenting **every** flag (name, applies-to,
  default, meaning).
- An **Examples** section covering: server binary install; server docker
  install; server + HTTPS (`--domain` / `--acme-email`); client
  `--enroll`; standalone with `--config`; `status`; `service restart`;
  `upgrade`; `config get`/`set` (+ `--restart`); `uninstall` and
  `uninstall --purge`.

## Breaking changes (call out in CHANGELOG)

1. Running `install.sh` with no arguments no longer launches a wizard; it
   prints usage and exits non-zero. First-time users must pass a role.
2. `--yes`, `--lang`, `--reset-lang`, `--menu-stdin`, `--print-i18n`,
   `--print-i18n-keys`, and `--systemd` are removed and now error.
   **Most impactful:** existing CI that passes `... --yes` breaks and
   must drop the flag (it is now a no-op concept — actions auto-proceed).
3. All installer output is English; the cached language preference and
   `--lang`/`--reset-lang` are gone.
4. `uninstall` and `config set` no longer prompt. `uninstall` proceeds
   immediately; data deletion is gated solely by `--purge` (no typed
   confirmation); `config set` restarts only with `--restart`.

## Non-goals

- No change to what gets installed, the service model, the meta format
  (beyond dropping the now-unused `lang=` field), or the config wire.
- No new deployment features. This is a UX/surface refactor only.
