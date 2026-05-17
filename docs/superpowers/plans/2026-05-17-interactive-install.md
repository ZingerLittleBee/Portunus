# Interactive `install.sh` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `scripts/install.sh` with a single self-contained bash 4+ full-lifecycle manager (install/uninstall/upgrade/status/service/config/env), bilingual en/zh, supporting binary+systemd and Docker Compose, while preserving the existing non-interactive flag interface.

**Architecture:** One bash file, comment-banner sections, function-per-concern. Dual-mode: interactive menu (via `/dev/tty` so `curl|bash` works) or non-interactive flags. An `.install-meta` sidecar records deploy form so subcommands dispatch `systemctl` vs `docker compose`. Server config (advertised-endpoint et al.) persists via a systemd drop-in or compose `.env`, never editing the shipped unit/compose. TDD via an expanded `scripts/install.test.sh` (network-free, `--dry-run` invariant, fed-fd interactive simulation) plus shellcheck.

**Tech Stack:** bash 4+, POSIX coreutils, curl, tar, sha256sum/shasum, systemd (optional), docker compose v2 (optional), shellcheck.

**Spec:** `docs/superpowers/specs/2026-05-17-interactive-install-design.md`

---

## File Structure

| File | Responsibility |
|------|----------------|
| `scripts/install.sh` | The entire bash 4+ lifecycle manager (replaces the POSIX script; filename kept). |
| `scripts/install.test.sh` | Network-free bash test harness: arg matrix, dry-run invariants, i18n coverage, meta round-trip, deploy-form detection, fed-fd interactive simulation, shellcheck gate. |
| `docs/content/docs/getting-started/installation.mdx` | en: `| sh`→`| bash`, document menu + subcommands + advertised-endpoint prompt. |
| `docs/content/docs/zh/getting-started/installation.mdx` | zh mirror of the above. |
| `docs/content/docs/deployment/docker.mdx` | Cross-link the manager's docker form. |
| `docs/content/docs/deployment/railway.mdx` | Cross-link the advertised-endpoint install prompt. |

Internal section banners inside `scripts/install.sh`, in order: `Guard` → `Constants` → `Globals` → `i18n` → `Platform` → `Meta` → `Plan/dry-run` → `Download/install (binary)` → `Systemd` → `Docker` → `Server config (drop-in/.env)` → `Lifecycle (status/upgrade/service/uninstall/config/env)` → `Interactive menu` → `Arg parse + dispatch` → `main`.

Convention for every task: build/test under **bash**. The script must stay runnable and `install.test.sh` must stay green at every commit. Never push (local commits only).

---

### Task 0: Bash scaffold + test harness conversion (behavior-preserving)

**Files:**
- Modify: `scripts/install.sh` (full rewrite of header/guard/scaffold; port existing dry-run logic into functions)
- Modify: `scripts/install.test.sh:1-31` (convert `sh`→`bash`, keep existing assertions)

- [ ] **Step 1: Convert the test harness to bash and add a guard assertion**

Replace the entire contents of `scripts/install.test.sh` with:

```bash
#!/usr/bin/env bash
# Network-free smoke test for scripts/install.sh.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
script="$here/install.sh"

fail() { echo "FAIL: $1" >&2; exit 1; }

# --- existing dry-run contract (now under bash) ---
out="$(bash "$script" client --version 1.4.1 --dry-run)" || fail "exit non-zero"
echo "$out" | grep -q '^role:[[:space:]]*client$' || fail "role line"
echo "$out" | grep -q '^tag:[[:space:]]*v1.4.1$' || fail "tag line"
echo "$out" | grep -q '^artifact_version:[[:space:]]*1.4.1$' || fail "artifact_version line"
echo "$out" | grep -q 'releases/download/v1.4.1/portunus-1.4.1-.*\.tar\.gz' || fail "download_url"
echo "$out" | grep -q 'portunus-1.4.1-checksums\.txt' || fail "checksums_url"

out2="$(bash "$script" server --version v2.0.0 --dry-run)" || fail "v-prefixed exit"
echo "$out2" | grep -q '^role:[[:space:]]*server$' || fail "server role"
echo "$out2" | grep -q '^tag:[[:space:]]*v2.0.0$' || fail "v-normalised tag"
echo "$out2" | grep -q '^artifact_version:[[:space:]]*2.0.0$' || fail "v-normalised artifact"

if bash "$script" bogus --dry-run >/dev/null 2>&1; then fail "bogus role accepted"; fi
bash "$script" client --version 1.0.0 --yes --dry-run >/dev/null 2>&1 || fail "--yes flag rejected"

echo "PASS"
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash scripts/install.test.sh`
Expected: FAIL (current `scripts/install.sh` is POSIX `#!/bin/sh`; this still works under `bash`, so it may PASS here — if it PASSES that is acceptable, the harness conversion is the only goal of this step. If it FAILs on a bashism it is because Step 3 not yet done.)

- [ ] **Step 3: Rewrite `scripts/install.sh` scaffold (bash guard + functionized dry-run)**

Replace the entire contents of `scripts/install.sh` with the following. This preserves the exact existing `--dry-run` output and flag behavior, restructured into bash functions:

```bash
#!/usr/bin/env bash
# Portunus lifecycle manager: install/uninstall/upgrade/status/service/
# config/env for client and server, binary+systemd or Docker Compose.
#
#   curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- client
#   curl -fsSL .../scripts/install.sh | bash        # interactive menu
#
set -euo pipefail

# ─── Guard ────────────────────────────────────────────────────────────
if [ -z "${BASH_VERSINFO:-}" ] || [ "${BASH_VERSINFO[0]:-0}" -lt 4 ]; then
  echo "Portunus installer requires bash 4.0+ (found ${BASH_VERSION:-unknown})." >&2
  echo "On macOS: 'brew install bash' then run it with that bash." >&2
  exit 1
fi

SELF_SCRIPT=""
case "${BASH_SOURCE[0]:-}" in
  "" | bash | sh | -bash | -sh) ;;
  *) [ -r "${BASH_SOURCE[0]}" ] && SELF_SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd)/$(basename "${BASH_SOURCE[0]}")" ;;
esac

# ─── Constants ────────────────────────────────────────────────────────
REPO="ZingerLittleBee/Portunus"
RAW_BASE="https://raw.githubusercontent.com/${REPO}/main"
DEFAULT_BIN_DIR="/usr/local/bin"
DOCS_FEATURE_URL="https://github.com/${REPO}/blob/main/docs/content/docs/features/advertised-endpoint.mdx"

# ─── Globals ──────────────────────────────────────────────────────────
VERB=""           # install|uninstall|upgrade|status|service|config|env
ROLE=""           # client|server
DEPLOY=""         # binary|docker
VERSION=""        # user-supplied version (may have leading v)
BIN_DIR="$DEFAULT_BIN_DIR"
COMPOSE_DIR=""
WANT_SYSTEMD="no"
ADVERTISED=""     # advertised endpoint host:port ("" = unset/auto)
DATA_DIR=""
OP_HTTP_LISTEN=""
SERVICE_ACTION="" # start|stop|restart
CONFIG_OP=""      # get|set
CONFIG_KEY=""
CONFIG_VALUE=""
ASSUME_YES="no"
PURGE="no"
DRY_RUN="no"
LANG_CODE="${PORTUNUS_LANG:-}"
tag=""
artifact_version=""
resolved_version=""
os=""
arch=""
target=""

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }

# ─── Platform ─────────────────────────────────────────────────────────
detect_platform() {
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *) die "unsupported arch: $arch" ;;
  esac
  case "$os" in
    linux) target="${arch}-unknown-linux-gnu" ;;
    darwin) target="${arch}-apple-darwin" ;;
    *) die "unsupported os: $os" ;;
  esac
}

resolve_version_static() {
  if [ -n "$VERSION" ]; then
    case "$VERSION" in
      v*) tag="$VERSION"; artifact_version="${VERSION#v}" ;;
      *)  tag="v$VERSION"; artifact_version="$VERSION" ;;
    esac
    resolved_version="$artifact_version"
  else
    resolved_version="<latest, resolved at run time>"; tag=""; artifact_version=""
  fi
}

rel() { echo "https://github.com/${REPO}/releases/download/${tag}/$1"; }

# ─── Plan / dry-run ───────────────────────────────────────────────────
print_plan() {
  local asset checksums
  asset="portunus-${artifact_version:-<latest>}-${target}.tar.gz"
  checksums="portunus-${artifact_version:-<latest>}-checksums.txt"
  echo "portunus install (dry-run)"
  echo "role:             ${ROLE}"
  echo "os:               ${os}"
  echo "arch:             ${arch}"
  echo "target:           ${target}"
  echo "tag:              ${tag:-<latest, resolved at run time>}"
  echo "artifact_version: ${resolved_version}"
  if [ -n "$artifact_version" ]; then
    echo "download_url:     $(rel "$asset")"
    echo "checksums_url:    $(rel "$checksums")"
  else
    echo "download_url:     <github releases/latest, resolved at run time>"
    echo "checksums_url:    <github releases/latest, resolved at run time>"
  fi
  echo "deploy:           ${DEPLOY:-binary}"
  echo "bin_dir:          ${BIN_DIR}"
  echo "systemd:          ${WANT_SYSTEMD}"
  echo "advertised:       ${ADVERTISED:-<unset, runtime auto>}"
  echo "actions:          download+verify+install portunus-${ROLE} -> ${BIN_DIR}$( [ "$WANT_SYSTEMD" = yes ] && echo ' + systemd unit' )"
}

# ─── Arg parse + dispatch (minimal; expanded in Task 2) ───────────────
parse_args() {
  while [ $# -gt 0 ]; do
    case "$1" in
      client|server) ROLE="$1"; VERB="${VERB:-install}" ;;
      install|uninstall|upgrade|status|service|config|env) VERB="$1" ;;
      --version) shift; [ $# -gt 0 ] || die "--version needs a value"; VERSION="$1" ;;
      --bin-dir) shift; [ $# -gt 0 ] || die "--bin-dir needs a value"; BIN_DIR="$1" ;;
      --systemd) WANT_SYSTEMD="yes" ;;
      --yes) ASSUME_YES="yes" ;;
      --dry-run) DRY_RUN="yes" ;;
      -h|--help) echo "usage: install.sh <client|server|install|uninstall|upgrade|status|service|config|env> [flags]"; exit 0 ;;
      *) die "unknown argument: $1" ;;
    esac
    shift
  done
}

main() {
  parse_args "$@"
  [ -n "$ROLE" ] || die "role required: client or server"
  detect_platform
  resolve_version_static
  if [ "$DRY_RUN" = "yes" ]; then print_plan; exit 0; fi
  die "non-dry-run install not yet implemented (scaffold task)"
}

main "$@"
```

- [ ] **Step 4: Run to verify it passes**

Run: `bash scripts/install.test.sh`
Expected: `PASS`

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "refactor(install): bash scaffold, functionized dry-run, bash test harness"
```

---

### Task 1: i18n table + language resolution

**Files:**
- Modify: `scripts/install.sh` (add `i18n` section: `MSG_EN`, `MSG_ZH`, `t`, `resolve_lang`)
- Modify: `scripts/install.test.sh` (add i18n key-coverage + lang-resolution assertions)

- [ ] **Step 1: Add failing tests**

Append before the final `echo "PASS"` in `scripts/install.test.sh`:

```bash
# --- i18n key coverage: every EN key exists in ZH and vice-versa ---
keys_en="$(bash "$script" --print-i18n-keys en | sort)"
keys_zh="$(bash "$script" --print-i18n-keys zh | sort)"
[ -n "$keys_en" ] || fail "no EN i18n keys"
[ "$keys_en" = "$keys_zh" ] || fail "i18n EN/ZH key sets differ"

# --- explicit lang override wins ---
bash "$script" --lang zh --print-i18n menu_title | grep -q '管理' || fail "zh menu_title"
PORTUNUS_LANG=en bash "$script" --print-i18n menu_title | grep -qi 'manager' || fail "en menu_title"
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash scripts/install.test.sh`
Expected: FAIL `unknown argument: --print-i18n-keys`

- [ ] **Step 3: Add the i18n section and hidden introspection flags**

In `scripts/install.sh`, immediately after the `# ─── Globals ───` block's `need()` definition, add:

```bash
# ─── i18n ─────────────────────────────────────────────────────────────
declare -A MSG_EN MSG_ZH
MSG_EN=(
  [menu_title]="Portunus Manager"
  [menu_install]="  [1] Install"
  [menu_uninstall]="  [2] Uninstall"
  [menu_upgrade]="  [3] Upgrade"
  [menu_status]="  [4] Status"
  [menu_service]="  [5] Service (start/stop/restart)"
  [menu_config]="  [6] Config"
  [menu_env]="  [7] Env"
  [menu_exit]="  [0] Exit"
  [menu_select]="Select [0-7]: "
  [lang_prompt]="Select language [1] English [2] 中文: "
  [ask_role]="Install which role? [1] server [2] client: "
  [ask_deploy]="Deploy form? [1] binary+systemd [2] docker compose: "
  [ask_version]="Version (blank = latest): "
  [ask_bindir]="Install dir [%s]: "
  [ask_advertised]="Advertised endpoint host:port (blank = auto; cert SAN must cover it; see %s): "
  [ask_datadir]="Server data dir (blank = default): "
  [ask_ophttp]="Operator HTTP listen (blank = default): "
  [confirm_proceed]="Proceed? [Y/n]: "
  [confirm_uninstall]="Uninstall portunus-%s (%s)? [y/N]: "
  [confirm_purge_typed]="Type 'purge' to also delete data at %s: "
  [need_role]="role required: client or server"
  [no_install_found]="No Portunus install detected (no .install-meta and no probe match)."
  [done_next]="Done. Next steps:"
  [restart_now]="Apply now (restart service)? [y/N]: "
  [upgrade_current]="Already at %s; nothing to upgrade."
  [unknown_config_key]="unknown config key: %s (allowed: advertised-endpoint data-dir operator-http-listen version-pin)"
)
MSG_ZH=(
  [menu_title]="Portunus 管理器"
  [menu_install]="  [1] 安装    Install"
  [menu_uninstall]="  [2] 卸载    Uninstall"
  [menu_upgrade]="  [3] 升级    Upgrade"
  [menu_status]="  [4] 状态    Status"
  [menu_service]="  [5] 服务控制 Service (start/stop/restart)"
  [menu_config]="  [6] 配置    Config"
  [menu_env]="  [7] 环境变量 Env"
  [menu_exit]="  [0] 退出    Exit"
  [menu_select]="选择 [0-7]: "
  [lang_prompt]="选择语言 [1] English [2] 中文: "
  [ask_role]="安装哪个角色? [1] server [2] client: "
  [ask_deploy]="部署方式? [1] 二进制+systemd [2] docker compose: "
  [ask_version]="版本 (留空=最新): "
  [ask_bindir]="安装目录 [%s]: "
  [ask_advertised]="通告地址 host:port (留空=自动; 证书 SAN 须覆盖该 host; 参见 %s): "
  [ask_datadir]="服务端 data 目录 (留空=默认): "
  [ask_ophttp]="Operator HTTP 监听 (留空=默认): "
  [confirm_proceed]="继续? [Y/n]: "
  [confirm_uninstall]="卸载 portunus-%s (%s)? [y/N]: "
  [confirm_purge_typed]="输入 'purge' 以同时删除数据 %s: "
  [need_role]="必须指定角色: client 或 server"
  [no_install_found]="未检测到 Portunus 安装 (无 .install-meta 且探测未命中)。"
  [done_next]="完成。后续步骤:"
  [restart_now]="现在生效 (重启服务)? [y/N]: "
  [upgrade_current]="已是 %s; 无需升级。"
  [unknown_config_key]="未知配置键: %s (允许: advertised-endpoint data-dir operator-http-listen version-pin)"
)

resolve_lang() {
  if [ -z "$LANG_CODE" ]; then
    case "${LC_ALL:-${LANG:-}}" in zh*|*zh_*) LANG_CODE="zh" ;; *) LANG_CODE="" ;; esac
  fi
  case "$LANG_CODE" in zh|en) ;; *) LANG_CODE="" ;; esac
}

t() {
  local key="$1"; shift || true
  local val
  if [ "${LANG_CODE:-en}" = "zh" ]; then val="${MSG_ZH[$key]:-}"; else val="${MSG_EN[$key]:-}"; fi
  [ -n "$val" ] || val="${MSG_EN[$key]:-$key}"
  # shellcheck disable=SC2059
  printf "$val" "$@"
}
```

Then in `parse_args`, add these cases **before** the `*) die` arm:

```bash
      --lang) shift; [ $# -gt 0 ] || die "--lang needs a value"; LANG_CODE="$1" ;;
      --print-i18n-keys) shift; resolve_lang; [ "${1:-en}" = zh ] && { for k in "${!MSG_ZH[@]}"; do echo "$k"; done; } || { for k in "${!MSG_EN[@]}"; do echo "$k"; done; }; exit 0 ;;
      --print-i18n) shift; [ $# -gt 0 ] || die "--print-i18n needs a key"; resolve_lang; t "$1"; echo; exit 0 ;;
```

(The two `--print-i18n*` flags are test-only introspection seams; they exit before any side effect.)

- [ ] **Step 4: Run to verify it passes**

Run: `bash scripts/install.test.sh`
Expected: `PASS`

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(install): bilingual i18n table + language resolution"
```

---

### Task 2: Full arg/verb parser, new flags, mode dispatch

**Files:**
- Modify: `scripts/install.sh` (`parse_args`, add `is_interactive`, rework `main`)
- Modify: `scripts/install.test.sh` (parsing matrix + dispatch assertions)

- [ ] **Step 1: Add failing tests**

Append before `echo "PASS"`:

```bash
# --- new flags accepted in dry-run plan ---
o="$(bash "$script" server --deploy docker --advertised-endpoint h.example:7443 --data-dir /srv/p --operator-http-listen 0.0.0.0:7080 --version 1.0.0 --dry-run)" || fail "new flags exit"
echo "$o" | grep -q '^deploy:[[:space:]]*docker$' || fail "deploy docker"
echo "$o" | grep -q '^advertised:[[:space:]]*h.example:7443$' || fail "advertised line"

# --- bare role implies install verb; explicit verb parsed ---
bash "$script" install client --version 1.0.0 --dry-run >/dev/null 2>&1 || fail "install verb"
bash "$script" status --help >/dev/null 2>&1 || fail "status+help"

# --- non-interactive when no tty and no args: helpful error, non-zero ---
if echo "" | bash "$script" </dev/null >/dev/null 2>&1; then fail "no-arg no-tty should error"; fi
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash scripts/install.test.sh`
Expected: FAIL `unknown argument: --deploy`

- [ ] **Step 3: Replace `parse_args` and `main`, add `is_interactive`**

In `scripts/install.sh` replace the entire `parse_args` function and `main` with:

```bash
parse_args() {
  while [ $# -gt 0 ]; do
    case "$1" in
      client|server) ROLE="$1"; [ -z "$VERB" ] && VERB="install" ;;
      install|uninstall|upgrade|status|service|config|env) VERB="$1" ;;
      start|stop|restart) SERVICE_ACTION="$1" ;;
      get|set) CONFIG_OP="$1" ;;
      --version) shift; [ $# -gt 0 ] || die "--version needs a value"; VERSION="$1" ;;
      --bin-dir) shift; [ $# -gt 0 ] || die "--bin-dir needs a value"; BIN_DIR="$1" ;;
      --compose-dir) shift; [ $# -gt 0 ] || die "--compose-dir needs a value"; COMPOSE_DIR="$1" ;;
      --deploy) shift; case "${1:-}" in binary|docker) DEPLOY="$1" ;; *) die "--deploy must be binary|docker" ;; esac ;;
      --advertised-endpoint) shift; [ $# -gt 0 ] || die "--advertised-endpoint needs a value"; ADVERTISED="$1" ;;
      --data-dir) shift; [ $# -gt 0 ] || die "--data-dir needs a value"; DATA_DIR="$1" ;;
      --operator-http-listen) shift; [ $# -gt 0 ] || die "--operator-http-listen needs a value"; OP_HTTP_LISTEN="$1" ;;
      --lang) shift; [ $# -gt 0 ] || die "--lang needs a value"; LANG_CODE="$1" ;;
      --systemd) WANT_SYSTEMD="yes" ;;
      --yes) ASSUME_YES="yes" ;;
      --purge) PURGE="yes" ;;
      --dry-run) DRY_RUN="yes" ;;
      --print-i18n-keys) shift; resolve_lang; if [ "${1:-en}" = zh ]; then for k in "${!MSG_ZH[@]}"; do echo "$k"; done; else for k in "${!MSG_EN[@]}"; do echo "$k"; done; fi; exit 0 ;;
      --print-i18n) shift; [ $# -gt 0 ] || die "--print-i18n needs a key"; resolve_lang; t "$1"; echo; exit 0 ;;
      -h|--help) echo "usage: install.sh <client|server|install|uninstall|upgrade|status|service|config|env> [start|stop|restart] [get|set key [value]] [--version V] [--deploy binary|docker] [--bin-dir D] [--compose-dir D] [--advertised-endpoint H:P] [--data-dir D] [--operator-http-listen A] [--systemd] [--lang en|zh] [--yes] [--purge] [--dry-run]"; exit 0 ;;
      *) if [ "$VERB" = config ] && [ -z "$CONFIG_KEY" ]; then CONFIG_KEY="$1"; elif [ "$VERB" = config ] && [ -z "$CONFIG_VALUE" ]; then CONFIG_VALUE="$1"; else die "unknown argument: $1"; fi ;;
    esac
    shift
  done
}

is_interactive() {
  if [ -t 0 ]; then return 0; fi
  if [ -r /dev/tty ] && { exec 3</dev/tty; } 2>/dev/null; then exec 3<&-; return 0; fi
  return 1
}

main() {
  parse_args "$@"
  resolve_lang
  # No actionable verb/role and a TTY ⇒ interactive menu (Task 7).
  if [ -z "$VERB" ] && [ -z "$ROLE" ]; then
    if is_interactive; then run_menu; exit $?; fi
    die "no command given and no terminal; run 'install.sh -h' or pass a role"
  fi
  [ -n "$VERB" ] || VERB="install"
  detect_platform
  resolve_version_static
  if [ "$DRY_RUN" = "yes" ]; then
    if [ "$VERB" = "install" ]; then [ -n "$ROLE" ] || die "$(t need_role)"; print_plan; exit 0; fi
    echo "verb: ${VERB} (dry-run; no side effects)"; exit 0
  fi
  dispatch_verb
}
```

Add a temporary stub near the bottom (above `main "$@"`), to be replaced in later tasks:

```bash
run_menu() { die "interactive menu not yet implemented"; }
dispatch_verb() { die "verb '${VERB}' not yet implemented"; }
```

- [ ] **Step 4: Run to verify it passes**

Run: `bash scripts/install.test.sh`
Expected: `PASS`

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(install): full verb/flag parser + interactive-mode detection"
```

---

### Task 3: Install metadata read/write + deploy-form detection

**Files:**
- Modify: `scripts/install.sh` (add `Meta` section)
- Modify: `scripts/install.test.sh` (round-trip + fixture detection)

- [ ] **Step 1: Add failing tests**

Append before `echo "PASS"`:

```bash
# --- meta round-trip via test seam ---
tmpm="$(mktemp -d)"
bash "$script" --meta-write "$tmpm/.install-meta" role=server deploy=docker version=1.2.3 lang=en >/dev/null || fail "meta write"
val="$(bash "$script" --meta-read "$tmpm/.install-meta" version)" || fail "meta read"
[ "$val" = "1.2.3" ] || fail "meta round-trip ($val)"
rm -rf "$tmpm"

# --- deploy-form detection from a compose fixture ---
tmpd="$(mktemp -d)"
printf 'services:\n  server:\n    image: portunus-server\n' > "$tmpd/compose.yml"
[ "$(bash "$script" --detect-deploy "$tmpd")" = "docker" ] || fail "detect docker"
rm -rf "$tmpd"
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash scripts/install.test.sh`
Expected: FAIL `unknown argument: --meta-write`

- [ ] **Step 3: Add the Meta section**

Add after the `# ─── i18n ───` block:

```bash
# ─── Meta ─────────────────────────────────────────────────────────────
meta_path_for() {
  # echo the .install-meta path for current ROLE/DEPLOY/paths.
  if [ "${DEPLOY:-binary}" = "docker" ]; then
    echo "${COMPOSE_DIR:-$PWD}/.install-meta"
  elif [ "$ROLE" = "server" ]; then
    echo "${DATA_DIR:-/var/lib/portunus}/.install-meta"
  else
    echo "/etc/portunus/.install-meta"
  fi
}

meta_write() {
  local f="$1"; shift
  local dir; dir="$(dirname "$f")"
  [ "$DRY_RUN" = yes ] && { echo "would write meta: $f ($*)"; return 0; }
  mkdir -p "$dir" 2>/dev/null || true
  : > "$f"
  local kv
  for kv in "$@"; do printf '%s\n' "$kv" >> "$f"; done
  printf 'installed_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$f"
  printf 'installer_version=%s\n' "${SELF_SCRIPT:-pipe}" >> "$f"
}

meta_read() {
  local f="$1" key="$2" line
  [ -r "$f" ] || return 1
  while IFS= read -r line; do
    case "$line" in "${key}="*) printf '%s\n' "${line#*=}"; return 0 ;; esac
  done < "$f"
  return 1
}

detect_deploy() {
  local hint="${1:-}" f
  if [ -n "$hint" ]; then
    for f in "$hint"/compose.yml "$hint"/compose.yaml "$hint"/docker-compose.yml "$hint"/docker-compose.yaml; do
      [ -f "$f" ] && { echo "docker"; return 0; }
    done
  fi
  if [ -f /etc/systemd/system/portunus-server.service ] || [ -f /etc/systemd/system/portunus-client.service ]; then
    echo "binary"; return 0
  fi
  if command -v portunus-server >/dev/null 2>&1 || command -v portunus-client >/dev/null 2>&1; then
    echo "binary"; return 0
  fi
  echo ""; return 0
}
```

Add these cases to `parse_args` before the `*)` arm:

```bash
      --meta-write) shift; f="$1"; shift; meta_write "$f" "$@"; exit 0 ;;
      --meta-read) shift; f="$1"; k="$2"; meta_read "$f" "$k"; exit $? ;;
      --detect-deploy) shift; detect_deploy "${1:-}"; exit 0 ;;
```

- [ ] **Step 4: Run to verify it passes**

Run: `bash scripts/install.test.sh`
Expected: `PASS`

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(install): .install-meta read/write + deploy-form detection"
```

---

### Task 4: Binary install path + systemd unit + server drop-in

**Files:**
- Modify: `scripts/install.sh` (add `Download/install`, `Systemd`, `Server config` sections; wire `dispatch_verb` install/binary)
- Modify: `scripts/install.test.sh` (dry-run no-side-effect for binary install + drop-in plan)

- [ ] **Step 1: Add failing tests**

Append before `echo "PASS"`:

```bash
# --- server binary dry-run mentions drop-in target, writes nothing ---
sentinel="$(mktemp -d)"
o="$(bash "$script" server --version 1.0.0 --systemd --advertised-endpoint h.example:7443 --data-dir "$sentinel/data" --dry-run)" || fail "server dry-run"
echo "$o" | grep -q 'drop-in:.*portunus-server.service.d/10-portunus.conf' || fail "drop-in plan line"
[ -z "$(ls -A "$sentinel" 2>/dev/null)" ] || fail "dry-run wrote files"
rm -rf "$sentinel"
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash scripts/install.test.sh`
Expected: FAIL (no `drop-in:` line in plan)

- [ ] **Step 3: Add binary install, systemd, drop-in; extend plan; wire dispatch**

Add after the `# ─── Plan / dry-run ───` section. First extend `print_plan` by inserting, just before its final `echo "actions:"` line:

```bash
  if [ "$ROLE" = "server" ] && [ "${DEPLOY:-binary}" != "docker" ]; then
    echo "drop-in:          /etc/systemd/system/portunus-server.service.d/10-portunus.conf"
    echo "data_dir:         ${DATA_DIR:-/var/lib/portunus}"
    echo "op_http_listen:   ${OP_HTTP_LISTEN:-<default>}"
  fi
```

Then add new sections:

```bash
# ─── Download / install (binary) ──────────────────────────────────────
resolve_latest_tag() {
  need curl; need sed
  tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
  [ -n "$tag" ] || die "could not resolve latest release tag"
  artifact_version="${tag#v}"; resolved_version="$artifact_version"
}

maybe_sudo() { if [ -w "$1" ] || [ "$(id -u)" = "0" ]; then SUDO=""; else SUDO="sudo"; fi; }

install_binary() {
  need curl; need tar
  [ -n "$tag" ] || resolve_latest_tag
  local asset checksums tmp src expected actual
  asset="portunus-${artifact_version}-${target}.tar.gz"
  checksums="portunus-${artifact_version}-checksums.txt"
  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  echo "→ downloading ${asset} (${tag})"
  curl -fsSL "$(rel "$asset")" -o "$tmp/$asset" || die "download failed: $asset"
  curl -fsSL "$(rel "$checksums")" -o "$tmp/$checksums" || die "download failed: $checksums"
  echo "→ verifying sha256"
  expected="$(grep -F " ${asset}" "$tmp/$checksums" | awk '{print $1}')"
  [ -n "$expected" ] || die "no checksum entry for $asset"
  if command -v sha256sum >/dev/null 2>&1; then actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
  else actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"; fi
  [ "$expected" = "$actual" ] || die "checksum mismatch for $asset"
  tar -xzf "$tmp/$asset" -C "$tmp"
  src="$tmp/portunus-${artifact_version}-${target}/portunus-${ROLE}"
  [ -f "$src" ] || die "binary not found in archive: portunus-${ROLE}"
  maybe_sudo "$BIN_DIR"
  echo "→ installing portunus-${ROLE} to ${BIN_DIR}"
  ${SUDO:-} install -m 0755 "$src" "${BIN_DIR}/portunus-${ROLE}"
}

# ─── Systemd ──────────────────────────────────────────────────────────
install_systemd_unit() {
  if [ "$os" != "linux" ] || ! command -v systemctl >/dev/null 2>&1; then
    echo "warning: --systemd ignored (not Linux or systemctl missing)" >&2; return 0
  fi
  local unit tmp; unit="portunus-${ROLE}.service"; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  curl -fsSL "${RAW_BASE}/deploy/systemd/${unit}" -o "$tmp/$unit" || die "unit download failed"
  if [ "$ROLE" = "client" ]; then
    id portunus-client >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin portunus-client
    sudo install -d -o root -g portunus-client -m 0750 /etc/portunus
  else
    id portunus-server >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin portunus-server
    sudo install -d -o portunus-server -g portunus-server -m 0750 "${DATA_DIR:-/var/lib/portunus}"
  fi
  sudo install -m 0644 "$tmp/$unit" "/etc/systemd/system/$unit"
  rm -rf "$tmp"
}

# ─── Server config (drop-in / .env) ───────────────────────────────────
write_server_dropin() {
  local d="/etc/systemd/system/portunus-server.service.d" f
  f="$d/10-portunus.conf"
  sudo install -d -m 0755 "$d"
  {
    echo "[Service]"
    [ -n "$ADVERTISED" ] && echo "Environment=PORTUNUS_ADVERTISED_ENDPOINT=${ADVERTISED}"
  } | sudo tee "$f" >/dev/null
  sudo systemctl daemon-reload || true
  echo "→ wrote $f"
}
```

Replace the temporary `dispatch_verb` stub with:

```bash
dispatch_verb() {
  case "$VERB" in
    install)
      [ -n "$ROLE" ] || die "$(t need_role)"
      [ -z "$DEPLOY" ] && DEPLOY="binary"
      if [ "$DEPLOY" = "docker" ]; then install_docker; else
        install_binary
        [ "$WANT_SYSTEMD" = yes ] && install_systemd_unit
        [ "$ROLE" = "server" ] && [ "$WANT_SYSTEMD" = yes ] && write_server_dropin
      fi
      meta_write "$(meta_path_for)" "role=$ROLE" "deploy=$DEPLOY" "version=$resolved_version" "lang=${LANG_CODE:-en}" "advertised_endpoint_set=$([ -n "$ADVERTISED" ] && echo yes || echo no)"
      echo; echo "$(t done_next)"
      ;;
    uninstall|upgrade|status|service|config|env) lifecycle_"$VERB" ;;
    *) die "verb '${VERB}' not yet implemented" ;;
  esac
}
```

Add stubs above `main "$@"` (replaced in Tasks 5 & 6):

```bash
install_docker() { die "docker install not yet implemented"; }
lifecycle_uninstall() { die "uninstall not yet implemented"; }
lifecycle_upgrade()   { die "upgrade not yet implemented"; }
lifecycle_status()    { die "status not yet implemented"; }
lifecycle_service()   { die "service not yet implemented"; }
lifecycle_config()    { die "config not yet implemented"; }
lifecycle_env()       { die "env not yet implemented"; }
```

- [ ] **Step 4: Run to verify it passes**

Run: `bash scripts/install.test.sh`
Expected: `PASS`

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(install): binary install + systemd unit + server advertised drop-in"
```

---

### Task 5: Docker deploy path (compose + .env sidecar)

**Files:**
- Modify: `scripts/install.sh` (replace `install_docker`, add `Docker` section)
- Modify: `scripts/install.test.sh` (docker dry-run + .env plan, no writes)

- [ ] **Step 1: Add failing tests**

Append before `echo "PASS"`:

```bash
sb="$(mktemp -d)"
o="$(bash "$script" server --deploy docker --compose-dir "$sb" --advertised-endpoint d.example:7443 --version 1.0.0 --dry-run)" || fail "docker dry-run"
echo "$o" | grep -q '^deploy:[[:space:]]*docker$' || fail "docker deploy plan"
echo "$o" | grep -q "compose_dir:.*$sb" || fail "compose_dir plan"
[ -z "$(ls -A "$sb" 2>/dev/null)" ] || fail "docker dry-run wrote files"
rm -rf "$sb"
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash scripts/install.test.sh`
Expected: FAIL (no `compose_dir:` plan line)

- [ ] **Step 3: Implement docker path + plan line**

In `print_plan`, replace the single `echo "deploy: ..."` line with:

```bash
  echo "deploy:           ${DEPLOY:-binary}"
  if [ "${DEPLOY:-binary}" = "docker" ]; then
    echo "compose_dir:      ${COMPOSE_DIR:-$PWD}"
    echo "env_file:         ${COMPOSE_DIR:-$PWD}/.env"
  fi
```

Add a `# ─── Docker ───` section and replace the `install_docker` stub:

```bash
# ─── Docker ───────────────────────────────────────────────────────────
compose_cmd() {
  if docker compose version >/dev/null 2>&1; then echo "docker compose";
  elif command -v docker-compose >/dev/null 2>&1; then echo "docker-compose";
  else die "docker compose v2 (or docker-compose) required"; fi
}

write_compose_env() {
  local dir="$1" f="$1/.env"
  mkdir -p "$dir"
  : > "$f"
  [ -n "$ADVERTISED" ]      && echo "PORTUNUS_ADVERTISED_ENDPOINT=${ADVERTISED}" >> "$f"
  [ -n "$OP_HTTP_LISTEN" ]  && echo "PORTUNUS_OPERATOR_HTTP_LISTEN=${OP_HTTP_LISTEN}" >> "$f"
  echo "→ wrote $f"
}

write_compose_file() {
  local dir="$1" f="$1/compose.yml"
  [ -f "$f" ] && { echo "→ keeping existing $f"; return 0; }
  cat > "$f" <<YAML
services:
  server:
    image: ghcr.io/zingerlittlebee/portunus-${ROLE}:${artifact_version:-latest}
    container_name: portunus-${ROLE}
    env_file: [ .env ]
    ports:
      - "7443:7443"
      - "127.0.0.1:7080:7080"
    volumes:
      - portunus-data:/var/lib/portunus
    restart: unless-stopped
volumes:
  portunus-data:
    name: portunus-data
YAML
  echo "→ wrote $f"
}

install_docker() {
  need docker
  local dir dc; dir="${COMPOSE_DIR:-$PWD}"; dc="$(compose_cmd)"
  [ -n "$tag" ] || resolve_latest_tag
  write_compose_file "$dir"
  write_compose_env "$dir"
  ( cd "$dir" && $dc pull && $dc up -d )
}
```

Note `write_compose_env` / `write_compose_file` / `install_docker` are only reached in non-dry-run (the `main` dry-run branch returns before `dispatch_verb`), so the no-write test holds.

- [ ] **Step 4: Run to verify it passes**

Run: `bash scripts/install.test.sh`
Expected: `PASS`

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(install): docker compose deploy form + .env sidecar"
```

---

### Task 6: Lifecycle subcommands (status/upgrade/service/uninstall/config/env)

**Files:**
- Modify: `scripts/install.sh` (replace the seven `lifecycle_*` stubs, add `Lifecycle` section)
- Modify: `scripts/install.test.sh` (dry-run + config-key validation + purge-token assertions)

- [ ] **Step 1: Add failing tests**

Append before `echo "PASS"`:

```bash
# config rejects unknown key
if bash "$script" config set bogus x --dry-run >/dev/null 2>&1; then fail "bogus config key accepted"; fi
# config accepts a scoped key in dry-run
bash "$script" config get advertised-endpoint --dry-run >/dev/null 2>&1 || fail "config get scoped key"
# uninstall dry-run performs nothing and exits 0
bash "$script" uninstall server --dry-run >/dev/null 2>&1 || fail "uninstall dry-run"
# status dry-run exits 0
bash "$script" status --dry-run >/dev/null 2>&1 || fail "status dry-run"
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash scripts/install.test.sh`
Expected: FAIL (`config` dry-run path currently prints `verb: config` and exits 0 — the unknown-key check is not enforced yet, so `bogus config key accepted` fails)

- [ ] **Step 3: Implement lifecycle + early config-key validation**

In `main`, replace the dry-run block with one that validates config keys even in dry-run:

```bash
  if [ "$DRY_RUN" = "yes" ]; then
    case "$VERB" in
      install) [ -n "$ROLE" ] || die "$(t need_role)"; print_plan; exit 0 ;;
      config) validate_config_key; echo "verb: config ${CONFIG_OP:-get} ${CONFIG_KEY} (dry-run)"; exit 0 ;;
      *) echo "verb: ${VERB} (dry-run; no side effects)"; exit 0 ;;
    esac
  fi
```

Add a `# ─── Lifecycle ───` section and replace all seven stubs:

```bash
# ─── Lifecycle ────────────────────────────────────────────────────────
SCOPED_KEYS="advertised-endpoint data-dir operator-http-listen version-pin"
validate_config_key() {
  [ -n "$CONFIG_KEY" ] || die "config key required (allowed: $SCOPED_KEYS)"
  case " $SCOPED_KEYS " in *" $CONFIG_KEY "*) ;; *) die "$(t unknown_config_key "$CONFIG_KEY")" ;; esac
}

current_meta_file() {
  local f
  for f in "/var/lib/portunus/.install-meta" "/etc/portunus/.install-meta" "${COMPOSE_DIR:-$PWD}/.install-meta"; do
    [ -r "$f" ] && { echo "$f"; return 0; }
  done
  return 1
}

resolved_deploy() {
  local mf; mf="$(current_meta_file 2>/dev/null || true)"
  if [ -n "$mf" ]; then meta_read "$mf" deploy || echo "binary"; else detect_deploy "${COMPOSE_DIR:-}"; fi
}

lifecycle_status() {
  local mf d; mf="$(current_meta_file 2>/dev/null || true)"
  if [ -z "$mf" ]; then echo "$(t no_install_found)"; return 0; fi
  d="$(meta_read "$mf" deploy || echo binary)"
  echo "meta:    $mf"
  echo "role:    $(meta_read "$mf" role || echo '?')"
  echo "deploy:  $d"
  echo "version: $(meta_read "$mf" version || echo '?')"
  echo "advertised_endpoint_set: $(meta_read "$mf" advertised_endpoint_set || echo '?')"
  if [ "$d" = docker ]; then ( cd "$(dirname "$mf")" && $(compose_cmd) ps ) 2>/dev/null || true
  else systemctl is-active "portunus-$(meta_read "$mf" role || echo server)" 2>/dev/null || true; fi
}

lifecycle_service() {
  [ -n "$SERVICE_ACTION" ] || die "service action required: start|stop|restart"
  local d r mf; mf="$(current_meta_file)" || die "$(t no_install_found)"
  d="$(meta_read "$mf" deploy || echo binary)"; r="$(meta_read "$mf" role || echo server)"
  if [ "$d" = docker ]; then
    ( cd "$(dirname "$mf")" && case "$SERVICE_ACTION" in start) $(compose_cmd) up -d ;; stop) $(compose_cmd) down ;; restart) $(compose_cmd) restart ;; esac )
  else sudo systemctl "$SERVICE_ACTION" "portunus-$r"; fi
}

lifecycle_upgrade() {
  local mf cur; mf="$(current_meta_file)" || die "$(t no_install_found)"
  ROLE="$(meta_read "$mf" role || echo server)"; DEPLOY="$(meta_read "$mf" deploy || echo binary)"
  cur="$(meta_read "$mf" version || echo 0)"
  detect_platform; resolve_latest_tag
  if [ "$cur" = "$artifact_version" ]; then echo "$(t upgrade_current "$cur")"; return 0; fi
  confirm "$(t confirm_proceed)" || return 0
  if [ "$DEPLOY" = docker ]; then COMPOSE_DIR="$(dirname "$mf")"; install_docker
  else install_binary; lifecycle_service_restart_quiet; fi
  meta_write "$mf" "role=$ROLE" "deploy=$DEPLOY" "version=$artifact_version" "lang=${LANG_CODE:-en}"
}
lifecycle_service_restart_quiet() { systemctl restart "portunus-${ROLE}" 2>/dev/null || true; }

lifecycle_uninstall() {
  local mf r d; mf="$(current_meta_file)" || die "$(t no_install_found)"
  r="$(meta_read "$mf" role || echo server)"; d="$(meta_read "$mf" deploy || echo binary)"
  if [ "$ASSUME_YES" != yes ]; then confirm "$(t confirm_uninstall "$r" "$d")" || return 0; fi
  if [ "$d" = docker ]; then ( cd "$(dirname "$mf")" && $(compose_cmd) down )
  else
    sudo rm -f "/usr/local/bin/portunus-$r" "/etc/systemd/system/portunus-$r.service"
    sudo rm -f "/etc/systemd/system/portunus-server.service.d/10-portunus.conf"
    sudo systemctl daemon-reload 2>/dev/null || true
  fi
  if [ "$PURGE" = yes ]; then
    local dd ans; dd="$(dirname "$mf")"
    read -r -p "$(t confirm_purge_typed "$dd")" ans < <(tty_in) || ans=""
    [ "$ans" = "purge" ] && { sudo rm -rf "$dd"; echo "→ purged $dd"; } || echo "purge skipped"
  fi
  rm -f "$mf" 2>/dev/null || true
}

lifecycle_config() {
  validate_config_key
  local mf; mf="$(current_meta_file)" || die "$(t no_install_found)"
  local d; d="$(meta_read "$mf" deploy || echo binary)"
  local target_file
  if [ "$d" = docker ]; then target_file="$(dirname "$mf")/.env"; else target_file="/etc/systemd/system/portunus-server.service.d/10-portunus.conf"; fi
  local envkey
  case "$CONFIG_KEY" in
    advertised-endpoint) envkey="PORTUNUS_ADVERTISED_ENDPOINT" ;;
    operator-http-listen) envkey="PORTUNUS_OPERATOR_HTTP_LISTEN" ;;
    data-dir) envkey="PORTUNUS_DATA_DIR" ;;
    version-pin) envkey="PORTUNUS_VERSION_PIN" ;;
  esac
  if [ "${CONFIG_OP:-get}" = get ]; then
    grep -E "(Environment=)?${envkey}=" "$target_file" 2>/dev/null | sed "s/.*${envkey}=//" || echo "<unset>"
    return 0
  fi
  [ -n "$CONFIG_VALUE" ] || die "config set needs a value"
  if [ "$d" = docker ]; then
    grep -v "^${envkey}=" "$target_file" 2>/dev/null > "$target_file.tmp" || true
    echo "${envkey}=${CONFIG_VALUE}" >> "$target_file.tmp"; mv "$target_file.tmp" "$target_file"
  else
    sudo install -d -m 0755 "$(dirname "$target_file")"
    { echo "[Service]"; echo "Environment=${envkey}=${CONFIG_VALUE}"; } | sudo tee "$target_file" >/dev/null
    sudo systemctl daemon-reload 2>/dev/null || true
  fi
  echo "→ set ${CONFIG_KEY}=${CONFIG_VALUE}"
  if confirm "$(t restart_now)"; then SERVICE_ACTION=restart; lifecycle_service; fi
}

lifecycle_env() { CONFIG_OP="get"; for CONFIG_KEY in advertised-endpoint operator-http-listen data-dir version-pin; do printf '%s=' "$CONFIG_KEY"; lifecycle_config; done; }

tty_in() { if [ -t 0 ]; then cat; else cat /dev/tty; fi; }
confirm() {
  local prompt="$1" ans
  [ "$ASSUME_YES" = yes ] && return 0
  read -r -p "$prompt" ans < <(tty_in) || return 1
  case "$ans" in y|Y|yes|YES|"") return 0 ;; *) return 1 ;; esac
}
```

(Note: `lifecycle_env` reuses `lifecycle_config`; the `printf '%s='` prefix plus `validate_config_key` keeps keys scoped. `confirm` default-yes matches the `[Y/n]` prompts; uninstall uses `[y/N]` so its caller checks the return explicitly.)

- [ ] **Step 4: Run to verify it passes**

Run: `bash scripts/install.test.sh`
Expected: `PASS`

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(install): lifecycle subcommands + scoped config/env + purge guard"
```

---

### Task 7: Interactive menu + install wizard

**Files:**
- Modify: `scripts/install.sh` (replace `run_menu` stub, add `Interactive` section)
- Modify: `scripts/install.test.sh` (drive the menu via a fed fd)

- [ ] **Step 1: Add failing test**

Append before `echo "PASS"`:

```bash
# Feed "0" (Exit) to the menu via stdin acting as the tty seam.
out="$(printf '0\n' | PORTUNUS_LANG=en bash "$script" --menu-stdin 2>&1)" || true
echo "$out" | grep -qi 'Portunus Manager' || fail "menu title not shown"
```

- [ ] **Step 2: Run to verify it fails**

Run: `bash scripts/install.test.sh`
Expected: FAIL `unknown argument: --menu-stdin`

- [ ] **Step 3: Implement the menu + wizard**

Add a `# ─── Interactive ───` section and replace the `run_menu` stub:

```bash
# ─── Interactive ──────────────────────────────────────────────────────
MENU_FORCE_STDIN="no"
ask() { # ask <prompt-msg-key> [printf-args...] ; echoes the answer
  local p; p="$(t "$@")"; local a
  if [ "$MENU_FORCE_STDIN" = yes ] || [ -t 0 ]; then read -r -p "$p" a || a=""
  else read -r -p "$p" a < /dev/tty || a=""; fi
  printf '%s' "$a"
}

first_run_lang() {
  [ -n "$LANG_CODE" ] && return 0
  local a; a="$(ask lang_prompt)"
  case "$a" in 2) LANG_CODE=zh ;; *) LANG_CODE=en ;; esac
}

wizard_install() {
  local a
  a="$(ask ask_role)"; case "$a" in 2) ROLE=client ;; *) ROLE=server ;; esac
  a="$(ask ask_deploy)"; case "$a" in 2) DEPLOY=docker ;; *) DEPLOY=binary; WANT_SYSTEMD=yes ;; esac
  VERSION="$(ask ask_version)"
  if [ "$DEPLOY" = binary ]; then BIN_DIR="$(ask ask_bindir "$DEFAULT_BIN_DIR")"; [ -z "$BIN_DIR" ] && BIN_DIR="$DEFAULT_BIN_DIR"; fi
  if [ "$ROLE" = server ]; then
    ADVERTISED="$(ask ask_advertised "$DOCS_FEATURE_URL")"
    DATA_DIR="$(ask ask_datadir)"
    OP_HTTP_LISTEN="$(ask ask_ophttp)"
  fi
  detect_platform; resolve_version_static
  print_plan
  confirm "$(t confirm_proceed)" || { echo "aborted"; return 1; }
  VERB=install; dispatch_verb
}

run_menu() {
  first_run_lang
  while :; do
    echo; echo "$(t menu_title)"
    echo "$(t menu_install)"; echo "$(t menu_uninstall)"; echo "$(t menu_upgrade)"
    echo "$(t menu_status)"; echo "$(t menu_service)"; echo "$(t menu_config)"
    echo "$(t menu_env)"; echo "$(t menu_exit)"
    local c; c="$(ask menu_select)"
    case "$c" in
      1) wizard_install || true ;;
      2) VERB=uninstall; lifecycle_uninstall || true ;;
      3) VERB=upgrade; lifecycle_upgrade || true ;;
      4) VERB=status; lifecycle_status || true ;;
      5) SERVICE_ACTION="$(ask menu_select)"; VERB=service; lifecycle_service || true ;;
      6) CONFIG_OP=set; CONFIG_KEY="$(ask ask_advertised "$DOCS_FEATURE_URL")"; lifecycle_config || true ;;
      7) VERB=env; lifecycle_env || true ;;
      0|q|Q) return 0 ;;
      *) ;;
    esac
  done
}
```

Add to `parse_args` before the `*)` arm:

```bash
      --menu-stdin) MENU_FORCE_STDIN="yes"; resolve_lang; run_menu; exit $? ;;
```

(`--menu-stdin` is the test seam: forces menu input from stdin instead of `/dev/tty`.)

- [ ] **Step 4: Run to verify it passes**

Run: `bash scripts/install.test.sh`
Expected: `PASS`

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(install): interactive menu + guided install wizard"
```

---

### Task 8: shellcheck gate + docs

**Files:**
- Modify: `scripts/install.test.sh` (append shellcheck gate)
- Modify: `docs/content/docs/getting-started/installation.mdx`
- Modify: `docs/content/docs/zh/getting-started/installation.mdx`
- Modify: `docs/content/docs/deployment/docker.mdx`
- Modify: `docs/content/docs/deployment/railway.mdx`

- [ ] **Step 1: Add the shellcheck gate**

Append before `echo "PASS"` in `scripts/install.test.sh`:

```bash
# --- shellcheck (skipped if not installed, but must pass if present) ---
if command -v shellcheck >/dev/null 2>&1; then
  shellcheck -s bash -S warning "$script" || fail "shellcheck warnings"
else
  echo "note: shellcheck not installed; skipping lint gate" >&2
fi
```

- [ ] **Step 2: Run to verify it fails (or surfaces real warnings)**

Run: `bash scripts/install.test.sh`
Expected: FAIL with concrete shellcheck warnings (fix each: add `# shellcheck disable=` only where intentional, e.g. the `printf "$val"` in `t`; quote expansions; use `${var}`). Re-run until `PASS`.

- [ ] **Step 3: Update the English installation doc**

In `docs/content/docs/getting-started/installation.mdx`, replace every `| sh -s --` with `| bash -s --` and every `| sudo sh -s --` with `| sudo bash -s --`. Then, immediately after the existing advertised-endpoint note added previously (the paragraph starting `**Deploying behind a proxy / on a cloud host?**`), add:

```mdx
### Interactive manager

Run the installer with no arguments for a guided menu (works piped too):

```sh
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash
```

It offers **Install / Uninstall / Upgrade / Status / Service / Config /
Env** for both binary+systemd and Docker Compose deployments, and the
server install prompts for the advertised endpoint (persisted to a
systemd drop-in or compose `.env`). Non-interactive flags are unchanged
for CI/automation; `--dry-run` still performs no network or writes.
Requires bash 4+.
```

- [ ] **Step 4: Update the Chinese installation doc**

In `docs/content/docs/zh/getting-started/installation.mdx`, apply the same `| sh`→`| bash` / `| sudo sh`→`| sudo bash` replacements, and after the existing zh advertised-endpoint note (`**部署在反向代理 / 云主机后面？**`) add:

```mdx
### 交互式管理器

无参数运行安装脚本进入向导菜单（管道方式也可用）：

```sh
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash
```

提供 **安装 / 卸载 / 升级 / 状态 / 服务 / 配置 / 环境变量**，覆盖
二进制+systemd 与 Docker Compose；server 安装会提示通告地址（持久化到
systemd drop-in 或 compose `.env`）。非交互 flag 不变，供 CI/自动化使用；
`--dry-run` 仍不联网、不写文件。需要 bash 4+。
```

- [ ] **Step 5: Cross-link deployment docs**

In `docs/content/docs/deployment/docker.mdx`, append a short paragraph at the end of the first section:

```mdx
> The interactive installer (`curl … | bash`) can manage a Docker
> Compose deployment end to end — see
> [Installation → Interactive manager](/en/docs/getting-started/installation).
```

In `docs/content/docs/deployment/railway.mdx`, append:

```mdx
> On Railway the gRPC host differs from the local bind; set the
> advertised endpoint during install (the wizard prompts for it) or via
> `Config` later — see
> [Advertised Endpoint](/en/docs/features/advertised-endpoint).
```

- [ ] **Step 6: Verify + commit**

Run: `bash scripts/install.test.sh`
Expected: `PASS`

```bash
git add scripts/install.sh scripts/install.test.sh docs/content/docs/getting-started/installation.mdx docs/content/docs/zh/getting-started/installation.mdx docs/content/docs/deployment/docker.mdx docs/content/docs/deployment/railway.mdx
git commit -m "feat(install): shellcheck gate; document interactive manager (en/zh)"
```

---

### Task 9: Final sweep + spec status

**Files:**
- Modify: `docs/superpowers/specs/2026-05-17-interactive-install-design.md:3` (Status line)

- [ ] **Step 1: Full test + lint**

Run: `bash scripts/install.test.sh`
Expected: `PASS`

Run: `shellcheck -s bash -S warning scripts/install.sh scripts/install.test.sh`
Expected: clean (or only intentional, annotated `disable`s).

- [ ] **Step 2: Spec-coverage checklist**

Confirm each spec section maps to a task; fix gaps before closing:
- §1 mode detection → Task 2 (`is_interactive`, `/dev/tty`).
- §2 CLI surface + back-compat → Task 2 (`parse_args`), Task 0 (legacy flags).
- §3 meta + detection → Task 3.
- §4 install flows (binary/docker, drop-in/.env, dry-run pre-network, idempotent) → Tasks 4, 5; idempotent re-install note: re-running `install` overwrites meta — acceptable (documented here as accepted behavior; full repair-prompt is out of scope per spec "offer upgrade/repair" is satisfied by `upgrade`).
- §5 lifecycle subcommands → Task 6.
- §6 i18n → Task 1.
- §7 safety (bash guard, dry-run, purge token, no unit edit) → Tasks 0, 4, 6.
- §8 testing & docs → Tasks 0–8.
- Non-goals respected (no Caddy/HTTPS, no generic kv, no Windows) → not implemented anywhere (correct).

- [ ] **Step 3: Mark the spec implemented**

In `docs/superpowers/specs/2026-05-17-interactive-install-design.md`, change line 3 from `> Status: Draft` to `> Status: Implemented`.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/specs/2026-05-17-interactive-install-design.md
git commit -m "docs: mark interactive-install spec implemented"
```

---

## Self-Review Notes (author)

- **Spec coverage:** every spec §1–§8 + non-goals maps to a task (see Task 9 Step 2). The spec's "idempotent re-install offers upgrade/repair" is partially satisfied: `upgrade` exists; blind `install` re-run overwrites meta — explicitly accepted in Task 9 to avoid over-building a repair state machine (YAGNI), consistent with the spec's primary intent.
- **Type/name consistency:** globals (`VERB ROLE DEPLOY VERSION BIN_DIR COMPOSE_DIR WANT_SYSTEMD ADVERTISED DATA_DIR OP_HTTP_LISTEN SERVICE_ACTION CONFIG_OP CONFIG_KEY CONFIG_VALUE ASSUME_YES PURGE DRY_RUN LANG_CODE`) are declared once in Task 0 and used identically thereafter. Functions referenced before definition (`run_menu`, `dispatch_verb`, `install_docker`, `lifecycle_*`) are introduced as stubs in the task that first calls them and replaced in the owning task — no forward reference is undefined at any commit.
- **Test seams** (`--print-i18n`, `--print-i18n-keys`, `--meta-read/-write`, `--detect-deploy`, `--menu-stdin`) all `exit` before side effects and are documented as test-only; they keep the harness network-free.
- **`--dry-run` invariant:** the `main` dry-run branch returns before `dispatch_verb`, so no install/lifecycle function performs writes under `--dry-run`; Tasks 4 and 5 assert no files are written.
- **Placeholder scan:** no TBD/TODO; every code step contains complete bash.
