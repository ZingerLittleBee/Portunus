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
  *) [ -r "${BASH_SOURCE[0]}" ] && SELF_SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd)/$(basename "${BASH_SOURCE[0]}")" || true ;;
esac

# ─── Constants ────────────────────────────────────────────────────────
REPO="ZingerLittleBee/Portunus"
RAW_BASE="https://raw.githubusercontent.com/${REPO}/main"
DEFAULT_BIN_DIR="/usr/local/bin"
LANG_CACHE="${XDG_CONFIG_HOME:-$HOME/.config}/portunus/installer-lang"

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
DETECTED_IP=""        # last detect_public_ip() result
DETECTED_PROV=""      # provenance i18n key: prov_detected|prov_nic|prov_loopback

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }

# Single script-level cleanup: a RETURN trap would re-fire on every later
# function return (where the local tmp is out of scope ⇒ set -u abort).
CLEANUP_DIRS=()
_cleanup() { local d; for d in "${CLEANUP_DIRS[@]:-}"; do [ -n "$d" ] && rm -rf "$d"; done; return 0; }
trap _cleanup EXIT
# Register from the caller's own shell — a helper used via $(...) would
# push to CLEANUP_DIRS in a subshell and the entry would be lost.
track_tmp() { CLEANUP_DIRS+=("$1"); }

# ─── i18n ─────────────────────────────────────────────────────────────
# Convention: every '%' in a message value MUST be a '%s' directive with a matching t() arg (values are used as printf format strings).
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
  [lang_prompt]="Select language\n  [1] English\n  [2] 中文"
  [ask_role]="Install which role?\n  [1] server\n  [2] client"
  [ask_deploy_server]="Deploy form? (Enter = recommended)\n  [1] docker compose  (recommended)\n  [2] binary + systemd"
  [ask_deploy_client]="Deploy form? (Enter = recommended)\n  [1] binary + systemd  (recommended)\n  [2] docker compose"
  [ask_version]="Version (blank = latest): "
  [ask_bindir]="Install dir [%s]: "
  [ask_datadir]="Server data dir (blank = default): "
  [ask_ophttp]="Operator HTTP listen (blank = default): "
  [confirm_proceed]="Proceed? [Y/n]: "
  [confirm_uninstall]="Uninstall portunus-%s (%s)? [y/N]: "
  [confirm_purge_typed]="Type 'purge' to also delete data at %s: "
  [need_role]="role required: client or server"
  [no_install_found]="No Portunus install detected (no .install-meta and no probe match)."
  [done_next]="Done. Next steps:"
  [next_systemd]="  start:   sudo systemctl enable --now portunus-%s"
  [next_docker]="  manage:  (cd %s && docker compose ps)"
  [next_status]="  status:  install.sh status"
  [restart_now]="Apply now (restart service)? [y/N]: "
  [upgrade_current]="Already at %s; nothing to upgrade."
  [unknown_config_key]="unknown config key: %s (allowed: advertised-endpoint data-dir operator-http-listen version-pin)"
  [ask_config_key]="Config key\n  [1] advertised-endpoint\n  [2] data-dir\n  [3] operator-http-listen\n  [4] version-pin"
  [ask_config_value]="New value for %s: "
  [ask_service_action]="Service action\n  [1] start\n  [2] stop\n  [3] restart"
  [menu_invalid]="invalid option: %s"
  [press_enter]="Press Enter to continue…"
  [bad_endpoint]="invalid host:port '%s' — expected like host.example:7443 (blank = auto)"
  [op_cancelled]="cancelled."
  [ask_advertised_pub]="Public advertised endpoint [%s] (Enter=accept, '-' = none/loopback): "
  [summary_title]="About to install:"
  [sum_role]="  role:                 %s"
  [sum_deploy]="  deploy:               %s"
  [sum_version]="  version:              %s"
  [sum_bindir]="  bin dir:              %s"
  [sum_datadir]="  data dir:             %s"
  [sum_ophttp]="  operator http:        %s"
  [sum_compose]="  compose dir:          %s"
  [sum_advertised]="  advertised endpoint:  %s"
  [prov_detected]="(detected public IP)"
  [prov_nic]="(local NIC)"
  [prov_loopback]="(loopback — local only)"
  [prov_user]="(you entered)"
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
  [lang_prompt]="选择语言\n  [1] English\n  [2] 中文"
  [ask_role]="安装哪个角色?\n  [1] server\n  [2] client"
  [ask_deploy_server]="部署方式? (回车=推荐)\n  [1] docker compose  (推荐)\n  [2] 二进制 + systemd"
  [ask_deploy_client]="部署方式? (回车=推荐)\n  [1] 二进制 + systemd  (推荐)\n  [2] docker compose"
  [ask_version]="版本 (留空=最新): "
  [ask_bindir]="安装目录 [%s]: "
  [ask_datadir]="服务端 data 目录 (留空=默认): "
  [ask_ophttp]="Operator HTTP 监听 (留空=默认): "
  [confirm_proceed]="继续? [Y/n]: "
  [confirm_uninstall]="卸载 portunus-%s (%s)? [y/N]: "
  [confirm_purge_typed]="输入 'purge' 以同时删除数据 %s: "
  [need_role]="必须指定角色: client 或 server"
  [no_install_found]="未检测到 Portunus 安装 (无 .install-meta 且探测未命中)。"
  [done_next]="完成。后续步骤:"
  [next_systemd]="  启动:   sudo systemctl enable --now portunus-%s"
  [next_docker]="  管理:   (cd %s && docker compose ps)"
  [next_status]="  状态:   install.sh status"
  [restart_now]="现在生效 (重启服务)? [y/N]: "
  [upgrade_current]="已是 %s; 无需升级。"
  [unknown_config_key]="未知配置键: %s (允许: advertised-endpoint data-dir operator-http-listen version-pin)"
  [ask_config_key]="配置键\n  [1] advertised-endpoint\n  [2] data-dir\n  [3] operator-http-listen\n  [4] version-pin"
  [ask_config_value]="%s 的新值: "
  [ask_service_action]="服务操作\n  [1] 启动\n  [2] 停止\n  [3] 重启"
  [menu_invalid]="无效选项: %s"
  [press_enter]="按回车继续…"
  [bad_endpoint]="无效 host:port '%s' — 形如 host.example:7443 (留空=自动)"
  [op_cancelled]="已取消。"
  [ask_advertised_pub]="公网通告地址 [%s] (回车=接受, '-' = 不设/回环): "
  [summary_title]="即将安装:"
  [sum_role]="  角色:                 %s"
  [sum_deploy]="  部署:                 %s"
  [sum_version]="  版本:                 %s"
  [sum_bindir]="  bin 目录:             %s"
  [sum_datadir]="  data 目录:            %s"
  [sum_ophttp]="  operator http:        %s"
  [sum_compose]="  compose 目录:         %s"
  [sum_advertised]="  通告地址:             %s"
  [prov_detected]="(探测到的公网 IP)"
  [prov_nic]="(本地网卡)"
  [prov_loopback]="(回环 — 仅本机)"
  [prov_user]="(手动输入)"
)

resolve_lang() {
  if [ -z "$LANG_CODE" ]; then
    case "${LC_ALL:-${LANG:-}}" in zh*|*zh_*) LANG_CODE="zh" ;; *) LANG_CODE="" ;; esac
  fi
  # Reuse a prior interactive choice so the menu doesn't re-ask each run.
  if [ -z "$LANG_CODE" ] && [ -r "$LANG_CACHE" ]; then
    case "$(cat "$LANG_CACHE" 2>/dev/null)" in zh) LANG_CODE=zh ;; en) LANG_CODE=en ;; esac
  fi
  case "$LANG_CODE" in zh|en) ;; *) LANG_CODE="" ;; esac
}

t() {
  local key="${1:-}"; shift || true
  local val
  if [ "${LANG_CODE:-en}" = "zh" ]; then val="${MSG_ZH[$key]:-}"; else val="${MSG_EN[$key]:-}"; fi
  [ -n "$val" ] || val="${MSG_EN[$key]:-$key}"
  # shellcheck disable=SC2059
  printf "$val" "$@"
}

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
  if [ "${DEPLOY:-binary}" = "docker" ]; then
    echo "compose_dir:      ${COMPOSE_DIR:-$PWD}"
    echo "env_file:         ${COMPOSE_DIR:-$PWD}/.env"
  fi
  echo "bin_dir:          ${BIN_DIR}"
  echo "systemd:          ${WANT_SYSTEMD}"
  echo "advertised:       ${ADVERTISED:-<unset, runtime auto>}"
  if [ "$ROLE" = "server" ] && [ "${DEPLOY:-binary}" != "docker" ]; then
    echo "drop-in:          /etc/systemd/system/portunus-server.service.d/10-portunus.conf"
    echo "data_dir:         ${DATA_DIR:-/var/lib/portunus}"
    echo "op_http_listen:   ${OP_HTTP_LISTEN:-<default>}"
  fi
  echo "actions:          download+verify+install portunus-${ROLE} -> ${BIN_DIR}$( [ "$WANT_SYSTEMD" = yes ] && echo ' + systemd unit' )"
}

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
  tmp="$(mktemp -d)"; track_tmp "$tmp"
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
  local unit tmp; unit="portunus-${ROLE}.service"; tmp="$(mktemp -d)"; track_tmp "$tmp"
  curl -fsSL "${RAW_BASE}/deploy/systemd/${unit}" -o "$tmp/$unit" || die "unit download failed"
  if [ "$ROLE" = "client" ]; then
    id portunus-client >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin portunus-client
    sudo install -d -o root -g portunus-client -m 0750 /etc/portunus
  else
    id portunus-server >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin portunus-server
    sudo install -d -o portunus-server -g portunus-server -m 0750 "${DATA_DIR:-/var/lib/portunus}"
  fi
  sudo install -m 0644 "$tmp/$unit" "/etc/systemd/system/$unit"
}

# ─── Server config (drop-in / .env) ───────────────────────────────────
# Pure: emit the drop-in body for the currently-set scoped values.
# Mirrors the `Environment=` lines `config set` writes so install-time
# --data-dir / --operator-http-listen persist identically (parity).
render_dropin() {
  echo "[Service]"
  [ -n "$ADVERTISED" ]     && echo "Environment=PORTUNUS_ADVERTISED_ENDPOINT=${ADVERTISED}"
  [ -n "$DATA_DIR" ]       && echo "Environment=PORTUNUS_DATA_DIR=${DATA_DIR}"
  [ -n "$OP_HTTP_LISTEN" ] && echo "Environment=PORTUNUS_OPERATOR_HTTP_LISTEN=${OP_HTTP_LISTEN}"
}
write_server_dropin() {
  local d="/etc/systemd/system/portunus-server.service.d" f
  f="$d/10-portunus.conf"
  sudo install -d -m 0755 "$d"
  render_dropin | sudo tee "$f" >/dev/null
  sudo systemctl daemon-reload || true
  echo "→ wrote $f"
}

# Actionable post-install hints (the installer never auto-starts).
print_next_steps() {
  echo "$(t done_next)"
  if [ "${DEPLOY:-binary}" = "docker" ]; then
    t next_docker "${COMPOSE_DIR:-$PWD}"; echo
  else
    t next_systemd "$ROLE"; echo
  fi
  t next_status; echo
}

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
  # Record the deploy BEFORE pull/up: a port conflict that fails
  # `up -d` must not leave compose files on disk with no .install-meta.
  meta_write "$dir/.install-meta" "role=$ROLE" "deploy=docker" \
    "version=${artifact_version:-$resolved_version}" "lang=${LANG_CODE:-en}" \
    "advertised_endpoint_set=$([ -n "$ADVERTISED" ] && echo yes || echo no)"
  ( cd "$dir" && $dc pull && $dc up -d )
}

# ─── Arg parse + dispatch (minimal; expanded in Task 2) ───────────────
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
      -h|--help) echo "usage: install.sh <client|server|install|uninstall|upgrade|status|service|config|env> [start|stop|restart] [get|set key [value]] [--version V] [--deploy binary|docker] [--bin-dir D] [--compose-dir D] [--advertised-endpoint H:P] [--data-dir D] [--operator-http-listen A] [--systemd] [--lang en|zh] [--reset-lang] [--yes] [--purge] [--dry-run]"; exit 0 ;;
      --meta-write) shift; f="$1"; shift; meta_write "$f" "$@"; exit 0 ;;
      --meta-read) shift; f="$1"; k="$2"; meta_read "$f" "$k"; exit $? ;;
      --detect-deploy) shift; detect_deploy "${1:-}"; exit 0 ;;
      --detect-ip) detect_public_ip; printf '%s %s\n' "$DETECTED_IP" "$DETECTED_PROV"; exit 0 ;;
      --reset-lang) rm -f "$LANG_CACHE" 2>/dev/null || true; echo "language preference reset ($LANG_CACHE); next interactive run will ask again"; exit 0 ;;
      --render-dropin) render_dropin; exit 0 ;;
      --valid-endpoint) shift; valid_host_port "${1:-}" && exit 0 || exit 1 ;;
      --resolve-meta) current_meta_file && exit 0 || exit 1 ;;
      --menu-stdin) MENU_FORCE_STDIN="yes" ;;  # defer to main so later --compose-dir et al. still parse
      *) if [ "$VERB" = config ] && [ -z "$CONFIG_KEY" ]; then CONFIG_KEY="$1"; elif [ "$VERB" = config ] && [ -z "$CONFIG_VALUE" ]; then CONFIG_VALUE="$1"; else die "unknown argument: $1"; fi ;;
    esac
    shift
  done
}

is_interactive() {
  [ "${MENU_FORCE_STDIN:-no}" = yes ] && return 0   # scripted-menu seam forces the menu
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
    case "$VERB" in
      install) [ -n "$ROLE" ] || die "$(t need_role)"; print_plan; exit 0 ;;
      config) validate_config_key; echo "verb: config ${CONFIG_OP:-get} ${CONFIG_KEY} (dry-run)"; exit 0 ;;
      *) echo "verb: ${VERB} (dry-run; no side effects)"; exit 0 ;;
    esac
  fi
  dispatch_verb
}

# ─── Interactive ──────────────────────────────────────────────────────
MENU_FORCE_STDIN="no"
ask() { # ask <prompt-msg-key> [printf-args...] ; echoes the answer
  local p; p="$(t "$@")"; local a
  printf '%s\n' "$p" >&2          # question on its own line; answer below
  if [ "$MENU_FORCE_STDIN" = yes ] || [ -t 0 ]; then read -r -p "> " a || a=""
  else read -r -p "> " a < /dev/tty 2>/dev/null || a=""; fi
  printf '%s' "$a"
}

first_run_lang() {
  [ -n "$LANG_CODE" ] && return 0
  local a; a="$(ask lang_prompt)"
  case "$a" in 2) LANG_CODE=zh ;; *) LANG_CODE=en ;; esac
  mkdir -p "$(dirname "$LANG_CACHE")" 2>/dev/null \
    && printf '%s' "$LANG_CODE" > "$LANG_CACHE" 2>/dev/null || true
}

# Seed the advertised-endpoint default. Public probe → local NIC →
# loopback. Sets DETECTED_IP + DETECTED_PROV (an i18n key). Never fatal.
# PORTUNUS_SKIP_IP_PROBE=1 skips the external probe (offline/test/CI).
valid_ip() { case "$1" in ""|*[!0-9a-fA-F.:]*) return 1 ;; *[.:]*) return 0 ;; *) return 1 ;; esac; }
detect_public_ip() {
  [ -n "$DETECTED_IP" ] && return 0
  local ip=""
  if [ "${PORTUNUS_SKIP_IP_PROBE:-0}" != 1 ] && command -v curl >/dev/null 2>&1; then
    local u
    for u in https://api.ipify.org https://ifconfig.me/ip https://icanhazip.com; do
      ip="$(curl -fsS --max-time 3 "$u" 2>/dev/null | tr -d '[:space:]' || true)"
      if valid_ip "$ip"; then DETECTED_IP="$ip"; DETECTED_PROV="prov_detected"; return 0; fi
    done
  fi
  ip=""
  if command -v ip >/dev/null 2>&1; then
    ip="$(ip route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -1 || true)"
  fi
  if [ -z "$ip" ] && command -v hostname >/dev/null 2>&1; then
    ip="$(hostname -I 2>/dev/null | tr ' ' '\n' | grep -v '^127\.' | head -1 || true)"
  fi
  if valid_ip "$ip"; then DETECTED_IP="$ip"; DETECTED_PROV="prov_nic"; return 0; fi
  DETECTED_IP="127.0.0.1"; DETECTED_PROV="prov_loopback"; return 0
}

# Print every effective install value before the final confirm.
print_install_summary() {
  local adv_prov="$1"   # i18n key for advertised provenance, or ""
  echo "$(t summary_title)"
  t sum_role "$ROLE"; echo
  if [ "$DEPLOY" = docker ]; then
    t sum_deploy "docker"; echo
  else
    t sum_deploy "binary + systemd"; echo
  fi
  t sum_version "${VERSION:-latest (resolved at run time)}"; echo
  if [ "$DEPLOY" = docker ]; then
    t sum_compose "${COMPOSE_DIR:-$PWD}"; echo
  else
    t sum_bindir "${BIN_DIR:-$DEFAULT_BIN_DIR}"; echo
  fi
  if [ "$ROLE" = server ]; then
    t sum_datadir "${DATA_DIR:-/var/lib/portunus}"; echo
    t sum_ophttp "${OP_HTTP_LISTEN:-127.0.0.1:7080}"; echo
    if [ -n "$ADVERTISED" ]; then
      t sum_advertised "$ADVERTISED $([ -n "$adv_prov" ] && t "$adv_prov")"; echo
    else
      t sum_advertised "$(t prov_loopback)"; echo
    fi
  fi
}

wizard_install() {
  local a adv_prov=""
  a="$(ask ask_role)"; case "$a" in 2) ROLE=client ;; *) ROLE=server ;; esac
  # Recommended deploy form differs by role: server ⇒ docker compose,
  # client ⇒ binary. Enter (empty) accepts the recommended one.
  if [ "$ROLE" = server ]; then
    a="$(ask ask_deploy_server)"
    case "$a" in 2|binary) DEPLOY=binary; WANT_SYSTEMD=yes ;; *) DEPLOY=docker ;; esac
  else
    a="$(ask ask_deploy_client)"
    case "$a" in 2|docker) DEPLOY=docker ;; *) DEPLOY=binary; WANT_SYSTEMD=yes ;; esac
  fi
  if [ "$ROLE" = server ]; then
    detect_public_ip
    adv_prov="$DETECTED_PROV"
    while :; do
      a="$(ask ask_advertised_pub "${DETECTED_IP}:7443")"
      if [ -z "$a" ]; then ADVERTISED="${DETECTED_IP}:7443"; break; fi
      if [ "$a" = "-" ]; then ADVERTISED=""; adv_prov="prov_loopback"; break; fi
      if valid_host_port "$a"; then ADVERTISED="$a"; adv_prov="prov_user"; break; fi
      t bad_endpoint "$a"; echo
    done
  fi
  detect_platform; resolve_version_static
  print_install_summary "$adv_prov"
  confirm "$(t confirm_proceed)" || { echo "$(t op_cancelled)"; return 1; }
  VERB=install; dispatch_verb
}

# An explicit CLI --compose-dir scopes the whole menu session; it must
# survive the per-iteration reset (wizard-collected fields do not).
MENU_COMPOSE_DIR=""
reset_menu_state() {
  ROLE=""; DEPLOY=""; VERSION=""; BIN_DIR="$DEFAULT_BIN_DIR"
  COMPOSE_DIR="$MENU_COMPOSE_DIR"
  WANT_SYSTEMD="no"; ADVERTISED=""; DATA_DIR=""; OP_HTTP_LISTEN=""
  SERVICE_ACTION=""; CONFIG_OP=""; CONFIG_KEY=""; CONFIG_VALUE=""; VERB=""
}

pause() {
  [ "$MENU_FORCE_STDIN" = yes ] && return 0   # scripted seam: never block
  read_tty "$(t press_enter)" || true
}

menu_service() {
  local a
  a="$(ask ask_service_action)"
  case "$a" in
    1|start)   SERVICE_ACTION="start" ;;
    2|stop)    SERVICE_ACTION="stop" ;;
    3|restart) SERVICE_ACTION="restart" ;;
    *) t menu_invalid "$a"; echo; return 0 ;;
  esac
  VERB="service"; lifecycle_service
}

menu_config() {
  local a k v
  a="$(ask ask_config_key)"
  case "$a" in
    1|advertised-endpoint)  k="advertised-endpoint" ;;
    2|data-dir)             k="data-dir" ;;
    3|operator-http-listen) k="operator-http-listen" ;;
    4|version-pin)          k="version-pin" ;;
    *) t unknown_config_key "$a"; echo; return 0 ;;
  esac
  v="$(ask ask_config_value "$k")"
  [ -n "$v" ] || { echo "$(t op_cancelled)"; return 0; }
  if [ "$k" = "advertised-endpoint" ] && ! valid_host_port "$v"; then
    t bad_endpoint "$v"; echo; return 0
  fi
  CONFIG_OP="set"; CONFIG_KEY="$k"; CONFIG_VALUE="$v"; VERB="config"; lifecycle_config
}

# Returns 2 to request menu exit; any other status keeps the loop alive.
menu_handle() {
  case "$1" in
    1) wizard_install ;;
    2) VERB="uninstall"; lifecycle_uninstall ;;
    3) VERB="upgrade";   lifecycle_upgrade ;;
    4) VERB="status";    lifecycle_status ;;
    5) menu_service ;;
    6) menu_config ;;
    7) VERB="env";       lifecycle_env ;;
    0|q|Q) return 2 ;;
    *) t menu_invalid "$1"; echo; return 0 ;;
  esac
}

run_menu() {
  first_run_lang
  MENU_COMPOSE_DIR="${COMPOSE_DIR:-}"
  while :; do
    reset_menu_state
    echo; echo "$(t menu_title)"
    echo "$(t menu_install)"; echo "$(t menu_uninstall)"; echo "$(t menu_upgrade)"
    echo "$(t menu_status)"; echo "$(t menu_service)"; echo "$(t menu_config)"
    echo "$(t menu_env)"; echo "$(t menu_exit)"
    local c
    printf '%s\n' "$(t menu_select)" >&2     # prompt on its own line; answer below
    if [ "$MENU_FORCE_STDIN" = yes ] || [ -t 0 ]; then read -r -p "> " c || return 0
    else read -r -p "> " c < /dev/tty || return 0; fi
    case "$c" in 0|q|Q) return 0 ;; esac
    # Subshell isolation: a die()/exit inside any lifecycle path ends
    # only this action, never the whole interactive session.
    local rc=0
    ( menu_handle "$c" ) || rc=$?
    [ "$rc" -eq 2 ] && return 0
    pause
  done
}
dispatch_verb() {
  case "$VERB" in
    install)
      [ -n "$ROLE" ] || die "$(t need_role)"
      [ -z "$DEPLOY" ] && DEPLOY="binary"
      if [ "$DEPLOY" = "docker" ]; then
        install_docker          # writes its own .install-meta pre-up
      else
        install_binary
        [ "$WANT_SYSTEMD" = yes ] && install_systemd_unit
        [ "$ROLE" = "server" ] && [ "$WANT_SYSTEMD" = yes ] && write_server_dropin
        meta_write "$(meta_path_for)" "role=$ROLE" "deploy=$DEPLOY" "version=$resolved_version" "lang=${LANG_CODE:-en}" "advertised_endpoint_set=$([ -n "$ADVERTISED" ] && echo yes || echo no)"
      fi
      echo; print_next_steps
      ;;
    uninstall|upgrade|status|service|config|env) lifecycle_"$VERB" ;;
    *) die "verb '${VERB}' not yet implemented" ;;
  esac
}

# ─── Lifecycle ────────────────────────────────────────────────────────
SCOPED_KEYS="advertised-endpoint data-dir operator-http-listen version-pin"
validate_config_key() {
  [ -n "$CONFIG_KEY" ] || die "config key required (allowed: $SCOPED_KEYS)"
  case " $SCOPED_KEYS " in *" $CONFIG_KEY "*) ;; *) die "$(t unknown_config_key "$CONFIG_KEY")" ;; esac
}

current_meta_file() {
  # An explicit --compose-dir is a hard scope: resolve ONLY its meta and
  # never fall back to a system binary path. Otherwise a destructive
  # `uninstall --purge --compose-dir X` could silently target
  # /var/lib/portunus when X has no meta.
  local f
  if [ -n "${COMPOSE_DIR:-}" ]; then
    f="$COMPOSE_DIR/.install-meta"
    [ -r "$f" ] && { echo "$f"; return 0; }
    return 1
  fi
  # No explicit compose-dir: cwd (Docker user's pwd) then system paths.
  for f in "$PWD/.install-meta" "/var/lib/portunus/.install-meta" "/etc/portunus/.install-meta"; do
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
    ( cd "$(dirname "$mf")" && case "$SERVICE_ACTION" in start) $(compose_cmd) up -d ;; stop) $(compose_cmd) stop ;; restart) $(compose_cmd) restart ;; esac )
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
  if [ "$ASSUME_YES" != yes ]; then confirm "$(t confirm_uninstall "$r" "$d")" no || return 0; fi
  if [ "$d" = docker ]; then ( cd "$(dirname "$mf")" && $(compose_cmd) down )
  else
    sudo rm -f "/usr/local/bin/portunus-$r" "/etc/systemd/system/portunus-$r.service"
    sudo rm -f "/etc/systemd/system/portunus-server.service.d/10-portunus.conf"
    sudo systemctl daemon-reload 2>/dev/null || true
  fi
  if [ "$PURGE" = yes ]; then
    local dd; dd="$(dirname "$mf")"
    read_tty "$(t confirm_purge_typed "$dd")" || REPLY_TTY=""
    [ "$REPLY_TTY" = "purge" ] && { sudo rm -rf "$dd"; echo "→ purged $dd"; } || echo "purge skipped"
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
    local keep=""
    [ -f "$target_file" ] && keep="$(grep -E '^Environment=' "$target_file" 2>/dev/null | grep -vE "^Environment=${envkey}=" || true)"
    sudo install -d -m 0755 "$(dirname "$target_file")"
    { echo "[Service]"; [ -n "$keep" ] && printf '%s\n' "$keep"; echo "Environment=${envkey}=${CONFIG_VALUE}"; } | sudo tee "$target_file" >/dev/null
    sudo systemctl daemon-reload 2>/dev/null || true
  fi
  echo "→ set ${CONFIG_KEY}=${CONFIG_VALUE}"
  if confirm "$(t restart_now)" no; then SERVICE_ACTION=restart; lifecycle_service; fi
}

lifecycle_env() { CONFIG_OP="get"; for CONFIG_KEY in advertised-endpoint operator-http-listen data-dir version-pin; do printf '%s=' "$CONFIG_KEY"; lifecycle_config; done; }

# Read one line straight from the terminal into REPLY_TTY. A per-prompt
# `cat`/process-sub left a zombie reader on the tty, so a second prompt
# in the same command (uninstall→purge, set→restart) raced it.
read_tty() {
  REPLY_TTY=""
  printf '%s\n' "$1" >&2           # question on its own line; answer below
  if [ -t 0 ]; then read -r -p "> " REPLY_TTY
  elif [ -r /dev/tty ]; then read -r -p "> " REPLY_TTY </dev/tty
  else return 1; fi
}
# confirm <prompt> [default]   default = yes (Enter ⇒ proceed) | no (Enter ⇒ abort)
# The prompt text's [Y/n] vs [y/N] MUST match the default passed here.
confirm() {
  local prompt="$1" def="${2:-yes}"
  [ "$ASSUME_YES" = yes ] && return 0
  read_tty "$prompt" || return 1
  case "$REPLY_TTY" in
    y|Y|yes|YES) return 0 ;;
    n|N|no|NO)   return 1 ;;
    "")          [ "$def" = yes ] && return 0 || return 1 ;;
    *)           return 1 ;;
  esac
}

# Light host:port sanity (authoritative SAN/grammar check is the server's).
valid_host_port() {
  case "$1" in
    "") return 0 ;;  # blank ⇒ auto (tier-3/4 at runtime)
    *[!A-Za-z0-9.:_-]*) return 1 ;;
  esac
  case "$1" in
    *:[0-9]|*:[0-9][0-9]|*:[0-9][0-9][0-9]|*:[0-9][0-9][0-9][0-9]|*:[0-9][0-9][0-9][0-9][0-9])
      [ -n "${1%:*}" ] && return 0 ;;
  esac
  return 1
}

main "$@"
