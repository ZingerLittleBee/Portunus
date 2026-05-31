# Installer POSIX-sh + init-abstraction + default-start + `--config` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rewrite `scripts/install.sh` to run under POSIX sh (dash / busybox ash), install-and-start the service by default on systemd **and** OpenRC hosts (single `--no-service` opt-out), and add `--config PATH` that the managed service reads.

**Architecture:** One cohesive POSIX-sh script. Init systems are handled by per-init driver function groups (`systemd_*`, `openrc_*`, `none_*`) behind a thin `svc()` dispatcher. Service unit/init content lives as version-controlled templates in `contrib/` and `deploy/`. A custom config path is injected via a systemd drop-in or an OpenRC `conf.d` file — base units stay pristine.

**Tech Stack:** POSIX sh, systemd, OpenRC (`supervise-daemon`), shellcheck, dash/busybox for test execution.

**Source of truth:** `docs/superpowers/specs/2026-06-01-installer-posix-sh-init-service-config-design.md`.

**Implementation deviation:** `--config PATH` ended up **standalone-only**. The
real binaries use `--bundle` (client) and `--data-dir … serve` (server), so a
generalised `--config` was impossible; passing `--config` with a non-standalone
role is now a hard error. See §0 of the spec for the full rationale. Wherever a
task below says "per-role config", read it as "standalone config; client/server
carry their native `bundle=` / `datadir=`+`server_args=` knobs instead".

**Post-merge revision:** the installer no longer **seeds** the standalone
config — tasks below that copy/curl `portunus.example.toml` into place are
superseded. The operator creates `portunus.toml` first (the binary exits if it
is missing); standalone auto-start is config-gated via `service_should_start`.
See the spec §0 follow-up.

---

## File Structure

- `scripts/install.sh` — the script (rewritten in place, POSIX sh).
- `scripts/install.test.sh` — extend; run under multiple interpreters.
- `crates/portunus-standalone/contrib/portunus-standalone.openrc` — **new** OpenRC init.d for standalone.
- `crates/portunus-standalone/contrib/portunus-standalone.confd` — **new** default `/etc/conf.d` snippet for standalone.
- `deploy/openrc/portunus-client.openrc`, `deploy/openrc/portunus-server.openrc` — **new** init.d for client/server.
- `deploy/openrc/portunus-client.confd`, `deploy/openrc/portunus-server.confd` — **new** conf.d snippets.
- `README.md`, `README.zh-CN.md` — docs sync.
- `.github/workflows/ci.yml` — add a `dash` test step.

> The conversion tasks edit `scripts/install.sh` in place. Because it is one
> interdependent file, the TDD driver is **(a)** `dash -n` / `sh -n` parse
> checks (catch bashisms that break parsing) and **(b)** the `install.test.sh`
> seam suite run under dash/busybox (catch behavioral regressions). Each task
> ends green on both before commit.

---

## Task 0: Multi-interpreter test harness (the failing test)

**Files:**
- Modify: `scripts/install.test.sh`

- [ ] **Step 1: Add a parse-check + interpreter matrix to the test harness**

At the top of `scripts/install.test.sh`, after `script="$here/install.sh"`, add a helper that records which interpreter is under test, and a parse gate:

```sh
# Interpreter under test: default sh, overridable via $TEST_SH.
SH="${TEST_SH:-sh}"

# Parse gate: the script must parse cleanly under the target interpreter.
"$SH" -n "$script" || fail "parse error under $SH"
```

Then replace every `bash "$script"` invocation in the file with `"$SH" "$script"`.

- [ ] **Step 2: Run under dash to verify it FAILS**

Run: `TEST_SH=dash sh scripts/install.test.sh`
Expected: FAIL at the parse gate — dash chokes on `declare -A` / arrays / `[[`.
(If `dash` is absent: `brew install dash` on macOS, or use `busybox sh`.)

- [ ] **Step 3: Commit the harness**

```bash
git add scripts/install.test.sh
git commit -m "test(installer): run install.test.sh under \$TEST_SH with a parse gate"
```

---

## Task 1: POSIX shell mechanics (shebang, set flags, arrays, self-path, `[[`/`((`)

**Files:**
- Modify: `scripts/install.sh` (header + scattered constructs)

- [ ] **Step 1: Header — shebang, drop bash guard, POSIX set flags**

Replace lines 1–16 (the shebang through the bash-version guard) with:

```sh
#!/bin/sh
# Portunus lifecycle manager: install/uninstall/upgrade/status/service/
# config/env for client/server/standalone, binary+systemd|openrc or Docker.
#
#   curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- standalone
#   curl -fsSL .../scripts/install.sh | sh        # interactive menu
#
# POSIX sh. The only non-POSIX builtin relied upon is `local`, which dash,
# busybox ash, and ksh all provide.
set -eu
```

(Removes `set -o pipefail` and the `BASH_VERSINFO` guard.)

- [ ] **Step 2: Self-path detection without `BASH_SOURCE`**

Replace the `case "${BASH_SOURCE[0]:-}" in … esac` block (lines ~17–20) with:

```sh
# When piped (curl|sh) $0 is the shell ("sh"/"-sh"/"dash"); when run as a
# file it is the path. Only a readable file path yields local templates.
SELF_SCRIPT=""
case "${0:-}" in
  ""|sh|-sh|dash|-dash|bash|-bash|ash|-ash) ;;
  *) [ -r "$0" ] && SELF_SCRIPT="$(cd "$(dirname "$0")" 2>/dev/null && pwd)/$(basename "$0")" || true ;;
esac
```

- [ ] **Step 3: Replace the cleanup array with a single dir + string list**

Replace `CLEANUP_DIRS=()` / `_cleanup` / `track_tmp` (lines ~67–73) with:

```sh
# Space-separated list of temp dirs to remove on exit (paths have no spaces).
CLEANUP_DIRS=""
_cleanup() { for d in $CLEANUP_DIRS; do [ -n "$d" ] && rm -rf "$d"; done; return 0; }
trap _cleanup EXIT
track_tmp() { CLEANUP_DIRS="$CLEANUP_DIRS $1"; }
```

- [ ] **Step 4: Convert `[[ … ]]` and `(( … ))` across the file**

Search and convert every occurrence:

Run: `grep -nE '\[\[|\(\(' scripts/install.sh`

Conversion rules (apply to each hit):
- `[[ "$a" = "$b" ]]` → `[ "$a" = "$b" ]`
- `[[ "$a" == pattern* ]]` → `case "$a" in pattern*) … ;; esac`
- `[[ -n "$x" && -f "$y" ]]` → `[ -n "$x" ] && [ -f "$y" ]`
- `(( x > 0 ))` → `[ "$x" -gt 0 ]`
- `$(( … ))` arithmetic expansion is POSIX — leave as is.

- [ ] **Step 5: Parse + seam tests pass under dash (i18n still bash here — temporarily stub)**

Because Task 2 converts i18n, this step's goal is only that the *non-i18n* mechanics parse. Verify the constructs converted so far:

Run: `grep -nE '\[\[|\(\(|BASH_SOURCE|BASH_VERSINFO|pipefail|CLEANUP_DIRS=\(\)' scripts/install.sh`
Expected: no matches.

- [ ] **Step 6: Commit**

```bash
git add scripts/install.sh
git commit -m "refactor(installer): POSIX shell mechanics (sh shebang, set -eu, no arrays/[[/((/BASH_SOURCE)"
```

---

## Task 2: i18n via `t()` case lookup (remove `declare -A`)

**Files:**
- Modify: `scripts/install.sh` (lines 77–~234: the two arrays + `t()` + `--print-i18n*`)

- [ ] **Step 1: Replace both `MSG_EN`/`MSG_ZH` arrays and the old `t()` with a case-lookup function**

Delete `declare -A MSG_EN MSG_ZH` and both `MSG_EN=( … )` / `MSG_ZH=( … )` blocks. Replace with this `t()` skeleton. For **every** key currently in the arrays, add two branches following the exact pattern shown (zh first, then the `*:` English default). Transcribe the existing EN/ZH strings verbatim, preserving `\n`, `%s`, and trailing spaces:

```sh
# t <key> [printf-args...] — localized printf. zh falls through to en default.
t() {
  _k="$1"; shift 2>/dev/null || true
  case "$LANG_CODE:$_k" in
    zh:menu_title)        _f='Portunus 管理器' ;;
    *:menu_title)         _f='Portunus Manager' ;;
    zh:done_next)         _f='安装完成，后续步骤：' ;;
    *:done_next)          _f='Done. Next steps:' ;;
    zh:next_standalone_config) _f='  编辑配置：sudoedit /etc/portunus/standalone.toml' ;;
    *:next_standalone_config)  _f='  edit:    sudoedit /etc/portunus/standalone.toml' ;;
    # … one zh: + one *: branch for EVERY key in the former arrays …
    *) _f="$_k" ;;   # unknown key prints the key itself (debug aid)
  esac
  # shellcheck disable=SC2059
  printf "$_f\\n" "$@"
}
```

> Note: the old `t()` (line 236) did `printf '%s\n'`-style formatting; keep
> the **same output contract** — `t` prints the formatted message followed by
> a newline. Audit existing call sites that append their own `echo`/newline
> and keep behavior identical (the seam tests below catch drift).

- [ ] **Step 2: Fix the two `--print-i18n*` seams (they iterated array keys)**

Replace the `--print-i18n-keys` branch (line ~599) with a static key list, and keep `--print-i18n` calling `t`:

```sh
--print-i18n-keys) shift 2>/dev/null || true
  printf '%s\n' menu_title menu_install menu_uninstall menu_upgrade menu_status \
    menu_service menu_config menu_env menu_exit menu_select lang_prompt ask_role \
    ask_deploy_server ask_deploy_client ask_deploy_standalone ask_version ask_bindir \
    ask_datadir ask_ophttp confirm_proceed confirm_uninstall confirm_purge_typed \
    need_role no_install_found done_next next_standalone_config next_systemd \
    next_docker next_status restart_now upgrade_current unknown_config_key \
    ask_config_key ask_config_value ask_service_action menu_invalid press_enter \
    bad_endpoint op_cancelled ask_advertised_pub summary_title sum_role sum_deploy \
    sum_version sum_bindir sum_datadir sum_ophttp sum_compose sum_advertised \
    prov_detected prov_nic prov_loopback prov_user val_latest val_binary val_docker \
    ask_domain sum_domain bad_domain dns_check dns_ok dns_mismatch dns_help \
    caddy_installing caddy_done caddy_verify caddy_verify_warn https_ready \
    https_public_note adv_from_domain config_na_standalone next_openrc
  exit 0 ;;
```

(Note `next_openrc` — a new key added in Task 7; add its `t()` branches there.)

- [ ] **Step 3: Verify i18n + parse under dash**

```bash
TEST_SH=dash sh scripts/install.test.sh
[ "$(LANG_CODE= dash scripts/install.sh --print-i18n done_next)" = "Done. Next steps:" ]
[ "$(PORTUNUS_LANG=zh dash scripts/install.sh --print-i18n done_next)" = "安装完成，后续步骤：" ]
```
Expected: parse gate passes; both assertions pass.

- [ ] **Step 4: Commit**

```bash
git add scripts/install.sh
git commit -m "refactor(installer): i18n via t() case lookup; drop declare -A"
```

---

## Task 3: `detect_init()` + `svc()` dispatcher + `--detect-init` seam

**Files:**
- Modify: `scripts/install.sh`
- Modify: `scripts/install.test.sh`

- [ ] **Step 1: Add the test seam assertion (failing test)**

In `scripts/install.test.sh` add:

```sh
ini="$("$SH" "$script" --detect-init)" || fail "--detect-init exit"
case "$ini" in systemd|openrc|none) ;; *) fail "bad init: $ini" ;; esac
```

Run: `TEST_SH=dash sh scripts/install.test.sh`
Expected: FAIL — `--detect-init` not implemented.

- [ ] **Step 2: Add `detect_init` and `svc` near `detect_platform` (after line ~309)**

```sh
INIT=""
detect_init() {
  if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
    INIT=systemd
  elif command -v rc-service >/dev/null 2>&1 || command -v openrc >/dev/null 2>&1; then
    INIT=openrc
  else
    INIT=none
  fi
}

# svc <op> [args] — dispatch to the detected init driver.
svc() { _op="$1"; shift 2>/dev/null || true; "${INIT}_${_op}" "$@"; }
```

- [ ] **Step 3: Wire the `--detect-init` seam in `parse_args` (near line ~605)**

```sh
--detect-init) detect_init; printf '%s\n' "$INIT"; exit 0 ;;
```

- [ ] **Step 4: Verify**

Run: `TEST_SH=dash sh scripts/install.test.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(installer): detect_init + svc dispatcher + --detect-init seam"
```

---

## Task 4: systemd driver group (refactor existing logic into `systemd_*`)

**Files:**
- Modify: `scripts/install.sh` (refactor `install_systemd_unit`, status/uninstall paths)

- [ ] **Step 1: Define the systemd driver functions**

Wrap the existing systemd logic into the standard verb set. `systemd_install` renders/installs the unit (existing `install_systemd_unit` body, minus the enable/start which never existed) and, when a custom config is set, the drop-in (Task 9 fills the drop-in body). Add:

```sh
systemd_install() {  # systemd_install <role> <config_path>
  install_systemd_unit "$1"          # existing unit-install body (refactored)
}
systemd_enable_start() { ${SUDO:-} systemctl enable --now "portunus-$1.service"; }
systemd_stop()    { ${SUDO:-} systemctl stop "portunus-$1.service" 2>/dev/null || true; }
systemd_disable() { ${SUDO:-} systemctl disable "portunus-$1.service" 2>/dev/null || true; }
systemd_status()  { ${SUDO:-} systemctl status "portunus-$1.service"; }
systemd_remove()  {
  ${SUDO:-} rm -f "/etc/systemd/system/portunus-$1.service"
  ${SUDO:-} rm -rf "/etc/systemd/system/portunus-$1.service.d"
  ${SUDO:-} systemctl daemon-reload 2>/dev/null || true
}
```

Refactor `install_systemd_unit` so it takes `$1=role` (it currently reads the global `$ROLE`; passing it explicitly keeps the driver interface uniform). Keep the unit-fetch logic (local template else `curl` from `RAW_BASE`) unchanged.

- [ ] **Step 2: Verify `--render-dropin` seam unchanged**

Run: `TEST_SH=dash sh scripts/install.test.sh`
Expected: PASS — the existing `--render-dropin` assertions (server) still produce identical output.

- [ ] **Step 3: Commit**

```bash
git add scripts/install.sh
git commit -m "refactor(installer): wrap systemd logic into systemd_* driver group"
```

---

## Task 5: OpenRC service templates (init.d + conf.d) for all roles

**Files:**
- Create: `crates/portunus-standalone/contrib/portunus-standalone.openrc`
- Create: `crates/portunus-standalone/contrib/portunus-standalone.confd`
- Create: `deploy/openrc/portunus-client.openrc`, `deploy/openrc/portunus-server.openrc`
- Create: `deploy/openrc/portunus-client.confd`, `deploy/openrc/portunus-server.confd`

- [ ] **Step 1: Standalone init.d** — `crates/portunus-standalone/contrib/portunus-standalone.openrc`

```sh
#!/sbin/openrc-run
# OpenRC service for portunus-standalone. Config path comes from
# /etc/conf.d/portunus-standalone (cfgfile=...).
description="Portunus standalone TCP/UDP forwarder"
command="/usr/local/bin/portunus-standalone"
command_args="--config ${cfgfile:-/etc/portunus/standalone.toml}"
command_user="portunus:portunus"
command_background=true
pidfile="/run/portunus-standalone.pid"
supervisor=supervise-daemon
respawn_delay=2
respawn_max=0
output_log="/var/log/portunus-standalone.log"
error_log="/var/log/portunus-standalone.log"

depend() {
  need net
  after firewall
}
```

- [ ] **Step 2: Standalone conf.d** — `crates/portunus-standalone/contrib/portunus-standalone.confd`

```sh
# Config file the service reads. The installer rewrites this line when
# `--config PATH` is given.
cfgfile="/etc/portunus/standalone.toml"
```

- [ ] **Step 3: Client init.d** — `deploy/openrc/portunus-client.openrc`

```sh
#!/sbin/openrc-run
description="Portunus edge client"
command="/usr/local/bin/portunus-client"
command_args="${client_args:-}"
command_user="portunus-client:portunus-client"
command_background=true
pidfile="/run/portunus-client.pid"
supervisor=supervise-daemon
respawn_delay=2
respawn_max=0
output_log="/var/log/portunus-client.log"
error_log="/var/log/portunus-client.log"

depend() {
  need net
  after firewall
}
```

- [ ] **Step 4: Client conf.d** — `deploy/openrc/portunus-client.confd`

```sh
# Extra arguments passed to portunus-client (e.g. --config /path).
client_args=""
```

- [ ] **Step 5: Server init.d** — `deploy/openrc/portunus-server.openrc`

```sh
#!/sbin/openrc-run
description="Portunus control-plane server"
command="/usr/local/bin/portunus-server"
command_args="--data-dir ${datadir:-/var/lib/portunus} serve ${server_args:-}"
command_user="portunus-server:portunus-server"
command_background=true
pidfile="/run/portunus-server.pid"
supervisor=supervise-daemon
respawn_delay=2
respawn_max=0
output_log="/var/log/portunus-server.log"
error_log="/var/log/portunus-server.log"

depend() {
  need net
  after firewall
}
```

- [ ] **Step 6: Server conf.d** — `deploy/openrc/portunus-server.confd`

```sh
# Server data dir and extra serve args (e.g. --operator-http-listen,
# --advertised-endpoint). The installer rewrites these when flags are given.
datadir="/var/lib/portunus"
server_args=""
```

- [ ] **Step 7: Commit**

```bash
git add crates/portunus-standalone/contrib/portunus-standalone.openrc \
        crates/portunus-standalone/contrib/portunus-standalone.confd \
        deploy/openrc/
git commit -m "feat(installer): OpenRC init.d + conf.d templates for all three roles"
```

---

## Task 6: openrc driver group + `--render-openrc` seam

**Files:**
- Modify: `scripts/install.sh`
- Modify: `scripts/install.test.sh`

- [ ] **Step 1: Add the seam assertion (failing test)**

In `scripts/install.test.sh`:

```sh
orc="$("$SH" "$script" --render-openrc standalone)" || fail "--render-openrc exit"
case "$orc" in *openrc-run*) ;; *) fail "render-openrc not an init.d script" ;; esac
```

Run: `TEST_SH=dash sh scripts/install.test.sh`
Expected: FAIL.

- [ ] **Step 2: Add the `openrc_*` driver group + a renderer**

```sh
openrc_unit_url() {  # role -> repo path
  case "$1" in
    standalone) printf '%s\n' "${RAW_BASE}/crates/portunus-standalone/contrib/portunus-standalone.openrc" ;;
    *)          printf '%s\n' "${RAW_BASE}/deploy/openrc/portunus-$1.openrc" ;;
  esac
}
openrc_confd_url() {
  case "$1" in
    standalone) printf '%s\n' "${RAW_BASE}/crates/portunus-standalone/contrib/portunus-standalone.confd" ;;
    *)          printf '%s\n' "${RAW_BASE}/deploy/openrc/portunus-$1.confd" ;;
  esac
}
render_openrc() {  # role -> init.d script to stdout (local file else curl)
  _r="$1"; _local=""
  [ -n "${SELF_SCRIPT:-}" ] && _local="$(dirname "$SELF_SCRIPT")"
  case "$_r" in
    standalone) _lp="$_local/../crates/portunus-standalone/contrib/portunus-standalone.openrc" ;;
    *)          _lp="$_local/../deploy/openrc/portunus-$_r.openrc" ;;
  esac
  if [ -n "$_local" ] && [ -r "$_lp" ]; then cat "$_lp"
  else curl -fsSL "$(openrc_unit_url "$_r")"; fi
}
openrc_install() {  # openrc_install <role> <config_path>
  command -v rc-update >/dev/null 2>&1 || die "openrc tools missing"
  render_openrc "$1" | ${SUDO:-} tee "/etc/init.d/portunus-$1" >/dev/null
  ${SUDO:-} chmod 0755 "/etc/init.d/portunus-$1"
  # conf.d written by apply_config_path (Task 9); seed default if absent.
  [ -f "/etc/conf.d/portunus-$1" ] || curl -fsSL "$(openrc_confd_url "$1")" | ${SUDO:-} tee "/etc/conf.d/portunus-$1" >/dev/null
}
openrc_enable_start() { ${SUDO:-} rc-update add "portunus-$1" default 2>/dev/null || true; ${SUDO:-} rc-service "portunus-$1" start; }
openrc_stop()    { ${SUDO:-} rc-service "portunus-$1" stop 2>/dev/null || true; }
openrc_disable() { ${SUDO:-} rc-update del "portunus-$1" default 2>/dev/null || true; }
openrc_status()  { ${SUDO:-} rc-service "portunus-$1" status; }
openrc_remove()  { ${SUDO:-} rm -f "/etc/init.d/portunus-$1" "/etc/conf.d/portunus-$1"; }
```

- [ ] **Step 3: Wire `--render-openrc` in `parse_args`**

```sh
--render-openrc) shift 2>/dev/null || true; render_openrc "${1:-standalone}"; exit 0 ;;
```

- [ ] **Step 4: Verify**

Run: `TEST_SH=dash sh scripts/install.test.sh`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(installer): openrc_* driver group + --render-openrc seam"
```

---

## Task 7: `none` driver group (degraded path)

**Files:**
- Modify: `scripts/install.sh`

- [ ] **Step 1: Add `next_openrc` + manual-run i18n keys to `t()`**

Add these branches in `t()`:

```sh
zh:next_openrc) _f='  启动服务：sudo rc-update add portunus-%s default && sudo rc-service portunus-%s start' ;;
*:next_openrc)  _f='  start:   sudo rc-update add portunus-%s default && sudo rc-service portunus-%s start' ;;
zh:next_manual) _f='  无受支持的 init 系统；手动运行：\n  nohup %s --config %s > /var/log/portunus.log 2>&1 &' ;;
*:next_manual)  _f='  no supported init system; run it manually:\n  nohup %s --config %s > /var/log/portunus.log 2>&1 &' ;;
```

(Append `next_manual` to the `--print-i18n-keys` list too.)

- [ ] **Step 2: Add the `none_*` driver group**

```sh
none_install() { :; }   # nothing to install; binary + config already placed
none_enable_start() {    # role -> print manual-run guidance, do not start
  t next_manual "/usr/local/bin/portunus-$1" "${2:-/etc/portunus/standalone.toml}"; echo
}
none_stop()    { :; }
none_disable() { :; }
none_status()  { echo "no service manager (init=none); not managed"; }
none_remove()  { :; }
```

- [ ] **Step 3: Verify parse + tests**

Run: `TEST_SH=dash sh scripts/install.test.sh`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add scripts/install.sh
git commit -m "feat(installer): none_* driver group for unsupported init systems"
```

---

## Task 8: default-start + `--no-service` + `--systemd` no-op

**Files:**
- Modify: `scripts/install.sh` (globals, `parse_args`, `dispatch_verb` install branch, `print_next_steps`)

- [ ] **Step 1: Add the global**

Near the other globals (line ~36 region) replace `WANT_SYSTEMD="no"` with:

```sh
NO_SERVICE="no"     # --no-service: install binary+config+unit, do not enable/start
WANT_SYSTEMD="no"   # legacy --systemd: accepted, now a no-op (service is default)
```

- [ ] **Step 2: parse_args — add `--no-service`, keep `--systemd` inert**

```sh
--no-service) NO_SERVICE="yes" ;;
--systemd) WANT_SYSTEMD="yes" ;;   # accepted for back-compat; no effect
```

- [ ] **Step 3: dispatch_verb install branch — drive via svc**

Replace the binary-deploy block in `dispatch_verb` (lines ~1039–1046) with:

```sh
else
  install_binary
  detect_init
  svc install "$ROLE" "$CONFIG_PATH"
  if [ "$NO_SERVICE" = yes ]; then
    :
  elif [ "$INIT" = none ]; then
    : # none_enable_start prints guidance in print_next_steps
  else
    svc enable_start "$ROLE"
  fi
  [ "$ROLE" = "server" ] && [ "$INIT" = systemd ] && write_server_dropin
  meta_write "$(meta_path_for)" "role=$ROLE" "deploy=$DEPLOY" "version=$resolved_version" \
    "lang=${LANG_CODE:-en}" "init=$INIT" \
    "advertised_endpoint_set=$([ -n "$ADVERTISED" ] && echo yes || echo no)"
fi
```

(`CONFIG_PATH` is defined in Task 9; until then it defaults via Task 9's `apply_install_defaults` change. If executing strictly in order, temporarily use the literal default path here and replace in Task 9.)

- [ ] **Step 4: print_next_steps — init-aware hints**

Replace the binary branch of `print_next_steps` (lines ~485–488):

```sh
else
  [ "$ROLE" = "standalone" ] && { t next_standalone_config; echo; }
  case "$INIT" in
    systemd) [ "$NO_SERVICE" = yes ] && { t next_systemd "$ROLE"; echo; } ;;
    openrc)  [ "$NO_SERVICE" = yes ] && { t next_openrc "$ROLE" "$ROLE"; echo; } ;;
    none)    none_enable_start "$ROLE" "$CONFIG_PATH" ;;
  esac
fi
```

- [ ] **Step 5: Verify**

Run: `TEST_SH=dash sh scripts/install.test.sh`
Plus a dry-run sanity check: `dash scripts/install.sh standalone --no-service --dry-run` (should print the plan, exit 0).
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add scripts/install.sh
git commit -m "feat(installer): install-and-start by default; --no-service opt-out; --systemd no-op"
```

---

## Task 9: `--config PATH` (flag, seed, inject, perms, warn)

**Files:**
- Modify: `scripts/install.sh`
- Modify: `scripts/install.test.sh`

- [ ] **Step 1: Add the test assertions (failing test)**

In `scripts/install.test.sh`:

```sh
# OpenRC conf.d carries the custom path through command_args via cfgfile.
cf="$("$SH" "$script" --render-confd standalone /custom/p.toml)" || fail "--render-confd exit"
case "$cf" in *'/custom/p.toml'*) ;; *) fail "confd missing custom path" ;; esac
# systemd drop-in carries --config when path != default.
dc="$("$SH" "$script" --render-config-dropin standalone /custom/p.toml)" || fail "--render-config-dropin exit"
case "$dc" in *'--config /custom/p.toml'*) ;; *) fail "dropin missing --config" ;; esac
```

Run: `TEST_SH=dash sh scripts/install.test.sh`
Expected: FAIL.

- [ ] **Step 2: Add the global + flag + default**

Global (near others): `CONFIG_PATH=""`.
parse_args: `--config) shift; [ $# -gt 0 ] || die "--config needs a value"; CONFIG_PATH="$1" ;;`
In `apply_install_defaults`, set the per-role default when unset:

```sh
if [ -z "$CONFIG_PATH" ]; then
  case "$ROLE" in
    standalone) CONFIG_PATH="/etc/portunus/standalone.toml" ;;
    client)     CONFIG_PATH="/etc/portunus/client.toml" ;;
    server)     CONFIG_PATH="" ;;   # server uses --data-dir, not --config
  esac
fi
```

- [ ] **Step 3: Add renderers + the seams**

```sh
config_default_for() { case "$1" in standalone) echo /etc/portunus/standalone.toml ;; client) echo /etc/portunus/client.toml ;; *) echo "" ;; esac; }

render_config_dropin() {  # role path -> systemd drop-in body (empty if path==default or server)
  _r="$1"; _p="$2"
  [ "$_r" = server ] && return 0
  [ "$_p" = "$(config_default_for "$_r")" ] && return 0
  printf '[Service]\nExecStart=\nExecStart=/usr/local/bin/portunus-%s --config %s\n' "$_r" "$_p"
}
render_confd() {  # role path -> conf.d body
  case "$1" in
    standalone) printf 'cfgfile="%s"\n' "$2" ;;
    client)     printf 'client_args="--config %s"\n' "$2" ;;
    server)     printf 'datadir="%s"\nserver_args=""\n' "${DATA_DIR:-/var/lib/portunus}" ;;
  esac
}
```

parse_args seams:

```sh
--render-confd) shift 2>/dev/null||true; render_confd "${1:-standalone}" "${2:-/etc/portunus/standalone.toml}"; exit 0 ;;
--render-config-dropin) shift 2>/dev/null||true; render_config_dropin "${1:-standalone}" "${2:-/etc/portunus/standalone.toml}"; exit 0 ;;
```

- [ ] **Step 4: Add `apply_config_path` and call it from the drivers**

```sh
# Seed config at CONFIG_PATH if absent, fix perms, warn if unreadable by svc user.
apply_config_path() {  # role config_path svc_user
  _r="$1"; _p="$2"; _u="$3"
  [ -z "$_p" ] && return 0
  _dir="$(dirname "$_p")"
  ${SUDO:-} mkdir -p "$_dir"
  if [ ! -f "$_p" ] && [ "$_r" = standalone ]; then
    _ex=""; [ -n "${SELF_SCRIPT:-}" ] && _ex="$(dirname "$SELF_SCRIPT")/../crates/portunus-standalone/contrib/portunus.example.toml"
    if [ -n "$_ex" ] && [ -r "$_ex" ]; then ${SUDO:-} cp "$_ex" "$_p"
    else ${SUDO:-} curl -fsSL "${RAW_BASE}/crates/portunus-standalone/contrib/portunus.example.toml" -o "$_p" || die "failed to seed config"; fi
  fi
  [ -f "$_p" ] && { ${SUDO:-} chown "root:$_u" "$_p" 2>/dev/null || true; ${SUDO:-} chmod 0640 "$_p" 2>/dev/null || true; }
  # Reachability warning: svc user must be able to read the file.
  if [ -f "$_p" ] && ! ${SUDO:-} su -s /bin/sh "$_u" -c "test -r '$_p'" 2>/dev/null; then
    echo "warning: $_u may not be able to read $_p (check directory permissions)" >&2
  fi
}
```

Wire into `systemd_install` and `openrc_install` (after placing the unit): resolve the svc user for the role (`portunus` / `portunus-client` / `portunus-server`), call `apply_config_path "$role" "$2" "$svc_user"`, then for systemd write the drop-in when `render_config_dropin` is non-empty, for openrc overwrite `/etc/conf.d/portunus-$role` with `render_confd`.

```sh
svc_user_for() { case "$1" in standalone) echo portunus ;; client) echo portunus-client ;; server) echo portunus-server ;; esac; }
```

In `systemd_install` append:

```sh
apply_config_path "$1" "$2" "$(svc_user_for "$1")"
_dropin="$(render_config_dropin "$1" "$2")"
if [ -n "$_dropin" ]; then
  ${SUDO:-} mkdir -p "/etc/systemd/system/portunus-$1.service.d"
  printf '%s\n' "$_dropin" | ${SUDO:-} tee "/etc/systemd/system/portunus-$1.service.d/10-config.conf" >/dev/null
  ${SUDO:-} systemctl daemon-reload || true
fi
```

In `openrc_install` append:

```sh
apply_config_path "$1" "$2" "$(svc_user_for "$1")"
render_confd "$1" "$2" | ${SUDO:-} tee "/etc/conf.d/portunus-$1" >/dev/null
```

- [ ] **Step 5: Verify (quote-safety included)**

```bash
TEST_SH=dash sh scripts/install.test.sh
# default path → empty dropin
[ -z "$(dash scripts/install.sh --render-config-dropin standalone /etc/portunus/standalone.toml)" ]
```
Expected: PASS; default-path drop-in is empty.

- [ ] **Step 6: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(installer): --config PATH with seed + drop-in/conf.d injection + perms/warn"
```

---

## Task 10: init-aware lifecycle verbs (uninstall/status/service/upgrade)

**Files:**
- Modify: `scripts/install.sh` (the `lifecycle_*` functions, lines ~849–1103)

- [ ] **Step 1: Read `init` from meta and detect as fallback**

Where lifecycle verbs load the meta file, read `init=` and set `INIT`; if absent, call `detect_init`:

```sh
INIT="$(meta_read "$mf" init 2>/dev/null || true)"
case "$INIT" in systemd|openrc|none) ;; *) detect_init ;; esac
```

- [ ] **Step 2: Route uninstall/status/service through svc**

- `lifecycle_uninstall`: replace systemd-specific stop/disable/rm with `svc stop "$role"; svc disable "$role"; svc remove "$role"`, then remove the binary; `--purge` still removes `/etc/portunus` and data dir.
- `lifecycle_status`: replace `systemctl status` with `svc status "$role"`.
- `lifecycle_service` (start/stop/restart): map start→`svc enable_start` (or a dedicated `svc start`), stop→`svc stop`, restart→`svc stop` + `svc enable_start`. (Add `systemd_start`/`openrc_start` aliases that start without enabling if a non-enabling start is wanted; otherwise reuse enable_start for start.)
- `lifecycle_upgrade`: after installing the new binary, `svc stop "$role"; svc enable_start "$role"` (or `svc restart`).

Add `systemd_restart() { ${SUDO:-} systemctl restart "portunus-$1.service"; }` and `openrc_restart() { ${SUDO:-} rc-service "portunus-$1" restart; }` and `none_restart() { :; }`, then use `svc restart` for restart.

- [ ] **Step 3: Verify seams + parse**

Run: `TEST_SH=dash sh scripts/install.test.sh`
Expected: PASS (meta round-trip now includes `init=`; existing meta assertions unaffected since they don't assert on absent keys).

- [ ] **Step 4: Commit**

```bash
git add scripts/install.sh
git commit -m "feat(installer): init-aware uninstall/status/service/upgrade via svc; meta records init"
```

---

## Task 11: Docs sync (README en/zh)

**Files:**
- Modify: `README.md`, `README.zh-CN.md`

- [ ] **Step 1: README.md — `| bash` → `| sh`, drop bash-4 note, add flags**

Change all `| bash -s --` to `| sh -s --`. In the Installation paragraph replace "(needs `bash` 4+)" with "(POSIX `sh`; runs on dash/busybox too)". Add one line after the standalone block:

```markdown
Installs and starts a service automatically (systemd or OpenRC). Use `--no-service` to install without starting, or `--config /path/to.toml` to point the service at a specific config file.
```

- [ ] **Step 2: README.zh-CN.md — mirror the change**

Change `| bash -s --` → `| sh -s --`; replace "（需要 `bash` 4+）" with "（POSIX `sh`，dash/busybox 亦可）"; add:

```markdown
安装后会自动安装并启动服务（systemd 或 OpenRC）。用 `--no-service` 只安装不启动，或用 `--config /路径/到.toml` 指定服务读取的配置文件。
```

- [ ] **Step 3: Commit**

```bash
git add README.md README.zh-CN.md
git commit -m "docs(readme): installer is POSIX sh; document default-start, --no-service, --config"
```

---

## Task 12: CI dash step

**Files:**
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Add a job/step running the test harness under dash**

In the existing test workflow add (Ubuntu has dash as `/bin/sh`):

```yaml
      - name: Installer POSIX smoke (dash)
        run: |
          sudo apt-get update && sudo apt-get install -y dash busybox
          TEST_SH=dash sh scripts/install.test.sh
          TEST_SH="busybox sh" sh scripts/install.test.sh
```

- [ ] **Step 2: Commit**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: run installer test harness under dash and busybox"
```

---

## Task 13: VPS integration test (systemd) + OpenRC-in-container

**Files:** none (operational verification on the VPS at `38.64.56.236:2222`).

- [ ] **Step 1: Push the branch so the VPS can `curl` the raw script** — or `scp` the script to the VPS to test the working copy directly (preferred: avoids needing the branch on `main`/raw).

- [ ] **Step 2: systemd host test** (the VPS is expected to be systemd):

```sh
# on VPS (scp'd script as /root/install.sh)
sh /root/install.sh --detect-init          # expect: systemd
sh /root/install.sh standalone             # installs + starts
systemctl is-active portunus-standalone    # expect: active
sh /root/install.sh standalone --config /root/custom.toml --no-service  # path injection, no start
cat /etc/systemd/system/portunus-standalone.service.d/10-config.conf    # shows --config /root/custom.toml
sh /root/install.sh uninstall              # stops, disables, removes
```

- [ ] **Step 3: OpenRC test via Alpine container on the VPS** (if Docker present):

```sh
docker run --rm -it -v /root/install.sh:/install.sh alpine sh -c '
  apk add --no-cache curl openrc;
  sh /install.sh --detect-init;     # expect: openrc (or none if /run/openrc absent in container)
  sh /install.sh standalone --no-service;
  cat /etc/conf.d/portunus-standalone;
  cat /etc/init.d/portunus-standalone | head -3'
```

> Containers often lack a running init; `--detect-init` may report `none`.
> That still exercises the OpenRC **file rendering** (init.d + conf.d) and the
> `none` degraded path. A full enable/start test needs an OpenRC VM or
> `--privileged` with openrc booted; note the limitation if the VPS can't.

- [ ] **Step 4: Record results; fix any failure, re-run the affected step.**

---

## Self-Review

- **Spec coverage:** §5.1 POSIX→Tasks 1–2; §5.2 init abstraction→Tasks 3,4,6,7; §5.3 default-start→Task 8; §5.4 `--config`→Task 9; §5.5 OpenRC artifacts→Task 5; §5.6 lifecycle→Task 10; §5.7 tests→Tasks 0,3,6,9 + Task 12; §5.8 docs→Task 11. Acceptance criteria 1–6→Task 13 + harness. Covered.
- **Placeholder scan:** Task 2 i18n transcription references the existing in-repo table as the verbatim source (mechanical 1:1, pattern + 3 concrete examples given) — acceptable for a pure transcription; not a logic placeholder. No TBD/TODO elsewhere.
- **Type/name consistency:** driver verbs uniform across groups (`<init>_install/enable_start/stop/disable/status/remove/restart`); `svc <op>` matches; `CONFIG_PATH`, `render_config_dropin`, `render_confd`, `apply_config_path`, `svc_user_for`, `config_default_for` used consistently across Tasks 8–10; meta `init=` key written in Task 8, read in Task 10.
- **Ordering note:** Task 8 references `CONFIG_PATH` introduced in Task 9; the inline note flags the temporary literal until Task 9 lands (or execute Task 9 before wiring Task 8 Step 3 — both noted).
