# Install Wizard UX Refactor Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Reduce the interactive install wizard to role + deploy form + (server) advertised endpoint, default everything else, pre-fill the endpoint from a probed public IP, and show a summary block before confirm.

**Architecture:** Edit `scripts/install.sh` only. Add three pure-ish helpers (`detect_public_ip`, `print_install_summary`, plus i18n keys), rewrite `wizard_install()`. Non-interactive parsing, CLI defaults, and `--dry-run` output are untouched. Network-free assertions go in `scripts/install.test.sh`; VPS does the real smoke.

**Tech Stack:** Bash 4+, curl, systemd/docker (runtime only), shellcheck.

---

### Task 1: i18n keys + `--detect-ip` seam

**Files:**
- Modify: `scripts/install.sh` (MSG_EN block ~line 101, MSG_ZH block ~line 132, seam block ~line 443)

- [ ] **Step 1: Add EN i18n keys**

In `MSG_EN=( … )`, after the `[op_cancelled]=…` line, add:

```bash
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
```

- [ ] **Step 2: Add ZH i18n keys (same keys, parity)**

In `MSG_ZH=( … )`, after its `[op_cancelled]=…` line, add:

```bash
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
```

- [ ] **Step 3: Add the `--detect-ip` seam**

In `parse_args()`, immediately after the `--detect-deploy) … exit 0 ;;` line, add:

```bash
      --detect-ip) detect_public_ip; printf '%s %s\n' "$DETECTED_IP" "$DETECTED_PROV"; exit 0 ;;
```

- [ ] **Step 4: Run shellcheck**

Run: `shellcheck -s bash -S warning scripts/install.sh`
Expected: clean (seam references `detect_public_ip`/globals defined in Task 2; shellcheck does not flag forward function refs in bash, but if it warns SC2154 on the globals, proceed — Task 2 declares them).

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh
git commit -m "feat(install): wizard summary + endpoint i18n keys, --detect-ip seam"
```

---

### Task 2: `detect_public_ip()` with fallback chain

**Files:**
- Modify: `scripts/install.sh` (add globals near other globals ~line 54; add function just above `wizard_install()` ~line 499)

- [ ] **Step 1: Declare globals**

After the `target=""` global declaration line, add:

```bash
DETECTED_IP=""        # last detect_public_ip() result
DETECTED_PROV=""      # provenance i18n key: prov_detected|prov_nic|prov_loopback
```

- [ ] **Step 2: Add `detect_public_ip()` (place directly above `wizard_install()`)**

```bash
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
      ip="$(curl -fsS --max-time 3 "$u" 2>/dev/null | tr -d '[:space:]')"
      if valid_ip "$ip"; then DETECTED_IP="$ip"; DETECTED_PROV="prov_detected"; return 0; fi
    done
  fi
  ip="$(ip route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -1)"
  [ -z "$ip" ] && ip="$(hostname -I 2>/dev/null | tr ' ' '\n' | grep -v '^127\.' | head -1)"
  if valid_ip "$ip"; then DETECTED_IP="$ip"; DETECTED_PROV="prov_nic"; return 0; fi
  DETECTED_IP="127.0.0.1"; DETECTED_PROV="prov_loopback"; return 0
}
```

- [ ] **Step 3: Network-free probe test (skip path)**

Append to `scripts/install.test.sh` before the shellcheck block:

```bash
# --- wizard: IP detection seam, offline path never hits network ---
di="$(PORTUNUS_SKIP_IP_PROBE=1 bash "$script" --detect-ip)" || fail "--detect-ip exit"
echo "$di" | grep -Eq '^[0-9a-fA-F.:]+ prov_(nic|loopback)$' || fail "skip-probe must yield NIC/loopback ($di)"
```

- [ ] **Step 4: Run test + shellcheck**

Run: `bash scripts/install.test.sh && shellcheck -s bash -S warning scripts/install.sh scripts/install.test.sh`
Expected: `PASS` and clean.

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(install): detect_public_ip with public/NIC/loopback fallback"
```

---

### Task 3: `print_install_summary()`

**Files:**
- Modify: `scripts/install.sh` (add function just above `wizard_install()`, after `detect_public_ip`)

- [ ] **Step 1: Add the function**

```bash
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
```

- [ ] **Step 2: Run shellcheck**

Run: `shellcheck -s bash -S warning scripts/install.sh`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add scripts/install.sh
git commit -m "feat(install): print_install_summary for pre-confirm review"
```

---

### Task 4: Rewrite `wizard_install()` (minimal flow)

**Files:**
- Modify: `scripts/install.sh:500-519` (`wizard_install`)

- [ ] **Step 1: Replace the function body**

Replace the whole `wizard_install() { … }` with:

```bash
wizard_install() {
  local a adv_prov=""
  a="$(ask ask_role)"; case "$a" in 2) ROLE=client ;; *) ROLE=server ;; esac
  a="$(ask ask_deploy)"; case "$a" in 2) DEPLOY=docker ;; *) DEPLOY=binary; WANT_SYSTEMD=yes ;; esac
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
```

Notes: `VERSION` stays "" ⇒ latest; `BIN_DIR` stays `$DEFAULT_BIN_DIR`
(its global default); `DATA_DIR`/`OP_HTTP_LISTEN` stay "" ⇒ install code
and drop-in already substitute `/var/lib/portunus` / `127.0.0.1:7080`.
No behavior change to `dispatch_verb`/`install_*`.

- [ ] **Step 2: Drive the minimal wizard via the menu seam**

Append to `scripts/install.test.sh` before the shellcheck block:

```bash
# --- minimal wizard: server+binary asks only role/deploy/endpoint ---
wo="$(printf '1\n1\n-\nn\n0\n' | PORTUNUS_SKIP_IP_PROBE=1 PORTUNUS_LANG=en bash "$script" --menu-stdin 2>&1)" || true
printf '%s\n' "$wo" | grep -q 'About to install:' || fail "no summary block"
printf '%s\n' "$wo" | grep -q 'data dir:.*\/var\/lib\/portunus' || fail "summary missing data-dir default"
printf '%s\n' "$wo" | grep -q 'operator http:.*127\.0\.0\.1:7080' || fail "summary missing op-http default"
printf '%s\n' "$wo" | grep -qi 'loopback' || fail "'-' input should mark loopback"
printf '%s\n' "$wo" | grep -q 'Version (blank = latest)' && fail "wizard still asks version"
printf '%s\n' "$wo" | grep -q 'Server data dir' && fail "wizard still asks data-dir"

# client: only role+deploy, no endpoint/summary advertised line
co="$(printf '2\n1\nn\n0\n' | PORTUNUS_SKIP_IP_PROBE=1 PORTUNUS_LANG=en bash "$script" --menu-stdin 2>&1)" || true
printf '%s\n' "$co" | grep -q 'About to install:' || fail "client no summary"
printf '%s\n' "$co" | grep -q 'advertised endpoint:' && fail "client must not show advertised line"
```

- [ ] **Step 3: Run tests + shellcheck**

Run: `bash scripts/install.test.sh && shellcheck -s bash -S warning scripts/install.sh scripts/install.test.sh`
Expected: `PASS` and clean.

- [ ] **Step 4: Confirm dry-run output unchanged (regression)**

Run: `bash scripts/install.sh server --version 1.0.0 --systemd --advertised-endpoint h:7443 --data-dir /tmp/x --dry-run | grep -E '^(role|deploy|drop-in):'`
Expected: same `role:`/`deploy:`/`drop-in:` lines as before (non-interactive path untouched).

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(install): minimal wizard — role/deploy/endpoint, defaults summarized"
```

---

### Task 5: Full local gate

**Files:** none (verification only)

- [ ] **Step 1: Full harness + lint**

Run: `bash scripts/install.test.sh && shellcheck -s bash -S warning scripts/install.sh scripts/install.test.sh && echo ALLGREEN`
Expected: `PASS` … `ALLGREEN`.

- [ ] **Step 2: i18n parity sanity**

Run: `diff <(bash scripts/install.sh --print-i18n-keys en|sort) <(bash scripts/install.sh --print-i18n-keys zh|sort) && echo PARITY_OK`
Expected: `PARITY_OK`.

---

### Task 6: VPS smoke (covers every point)

**Files:** none (remote verification). VPS: `207.241.173.217` root.

- [ ] **Step 1: Upload + harness on VPS**

Upload `scripts/install.sh` + `scripts/install.test.sh` to `/root/smoke/`; run `bash /root/smoke/install.test.sh` ⇒ `PASS`.

- [ ] **Step 2: server binary — public IP probe pre-fill + Enter accepts**

Drive the menu over `ssh -tt` (real PTY): role=1, deploy=1, Enter at endpoint (accept detected), `y` at confirm. Verify: probe filled a public IP into the default; summary block shows it `(detected public IP)`; binary installed; `/var/lib/portunus/.install-meta` written; drop-in has `PORTUNUS_ADVERTISED_ENDPOINT=<ip>:7443`, `PORTUNUS_DATA_DIR=/var/lib/portunus`, `PORTUNUS_OPERATOR_HTTP_LISTEN=127.0.0.1:7080` (Fix-1 parity still holds with defaults). Then purge-clean.

- [ ] **Step 3: server binary — `-` ⇒ loopback**

Drive: role=1, deploy=1, `-` at endpoint, `y`. Verify summary shows `(loopback — local only)`, install proceeds, drop-in has no `PORTUNUS_ADVERTISED_ENDPOINT`. Purge-clean.

- [ ] **Step 4: server docker — defaults + compose-dir = cwd**

`cd /root/dk`; drive role=1, deploy=2, Enter endpoint, `y`. Verify `compose dir:` in summary = `/root/dk`, `.env` has the endpoint, container Up. `uninstall --purge` clean.

- [ ] **Step 5: client binary — only role/deploy asked**

Drive role=2, deploy=1, `y`. Verify no endpoint prompt, no advertised line, client binary + `/etc/portunus/.install-meta` present. Uninstall clean.

- [ ] **Step 6: `PORTUNUS_SKIP_IP_PROBE=1` ⇒ no external call**

`PORTUNUS_SKIP_IP_PROBE=1 … --detect-ip` on the VPS returns a NIC IP with `prov_nic` (VPS has a routable NIC); confirm wizard default uses it without any curl.

- [ ] **Step 7: Regression — non-interactive unchanged**

`bash /root/smoke/install.sh install server --yes --systemd --advertised-endpoint 1.2.3.4:7443 --data-dir /var/lib/portunus` still works exactly as before (no wizard, no probe); `--dry-run` output identical to pre-refactor. Final full cleanup of the VPS (binaries, units, drop-ins, containers, volumes, users, /root/smoke, /root/dk, ~/.config/portunus).

- [ ] **Step 8: Commit any test tweaks discovered during smoke**

```bash
git add -A && git commit -m "test(install): smoke-driven wizard assertions"
```

---

## Self-Review

**Spec coverage:** §1 minimal flow → Task 4. §2 defaults+summary → Tasks 1,3,4. §3 IP detection+fallback+`PORTUNUS_SKIP_IP_PROBE` → Task 2. §4 testing (seam, menu-driven, parity, shellcheck, VPS) → Tasks 2,4,5,6. Non-goals respected: no advanced tier, non-interactive untouched (Task 4 Step 4 regression), no probe outside server wizard (probe call only in `wizard_install` server branch + the explicit `--detect-ip` test seam).

**Placeholder scan:** none — every code step is complete.

**Type/name consistency:** `DETECTED_IP`/`DETECTED_PROV` declared Task 2 Step 1, used in `detect_public_ip` (T2), `--detect-ip` seam (T1 — defined before its producer but bash resolves at call time; seam only runs via `main`/`parse_args` after sourcing), `wizard_install` (T4), `print_install_summary` (T3) consumes the `adv_prov` arg whose values are i18n keys `prov_detected|prov_nic|prov_loopback|prov_user` all defined T1. i18n keys used in T3/T4 (`ask_advertised_pub`, `summary_title`, `sum_*`, `prov_*`) all added in T1 to both tables.
