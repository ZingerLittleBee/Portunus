# Caddy HTTPS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans (inline). Steps use `- [ ]`.

**Goal:** Optional host-Caddy HTTPS reverse proxy in front of the loopback operator HTTP for the Portunus server install, with a mandatory DNS precheck.

**Architecture:** Edit `scripts/install.sh` only. Add globals/flags, i18n keys, helpers (`valid_fqdn`, `dns_points_here`, `ensure_caddy`, `write_caddy_block`, `setup_caddy_domain`, `lifecycle_domain`), wire into `parse_args`, `dispatch_verb`, the wizard, summary, uninstall. Network-free assertions in `scripts/install.test.sh`; VPS does the real ACME smoke with `serverbee-test.900040.xyz`.

**Tech Stack:** Bash 4+, curl, getent/dig, Caddy (apt Cloudsmith / dnf-yum COPR), systemd, shellcheck.

---

### Task 1: globals, flags, i18n, test seams

**Files:** `scripts/install.sh`

- [ ] **Step 1: Globals** — after `DETECTED_PROV=""` line add:

```bash
DOMAIN=""             # optional HTTPS domain for host Caddy
ACME_EMAIL=""         # optional Let's Encrypt account email
SKIP_DNS_CHECK="no"   # --skip-dns-check
CADDYFILE="/etc/caddy/Caddyfile"
```

- [ ] **Step 2: EN i18n** — after `[val_docker]=...` in `MSG_EN` add:

```bash
  [ask_domain]="HTTPS domain for the web UI (blank = skip Caddy/HTTPS): "
  [sum_domain]="  https domain:         %s"
  [bad_domain]="invalid domain '%s' — expected an FQDN like portunus.example.com"
  [dns_check]="Checking %s resolves to this server (%s)…"
  [dns_ok]="DNS OK: %s → %s"
  [dns_mismatch]="DNS for %s does not point here. A record(s): %s ; this server: %s"
  [dns_help]="Add this DNS record, then press Enter to re-check (Ctrl-C to abort):\n  %s  A  %s"
  [caddy_installing]="Installing Caddy…"
  [caddy_done]="Caddy configured for %s"
  [caddy_verify]="Verifying https://%s/ (Let's Encrypt issuance can take ~30s)…"
  [caddy_verify_warn]="Could not verify https://%s/ yet. Check: journalctl -u caddy -e ; DNS propagation."
  [https_ready]="HTTPS ready: https://%s/"
  [https_public_note]="Note: the web UI is now publicly reachable over HTTPS; it stays protected by operator login/token."
```

- [ ] **Step 3: ZH i18n** — after `[val_docker]=...` in `MSG_ZH` add (natural phrasing):

```bash
  [ask_domain]="Web UI 的 HTTPS 域名（留空则跳过 Caddy/HTTPS）: "
  [sum_domain]="  HTTPS 域名：%s"
  [bad_domain]="无效的域名：'%s'（需为完整域名，如 portunus.example.com）"
  [dns_check]="正在检查 %s 是否解析到本机（%s）…"
  [dns_ok]="DNS 校验通过：%s → %s"
  [dns_mismatch]="%s 的解析未指向本机。A 记录：%s ；本机公网 IP：%s"
  [dns_help]="请添加以下 DNS 记录，然后按回车重新校验（Ctrl-C 取消）：\n  %s  A  %s"
  [caddy_installing]="正在安装 Caddy…"
  [caddy_done]="Caddy 已为 %s 配置完成"
  [caddy_verify]="正在验证 https://%s/（Let's Encrypt 签发约需 30 秒）…"
  [caddy_verify_warn]="暂时无法验证 https://%s/。请检查：journalctl -u caddy -e；以及 DNS 是否已生效。"
  [https_ready]="HTTPS 已就绪：https://%s/"
  [https_public_note]="提示：Web UI 现已通过 HTTPS 公开可访问，仍由运维登录/令牌保护。"
```

- [ ] **Step 4: Parse flags** — in `parse_args()` after the `--advertised-endpoint) … ;;` line add:

```bash
      --domain) shift; [ $# -gt 0 ] || die "--domain needs a value"; DOMAIN="$1" ;;
      --acme-email) shift; [ $# -gt 0 ] || die "--acme-email needs a value"; ACME_EMAIL="$1" ;;
      --skip-dns-check) SKIP_DNS_CHECK="yes" ;;
```

- [ ] **Step 5: Add `domain` verb to the verb case** — change line `install|uninstall|upgrade|status|service|config|env) VERB="$1" ;;` to include `domain`:

```bash
      install|uninstall|upgrade|status|service|config|env|domain) VERB="$1" ;;
```

- [ ] **Step 6: Test seams** — after the `--reset-lang) … ;;` line add:

```bash
      --valid-fqdn) shift; valid_fqdn "${1:-}" && exit 0 || exit 1 ;;
      --render-caddy) shift; DOMAIN="${1:-}"; render_caddy_block "${2:-7080}"; exit 0 ;;
```

- [ ] **Step 7: help text** — append `[--domain FQDN] [--acme-email A] [--skip-dns-check]` and `|domain` to the `-h|--help` usage string.

- [ ] **Step 8: shellcheck + commit**

Run: `shellcheck -s bash -S warning scripts/install.sh`
Expected: clean (helpers defined in Task 2; bash resolves at call time).

```bash
git add scripts/install.sh
git commit -m "feat(install): Caddy HTTPS globals, flags, i18n, seams"
```

---

### Task 2: helpers — valid_fqdn, DNS precheck, ensure_caddy, Caddyfile, setup

**Files:** `scripts/install.sh` (add a `# ─── Caddy ───` section directly above `wizard_install()`)

- [ ] **Step 1: Add the section**

```bash
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
  local d="$1" i
  t caddy_verify "$d"; echo
  for i in $(seq 1 12); do
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
  [ -n "$DOMAIN" ] || DOMAIN="$CONFIG_KEY"   # `domain <fqdn>` parsed as config-ish positional
  [ -n "$DOMAIN" ] || die "usage: install.sh domain <fqdn>"
  OP_HTTP_LISTEN="$(meta_read "$mf" op_http_listen 2>/dev/null || echo '')"
  setup_caddy_domain
  [ "$DRY_RUN" = yes ] || meta_write "$mf" "$(meta_kv_from "$mf")" "domain=$DOMAIN"
}
```

Note: `meta_kv_from` does not exist; simplify — `lifecycle_domain` instead
appends/refreshes the domain by rewriting meta via the existing helper.
Replace the last two lines of `lifecycle_domain` with:

```bash
  setup_caddy_domain
  if [ "$DRY_RUN" != yes ]; then
    DEPLOY="$(meta_read "$mf" deploy || echo binary)"
    meta_write "$mf" "role=$ROLE" "deploy=$DEPLOY" \
      "version=$(meta_read "$mf" version || echo '?')" \
      "lang=${LANG_CODE:-en}" \
      "advertised_endpoint_set=$(meta_read "$mf" advertised_endpoint_set || echo no)" \
      "domain=$DOMAIN"
  fi
```

- [ ] **Step 2: positional fqdn for the `domain` verb** — in `parse_args()` the catch-all `*)` currently only fills config key/value. Extend so a bare token after `VERB=domain` is captured. Find:

```bash
      *) if [ "$VERB" = config ] && [ -z "$CONFIG_KEY" ]; then CONFIG_KEY="$1"; elif [ "$VERB" = config ] && [ -z "$CONFIG_VALUE" ]; then CONFIG_VALUE="$1"; else die "unknown argument: $1"; fi ;;
```

Replace with:

```bash
      *) if [ "$VERB" = domain ] && [ -z "$DOMAIN" ]; then DOMAIN="$1";
         elif [ "$VERB" = config ] && [ -z "$CONFIG_KEY" ]; then CONFIG_KEY="$1";
         elif [ "$VERB" = config ] && [ -z "$CONFIG_VALUE" ]; then CONFIG_VALUE="$1";
         else die "unknown argument: $1"; fi ;;
```

- [ ] **Step 3: shellcheck + commit**

Run: `shellcheck -s bash -S warning scripts/install.sh`
Expected: clean.

```bash
git add scripts/install.sh
git commit -m "feat(install): Caddy helpers — fqdn, DNS precheck, install, Caddyfile, setup"
```

---

### Task 3: wire install / wizard / summary / uninstall / dispatch

**Files:** `scripts/install.sh`

- [ ] **Step 1: dispatch the `domain` verb** — change:

```bash
    uninstall|upgrade|status|service|config|env) lifecycle_"$VERB" ;;
```
to
```bash
    uninstall|upgrade|status|service|config|env|domain) lifecycle_"$VERB" ;;
```

- [ ] **Step 2: run Caddy after a server install with --domain/wizard** — in `dispatch_verb` install branch, immediately AFTER the binary-branch `meta_write …` line and the docker path, before `echo; print_next_steps`, add:

```bash
      if [ "$ROLE" = server ] && [ -n "$DOMAIN" ]; then
        meta_write "$(meta_path_for)" "role=$ROLE" "deploy=$DEPLOY" "version=$resolved_version" "lang=${LANG_CODE:-en}" "advertised_endpoint_set=$([ -n "$ADVERTISED" ] && echo yes || echo no)" "domain=$DOMAIN"
        setup_caddy_domain
      fi
```

(Place it so it runs for both deploy forms; `meta_path_for` resolves the
right `.install-meta`. The extra meta_write refreshes with `domain=`.)

- [ ] **Step 3: wizard prompt** — in `wizard_install` server branch, after the advertised-endpoint `while` loop and before `detect_platform`, add:

```bash
    while :; do
      DOMAIN="$(ask ask_domain)"
      [ -z "$DOMAIN" ] && break
      valid_fqdn "$DOMAIN" && break
      t bad_domain "$DOMAIN"; echo
    done
```

- [ ] **Step 4: summary line** — in `print_install_summary`, inside the `if [ "$ROLE" = server ]` block after the advertised line, add:

```bash
    [ -n "$DOMAIN" ] && { t sum_domain "$DOMAIN"; echo; }
```

- [ ] **Step 5: uninstall removes the managed block** — in `lifecycle_uninstall`, after the deploy-form removal and before the purge block, add:

```bash
  if [ -f "$CADDYFILE" ] && grep -q '^# >>> portunus >>>$' "$CADDYFILE" 2>/dev/null; then
    sudo cp "$CADDYFILE" "${CADDYFILE}.portunus.$(date +%Y%m%d%H%M%S).bak"
    sudo sed -i '/^# >>> portunus >>>$/,/^# <<< portunus <<<$/d' "$CADDYFILE"
    command -v systemctl >/dev/null 2>&1 && sudo systemctl reload caddy 2>/dev/null || true
    echo "→ removed Caddy block from $CADDYFILE"
  fi
```

- [ ] **Step 6: client + --domain guard** — in `dispatch_verb` install branch top (after role/deploy resolved), add:

```bash
      [ -n "$DOMAIN" ] && [ "$ROLE" != server ] && die "--domain is server-only"
```

- [ ] **Step 7: shellcheck + commit**

Run: `shellcheck -s bash -S warning scripts/install.sh`
Expected: clean.

```bash
git add scripts/install.sh
git commit -m "feat(install): wire Caddy into install/wizard/summary/uninstall/domain verb"
```

---

### Task 4: network-free tests

**Files:** `scripts/install.test.sh` (before the shellcheck block)

- [ ] **Step 1: Add assertions**

```bash
# --- Caddy: FQDN validation ---
bash "$script" --valid-fqdn serverbee-test.900040.xyz || fail "valid fqdn rejected"
if bash "$script" --valid-fqdn no_dot 2>/dev/null; then fail "fqdn without dot accepted"; fi
if bash "$script" --valid-fqdn "bad host.com" 2>/dev/null; then fail "fqdn with space accepted"; fi
if bash "$script" --valid-fqdn "-lead.com" 2>/dev/null; then fail "fqdn leading dash accepted"; fi
if bash "$script" --valid-fqdn "" 2>/dev/null; then fail "empty fqdn accepted"; fi

# --- Caddy: managed block render ---
cb="$(bash "$script" --render-caddy serverbee-test.900040.xyz 7080)" || fail "render-caddy exit"
echo "$cb" | grep -qx '# >>> portunus >>>' || fail "missing start marker"
echo "$cb" | grep -qx '# <<< portunus <<<' || fail "missing end marker"
echo "$cb" | grep -qx 'serverbee-test.900040.xyz {' || fail "missing site line"
echo "$cb" | grep -q 'reverse_proxy 127.0.0.1:7080' || fail "missing reverse_proxy"

# --- Caddy: server dry-run plan includes domain; client+--domain errors ---
od="$(bash "$script" server --domain serverbee-test.900040.xyz --skip-dns-check --dry-run 2>&1)" || fail "server --domain dry-run exit"
echo "$od" | grep -q '^role:[[:space:]]*server$' || fail "domain dry-run role"
if bash "$script" client --domain x.example.com --dry-run >/dev/null 2>&1; then fail "client --domain must error"; fi

# --- Caddy: domain verb dry-run writes nothing, exits 0 ---
bash "$script" domain serverbee-test.900040.xyz --skip-dns-check --dry-run >/dev/null 2>&1 || fail "domain verb dry-run"
```

- [ ] **Step 2: run full harness + shellcheck + parity**

Run: `bash scripts/install.test.sh && shellcheck -s bash -S warning scripts/install.sh scripts/install.test.sh && diff <(bash scripts/install.sh --print-i18n-keys en|sort) <(bash scripts/install.sh --print-i18n-keys zh|sort) && echo OKALL`
Expected: `PASS` … `OKALL`.

- [ ] **Step 3: commit**

```bash
git add scripts/install.test.sh
git commit -m "test(install): Caddy fqdn/block/plan network-free assertions"
```

---

### Task 5: VPS smoke (domain serverbee-test.900040.xyz → 207.241.173.217)

**Files:** none (remote). Helper scripts `/tmp/vps.sh`, `/tmp/vpstty.sh`.

- [ ] **Step 1** Upload `install.sh`+`install.test.sh` to `/root/smoke/`; `bash install.test.sh` ⇒ PASS.
- [ ] **Step 2** DNS precheck: `--render-caddy`/`--valid-fqdn` on VPS; `dns_points_here` indirectly via a real `--domain` server install.
- [ ] **Step 3** server **binary** install `install server --yes --systemd --advertised-endpoint 207.241.173.217:7443 --domain serverbee-test.900040.xyz` ⇒ Caddy installed, `/etc/caddy/Caddyfile` has the marked block + `reverse_proxy 127.0.0.1:7080`, `systemctl is-active caddy` = active, `curl -fsS https://serverbee-test.900040.xyz/` returns 200 with a valid (non-self-signed) cert (`curl` without `-k`), meta has `domain=`. Capture `journalctl -u caddy` on failure.
- [ ] **Step 4** `install.sh domain serverbee-test.900040.xyz` re-run idempotent: exactly one portunus block; a pre-existing unrelated `:8888 { respond "x" }` site added before re-run survives.
- [ ] **Step 5** bogus domain `install.sh domain nope.invalid.serverbee-test.900040.xyz` (no A record) ⇒ guided DNS failure non-zero; `--skip-dns-check` makes setup proceed (cert will fail → warn, non-fatal, exit 0).
- [ ] **Step 6** purge the binary server; `uninstall` removed the portunus Caddy block and reloaded caddy; `https://…/` no longer served by portunus; caddy package still present.
- [ ] **Step 7** server **docker** install `--deploy docker --compose-dir /root/dk --domain serverbee-test.900040.xyz --yes` ⇒ same HTTPS end state (Caddy → 127.0.0.1:7080 → container); cleanup `uninstall --purge`.
- [ ] **Step 8** interactive wizard (PTY): server, accept detected IP, enter domain at the new prompt ⇒ HTTPS works; a second run with blank domain skips Caddy entirely (no Caddyfile change).
- [ ] **Step 9** regression: `install server --yes --systemd --advertised-endpoint …` with NO `--domain` does not touch Caddy/Caddyfile; `--dry-run` zero network/writes. Full host cleanup (binaries, units, drop-ins, containers, volumes, the portunus Caddy block, optionally `apt-get remove -y caddy`, /root/smoke, /root/dk, ~/.config/portunus).
- [ ] **Step 10** commit any smoke-driven fixes.

---

## Self-Review

**Spec coverage:** what-Caddy-fronts → T2 `write_caddy_block`/`op_http_port`. DNS precheck → T2 `dns_points_here` (interactive re-check + non-interactive die + `--skip-dns-check`). Caddy install → T2 `ensure_caddy` (apt/dnf/yum, manual fallback). Idempotent Caddyfile block + backup → T2 `write_caddy_block`. setup flow + dry-run plan → T2 `setup_caddy_domain`. Interface (flags/verb/wizard/summary/meta) → T1, T3. uninstall block removal → T3. i18n parity → T1. Testing → T4 (network-free) + T5 (VPS ACME). Non-goals respected (gRPC untouched; no Caddy removal beyond block).

**Placeholder scan:** none — Task 2 Step 1 flags the `meta_kv_from` dead-end inline and gives the concrete replacement.

**Type/name consistency:** globals `DOMAIN/ACME_EMAIL/SKIP_DNS_CHECK/CADDYFILE` (T1) used by every helper (T2) and the wiring (T3). Helper names stable: `valid_fqdn`, `op_http_port`, `dns_a_records`, `dns_points_here`, `render_caddy_block`, `ensure_caddy`, `write_caddy_block`, `caddy_reload`, `verify_https`, `setup_caddy_domain`, `lifecycle_domain`. Seams `--valid-fqdn`/`--render-caddy` (T1) call helpers defined T2 (bash resolves at call time; seams run via `main` after sourcing). `read_tty`/`detect_public_ip`/`meta_read`/`current_meta_file`/`meta_path_for` are pre-existing.
