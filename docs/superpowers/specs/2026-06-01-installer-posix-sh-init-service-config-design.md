# Installer rework: POSIX sh, init-system abstraction, default-start, `--config`

- **Date:** 2026-06-01
- **Status:** Approved (design); implemented with one deviation (see below)
- **Scope target:** `scripts/install.sh` and its supporting service/template
  files; docs that reference the installer.

## 0. Implementation deviation — `--config` is standalone-only

This design (§5.4, D3) describes `--config PATH` as a per-role override with
per-role defaults. During implementation we confirmed the real binary CLIs:
`portunus-client` takes `--bundle PATH` and `portunus-server` takes
`--data-dir DIR serve …` — **neither accepts a `--config` flag**. Generalising
`--config` across roles would have meant inventing a flag the binaries cannot
honor. The flag is therefore **scoped to the `standalone` role only**:

- `--config PATH` with `standalone` → seeds/points the service at `PATH`
  (default `/etc/portunus/standalone.toml`).
- `--config PATH` with `client`/`server` → hard error
  (`--config is only valid for the standalone role`).
- The client/server service definitions instead carry their native knobs
  (`bundle=` / `datadir=`+`server_args=` for OpenRC conf.d; the systemd
  drop-in for server `--data-dir`/`serve` args is unchanged from prior art).

References to "per-role config path" below should be read with this scope.

### Follow-up: standalone is no longer seeded (post-merge revision)

The original design (§5.4, D3) had the installer **seed** the standalone
config from `contrib/portunus.example.toml` when absent. This was reversed
after merge per operator feedback:

- The installer **never writes a config**. `apply_config_path` only prepares
  the directory and fixes ownership/permissions on a file the operator
  already created.
- The docs guide the user to **create `portunus.toml` first** (heredoc), then
  install.
- The `portunus-standalone` binary already **exits with code 2** when the
  config is missing (`main.rs`), so a bogus/example-rule run is impossible.
- Auto-start is now **config-gated for standalone**: `service_should_start`
  returns false when the standalone config does not yet exist, so a bare
  install lays down the unit, skips the start, and prints how to create the
  config and start (`next_standalone_create`). server/client are unaffected
  and always start unless `--no-service` / no init manager. This avoids a
  failed unit on first install while preserving "install starts the service"
  for the documented create-first flow.

## 1. Problem

`scripts/install.sh` is a 1224-line **bash 4+** lifecycle manager. Three
problems motivate this rework:

1. **bash-only.** It uses `declare -A` associative arrays (i18n), `local`,
   `[[ ]]`, `(( ))`, shell arrays, and `${BASH_SOURCE[0]}` / `BASH_VERSINFO`.
   Alpine ships no bash by default (`/bin/sh` is busybox ash), so
   `curl … | sh` fails and users must `apk add bash` first.
2. **Counter-intuitive default.** `curl … | bash -s -- standalone` installs
   the binary and a seed config but **does not** install or start a service.
   The user is left running a foreground process that dies on SSH
   disconnect (SIGHUP).
3. **Fixed config path.** The standalone systemd unit hardcodes
   `--config /etc/portunus/standalone.toml`; there is no way to point the
   managed service at a different config location.

## 2. Goals

- Run under POSIX sh (`dash`, busybox `ash`) as well as bash.
- After install, **start the service by default** on Linux with a supported
  init system; one opt-out flag.
- Support **systemd and OpenRC**; degrade gracefully elsewhere.
- New `--config PATH` selects the config file the managed service reads.
- Apply uniformly to all three roles: `standalone`, `client`, `server`.
- Preserve the existing hidden test seams and the `install.test.sh` contract.

## 3. Non-goals

- Native packages (`.deb` / `.rpm` / `.apk`) — deferred.
- Native `runit` / `s6` support — folded into the `none` degraded path.
- Changing the default config directory away from `/etc/portunus`
  (`/etc` is FHS-correct; `--config` already lets users override location).

## 4. Decisions (from brainstorming)

| # | Decision |
|---|----------|
| D1 | Non-systemd scope: **systemd + OpenRC**; runit/s6/none degrade to binary + config + printed manual-run instructions. |
| D2 | Default behavior: install service + enable + **start now**. Opt-out is a single flag **`--no-service`** (binary + config only — today's behavior). `--systemd` kept as a **compatibility no-op**. |
| D3 | `--config PATH`: PATH becomes the path the **service** reads. Installer seeds the example there if absent, injects the path into the service definition, and fixes ownership/permissions so the service user can read it; warns if PATH is unreachable by that user. Default stays `/etc/portunus/standalone.toml` (and the per-role defaults). |
| D4 | POSIX rewrite is whole-file. i18n reimplemented as a **case-lookup function** (no associative arrays). |
| D5 | Roles: all three (`standalone`/`client`/`server`) get the systemd + OpenRC abstraction; lifecycle verbs (`upgrade`/`uninstall`/`status`/`service`) become init-aware. |
| D6 | Init abstraction structure: **per-init driver function groups** (`systemd_*`, `openrc_*`, `none_*`) behind a thin `svc()` dispatcher. Unit/init content lives as version-controlled templates. |

## 5. Design

### 5.1 POSIX sh conversion

- Shebang `#!/bin/sh`; invocation becomes `curl … | sh …`. Remove the
  bash-version guard.
- `set -eu` only. **No `pipefail`** (absent in dash/ash). Audit every
  pipeline whose failure matters and rewrite to check explicitly
  (e.g. `curl … -o "$f"` then `[ -s "$f" ]`, or `if ! cmd; then …`).
- **i18n → `t()` case lookup.** Replace `declare -A MSG_EN MSG_ZH` with a
  single function:
  ```sh
  t() {  # t <key> [printf-args...]
    key="$1"; shift
    case "$LANG_CODE:$key" in
      zh:done_next) fmt='下一步：' ;;
      *:done_next)  fmt='Next steps:' ;;
      …
    esac
    # shellcheck disable=SC2059
    printf "$fmt" "$@"
  }
  ```
  `--print-i18n-keys` returns a static, hand-maintained key list (it no
  longer iterates array keys).
- **Remove shell arrays.** `CLEANUP_DIRS=()` → a single `CLEANUP_DIR`
  variable (the script only ever needs one temp dir at a time) or a
  space-separated string iterated with `for`; keep `trap _cleanup EXIT`.
- **Self-path.** Drop `${BASH_SOURCE[0]}` / `BASH_VERSINFO`. Use `$0`:
  when piped (`curl|sh`) `$0` is the shell name (no local file → use the
  network fetch path for templates); when run as a file `$0` is the path
  (use local templates if readable). Keep the existing
  "local template else curl from RAW_BASE" fallback, keyed on this.
- Replace `[[ … ]]` → `[ … ]` / `case`; `(( … ))` → `$(( … ))` or `[ … ]`.
- `local`: dash, busybox ash, and ksh all support it. Keep using it; add a
  one-line header comment noting it is the single non-strict-POSIX builtin
  relied upon and that all target shells provide it.

### 5.2 Init-system abstraction (per-init driver groups)

- `detect_init()` sets `INIT`:
  - `systemd` — `systemctl` present and PID 1 is systemd
    (`[ -d /run/systemd/system ]`).
  - `openrc` — `rc-service`/`openrc` present (`command -v rc-service`).
  - `none` — neither.
- Cohesive driver groups, one per init, each implementing the same verbs:
  - `systemd_install / systemd_enable_start / systemd_stop / systemd_disable / systemd_remove / systemd_status`
  - `openrc_install / openrc_enable_start / …`
  - `none_install / none_enable_start / …` (install = write nothing or only
    print; enable_start = print manual-run instructions).
- Thin dispatcher: `svc() { op="$1"; shift; "${INIT}_${op}" "$@"; }`.
- New hidden test seams: `--detect-init` (prints `INIT`) and
  `--render-openrc <role>` (prints the rendered init.d script to stdout,
  mirroring `--render-dropin`).

### 5.3 Default-start + opt-out

- Install flow (binary deploy): `install_binary` →
  `svc install "$ROLE" "$CONFIG_PATH"` → unless `--no-service`,
  `svc enable_start "$ROLE"`.
- `--no-service`: install binary + seed config + write the service
  definition, but do **not** enable/start.
- `--systemd`: accepted, sets nothing (now the default); no error, for
  backward compatibility with existing docs/automation.
- `INIT=none`: behave as if service install is impossible — install binary +
  config only and print nohup/manual-run guidance, regardless of
  `--no-service`.
- The installer still **never auto-starts** under Docker deploy (compose is
  written but not `up`-ed) — unchanged.

### 5.4 `--config PATH` semantics

Let `CONFIG_PATH` default to the per-role path
(`/etc/portunus/standalone.toml`, etc.) and be overridden by `--config`.

1. **Seed if absent.** If `CONFIG_PATH` does not exist, write the example
   config there (reuse the existing local-or-`curl` example fetch). Create
   parent dir if needed.
2. **Inject the path into the service definition** without forking the
   pristine base unit/init file:
   - **systemd:** write a drop-in
     `/etc/systemd/system/portunus-<role>.service.d/10-config.conf` with
     `ExecStart=` cleared then re-set to use `--config CONFIG_PATH`
     (mirrors the existing server drop-in idiom). Written **only when
     `CONFIG_PATH` differs from the unit's baked-in default**; when they
     match, the pristine base unit is sufficient and no drop-in is written.
   - **OpenRC:** the init.d script **always** sources
     `/etc/conf.d/portunus-<role>`, so the installer **always** writes that
     conf.d file with `command_args="--config CONFIG_PATH"` (using the
     per-role default path when `--config` is omitted).
3. **Permissions.** `chown root:<svc-user>` + `chmod 0640` on the config
   file so the service user can read it (svc users: `portunus`,
   `portunus-client`, `portunus-server`).
4. **Reachability warning.** If `CONFIG_PATH` lives somewhere the service
   user cannot traverse (e.g. a `0700` home dir), print a warning but
   continue — the operator may fix perms or intends `--no-service`.

### 5.5 OpenRC artifacts (new templates)

- `crates/portunus-standalone/contrib/portunus-standalone.openrc`
  (init.d). Uses `supervise-daemon` (OpenRC 0.30+, satisfied by current
  Alpine): `command=/usr/local/bin/portunus-standalone`,
  `command_args` (from conf.d), `command_user=portunus:portunus`,
  `supervisor=supervise-daemon`, `pidfile`, `command_background` as needed.
- `deploy/openrc/portunus-client.openrc`,
  `deploy/openrc/portunus-server.openrc` (parallel to `deploy/systemd/`).
- Matching default `conf.d` snippets for each role.
- Reuse existing system-user creation logic (`portunus*`).

### 5.6 Init-aware lifecycle verbs

Route all lifecycle operations through `svc`:

- `uninstall`: `svc stop` + `svc disable` + remove service files
  (systemd unit/drop-in, or init.d + conf.d) + remove binary;
  `--purge` additionally removes `/etc/portunus` and data dirs.
- `status`: `svc status` → `systemctl status …` / `rc-service … status`.
- `service start|stop|restart`: mapped per init.
- `upgrade`: download+verify new binary, then `svc restart`.
- The `.install-meta` file records `init=systemd|openrc` so later verbs
  route correctly even if detection changes.

### 5.7 Test contract

- **Preserve every existing seam** with identical output:
  `--meta-write`, `--meta-read`, `--detect-deploy`, `--render-dropin`,
  `--valid-endpoint`, `--detect-ip`, `--valid-fqdn`, `--render-caddy`,
  `--effective-advertised`, `--resolve-meta`, `--menu-stdin`,
  `--print-i18n`, `--print-i18n-keys`.
- `scripts/install.test.sh` runs under multiple interpreters: `sh`, `dash`,
  and `busybox sh` when available, to prove POSIX-ness. Add assertions for
  `--detect-init` and `--render-openrc`.
- CI: add a lightweight `dash scripts/install.test.sh` step.

### 5.8 Docs sync

- `README.md` / `README.zh-CN.md`: change `| bash -s --` → `| sh -s --`;
  drop "needs `bash` 4+" and the macOS `brew install bash` note; mention
  `--config`, `--no-service`, and one line on Alpine/OpenRC support.
- `docs/` installation and standalone pages updated to match.

## 6. Risks

- `set -eu` without `pipefail`: pipeline failures must be re-checked
  individually — easy to miss. Mitigate with a careful audit pass and the
  multi-interpreter test run.
- busybox `ash` vs `dash` have subtle differences (e.g. `local`, `printf`
  edge cases); must be tested on both, not just one.
- `deploy/systemd/install.sh` is a separate file referenced by the repo;
  confirm during implementation whether it is in scope or unrelated.
- Rendering `ExecStart`/`command_args` with a user-supplied path must be
  safe against paths containing spaces; quote consistently and add a test.

## 7. Acceptance criteria

1. `sh scripts/install.sh standalone` (and via `curl | sh`) installs the
   binary, seeds the config, installs and **starts** the service on a
   systemd host and on an OpenRC host.
2. `--no-service` installs without enabling/starting.
3. `--config /custom/path.toml` results in the running service reading that
   file (verified via the rendered drop-in / conf.d and a smoke run).
4. `scripts/install.test.sh` passes under `dash` and `busybox sh`.
5. All pre-existing hidden seams produce identical output to before.
6. On a `none`-init host, the script installs binary + config and prints
   manual-run guidance without error.
