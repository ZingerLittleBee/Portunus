# Flag-only install.sh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn `scripts/install.sh` into a pure flag-driven CLI — delete the interactive menu/wizard and the bilingual i18n table, replace every prompt with a flag or deterministic default, and document the full flag surface.

**Architecture:** A single linear pipeline: `parse_args` → cross-flag guards → `dispatch_verb` → the unchanged executor/lifecycle layer. The redesign deletes only the *input* layer (menu/wizard/prompts) and the *output* layer (i18n); the install/service/config/Caddy executors are untouched. Work proceeds in four script tasks ordered so the tree stays green after each (the script uses `set -eu`, so each variable's initializer is removed in the same task as its last reference), then docs and changelog.

**Tech Stack:** POSIX `sh` (must run under `dash` and busybox `ash`; `local` is the only non-POSIX builtin, `shellcheck -s sh -S warning` must stay clean). Test harness `scripts/install.test.sh` is bash (`set -euo pipefail`, runs the script under `$SH`, default bash; `TEST_SH=dash` for the POSIX pass).

**Reference spec:** `docs/superpowers/specs/2026-06-16-install-flag-only-redesign-design.md`

**The gate (run after every task):**

```sh
shellcheck -s sh -S warning scripts/install.sh \
  && bash scripts/install.test.sh \
  && TEST_SH=dash bash scripts/install.test.sh
```

Expected on success: shellcheck silent, both harness runs end with `PASS`.

---

## Task 1: Remove the interactive menu + install wizard

Deletes the entire TTY-driven UI. Keeps `t()` (still called by lifecycle/install code) and `read_tty`/`confirm` (rewired in Task 2). Keeps the `MENU_FORCE_STDIN="no"` initializer because `read_tty`/`confirm` still reference it until Task 2; only the `--menu-stdin` arm that could set it to `yes` is removed.

**Files:**
- Modify: `scripts/install.sh` (delete UI functions, rewire `main`, drop `--menu-stdin`)
- Modify: `scripts/install.test.sh` (delete menu/wizard tests; de-seam config tests)

- [ ] **Step 1: Delete the wizard + menu functions**

In `scripts/install.sh`, delete these functions in full (each from its `name() {`/`name()` line through its closing `}`):

- `is_interactive` (the `is_interactive() { … return 1; }` block)
- `print_install_summary`
- `ask`
- `first_run_lang`
- `build_equiv_cmd`
- `run_install_flow`
- `reset_menu_state`
- `pause`
- `read_menu`
- `menu_service`
- `menu_config`
- `menu_screen_install`
- `menu_screen_manage`
- `menu_screen_main`
- `run_menu`

Also delete the two menu-state globals (they are referenced only by the deleted functions):

```sh
MENU_SCREEN=""
REPLY_MENU=""
```

Leave `MENU_FORCE_STDIN="no"` in place for now.

- [ ] **Step 2: Rewire `main()` no-command behavior**

In `main()`, replace this block:

```sh
  # No actionable verb/role and a TTY ⇒ interactive menu (Task 7).
  if [ -z "$VERB" ] && [ -z "$ROLE" ]; then
    if is_interactive; then run_menu; exit $?; fi
    die "no command given and no terminal; run 'install.sh -h' or pass a role"
  fi
```

with:

```sh
  # No actionable verb/role: this is a flag-only CLI — print usage and exit.
  if [ -z "$VERB" ] && [ -z "$ROLE" ]; then
    print_usage >&2
    exit 2
  fi
```

(Leave the `resolve_lang` call in `main()` for now; it is removed in Task 3.)

- [ ] **Step 3: Remove the `--menu-stdin` parse arm**

In `parse_args`, delete this line:

```sh
      --menu-stdin) MENU_FORCE_STDIN="yes" ;;  # defer to main so later --compose-dir et al. still parse
```

- [ ] **Step 4: Delete the menu/wizard tests from the harness**

In `scripts/install.test.sh`, delete these test blocks entirely (identified by their leading comment):

- `# --- smart routing: a clean host lands on the install wizard …` through the end of the `[m] …` assertion (the block creating `emptyd`, but KEEP the `emptyd="$(mktemp -d)"` line if later blocks reuse it — see Step 5).
- `# --- smart routing: an existing install lands on the manage menu ---` (the `seededd` block).
- `# --- P1#2: a die() inside a menu action … ---` (`mo=` block).
- `# --- P2#4: invalid menu choice … ---` (`io=` block).
- `# --- intent wizard: [2]=server … ---` through `# --- recommended deploy default differs by role … ---` inclusive (all `wo=`/`sa=`/`co=`/`so=`/`ro=` blocks) ending at `rm -rf "$emptyd"`.

Because `emptyd` is created in the first deleted block and removed in the last, delete the now-orphaned `emptyd="$(mktemp -d)"` creation and its `rm -rf "$emptyd"` too.

- [ ] **Step 5: De-seam the docker/binary config tests (drop `--menu-stdin`)**

The config round-trip tests answered the old restart prompt via `printf 'n\n' | … --menu-stdin`. With `--menu-stdin` gone (and no TTY in CI), the restart `confirm` auto-answers "no" and the write still happens, so the prompt plumbing is unnecessary. In `scripts/install.test.sh`, for every `config set` invocation, remove the leading `printf 'n\n' | ` and the `--menu-stdin ` flag. Concretely, transform each occurrence of the form:

```sh
printf 'n\n' | $SH "$script" --menu-stdin --compose-dir "$X" config set KEY VALUE >/dev/null 2>&1 \
```

into:

```sh
$SH "$script" --compose-dir "$X" config set KEY VALUE >/dev/null 2>&1 \
```

Apply to all such lines: the `dk_tmp`, `dy_tmp`, `db_tmp`, `bs_root`/`bs_tmp`, and `or_root`/`or_tmp` blocks (advertised-endpoint, data-dir reject, operator-http-listen, JSON-breakout reject, openrc data-dir). The surrounding `PORTUNUS_TEST_CONFIG_ROOT=…` prefixes stay.

- [ ] **Step 6: Run the gate**

Run:

```sh
shellcheck -s sh -S warning scripts/install.sh \
  && bash scripts/install.test.sh \
  && TEST_SH=dash bash scripts/install.test.sh
```

Expected: shellcheck silent; both harness runs print `PASS`.

- [ ] **Step 7: Commit**

```sh
git add scripts/install.sh scripts/install.test.sh
git commit -m "refactor(install): remove interactive menu and install wizard"
```

---

## Task 2: Replace confirmation prompts with flags/defaults

Removes every TTY confirmation. `upgrade`/`uninstall` proceed immediately; `--purge` is the sole data-deletion gate; `config set` restarts only with the new `--restart`; a DNS mismatch is a hard error. Deletes `confirm`, `read_tty`, `ASSUME_YES`, `MENU_FORCE_STDIN`, and the `--yes` flag.

**Files:**
- Modify: `scripts/install.sh`
- Modify: `scripts/install.test.sh`

- [ ] **Step 1: Add the `--restart` flag and global**

In `scripts/install.sh`, near the other intent globals (where `ASSUME_YES="no"` is defined), add:

```sh
RESTART="no"
```

In `parse_args`, replace the `--yes` arm:

```sh
      --yes) ASSUME_YES="yes" ;;
```

with:

```sh
      --restart) RESTART="yes" ;;
```

Delete the `ASSUME_YES="no"` initializer line.

- [ ] **Step 2: Make `upgrade` proceed without confirmation**

In `lifecycle_upgrade`, delete this line:

```sh
  confirm "$(t confirm_proceed)" || return 0
```

- [ ] **Step 3: Make `uninstall` proceed without confirmation; `--purge` is the only gate**

In `lifecycle_uninstall`, delete this line:

```sh
  if [ "$ASSUME_YES" != yes ]; then confirm "$(t confirm_uninstall "$r" "$d")" no || return 0; fi
```

Then replace the purge block:

```sh
  if [ "$PURGE" = yes ]; then
    local dd; dd="$(dirname "$mf")"
    read_tty "$(t confirm_purge_typed "$dd")" || REPLY_TTY=""
    [ "$REPLY_TTY" = "purge" ] && { sudo rm -rf "$dd"; echo "→ purged $dd"; } || echo "purge skipped"
  fi
```

with:

```sh
  if [ "$PURGE" = yes ]; then
    local dd; dd="$(dirname "$mf")"
    sudo rm -rf "$dd"; echo "→ purged $dd"
  fi
```

- [ ] **Step 4: Gate the docker `config set` restart behind `--restart`**

In `lifecycle_config` (docker branch), replace:

```sh
    # `compose restart` keeps the old command; only `up -d` recreates with it.
    if confirm "$(t restart_now)" no; then ( cd "$dir" && $(compose_cmd) up -d ); fi
    return 0
```

with:

```sh
    # `compose restart` keeps the old command; only `up -d` recreates with it.
    if [ "$RESTART" = yes ]; then ( cd "$dir" && $(compose_cmd) up -d )
    else echo "→ restart to apply: re-run with --restart (recreates the container)"; fi
    return 0
```

- [ ] **Step 5: Gate the binary `config set` restart behind `--restart`**

In `lifecycle_config` (binary branch, the last lines of the function), replace:

```sh
  if confirm "$(t restart_now)" no; then SERVICE_ACTION=restart; lifecycle_service; fi
```

with:

```sh
  if [ "$RESTART" = yes ]; then SERVICE_ACTION=restart; lifecycle_service
  else echo "→ restart to apply: install.sh service restart (or re-run with --restart)"; fi
```

- [ ] **Step 6: Rewrite `dns_points_here` as a single fail-closed check**

Replace the whole `dns_points_here` function:

```sh
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
```

with:

```sh
dns_points_here() {  # $1 domain ; uses detect_public_ip
  local d="$1" a ip
  detect_public_ip; ip="$DETECTED_IP"
  printf 'Checking %s resolves to this server (%s)…\n' "$d" "$ip"
  a="$(dns_a_records "$d" | tr '\n' ' ')"
  if printf '%s' "$a" | grep -qw "$ip"; then
    printf 'DNS OK: %s → %s\n' "$d" "$ip"; return 0
  fi
  printf 'DNS for %s does not point here. A record(s): %s ; this server: %s\n' \
    "$d" "${a:-none}" "$ip"
  die "DNS for $d must point to $ip (or pass --skip-dns-check)"
}
```

- [ ] **Step 7: Delete `confirm`, `read_tty`, and the `MENU_FORCE_STDIN` global**

Delete the `confirm()` function in full, the `read_tty()` function in full (and its preceding comment block beginning `# Read one line straight from the terminal…`), and the line:

```sh
MENU_FORCE_STDIN="no"
```

Verify no references remain:

```sh
grep -nE 'confirm|read_tty|ASSUME_YES|MENU_FORCE_STDIN|REPLY_TTY' scripts/install.sh
```

Expected: no matches (other than possibly inside i18n table text, which is removed in Task 3 — there are none).

- [ ] **Step 8: Update the harness — drop the `--yes` test, add `--restart`**

In `scripts/install.test.sh`:

Delete this line:

```sh
$SH "$script" client --version 1.0.0 --yes --dry-run >/dev/null 2>&1 || fail "--yes flag rejected"
```

Add, immediately after the existing `# --- new flags accepted in dry-run plan ---` block (around the `advertised line` assertion):

```sh
# --- --yes is removed: now an unknown argument ---
if $SH "$script" client --version 1.0.0 --yes --dry-run >/dev/null 2>&1; then fail "--yes must now error (removed)"; fi

# --- --restart is accepted on config set (binary/systemd test seam) ---
rs_tmp="$(mktemp -d)"; rs_root="$(mktemp -d)"
printf 'role=server\ndeploy=binary\nversion=2.2.0\ninit=systemd\n' > "$rs_tmp/.install-meta"
PORTUNUS_TEST_CONFIG_ROOT="$rs_root" $SH "$script" --compose-dir "$rs_tmp" config set advertised-endpoint h.example:7443 --restart >/dev/null 2>&1 \
  || fail "--restart accepted on config set"
grep -q -- '--advertised-endpoint h.example:7443' "$rs_root/etc/systemd/system/portunus-server.service.d/10-portunus.conf" \
  || fail "--restart path still writes the drop-in"
rm -rf "$rs_tmp" "$rs_root"
```

(Under `PORTUNUS_TEST_CONFIG_ROOT`, `config_sudo` runs commands directly and `systemctl` is disabled, so `--restart` exercises the parse + write path without a real service.)

- [ ] **Step 9: Run the gate**

Run the gate command. Expected: `PASS` on both harness runs, shellcheck silent.

- [ ] **Step 10: Commit**

```sh
git add scripts/install.sh scripts/install.test.sh
git commit -m "refactor(install): replace confirmation prompts with flags"
```

---

## Task 3: Drop the bilingual i18n table — English-only output

Inlines every surviving `t <key>` call site with its English string, then deletes `t()`, `resolve_lang`, the `I18N_KEYS` manifest, the language cache, `LANG_CODE`, and the `--lang`/`--reset-lang`/`--print-i18n*` flags. Drops the now-unused `lang=` field from `meta_write`.

**Files:**
- Modify: `scripts/install.sh`
- Modify: `scripts/install.test.sh`

- [ ] **Step 1: Inline `place_client_bundle` strings**

Replace:

```sh
    die "$(t enroll_failed)"
```

with:

```sh
    die "Enrollment failed; the binary and service are installed — retry with a fresh enroll link."
```

Replace:

```sh
  t enroll_placed "/etc/portunus/client.bundle.json"; echo
```

with:

```sh
  echo "Enrollment bundle placed at /etc/portunus/client.bundle.json"
```

- [ ] **Step 2: Inline `none_enable_start`**

Replace:

```sh
none_enable_start() { t next_manual "/usr/local/bin/portunus-$1" "${2:-$(config_default_for "$1")}"; echo; }
```

with:

```sh
none_enable_start() { printf '  no supported init system; run it in the background manually:\n  nohup %s --config %s > /var/log/portunus.log 2>&1 &\n' "/usr/local/bin/portunus-$1" "${2:-$(config_default_for "$1")}"; }
```

- [ ] **Step 3: Inline `print_server_next_steps`**

Replace the body lines:

```sh
  if service_should_start; then t srv_running "$resolved_version"; echo
  else t srv_installed_only "$resolved_version"; echo; t srv_start_hint; echo; fi
  echo
  t srv_next_title;      echo
  t srv_step_token;      echo
  t srv_step_ui "$url";  echo
  t srv_step_ui_remote "$port" "$port"; echo
  t srv_step_super;      echo
  echo
  t srv_handy;           echo
```

with:

```sh
  if service_should_start; then printf '✓ Portunus server %s is installed and running.\n' "$resolved_version"
  else printf '✓ Portunus server %s is installed (service not started: --no-service).\n' "$resolved_version"
       echo "  Start it first:  sudo systemctl enable --now portunus-server"; fi
  echo
  echo "Next steps:"
  printf '  1) Get the onboarding setup token (first run only, to create the first admin):\n       sudo journalctl -u portunus-server | grep '\''onboarding setup token'\''\n'
  printf '  2) Open the Web UI in a browser:  %s\n       (bound to loopback by design — not reachable from the public network)\n' "$url"
  printf '       Remote server? From your own machine, tunnel first, then open the URL locally:\n       ssh -L %s:127.0.0.1:%s <user>@<this-server>\n' "$port" "$port"
  echo "  3) Paste the token in the browser, then create the first _superadmin account."
  echo
  printf 'Handy commands:\n  status:  install.sh status\n  logs:    sudo journalctl -u portunus-server -f\n  stop:    sudo systemctl stop portunus-server\n'
```

- [ ] **Step 4: Inline `print_next_steps`**

Replace:

```sh
  echo "$(t done_next)"
  if [ "${DEPLOY:-binary}" = "docker" ]; then
    t next_docker "${COMPOSE_DIR:-$PWD}"; echo
  else
    _cfg="${CONFIG_PATH:-/etc/portunus/standalone.toml}"
    if [ "$ROLE" = "standalone" ]; then
      if [ -f "$_cfg" ]; then t next_standalone_config "$_cfg"; echo
      else t next_standalone_create "$_cfg"; echo; fi
    fi
    # If we did not start the service, show how to start it.
    if ! service_should_start; then
      case "${INIT:-}" in
        openrc) t next_openrc "$ROLE" "$ROLE"; echo ;;
        none)   none_enable_start "$ROLE" "$_cfg" ;;
        *)      t next_systemd "$ROLE"; echo ;;
      esac
    fi
  fi
  t next_status; echo
```

with:

```sh
  echo "Done. Next steps:"
  if [ "${DEPLOY:-binary}" = "docker" ]; then
    printf '  manage:  (cd %s && docker compose ps)\n' "${COMPOSE_DIR:-$PWD}"
  else
    _cfg="${CONFIG_PATH:-/etc/portunus/standalone.toml}"
    if [ "$ROLE" = "standalone" ]; then
      if [ -f "$_cfg" ]; then printf '  edit:    sudoedit %s\n' "$_cfg"
      else printf "  create config first: write your forwarding rules to %s (the service exits and won'\''t start without it)\n" "$_cfg"; fi
    fi
    # If we did not start the service, show how to start it.
    if ! service_should_start; then
      case "${INIT:-}" in
        openrc) printf '  start:   sudo rc-update add portunus-%s default && sudo rc-service portunus-%s start\n' "$ROLE" "$ROLE" ;;
        none)   none_enable_start "$ROLE" "$_cfg" ;;
        *)      printf '  start:   sudo systemctl enable --now portunus-%s\n' "$ROLE" ;;
      esac
    fi
  fi
  echo "  status:  install.sh status"
```

- [ ] **Step 5: Inline the `main()` dry-run messages**

Replace:

```sh
      install) [ -n "$ROLE" ] || die "$(t need_role)"; print_plan; exit 0 ;;
```

with:

```sh
      install) [ -n "$ROLE" ] || die "role required: client, server, or standalone"; print_plan; exit 0 ;;
```

Replace (in the same dry-run `config` arm):

```sh
        case "${_drr:-server}" in server) ;; *) echo "$(t config_server_only)" >&2; exit 2 ;; esac
```

with:

```sh
        case "${_drr:-server}" in server) ;; *) echo "config get/set applies to the server role only (standalone: edit /etc/portunus/standalone.toml directly; client has no such knobs)" >&2; exit 2 ;; esac
```

- [ ] **Step 6: Inline the Caddy/HTTPS messages**

In `ensure_caddy`, replace `echo "$(t caddy_installing)"` with `echo "Installing Caddy…"`.

In `verify_https`, replace:

```sh
  t caddy_verify "$d"; echo
```
with
```sh
  printf 'Verifying https://%s/ (Let'\''s Encrypt issuance can take ~30s)…\n' "$d"
```

replace:

```sh
      t https_ready "$d"; echo; return 0
```
with
```sh
      printf 'HTTPS ready: https://%s/\n' "$d"; return 0
```

replace:

```sh
  t caddy_verify_warn "$d"; echo; return 0
```
with
```sh
  printf 'Could not verify https://%s/ yet. Check: journalctl -u caddy -e ; DNS propagation.\n' "$d"; return 0
```

In `setup_caddy_domain`, replace:

```sh
  valid_fqdn "$DOMAIN" || die "$(t bad_domain "$DOMAIN")"
```
with
```sh
  valid_fqdn "$DOMAIN" || die "invalid domain '$DOMAIN' — expected an FQDN like portunus.example.com"
```

replace:

```sh
  t caddy_done "$DOMAIN"; echo
```
with
```sh
  printf 'Caddy configured for %s\n' "$DOMAIN"
```

replace:

```sh
  t https_public_note; echo
```
with
```sh
  echo "Note: the web UI is now publicly reachable over HTTPS; it stays protected by operator login/token."
```

- [ ] **Step 7: Inline the lifecycle messages**

Replace every remaining `die "$(t no_install_found)"` (in `lifecycle_domain`, `lifecycle_service`, `lifecycle_upgrade`, `lifecycle_uninstall`, `lifecycle_config`) with:

```sh
die "No Portunus install detected (no .install-meta and no probe match)."
```

In `lifecycle_status`, replace `if [ -z "$mf" ]; then echo "$(t no_install_found)"; return 0; fi` with:

```sh
  if [ -z "$mf" ]; then echo "No Portunus install detected (no .install-meta and no probe match)."; return 0; fi
```

In `dispatch_verb` (install arm), replace `[ -n "$ROLE" ] || die "$(t need_role)"` with:

```sh
      [ -n "$ROLE" ] || die "role required: client, server, or standalone"
```

In `validate_config_key`, replace:

```sh
  case " $SCOPED_KEYS " in *" $CONFIG_KEY "*) ;; *) die "$(t unknown_config_key "$CONFIG_KEY")" ;; esac
```
with
```sh
  case " $SCOPED_KEYS " in *" $CONFIG_KEY "*) ;; *) die "unknown config key: $CONFIG_KEY (allowed: advertised-endpoint data-dir operator-http-listen)" ;; esac
```

In `lifecycle_upgrade`, replace:

```sh
  if [ "$cur" = "$artifact_version" ]; then echo "$(t upgrade_current "$cur")"; return 0; fi
```
with
```sh
  if [ "$cur" = "$artifact_version" ]; then echo "Already at $cur; nothing to upgrade."; return 0; fi
```

In `lifecycle_config`, replace the `config_server_only` echo (the binary/real path, not the dry-run one already done in Step 5):

```sh
  case "${_r:-server}" in server) ;; *) echo "$(t config_server_only)" >&2; return 2 ;; esac
```
with
```sh
  case "${_r:-server}" in server) ;; *) echo "config get/set applies to the server role only (standalone: edit /etc/portunus/standalone.toml directly; client has no such knobs)" >&2; return 2 ;; esac
```

replace:

```sh
    [ "$CONFIG_KEY" = data-dir ] && { echo "$(t config_docker_datadir)" >&2; return 2; }
```
with
```sh
    [ "$CONFIG_KEY" = data-dir ] && { echo "data-dir is fixed to the in-container volume path for Docker deploys and cannot be changed via config; edit the volume mount in compose.yml instead." >&2; return 2; }
```

replace both occurrences (docker and binary branches) of:

```sh
    valid_config_value "$CONFIG_KEY" "$CONFIG_VALUE" || { echo "$(t bad_config_value "$CONFIG_KEY" "$CONFIG_VALUE")" >&2; return 2; }
```
with
```sh
    valid_config_value "$CONFIG_KEY" "$CONFIG_VALUE" || { echo "invalid value for $CONFIG_KEY: $CONFIG_VALUE" >&2; return 2; }
```

(use `replace_all` since the two lines are identical; one is indented for the docker branch and one for the binary branch — verify both are updated.)

- [ ] **Step 8: Verify no `t ` call sites remain**

Run:

```sh
grep -nE '(\$\(t |[[:space:];]t |^t )[a-z_]+' scripts/install.sh | grep -v 'case "\$LANG_CODE'
```

Expected: no matches (every call site is inlined). If any remain, inline them with the corresponding English string from the `*:<key>` arm before continuing.

- [ ] **Step 9: Delete the i18n machinery**

Delete, in `scripts/install.sh`:

- The `LANG_CACHE="…"` line.
- The `I18N_KEYS="…"` line (the long manifest).
- The `LANG_CODE="${PORTUNUS_LANG:-}"` line.
- The entire `resolve_lang()` function.
- The entire `t()` function (from `t() {` through its closing `}` and the `printf "$_f" "$@"` line).
- In `main()`, the `resolve_lang` call line.

In `parse_args`, delete these arms:

```sh
      --lang) shift; [ $# -gt 0 ] || die "--lang needs a value"; LANG_CODE="$1" ;;
```
```sh
      --print-i18n-keys) shift 2>/dev/null || true; for k in $I18N_KEYS; do echo "$k"; done; exit 0 ;;
      --print-i18n) shift; [ $# -gt 0 ] || die "--print-i18n needs a key"; resolve_lang; t "$1"; echo; exit 0 ;;
```
```sh
      --reset-lang) rm -f "$LANG_CACHE" 2>/dev/null || true; echo "language preference reset ($LANG_CACHE); next interactive run will ask again"; exit 0 ;;
```

- [ ] **Step 10: Drop the `lang=` field from `meta_write` calls**

Remove the `"lang=${LANG_CODE:-en}"` argument (and, for the multi-line call in `lifecycle_domain`, the `"lang=${LANG_CODE:-en}" \` continuation line) from all five `meta_write` invocations:

- `install_docker` (the `"version=${artifact_version:-$resolved_version}" "lang=${LANG_CODE:-en}" \` line → drop `"lang=${LANG_CODE:-en}"`).
- `dispatch_verb` install arm — the first `meta_write` (with `init=$INIT`).
- `dispatch_verb` install arm — the second `meta_write` (with `domain=$DOMAIN`).
- `lifecycle_upgrade` — the trailing `meta_write`.
- `lifecycle_domain` — the multi-line `meta_write` (delete the `"lang=${LANG_CODE:-en}" \` line).

Update the `DETECTED_PROV` comment (it says "provenance i18n key") to:

```sh
DETECTED_PROV=""      # provenance token: prov_detected|prov_nic|prov_loopback
```

(Keep the assigned values `prov_detected`/`prov_nic`/`prov_loopback` in `detect_public_ip` — they are machine-readable tokens consumed by the `--detect-ip` seam and asserted by the harness.)

- [ ] **Step 11: Delete the i18n tests from the harness**

In `scripts/install.test.sh`, delete these blocks in full:

- `# --- i18n key coverage … ---` (the `keys_en`/`keys_zh` block).
- `# --- explicit lang override wins ---` (`o=`/`PORTUNUS_LANG=` block).
- `# --- Fix 4: next-step i18n keys exist in both languages ---`.
- `# --- new enroll i18n keys resolve … ---` (the `for key in enroll_placed enroll_failed` loop).
- `# --- P1/P2: new interactive i18n keys present in both languages ---`.
- `# --- --reset-lang clears the cached language preference ---` (`fakehome` block).
- `# --- Caddy: EN/ZH i18n parity for the new keys ---` (the `diff <(…) <(…)` line and the following `for _k in config_server_only bad_config_value` loop, through its `done`).

- [ ] **Step 12: Add removed-flag assertions for the i18n flags**

In `scripts/install.test.sh`, after the `--yes must now error` line added in Task 2, add:

```sh
# --- i18n flags are removed: now unknown arguments ---
for badflag in --lang --reset-lang --print-i18n --print-i18n-keys; do
  if $SH "$script" "$badflag" en >/dev/null 2>&1; then fail "$badflag must now error (removed)"; fi
done
```

- [ ] **Step 13: Run the gate**

Run the gate command. Expected: `PASS` on both harness runs, shellcheck silent.

- [ ] **Step 14: Commit**

```sh
git add scripts/install.sh scripts/install.test.sh
git commit -m "refactor(install): drop bilingual i18n, english-only output"
```

---

## Task 4: Remove `--systemd`, rewrite help, finalize removed-flag errors

Removes the last legacy no-op flag and replaces the layered `--help`/`--help-all` with one comprehensive reference (plus a seams appendix in `--help-all`).

**Files:**
- Modify: `scripts/install.sh` (`print_usage`, `print_usage_all`, `--systemd` arm, header comment)
- Modify: `scripts/install.test.sh`

- [ ] **Step 1: Remove the `--systemd` parse arm**

In `parse_args`, delete:

```sh
      --systemd) : ;;  # back-compat no-op: the service is installed by default now
```

- [ ] **Step 2: Rewrite `print_usage`**

Replace the entire `print_usage()` function with:

```sh
print_usage() {
  cat <<'USAGE'
Portunus installer & lifecycle manager (flag-driven, non-interactive)

Usage:
  install.sh <role> [options]    install a role
  install.sh <verb> [options]    manage an existing install

Roles:
  standalone   forward ports/traffic on THIS machine (no control plane)
  server       run a control panel for many nodes (with Web UI)
  client       connect THIS machine to an existing control panel

Manage verbs:
  status                         show what is installed and running
  service start|stop|restart     control the service
  upgrade                        upgrade to the latest release
  config get|set <key> [value]   view/change a server config key
  env                            print all server config keys + values
  uninstall [--purge]            remove (--purge also deletes data)
  domain <fqdn>                  set up HTTPS via Caddy (server)

Options:
  --version V                    install a specific version (default: latest)
  --deploy binary|docker         deployment form (default: binary)
  --bin-dir D                    binary install dir (default: /usr/local/bin)
  --compose-dir D                docker compose project dir (default: cwd)
  --enroll '<uri>'               (client) self-enroll during install
  --domain FQDN                  (server) HTTPS via Caddy + Let's Encrypt
  --acme-email A                 (server --domain) ACME contact email
  --skip-dns-check               (server --domain) skip the DNS pre-check
  --data-dir D                   (server) data directory
  --advertised-endpoint H:P      (server) endpoint clients dial
  --operator-http-listen A       (server) operator HTTP bind (default 127.0.0.1:7080)
  --config PATH                  (standalone) config file the service reads
  --no-service                   install but do not enable/start the service
  --restart                      (config set) restart the service to apply
  --purge                        (uninstall) also delete the data dir / volume
  --dry-run                      print the plan and change nothing

Examples:
  install.sh server --advertised-endpoint panel.example:7443
  install.sh server --deploy docker --compose-dir ~/portunus
  install.sh server --domain panel.example.com --acme-email ops@example.com
  install.sh client --enroll 'portunus://panel.example.com:7443/enroll?...'
  install.sh standalone --config /etc/portunus/standalone.toml
  install.sh status
  install.sh service restart
  install.sh upgrade
  install.sh config set advertised-endpoint panel.example:7443 --restart
  install.sh config get advertised-endpoint
  install.sh uninstall --purge

More:  install.sh --help-all     adds automation/CI seam flags
USAGE
}
```

- [ ] **Step 3: Rewrite `print_usage_all`**

Replace the entire `print_usage_all()` function with:

```sh
print_usage_all() {
  print_usage
  cat <<'USAGE'

Automation / CI seams (stable; exercised by scripts/install.test.sh):
  --effective-advertised         print the resolved advertised endpoint, exit
  --detect-deploy [DIR]          print binary|docker for DIR/host, exit
  --detect-init                  print systemd|openrc|none, exit
  --detect-ip                    print "<ip> <provenance>", exit
  --resolve-meta                 print the resolved .install-meta path, exit
  --meta-write FILE k=v...       write an install-meta file, exit
  --meta-read FILE KEY           read one key from an install-meta file, exit
  --valid-endpoint H:P           exit 0/1 on host:port validity
  --valid-fqdn FQDN              exit 0/1 on FQDN validity
  --valid-email ADDR             exit 0/1 on ACME-email validity
  --render-dropin                print the systemd ExecStart drop-in, exit
  --render-caddy FQDN [PORT]     print the managed Caddy block, exit
  --render-openrc ROLE           print the OpenRC init script, exit
  --render-confd ROLE [CFG]      print the OpenRC conf.d body, exit
  --render-config-dropin ROLE CFG  print the standalone config drop-in, exit
USAGE
}
```

- [ ] **Step 4: Fix the header comment**

Near the top of the file, replace the comment line:

```sh
#   curl -fsSL .../scripts/install.sh | sh        # interactive menu
```

with:

```sh
#   curl -fsSL .../scripts/install.sh | sh -s -- server --advertised-endpoint host:7443
```

- [ ] **Step 5: Update the help test in the harness**

In `scripts/install.test.sh`, replace the block:

```sh
# --- layered help: short --help stays terse; --help-all carries the CI seams ---
h="$($SH "$script" --help 2>&1)" || fail "--help exit"
printf '%s\n' "$h" | grep -qi 'interactive wizard' || fail "--help should mention the interactive wizard"
if printf '%s\n' "$h" | grep -q -- '--meta-write'; then fail "--help must not expose CI seams"; fi
ha="$($SH "$script" --help-all 2>&1)" || fail "--help-all exit"
printf '%s\n' "$ha" | grep -q -- '--meta-write' || fail "--help-all should list CI seams"
printf '%s\n' "$ha" | grep -q -- '--render-caddy' || fail "--help-all should list render seams"
```

with:

```sh
# --- help: --help is the comprehensive user reference; --help-all adds seams ---
h="$($SH "$script" --help 2>&1)" || fail "--help exit"
printf '%s\n' "$h" | grep -qi 'flag-driven' || fail "--help should state it is flag-driven"
printf '%s\n' "$h" | grep -q 'Examples:' || fail "--help should carry an Examples block"
printf '%s\n' "$h" | grep -q -- '--restart' || fail "--help should document --restart"
if printf '%s\n' "$h" | grep -qi 'interactive'; then fail "--help must not mention interactive mode"; fi
if printf '%s\n' "$h" | grep -q -- '--meta-write'; then fail "--help must not expose CI seams"; fi
ha="$($SH "$script" --help-all 2>&1)" || fail "--help-all exit"
printf '%s\n' "$ha" | grep -q -- '--meta-write' || fail "--help-all should list CI seams"
printf '%s\n' "$ha" | grep -q -- '--render-caddy' || fail "--help-all should list render seams"
```

- [ ] **Step 6: Update the `--systemd` tests**

In `scripts/install.test.sh`, replace:

```sh
# --- --systemd is accepted as a back-compat no-op ---
$SH "$script" standalone --version 1.4.1 --systemd --dry-run >/dev/null 2>&1 || fail "--systemd back-compat no-op rejected"
```

with:

```sh
# --- --systemd is removed: now an unknown argument ---
if $SH "$script" standalone --version 1.4.1 --systemd --dry-run >/dev/null 2>&1; then fail "--systemd must now error (removed)"; fi
```

Two other tests pass `--systemd` as a benign extra flag while asserting drop-in rendering; remove the `--systemd ` token from each (the render path does not need it):

- `# --- server binary dry-run mentions drop-in target … ---`: change `server --version 1.0.0 --systemd --advertised-endpoint …` to `server --version 1.0.0 --advertised-endpoint …`.
- `# --- drop-in is an ExecStart= override … ---`: change `server --systemd --advertised-endpoint h:7443 …` to `server --advertised-endpoint h:7443 …`.

- [ ] **Step 7: Add a no-argument usage assertion**

In `scripts/install.test.sh`, replace:

```sh
# --- non-interactive when no tty and no args: helpful error, non-zero ---
if echo "" | $SH "$script" </dev/null >/dev/null 2>&1; then fail "no-arg no-tty should error"; fi
```

with:

```sh
# --- no command prints usage to stderr and exits 2 ---
if $SH "$script" </dev/null >/dev/null 2>&1; then fail "no-arg must exit non-zero"; fi
no_arg_err="$($SH "$script" </dev/null 2>&1 || true)"
printf '%s\n' "$no_arg_err" | grep -qi 'Usage:' || fail "no-arg must print usage"
```

- [ ] **Step 8: Run the gate**

Run the gate command. Expected: `PASS` on both harness runs, shellcheck silent.

- [ ] **Step 9: Commit**

```sh
git add scripts/install.sh scripts/install.test.sh
git commit -m "refactor(install): single comprehensive help, drop legacy flags"
```

---

## Task 5: Rewrite the installer docs (EN + ZH mirror)

Replace the interactive/i18n documentation with a flag-first reference.

**Files:**
- Modify: `docs/content/docs/cli/installer.mdx`
- Modify: `docs/content/docs/zh/cli/installer.mdx`

- [ ] **Step 1: Update the EN intro + remove the "Two modes" section**

In `docs/content/docs/cli/installer.mdx`:

- In the opening paragraph, delete the clause `and it is bilingual (English / 中文)` and the final two sentences beginning `The non-interactive flag interface that CI…` through `the interactive menu is purely additive.` Replace with: `It is a non-interactive, flag-driven CLI — every action is selected by arguments, with no prompts.`
- In the "reachable two ways" code block, change the first comment from `# Always-current copy, piped (interactive menu or non-interactive verbs):` to `# Always-current copy, piped:` and the example from a bare `| sh` to `| sh -s -- server --advertised-endpoint host:7443`.
- Delete the entire `## Two modes` section (from the heading through the line `Because it reads from /dev/tty when piped, curl … | sh reaches the wizard.` and the following `**Non-interactive** — …` paragraph).

- [ ] **Step 2: Update the EN Synopsis + Flags**

- In `## Synopsis`, delete the `[--lang en|zh] [--reset-lang]` line and the `[--yes]` token; add `[--restart]` to the line with `[--purge] [--dry-run]`.
- In the `## Verbs` table, change the `uninstall` row's trailing `Confirms first.` to `Runs immediately; pair with --purge to also delete data.`
- In the `## Flags` table: delete the `--systemd`, `--lang`, `--reset-lang`, and `--yes` rows. Change the `--purge` row to: `| `--purge` | With `uninstall`, also delete the data dir / compose volume. No confirmation prompt. |`. Add a row: `| `--restart` | With `config set`, restart the service so the new value takes effect. Default: write the value but do not restart. |`. Delete the trailing paragraph `--version, --bin-dir, --yes, --dry-run and the bare client / server form are the original downloader interface…` and replace with: `A bare role is shorthand for install (e.g. `server` ≡ `install server`).`

- [ ] **Step 3: Update the EN config/domain/safety/language sections**

- In `## Scoped config & env`, change `then offers a service restart (up -d recreates the container for docker)` to `then, with --restart, restarts the service (up -d recreates the container for docker)`. Add a `config set … --restart` example to the code block.
- In `## Server advertised endpoint`, change `the wizard prompts for the advertised endpoint` to `set the advertised endpoint with --advertised-endpoint`.
- In `## Domain & HTTPS`, change `When you supply a --domain (or answer the wizard's HTTPS prompt)` to `When you supply a --domain`; change the DNS pre-flight bullet to note a mismatch is a hard error unless `--skip-dns-check`.
- In `## Lifecycle examples`, change the two uninstall comments: `# Uninstall (keeps data) — confirms first` → `# Uninstall (keeps data)`, and `# Uninstall and delete data — requires typing the token 'purge'` → `# Uninstall and delete data (no prompt)`.
- Delete the entire `## Language` section.
- In `## Safety`, delete the `--purge` typed-token bullet and the `Destructive actions always confirm; uninstall defaults to [y/N].` bullet; replace with: `**--purge** deletes the data dir / compose volume with no confirmation — it is the explicit opt-in. Without it, uninstall keeps data.` Keep the `--dry-run`, shipped-unit, and re-run bullets.

- [ ] **Step 4: Update the install metadata note**

In `## Install metadata`, delete `lang`, from the Fields list (`role`, `deploy`, `version`, `lang`, `init` → `role`, `deploy`, `version`, `init`).

- [ ] **Step 5: Mirror all changes into the ZH doc**

Apply the structurally-identical edits to `docs/content/docs/zh/cli/installer.mdx`: remove the bilingual clause, the "两种模式"/Two-modes section, the language section, `--lang`/`--reset-lang`/`--yes`/`--systemd` flag rows, the `lang` metadata field; add `--restart`; reword `--purge` and uninstall to "立即执行、无确认"; reword config restart to "--restart 生效". Keep the Chinese prose; the page stays a Chinese translation, only the now-removed features are dropped.

- [ ] **Step 6: Verify the docs build (if the toolchain is present)**

If a docs build is available, run it; otherwise visually confirm no dangling references to removed flags remain:

```sh
grep -nE -- '--lang|--reset-lang|--yes|--systemd|interactive (menu|wizard)|Two modes|两种模式' docs/content/docs/cli/installer.mdx docs/content/docs/zh/cli/installer.mdx
```

Expected: no matches.

- [ ] **Step 7: Commit**

```sh
git add docs/content/docs/cli/installer.mdx docs/content/docs/zh/cli/installer.mdx
git commit -m "docs(install): rewrite installer reference as flag-only"
```

---

## Task 6: Changelog entry + final full-suite verification

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add an Unreleased changelog entry**

In `CHANGELOG.md`, immediately below the `and this project adheres to [Semantic Versioning]…` line and above `## [2.2.0] — 2026-06-14`, insert:

```markdown
## [Unreleased]

The `install.sh` lifecycle manager becomes a pure flag-driven CLI. The
interactive menu, the guided install wizard, and the bilingual (en/zh)
output are removed; every action is now selected by arguments, and all
output is English. The installer's executor — binary/Docker install,
service lifecycle, scoped config rendering, and Caddy/HTTPS setup — is
unchanged.

### Changed
- **No interactive mode.** Running `install.sh` with no role/verb now
  prints usage and exits non-zero instead of launching a menu/wizard.
- **Flags are consent.** `uninstall` and `upgrade` run immediately with
  no confirmation. Data deletion is gated solely by `--purge` (the typed
  `purge` challenge is gone). `config set` restarts the service only when
  passed the new `--restart` flag. A `domain` DNS mismatch is a hard
  error unless `--skip-dns-check` is given.
- **English-only output.** The i18n table and language cache are removed.

### Removed
- The `--yes`, `--lang`, `--reset-lang`, `--menu-stdin`, `--print-i18n`,
  `--print-i18n-keys`, and `--systemd` flags. Passing any of them is now
  an "unknown argument" error. Scripts that passed `--yes` should drop it
  (actions auto-proceed); scripts that passed `--systemd` should drop it
  (the service is installed by default).
```

- [ ] **Step 2: Run the full gate one final time**

```sh
shellcheck -s sh -S warning scripts/install.sh \
  && bash scripts/install.test.sh \
  && TEST_SH=dash bash scripts/install.test.sh
```

Expected: shellcheck silent; both harness runs print `PASS`.

- [ ] **Step 3: Commit**

```sh
git add CHANGELOG.md
git commit -m "docs: changelog entry for flag-only installer"
```

---

## Self-review checklist (completed by plan author)

- **Spec coverage:** No interactivity (Task 1) ✓ · flags-are-consent / `--restart` / `--purge` / DNS hard-error (Task 2) ✓ · English-only, i18n deleted (Task 3) ✓ · clean break on removed flags + comprehensive help (Tasks 2–4) ✓ · docs incl. zh mirror (Task 5) ✓ · breaking-change record (Task 6) ✓ · executor untouched (no task modifies install/service/Caddy executors) ✓.
- **Type/name consistency:** New global `RESTART` defined in Task 2 Step 1 and referenced in Steps 4–5 and tested in Step 8 — consistent spelling. `--restart` flag consistent across script, tests, help, docs, changelog.
- **`set -eu` safety:** `MENU_FORCE_STDIN` kept through Task 1, removed in Task 2 with its last reference; `ASSUME_YES` removed in Task 2 with all four references; `LANG_CODE`/`I18N_KEYS`/`LANG_CACHE` removed in Task 3 with all references and the five `meta_write lang=` sites.
- **Placeholder scan:** every code step shows literal before/after; English strings are the exact `*:<key>` arms from the current `t()` table.
- **Gate after every task:** shellcheck + bash + dash, defined once at the top.
