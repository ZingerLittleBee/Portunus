#!/bin/sh
# Portunus lifecycle manager: install/uninstall/upgrade/status/service/
# config/env for client/server/standalone, binary+systemd|openrc or Docker.
#
#   curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- standalone
#   curl -fsSL .../scripts/install.sh | sh        # interactive menu
#
# POSIX sh. The only non-POSIX builtin relied upon is `local`, which dash,
# busybox ash, and ksh all provide.
# shellcheck disable=SC3043  # 'local' is provided by dash/busybox-ash/ksh
set -eu

# When piped (curl|sh) $0 is the shell name; when run as a file it is the
# path. Only a readable file path yields local templates.
SELF_SCRIPT=""
case "${0:-}" in
  ""|sh|-sh|dash|-dash|bash|-bash|ash|-ash) ;;
  *) [ -r "$0" ] && SELF_SCRIPT="$(cd "$(dirname "$0")" 2>/dev/null && pwd)/$(basename "$0")" || true ;;
esac

# ─── Constants ────────────────────────────────────────────────────────
REPO="ZingerLittleBee/Portunus"
RAW_BASE="https://raw.githubusercontent.com/${REPO}/main"
DEFAULT_BIN_DIR="/usr/local/bin"
LANG_CACHE="${XDG_CONFIG_HOME:-$HOME/.config}/portunus/installer-lang"
I18N_KEYS="menu_title menu_install menu_uninstall menu_upgrade menu_status menu_service menu_config menu_env menu_exit menu_select lang_prompt ask_role ask_deploy_server ask_deploy_client ask_deploy_standalone ask_version ask_bindir ask_datadir ask_ophttp confirm_proceed confirm_uninstall confirm_purge_typed need_role no_install_found done_next next_standalone_config next_systemd next_docker next_status restart_now upgrade_current unknown_config_key ask_config_key ask_config_value ask_service_action menu_invalid press_enter bad_endpoint op_cancelled ask_advertised_pub summary_title sum_role sum_deploy sum_version sum_bindir sum_datadir sum_ophttp sum_compose sum_advertised prov_detected prov_nic prov_loopback prov_user val_latest val_binary val_docker ask_domain sum_domain bad_domain dns_check dns_ok dns_mismatch dns_help caddy_installing caddy_done caddy_verify caddy_verify_warn https_ready https_public_note adv_from_domain config_na_standalone"

# ─── Globals ──────────────────────────────────────────────────────────
VERB=""           # install|uninstall|upgrade|status|service|config|env
ROLE=""           # client|server|standalone
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
DOMAIN=""             # optional HTTPS domain for host Caddy
ACME_EMAIL=""         # optional Let's Encrypt account email
SKIP_DNS_CHECK="no"   # --skip-dns-check
CADDYFILE="/etc/caddy/Caddyfile"
ADVERTISED_FROM_DOMAIN="no"  # set yes when ADVERTISED was derived from DOMAIN
PRINT_EFF="no"               # --effective-advertised seam

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }

# Space-separated list of temp dirs removed on exit (paths contain no spaces).
CLEANUP_DIRS=""
_cleanup() { for d in $CLEANUP_DIRS; do [ -n "$d" ] && rm -rf "$d"; done; return 0; }
trap _cleanup EXIT
track_tmp() { CLEANUP_DIRS="$CLEANUP_DIRS $1"; }

# ─── i18n ─────────────────────────────────────────────────────────────
# Convention: every '%' in a message value MUST be a '%s' directive with a matching t() arg (values are used as printf format strings).
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

t() {  # t <key> [printf-args...] — localized printf, no trailing newline (callers add it)
  _k="${1:-}"; shift 2>/dev/null || true
  case "$LANG_CODE:$_k" in
    zh:menu_title) _f="Portunus 管理器" ;;
    zh:menu_install) _f="  [1] 安装    Install" ;;
    zh:menu_uninstall) _f="  [2] 卸载    Uninstall" ;;
    zh:menu_upgrade) _f="  [3] 升级    Upgrade" ;;
    zh:menu_status) _f="  [4] 状态    Status" ;;
    zh:menu_service) _f="  [5] 服务控制 Service (start/stop/restart)" ;;
    zh:menu_config) _f="  [6] 配置    Config" ;;
    zh:menu_env) _f="  [7] 环境变量 Env" ;;
    zh:menu_exit) _f="  [0] 退出    Exit" ;;
    zh:menu_select) _f="请选择 [0-7]: " ;;
    zh:lang_prompt) _f="请选择语言\n  [1] English\n  [2] 中文" ;;
    zh:ask_role) _f="请选择要安装的角色\n  [1] server（服务端）\n  [2] client（客户端）\n  [3] standalone（独立转发器）" ;;
    zh:ask_deploy_server) _f="请选择部署方式（直接回车选择推荐项）\n  [1] docker compose  （推荐）\n  [2] 二进制 + systemd" ;;
    zh:ask_deploy_client) _f="请选择部署方式（直接回车选择推荐项）\n  [1] 二进制 + systemd  （推荐）\n  [2] docker compose" ;;
    zh:ask_deploy_standalone) _f="请选择部署方式（直接回车选择推荐项）\n  [1] 二进制 + systemd  （推荐）\n  [2] docker compose" ;;
    zh:ask_version) _f="版本号（留空则使用最新版）: " ;;
    zh:ask_bindir) _f="程序安装目录 [%s]: " ;;
    zh:ask_datadir) _f="服务端数据目录（留空则使用默认值）: " ;;
    zh:ask_ophttp) _f="运维 HTTP 监听地址（留空则使用默认值）: " ;;
    zh:confirm_proceed) _f="确认继续吗？[Y/n]: " ;;
    zh:confirm_uninstall) _f="确定要卸载 portunus-%s（%s）吗？[y/N]: " ;;
    zh:confirm_purge_typed) _f="如需连同数据一并删除 %s，请输入 'purge' 确认: " ;;
    zh:need_role) _f="请指定角色：client、server 或 standalone" ;;
    zh:no_install_found) _f="未检测到 Portunus 安装（缺少 .install-meta，且自动探测未命中）。" ;;
    zh:done_next) _f="安装完成，后续步骤：" ;;
    zh:next_standalone_config) _f="  编辑配置：sudoedit /etc/portunus/standalone.toml" ;;
    zh:next_systemd) _f="  启动服务：sudo systemctl enable --now portunus-%s" ;;
    zh:next_docker) _f="  查看容器：cd %s && docker compose ps" ;;
    zh:next_status) _f="  查看状态：install.sh status" ;;
    zh:restart_now) _f="是否立即生效（重启服务）？[y/N]: " ;;
    zh:upgrade_current) _f="当前已是最新版 %s，无需升级。" ;;
    zh:unknown_config_key) _f="未知的配置项：%s（可用：advertised-endpoint data-dir operator-http-listen version-pin）" ;;
    zh:ask_config_key) _f="请选择要修改的配置项\n  [1] advertised-endpoint\n  [2] data-dir\n  [3] operator-http-listen\n  [4] version-pin" ;;
    zh:ask_config_value) _f="请输入 %s 的新值: " ;;
    zh:ask_service_action) _f="请选择服务操作\n  [1] 启动\n  [2] 停止\n  [3] 重启" ;;
    zh:menu_invalid) _f="无效的选项：%s" ;;
    zh:press_enter) _f="按回车键继续…" ;;
    zh:bad_endpoint) _f="无效的 host:port：'%s'（示例：host.example:7443；留空则自动）" ;;
    zh:op_cancelled) _f="已取消。" ;;
    zh:ask_advertised_pub) _f="对外通告地址 [%s]（回车采用此默认值，输入 - 表示不设置/仅本机回环）: " ;;
    zh:summary_title) _f="即将安装：" ;;
    zh:sum_role) _f="  角色：%s" ;;
    zh:sum_deploy) _f="  部署方式：%s" ;;
    zh:sum_version) _f="  版本：%s" ;;
    zh:sum_bindir) _f="  程序目录：%s" ;;
    zh:sum_datadir) _f="  数据目录：%s" ;;
    zh:sum_ophttp) _f="  运维 HTTP：%s" ;;
    zh:sum_compose) _f="  compose 目录：%s" ;;
    zh:sum_advertised) _f="  对外通告地址：%s" ;;
    zh:prov_detected) _f="（自动探测到的公网 IP）" ;;
    zh:prov_nic) _f="（本机网卡地址）" ;;
    zh:prov_loopback) _f="（回环地址，仅本机可用）" ;;
    zh:prov_user) _f="（手动输入）" ;;
    zh:val_latest) _f="最新版（运行时解析）" ;;
    zh:val_binary) _f="二进制 + systemd" ;;
    zh:val_docker) _f="docker compose" ;;
    zh:ask_domain) _f="Web UI 的 HTTPS 域名（留空则跳过 Caddy/HTTPS）: " ;;
    zh:sum_domain) _f="  HTTPS 域名：%s" ;;
    zh:bad_domain) _f="无效的域名：'%s'（需为完整域名，如 portunus.example.com）" ;;
    zh:dns_check) _f="正在检查 %s 是否解析到本机（%s）…" ;;
    zh:dns_ok) _f="DNS 校验通过：%s → %s" ;;
    zh:dns_mismatch) _f="%s 的解析未指向本机。A 记录：%s ；本机公网 IP：%s" ;;
    zh:dns_help) _f="请添加以下 DNS 记录，然后按回车重新校验（Ctrl-C 取消）：\n  %s  A  %s" ;;
    zh:caddy_installing) _f="正在安装 Caddy…" ;;
    zh:caddy_done) _f="Caddy 已为 %s 配置完成" ;;
    zh:caddy_verify) _f="正在验证 https://%s/（Let's Encrypt 签发约需 30 秒）…" ;;
    zh:caddy_verify_warn) _f="暂时无法验证 https://%s/。请检查：journalctl -u caddy -e；以及 DNS 是否已生效。" ;;
    zh:https_ready) _f="HTTPS 已就绪：https://%s/" ;;
    zh:https_public_note) _f="提示：Web UI 现已通过 HTTPS 公开可访问，仍由运维登录/令牌保护。" ;;
    zh:adv_from_domain) _f="  对外通告地址：%s（由域名推导）" ;;
    zh:config_na_standalone) _f="standalone 角色不支持 config get/set —— 请直接编辑 /etc/portunus/standalone.toml" ;;
    *:menu_title) _f="Portunus Manager" ;;
    *:menu_install) _f="  [1] Install" ;;
    *:menu_uninstall) _f="  [2] Uninstall" ;;
    *:menu_upgrade) _f="  [3] Upgrade" ;;
    *:menu_status) _f="  [4] Status" ;;
    *:menu_service) _f="  [5] Service (start/stop/restart)" ;;
    *:menu_config) _f="  [6] Config" ;;
    *:menu_env) _f="  [7] Env" ;;
    *:menu_exit) _f="  [0] Exit" ;;
    *:menu_select) _f="Select [0-7]: " ;;
    *:lang_prompt) _f="Select language\n  [1] English\n  [2] 中文" ;;
    *:ask_role) _f="Install which role?\n  [1] server\n  [2] client\n  [3] standalone" ;;
    *:ask_deploy_server) _f="Deploy form? (Enter = recommended)\n  [1] docker compose  (recommended)\n  [2] binary + systemd" ;;
    *:ask_deploy_client) _f="Deploy form? (Enter = recommended)\n  [1] binary + systemd  (recommended)\n  [2] docker compose" ;;
    *:ask_deploy_standalone) _f="Deploy form? (Enter = recommended)\n  [1] binary + systemd  (recommended)\n  [2] docker compose" ;;
    *:ask_version) _f="Version (blank = latest): " ;;
    *:ask_bindir) _f="Install dir [%s]: " ;;
    *:ask_datadir) _f="Server data dir (blank = default): " ;;
    *:ask_ophttp) _f="Operator HTTP listen (blank = default): " ;;
    *:confirm_proceed) _f="Proceed? [Y/n]: " ;;
    *:confirm_uninstall) _f="Uninstall portunus-%s (%s)? [y/N]: " ;;
    *:confirm_purge_typed) _f="Type 'purge' to also delete data at %s: " ;;
    *:need_role) _f="role required: client, server, or standalone" ;;
    *:no_install_found) _f="No Portunus install detected (no .install-meta and no probe match)." ;;
    *:done_next) _f="Done. Next steps:" ;;
    *:next_standalone_config) _f="  edit:    sudoedit /etc/portunus/standalone.toml" ;;
    *:next_systemd) _f="  start:   sudo systemctl enable --now portunus-%s" ;;
    *:next_docker) _f="  manage:  (cd %s && docker compose ps)" ;;
    *:next_status) _f="  status:  install.sh status" ;;
    *:restart_now) _f="Apply now (restart service)? [y/N]: " ;;
    *:upgrade_current) _f="Already at %s; nothing to upgrade." ;;
    *:unknown_config_key) _f="unknown config key: %s (allowed: advertised-endpoint data-dir operator-http-listen version-pin)" ;;
    *:ask_config_key) _f="Config key\n  [1] advertised-endpoint\n  [2] data-dir\n  [3] operator-http-listen\n  [4] version-pin" ;;
    *:ask_config_value) _f="New value for %s: " ;;
    *:ask_service_action) _f="Service action\n  [1] start\n  [2] stop\n  [3] restart" ;;
    *:menu_invalid) _f="invalid option: %s" ;;
    *:press_enter) _f="Press Enter to continue…" ;;
    *:bad_endpoint) _f="invalid host:port '%s' — expected like host.example:7443 (blank = auto)" ;;
    *:op_cancelled) _f="cancelled." ;;
    *:ask_advertised_pub) _f="Public advertised endpoint [%s] (Enter=accept, '-' = none/loopback): " ;;
    *:summary_title) _f="About to install:" ;;
    *:sum_role) _f="  role:                 %s" ;;
    *:sum_deploy) _f="  deploy:               %s" ;;
    *:sum_version) _f="  version:              %s" ;;
    *:sum_bindir) _f="  bin dir:              %s" ;;
    *:sum_datadir) _f="  data dir:             %s" ;;
    *:sum_ophttp) _f="  operator http:        %s" ;;
    *:sum_compose) _f="  compose dir:          %s" ;;
    *:sum_advertised) _f="  advertised endpoint:  %s" ;;
    *:prov_detected) _f="(detected public IP)" ;;
    *:prov_nic) _f="(local NIC)" ;;
    *:prov_loopback) _f="(loopback — local only)" ;;
    *:prov_user) _f="(you entered)" ;;
    *:val_latest) _f="latest (resolved at run time)" ;;
    *:val_binary) _f="binary + systemd" ;;
    *:val_docker) _f="docker compose" ;;
    *:ask_domain) _f="HTTPS domain for the web UI (blank = skip Caddy/HTTPS): " ;;
    *:sum_domain) _f="  https domain:         %s" ;;
    *:bad_domain) _f="invalid domain '%s' — expected an FQDN like portunus.example.com" ;;
    *:dns_check) _f="Checking %s resolves to this server (%s)…" ;;
    *:dns_ok) _f="DNS OK: %s → %s" ;;
    *:dns_mismatch) _f="DNS for %s does not point here. A record(s): %s ; this server: %s" ;;
    *:dns_help) _f="Add this DNS record, then press Enter to re-check (Ctrl-C to abort):\n  %s  A  %s" ;;
    *:caddy_installing) _f="Installing Caddy…" ;;
    *:caddy_done) _f="Caddy configured for %s" ;;
    *:caddy_verify) _f="Verifying https://%s/ (Let's Encrypt issuance can take ~30s)…" ;;
    *:caddy_verify_warn) _f="Could not verify https://%s/ yet. Check: journalctl -u caddy -e ; DNS propagation." ;;
    *:https_ready) _f="HTTPS ready: https://%s/" ;;
    *:https_public_note) _f="Note: the web UI is now publicly reachable over HTTPS; it stays protected by operator login/token." ;;
    *:adv_from_domain) _f="  advertised endpoint:  %s  (from domain)" ;;
    *:config_na_standalone) _f="config get/set is not applicable for the standalone role — edit /etc/portunus/standalone.toml directly" ;;
    *) _f="$_k" ;;
  esac
  # shellcheck disable=SC2059
  printf "$_f" "$@"
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
    linux) target="${arch}-unknown-linux-musl" ;;
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

  if [ "$ROLE" = "standalone" ]; then
    # Create the system user (idempotent) and seed the config dir.
    if ! id -u portunus >/dev/null 2>&1; then
      ${SUDO:-} useradd --system --no-create-home --shell /usr/sbin/nologin portunus \
        || die "failed to create portunus user"
    fi
    ${SUDO:-} mkdir -p /etc/portunus
    if [ ! -f /etc/portunus/standalone.toml ]; then
      local self_dir2=""
      if [ -n "${SELF_SCRIPT:-}" ]; then
        self_dir2="$(dirname "$SELF_SCRIPT")"
      fi
      if [ -n "$self_dir2" ] && [ -r "$self_dir2/../crates/portunus-standalone/contrib/portunus.example.toml" ]; then
        ${SUDO:-} cp "$self_dir2/../crates/portunus-standalone/contrib/portunus.example.toml" /etc/portunus/standalone.toml
      else
        ${SUDO:-} curl -fsSL "${RAW_BASE}/crates/portunus-standalone/contrib/portunus.example.toml" -o /etc/portunus/standalone.toml \
          || die "failed to fetch starter standalone.toml"
      fi
      ${SUDO:-} chown root:portunus /etc/portunus/standalone.toml
      ${SUDO:-} chmod 0640 /etc/portunus/standalone.toml
    fi
  fi
}

# ─── Systemd ──────────────────────────────────────────────────────────
install_systemd_unit() {
  if [ "$os" != "linux" ] || ! command -v systemctl >/dev/null 2>&1; then
    echo "warning: --systemd ignored (not Linux or systemctl missing)" >&2; return 0
  fi
  maybe_sudo "/etc/systemd/system"
  local unit tmp; unit="portunus-${ROLE}.service"; tmp="$(mktemp -d)"; track_tmp "$tmp"
  if [ "$ROLE" = "standalone" ]; then
    # Use the hardened contrib unit verbatim; user edits live in
    # /etc/portunus/standalone.toml, not in the unit file.
    local self_dir=""
    if [ -n "${SELF_SCRIPT:-}" ]; then
      self_dir="$(dirname "$SELF_SCRIPT")"
    fi
    if [ -n "$self_dir" ] && [ -r "$self_dir/../crates/portunus-standalone/contrib/portunus-standalone.service" ]; then
      cp "$self_dir/../crates/portunus-standalone/contrib/portunus-standalone.service" "$tmp/$unit"
    else
      # Network-resolved (curl|bash) invocation: fetch the unit from the repo.
      curl -fsSL "${RAW_BASE}/crates/portunus-standalone/contrib/portunus-standalone.service" -o "$tmp/$unit" \
        || die "failed to fetch portunus-standalone.service"
    fi
  else
    curl -fsSL "${RAW_BASE}/deploy/systemd/${unit}" -o "$tmp/$unit" || die "unit download failed"
  fi
  if [ "$ROLE" = "client" ]; then
    id portunus-client >/dev/null 2>&1 || ${SUDO:-} useradd --system --no-create-home --shell /usr/sbin/nologin portunus-client
    ${SUDO:-} install -d -o root -g portunus-client -m 0750 /etc/portunus
  elif [ "$ROLE" = "standalone" ]; then
    # User and /etc/portunus already handled in install_binary(); only
    # the systemd unit file remains here.
    :
  else
    id portunus-server >/dev/null 2>&1 || ${SUDO:-} useradd --system --no-create-home --shell /usr/sbin/nologin portunus-server
    ${SUDO:-} install -d -o portunus-server -g portunus-server -m 0750 "${DATA_DIR:-/var/lib/portunus}"
  fi
  ${SUDO:-} install -m 0644 "$tmp/$unit" "/etc/systemd/system/$unit"
  ${SUDO:-} systemctl daemon-reload || true
}

# ─── Server config (drop-in / .env) ───────────────────────────────────
# Pure: emit the drop-in body for the currently-set scoped values.
# Mirrors the `Environment=` lines `config set` writes so install-time
# --data-dir / --operator-http-listen persist identically (parity).
render_dropin() {
  # The server has no env binding for these; an inert Environment= line
  # is ignored. Emit a real ExecStart= override (cleared then re-set —
  # the standard systemd drop-in idiom) so the flags actually take.
  local dd="${DATA_DIR:-/var/lib/portunus}" args
  args="--data-dir ${dd} serve"
  [ -n "$OP_HTTP_LISTEN" ] && args="${args} --operator-http-listen ${OP_HTTP_LISTEN}"
  [ -n "$ADVERTISED" ]     && args="${args} --advertised-endpoint ${ADVERTISED}"
  printf '[Service]\nExecStart=\nExecStart=/usr/local/bin/portunus-server %s\n' "$args"
  return 0
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
    [ "$ROLE" = "standalone" ] && { t next_standalone_config; echo; }
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
  local dir="$1" f="$1/compose.yml" port; port="$(op_http_port)"
  mkdir -p "$dir"
  if [ "$ROLE" = "standalone" ]; then
    # No GHCR image is published for standalone — copy the reference
    # compose file from contrib/ and the user builds locally.
    local self_dir3=""
    if [ -n "${SELF_SCRIPT:-}" ]; then
      self_dir3="$(dirname "$SELF_SCRIPT")"
    fi
    if [ -n "$self_dir3" ] && [ -r "$self_dir3/../crates/portunus-standalone/contrib/docker-compose.yml" ]; then
      cp "$self_dir3/../crates/portunus-standalone/contrib/docker-compose.yml" "$dir/docker-compose.yml"
    else
      curl -fsSL "${RAW_BASE}/crates/portunus-standalone/contrib/docker-compose.yml" -o "$dir/docker-compose.yml" \
        || die "failed to fetch contrib/docker-compose.yml"
    fi
    if [ ! -f "$dir/portunus.toml" ]; then
      if [ -n "$self_dir3" ] && [ -r "$self_dir3/../crates/portunus-standalone/contrib/portunus.example.toml" ]; then
        cp "$self_dir3/../crates/portunus-standalone/contrib/portunus.example.toml" "$dir/portunus.toml"
      else
        curl -fsSL "${RAW_BASE}/crates/portunus-standalone/contrib/portunus.example.toml" -o "$dir/portunus.toml" \
          || die "failed to fetch contrib/portunus.example.toml"
      fi
    fi
    return 0
  fi
  [ -f "$f" ] && { echo "→ keeping existing $f"; return 0; }
  # The server's --operator-http-listen has no env binding and defaults
  # to container-internal 127.0.0.1, which Docker's published port (and
  # host Caddy) cannot reach. Override the image CMD to bind 0.0.0.0
  # inside the container (mirrors deploy/docker/docker-compose.yml); the
  # host only publishes 127.0.0.1:<port> so it stays loopback-exposed.
  local advcmd=""
  [ -n "$ADVERTISED" ] && advcmd=", \"--advertised-endpoint\", \"${ADVERTISED}\""
  cat > "$f" <<YAML
services:
  server:
    image: ghcr.io/zingerlittlebee/portunus-${ROLE}:${artifact_version:-latest}
    container_name: portunus-${ROLE}
    env_file: [ .env ]
    command: ["--data-dir", "/var/lib/portunus", "serve", "--operator-http-listen", "0.0.0.0:${port}"${advcmd}]
    ports:
      - "7443:7443"
      - "127.0.0.1:${port}:${port}"
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
      client|server|standalone) ROLE="$1"; [ -z "$VERB" ] && VERB="install" ;;
      install|uninstall|upgrade|status|service|config|env|domain) VERB="$1" ;;
      start|stop|restart) SERVICE_ACTION="$1" ;;
      get|set) CONFIG_OP="$1" ;;
      --version) shift; [ $# -gt 0 ] || die "--version needs a value"; VERSION="$1" ;;
      --bin-dir) shift; [ $# -gt 0 ] || die "--bin-dir needs a value"; BIN_DIR="$1" ;;
      --compose-dir) shift; [ $# -gt 0 ] || die "--compose-dir needs a value"; COMPOSE_DIR="$1" ;;
      --deploy) shift; case "${1:-}" in binary|docker) DEPLOY="$1" ;; *) die "--deploy must be binary|docker" ;; esac ;;
      --advertised-endpoint) shift; [ $# -gt 0 ] || die "--advertised-endpoint needs a value"; ADVERTISED="$1" ;;
      --domain) shift; [ $# -gt 0 ] || die "--domain needs a value"; DOMAIN="$1" ;;
      --acme-email) shift; [ $# -gt 0 ] || die "--acme-email needs a value"; ACME_EMAIL="$1" ;;
      --skip-dns-check) SKIP_DNS_CHECK="yes" ;;
      --data-dir) shift; [ $# -gt 0 ] || die "--data-dir needs a value"; DATA_DIR="$1" ;;
      --operator-http-listen) shift; [ $# -gt 0 ] || die "--operator-http-listen needs a value"; OP_HTTP_LISTEN="$1" ;;
      --lang) shift; [ $# -gt 0 ] || die "--lang needs a value"; LANG_CODE="$1" ;;
      --systemd) WANT_SYSTEMD="yes" ;;
      --yes) ASSUME_YES="yes" ;;
      --purge) PURGE="yes" ;;
      --dry-run) DRY_RUN="yes" ;;
      --print-i18n-keys) shift 2>/dev/null || true; for k in $I18N_KEYS; do echo "$k"; done; exit 0 ;;
      --print-i18n) shift; [ $# -gt 0 ] || die "--print-i18n needs a key"; resolve_lang; t "$1"; echo; exit 0 ;;
      -h|--help) echo "usage: install.sh <client|server|install|uninstall|upgrade|status|service|config|env|domain> [start|stop|restart] [get|set key [value]] [--version V] [--deploy binary|docker] [--bin-dir D] [--compose-dir D] [--advertised-endpoint H:P] [--data-dir D] [--operator-http-listen A] [--domain FQDN] [--acme-email A] [--skip-dns-check] [--systemd] [--lang en|zh] [--reset-lang] [--yes] [--purge] [--dry-run]"; exit 0 ;;
      --meta-write) shift; f="$1"; shift; meta_write "$f" "$@"; exit 0 ;;
      --meta-read) shift; f="$1"; k="$2"; meta_read "$f" "$k"; exit $? ;;
      --detect-deploy) shift; detect_deploy "${1:-}"; exit 0 ;;
      --detect-ip) detect_public_ip; printf '%s %s\n' "$DETECTED_IP" "$DETECTED_PROV"; exit 0 ;;
      --reset-lang) rm -f "$LANG_CACHE" 2>/dev/null || true; echo "language preference reset ($LANG_CACHE); next interactive run will ask again"; exit 0 ;;
      --valid-fqdn) shift; valid_fqdn "${1:-}" && exit 0 || exit 1 ;;
      --render-caddy) shift; DOMAIN="${1:-}"; render_caddy_block "${2:-7080}"; exit 0 ;;
      --render-dropin) render_dropin; exit 0 ;;
      --effective-advertised) PRINT_EFF=yes ;;
      --valid-endpoint) shift; valid_host_port "${1:-}" && exit 0 || exit 1 ;;
      --resolve-meta) current_meta_file && exit 0 || exit 1 ;;
      --menu-stdin) MENU_FORCE_STDIN="yes" ;;  # defer to main so later --compose-dir et al. still parse
      *) if [ "$VERB" = domain ] && [ -z "$DOMAIN" ]; then DOMAIN="$1";
         elif [ "$VERB" = config ] && [ -z "$CONFIG_KEY" ]; then CONFIG_KEY="$1";
         elif [ "$VERB" = config ] && [ -z "$CONFIG_VALUE" ]; then CONFIG_VALUE="$1";
         else die "unknown argument: $1"; fi ;;
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
  [ -n "$DOMAIN" ] && [ -n "$ROLE" ] && [ "$ROLE" != server ] && die "--domain is server-only"
  apply_advertised_default
  apply_install_defaults
  if [ "$PRINT_EFF" = yes ]; then printf '%s\n' "$ADVERTISED"; exit 0; fi
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
  if [ "$MENU_FORCE_STDIN" = yes ] || [ -t 0 ]; then printf '> ' >&2; read -r a || a=""
  else printf '> ' >&2; read -r a < /dev/tty 2>/dev/null || a=""; fi
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
    t sum_deploy "$(t val_docker)"; echo
  else
    t sum_deploy "$(t val_binary)"; echo
  fi
  t sum_version "${VERSION:-$(t val_latest)}"; echo
  if [ "$DEPLOY" = docker ]; then
    t sum_compose "${COMPOSE_DIR:-$PWD}"; echo
  else
    t sum_bindir "${BIN_DIR:-$DEFAULT_BIN_DIR}"; echo
  fi
  if [ "$ROLE" = server ]; then
    t sum_datadir "${DATA_DIR:-/var/lib/portunus}"; echo
    t sum_ophttp "${OP_HTTP_LISTEN:-127.0.0.1:7080}"; echo
    if [ "$ADVERTISED_FROM_DOMAIN" = yes ]; then
      t adv_from_domain "$ADVERTISED"; echo
    elif [ -n "$ADVERTISED" ]; then
      t sum_advertised "$ADVERTISED $([ -n "$adv_prov" ] && t "$adv_prov")"; echo
    else
      t sum_advertised "$(t prov_loopback)"; echo
    fi
    [ -n "$DOMAIN" ] && { t sum_domain "$DOMAIN"; echo; }
  fi
}

# ─── Caddy / HTTPS ────────────────────────────────────────────────────
valid_fqdn() {
  case "$1" in
    ""|*[!a-zA-Z0-9.-]*) return 1 ;;
    .*|-*|*.|*-|*..*) return 1 ;;
  esac
  case "$1" in *.*) return 0 ;; *) return 1 ;; esac
}

op_http_port() {  # echo the loopback port Caddy must proxy to
  local p="${OP_HTTP_LISTEN##*:}"
  case "$p" in ''|*[!0-9]*) echo 7080 ;; *) echo "$p" ;; esac
}

dns_a_records() {
  if command -v getent >/dev/null 2>&1; then
    getent ahostsv4 "$1" 2>/dev/null | awk '{print $1}' | sort -u
  elif command -v dig >/dev/null 2>&1; then
    dig +short A "$1" 2>/dev/null | sed '/^$/d'
  fi
}

dns_points_here() {  # $1 domain ; uses detect_public_ip
  local d="$1" a ip
  detect_public_ip; ip="$DETECTED_IP"
  t dns_check "$d" "$ip"; echo
  while :; do
    a="$(dns_a_records "$d" | tr '\n' ' ')"
    if printf '%s' "$a" | grep -qw "$ip"; then
      t dns_ok "$d" "$ip"; echo; return 0
    fi
    t dns_mismatch "$d" "${a:-none}" "$ip"; echo
    if [ "$ASSUME_YES" = yes ] || ! { [ -t 0 ] || [ -r /dev/tty ]; }; then
      die "DNS for $d must point to $ip (or pass --skip-dns-check)"
    fi
    t dns_help "$d" "$ip"; echo
    read_tty "" || die "DNS for $d must point to $ip"
  done
}

render_caddy_block() {  # $1 op-http port ; prints managed block
  local port="$1"
  echo "# >>> portunus >>>"
  [ -n "$ACME_EMAIL" ] && echo "{ email ${ACME_EMAIL} }"
  echo "${DOMAIN} {"
  echo "    reverse_proxy 127.0.0.1:${port}"
  echo "}"
  echo "# <<< portunus <<<"
}

ensure_caddy() {
  command -v caddy >/dev/null 2>&1 && return 0
  echo "$(t caddy_installing)"
  local id="" like=""
  [ -r /etc/os-release ] && . /etc/os-release && id="${ID:-}" && like="${ID_LIKE:-}"
  case "$id $like" in
    *debian*|*ubuntu*)
      sudo apt-get install -y -qq debian-keyring debian-archive-keyring apt-transport-https curl >/dev/null 2>&1 || true
      curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor --batch --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
      curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list >/dev/null
      sudo chmod o+r /usr/share/keyrings/caddy-stable-archive-keyring.gpg /etc/apt/sources.list.d/caddy-stable.list
      sudo apt-get update -qq >/dev/null 2>&1 || true
      sudo apt-get install -y -qq caddy >/dev/null 2>&1 || die "caddy install failed (apt)" ;;
    *fedora*|*rhel*|*centos*)
      if command -v dnf >/dev/null 2>&1; then sudo dnf copr enable -y @caddy/caddy >/dev/null 2>&1 && sudo dnf install -y -q caddy >/dev/null 2>&1 || die "caddy install failed (dnf)"
      else sudo yum copr enable -y @caddy/caddy >/dev/null 2>&1 && sudo yum install -y -q caddy >/dev/null 2>&1 || die "caddy install failed (yum)"; fi ;;
    *) die "cannot auto-install Caddy on this distro; install it and add:\n  ${DOMAIN} {\n      reverse_proxy 127.0.0.1:$(op_http_port)\n  }" ;;
  esac
}

write_caddy_block() {
  local port; port="$(op_http_port)"
  sudo install -d -m 0755 "$(dirname "$CADDYFILE")"
  if [ -f "$CADDYFILE" ]; then
    sudo cp "$CADDYFILE" "${CADDYFILE}.portunus.$(date +%Y%m%d%H%M%S).bak"
    sudo sed -i '/^# >>> portunus >>>$/,/^# <<< portunus <<<$/d' "$CADDYFILE"
  fi
  render_caddy_block "$port" | sudo tee -a "$CADDYFILE" >/dev/null
}

caddy_reload() {
  if command -v systemctl >/dev/null 2>&1; then
    sudo systemctl enable caddy >/dev/null 2>&1 || true
    sudo systemctl restart caddy
  else
    caddy reload --config "$CADDYFILE" 2>/dev/null || echo "start Caddy manually: caddy run --config $CADDYFILE"
  fi
}

verify_https() {
  local d="$1"
  t caddy_verify "$d"; echo
  for _ in $(seq 1 12); do
    if curl -fsS --max-time 5 -o /dev/null "https://${d}/" 2>/dev/null; then
      t https_ready "$d"; echo; return 0
    fi
    sleep 5
  done
  t caddy_verify_warn "$d"; echo; return 0
}

setup_caddy_domain() {
  [ "$ROLE" = server ] || die "domain/HTTPS is server-only"
  valid_fqdn "$DOMAIN" || die "$(t bad_domain "$DOMAIN")"
  if [ "$DRY_RUN" = yes ]; then
    echo "domain:           $DOMAIN"
    echo "reverse_proxy:    127.0.0.1:$(op_http_port)"
    echo "caddyfile:        $CADDYFILE"
    echo "dns_precheck:     $([ "$SKIP_DNS_CHECK" = yes ] && echo skipped || echo enabled)"
    return 0
  fi
  [ "$SKIP_DNS_CHECK" = yes ] || dns_points_here "$DOMAIN"
  ensure_caddy
  write_caddy_block
  caddy_reload
  t caddy_done "$DOMAIN"; echo
  verify_https "$DOMAIN"
  t https_public_note; echo
}

lifecycle_domain() {
  local mf; mf="$(current_meta_file)" || die "$(t no_install_found)"
  ROLE="$(meta_read "$mf" role || echo server)"
  [ "$ROLE" = server ] || die "domain/HTTPS is server-only"
  [ -n "$DOMAIN" ] || die "usage: install.sh domain <fqdn>"
  OP_HTTP_LISTEN="$(meta_read "$mf" op_http_listen 2>/dev/null || echo '')"
  DEPLOY="$(meta_read "$mf" deploy || echo binary)"
  setup_caddy_domain
  apply_advertised_default
  if [ "$DRY_RUN" != yes ] && [ -n "$ADVERTISED" ]; then
    if [ "$DEPLOY" = docker ]; then
      COMPOSE_DIR="$(dirname "$mf")"
      [ -f "$COMPOSE_DIR/compose.yml" ] && sudo cp "$COMPOSE_DIR/compose.yml" "$COMPOSE_DIR/compose.yml.portunus.$(date +%Y%m%d%H%M%S).bak" && rm -f "$COMPOSE_DIR/compose.yml"
      write_compose_file "$COMPOSE_DIR"; write_compose_env "$COMPOSE_DIR"
      ( cd "$COMPOSE_DIR" && $(compose_cmd) up -d ) || true
    else
      write_server_dropin
      command -v systemctl >/dev/null 2>&1 && sudo systemctl restart "portunus-$ROLE" 2>/dev/null || true
    fi
    echo "→ advertised endpoint set to ${ADVERTISED}; the server re-aligns its gRPC cert SAN on restart."
    echo "→ existing client bundles must be re-issued: portunus-server enroll-client <name>"
  fi
  if [ "$DRY_RUN" != yes ]; then
    meta_write "$mf" "role=$ROLE" "deploy=$DEPLOY" \
      "version=$(meta_read "$mf" version || echo '?')" \
      "lang=${LANG_CODE:-en}" \
      "advertised_endpoint_set=$(meta_read "$mf" advertised_endpoint_set || echo no)" \
      "domain=$DOMAIN"
  fi
}

# Explicit --advertised-endpoint wins; otherwise a server --domain
# derives advertised = <domain>:7443. Idempotent; safe to call twice.
apply_advertised_default() {
  [ "$ROLE" = server ] || return 0
  [ -n "$DOMAIN" ] || return 0
  [ -z "$ADVERTISED" ] || return 0
  ADVERTISED="${DOMAIN}:7443"
  ADVERTISED_FROM_DOMAIN=yes
  return 0
}

apply_install_defaults() {
  [ "${VERB:-install}" = "install" ] || return 0
  [ -n "${ROLE:-}" ] || return 0
  if [ -z "${DEPLOY:-}" ]; then
    DEPLOY="binary"
    [ "$ROLE" = "standalone" ] && WANT_SYSTEMD="yes"
    return 0
  fi
  if [ "$ROLE" = "standalone" ] && [ "$DEPLOY" = "binary" ]; then
    WANT_SYSTEMD="yes"
  fi
}

wizard_install() {
  local a adv_prov=""
  a="$(ask ask_role)"; case "$a" in 2) ROLE=client ;; 3) ROLE=standalone ;; *) ROLE=server ;; esac
  # Recommended deploy form differs by role: server ⇒ docker compose,
  # client ⇒ binary. Enter (empty) accepts the recommended one.
  if [ "$ROLE" = server ]; then
    a="$(ask ask_deploy_server)"
    case "$a" in 2|binary) DEPLOY=binary; WANT_SYSTEMD=yes ;; *) DEPLOY=docker ;; esac
  elif [ "$ROLE" = standalone ]; then
    a="$(ask ask_deploy_standalone)"
    case "$a" in 2) DEPLOY=docker ;; *) DEPLOY=binary; WANT_SYSTEMD=yes ;; esac
  else
    a="$(ask ask_deploy_client)"
    case "$a" in 2|docker) DEPLOY=docker ;; *) DEPLOY=binary; WANT_SYSTEMD=yes ;; esac
  fi
  if [ "$ROLE" = server ]; then
    while :; do
      DOMAIN="$(ask ask_domain)"
      [ -z "$DOMAIN" ] && break
      valid_fqdn "$DOMAIN" && break
      t bad_domain "$DOMAIN"; echo
    done
    if [ -n "$DOMAIN" ]; then
      ADVERTISED="${DOMAIN}:7443"; ADVERTISED_FROM_DOMAIN=yes
    else
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
  local _mf _r
  _mf="$(current_meta_file 2>/dev/null || true)"
  _r="$([ -n "$_mf" ] && meta_read "$_mf" role 2>/dev/null || true)"
  if [ "${_r:-$ROLE}" = "standalone" ]; then
    echo "$(t config_na_standalone)" >&2
    return 2
  fi
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
    if [ "$MENU_FORCE_STDIN" = yes ] || [ -t 0 ]; then printf '> ' >&2; read -r c || return 0
    else printf '> ' >&2; read -r c < /dev/tty || return 0; fi
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
      if [ "$ROLE" = server ] && [ -n "$DOMAIN" ]; then
        meta_write "$(meta_path_for)" "role=$ROLE" "deploy=$DEPLOY" "version=$resolved_version" "lang=${LANG_CODE:-en}" "advertised_endpoint_set=$([ -n "$ADVERTISED" ] && echo yes || echo no)" "domain=$DOMAIN"
        setup_caddy_domain
      fi
      echo; print_next_steps
      ;;
    uninstall|upgrade|status|service|config|env|domain) lifecycle_"$VERB" ;;
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
    command -v systemctl >/dev/null 2>&1 && sudo systemctl disable --now "portunus-$r" 2>/dev/null || true
    sudo rm -f "/usr/local/bin/portunus-$r" "/etc/systemd/system/portunus-$r.service"
    sudo rm -f "/etc/systemd/system/portunus-server.service.d/10-portunus.conf"
    sudo systemctl daemon-reload 2>/dev/null || true
  fi
  if [ -f "$CADDYFILE" ] && grep -q '^# >>> portunus >>>$' "$CADDYFILE" 2>/dev/null; then
    sudo cp "$CADDYFILE" "${CADDYFILE}.portunus.$(date +%Y%m%d%H%M%S).bak"
    sudo sed -i '/^# >>> portunus >>>$/,/^# <<< portunus <<<$/d' "$CADDYFILE"
    command -v systemctl >/dev/null 2>&1 && sudo systemctl reload caddy 2>/dev/null || true
    echo "→ removed Caddy block from $CADDYFILE"
  fi
  if [ "$PURGE" = yes ]; then
    local dd; dd="$(dirname "$mf")"
    read_tty "$(t confirm_purge_typed "$dd")" || REPLY_TTY=""
    [ "$REPLY_TTY" = "purge" ] && { sudo rm -rf "$dd"; echo "→ purged $dd"; } || echo "purge skipped"
  fi
  rm -f "$mf" 2>/dev/null || true
}

lifecycle_config() {
  local mf; mf="$(current_meta_file)" || die "$(t no_install_found)"
  local _r; _r="$(meta_read "$mf" role 2>/dev/null || true)"
  if [ "${_r:-$ROLE}" = "standalone" ]; then
    echo "$(t config_na_standalone)" >&2
    return 2
  fi
  validate_config_key
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
  if [ -t 0 ]; then printf '> ' >&2; read -r REPLY_TTY
  elif [ -r /dev/tty ]; then printf '> ' >&2; read -r REPLY_TTY </dev/tty
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
