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

# --- i18n key coverage: every EN key exists in ZH and vice-versa ---
keys_en="$(bash "$script" --print-i18n-keys en | sort)"
keys_zh="$(bash "$script" --print-i18n-keys zh | sort)"
[ -n "$keys_en" ] || fail "no EN i18n keys"
[ "$keys_en" = "$keys_zh" ] || fail "i18n EN/ZH key sets differ"

# --- explicit lang override wins ---
bash "$script" --lang zh --print-i18n menu_title | grep -q '管理' || fail "zh menu_title"
PORTUNUS_LANG=en bash "$script" --print-i18n menu_title | grep -qi 'manager' || fail "en menu_title"

# --- new flags accepted in dry-run plan ---
o="$(bash "$script" server --deploy docker --advertised-endpoint h.example:7443 --data-dir /srv/p --operator-http-listen 0.0.0.0:7080 --version 1.0.0 --dry-run)" || fail "new flags exit"
echo "$o" | grep -q '^deploy:[[:space:]]*docker$' || fail "deploy docker"
echo "$o" | grep -q '^advertised:[[:space:]]*h.example:7443$' || fail "advertised line"

# --- bare role implies install verb; explicit verb parsed ---
bash "$script" install client --version 1.0.0 --dry-run >/dev/null 2>&1 || fail "install verb"
bash "$script" status --help >/dev/null 2>&1 || fail "status+help"

# --- non-interactive when no tty and no args: helpful error, non-zero ---
if echo "" | bash "$script" </dev/null >/dev/null 2>&1; then fail "no-arg no-tty should error"; fi

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

# --- server binary dry-run mentions drop-in target, writes nothing ---
sentinel="$(mktemp -d)"
o="$(bash "$script" server --version 1.0.0 --systemd --advertised-endpoint h.example:7443 --data-dir "$sentinel/data" --dry-run)" || fail "server dry-run"
echo "$o" | grep -q 'drop-in:.*portunus-server.service.d/10-portunus.conf' || fail "drop-in plan line"
[ -z "$(ls -A "$sentinel" 2>/dev/null)" ] || fail "dry-run wrote files"
rm -rf "$sentinel"

sb="$(mktemp -d)"
o="$(bash "$script" server --deploy docker --compose-dir "$sb" --advertised-endpoint d.example:7443 --version 1.0.0 --dry-run)" || fail "docker dry-run"
echo "$o" | grep -q '^deploy:[[:space:]]*docker$' || fail "docker deploy plan"
echo "$o" | grep -q "compose_dir:.*$sb" || fail "compose_dir plan"
[ -z "$(ls -A "$sb" 2>/dev/null)" ] || fail "docker dry-run wrote files"
rm -rf "$sb"

# config rejects unknown key
if bash "$script" config set bogus x --dry-run >/dev/null 2>&1; then fail "bogus config key accepted"; fi
# config accepts a scoped key in dry-run
bash "$script" config get advertised-endpoint --dry-run >/dev/null 2>&1 || fail "config get scoped key"
# uninstall dry-run performs nothing and exits 0
bash "$script" uninstall server --dry-run >/dev/null 2>&1 || fail "uninstall dry-run"
# status dry-run exits 0
bash "$script" status --dry-run >/dev/null 2>&1 || fail "status dry-run"

# Feed "0" (Exit) to the menu via stdin acting as the tty seam.
out="$(printf '0\n' | PORTUNUS_LANG=en bash "$script" --menu-stdin 2>&1)" || true
echo "$out" | grep -qi 'Portunus Manager' || fail "menu title not shown"

# --- Fix 1: install-time drop-in persists data-dir + operator-http-listen ---
dr="$(bash "$script" server --systemd --advertised-endpoint h:7443 --data-dir /srv/p --operator-http-listen 0.0.0.0:7080 --render-dropin)" || fail "render-dropin exit"
echo "$dr" | grep -qx 'Environment=PORTUNUS_ADVERTISED_ENDPOINT=h:7443' || fail "dropin advertised"
echo "$dr" | grep -qx 'Environment=PORTUNUS_DATA_DIR=/srv/p' || fail "dropin data-dir (Fix 1)"
echo "$dr" | grep -qx 'Environment=PORTUNUS_OPERATOR_HTTP_LISTEN=0.0.0.0:7080' || fail "dropin op-http (Fix 1)"

# --- Fix 2: explicit --compose-dir never falls back to a system meta ---
cdtmp="$(mktemp -d)"            # empty: no .install-meta here
if bash "$script" --compose-dir "$cdtmp" --resolve-meta >/dev/null 2>&1; then
  fail "Fix 2: empty --compose-dir must NOT resolve a fallback meta"
fi
printf 'role=server\ndeploy=docker\n' > "$cdtmp/.install-meta"
[ "$(bash "$script" --compose-dir "$cdtmp" --resolve-meta)" = "$cdtmp/.install-meta" ] || fail "Fix 2: scoped meta not resolved"
rm -rf "$cdtmp"

# --- Fix 4: next-step i18n keys exist in both languages ---
bash "$script" --lang en --print-i18n next_systemd | grep -qi 'systemctl' || fail "en next_systemd"
bash "$script" --lang zh --print-i18n next_docker | grep -q 'docker compose' || fail "zh next_docker"

# --- P2: light host:port validation ---
bash "$script" --valid-endpoint "host.example:7443" || fail "valid endpoint rejected"
bash "$script" --valid-endpoint "" || fail "blank endpoint must be allowed (auto)"
if bash "$script" --valid-endpoint "no-port" 2>/dev/null; then fail "missing port accepted"; fi
if bash "$script" --valid-endpoint "bad host:7443" 2>/dev/null; then fail "space in host accepted"; fi
if bash "$script" --valid-endpoint "h:99999x" 2>/dev/null; then fail "non-numeric port accepted"; fi

# --- P1/P2: new interactive i18n keys present in both languages ---
bash "$script" --lang en --print-i18n ask_config_key | grep -qi 'advertised-endpoint' || fail "en ask_config_key"
bash "$script" --lang zh --print-i18n ask_service_action | grep -q '启动' || fail "zh ask_service_action"
bash "$script" --lang en --print-i18n menu_invalid bogus | grep -qi 'invalid' || fail "en menu_invalid"

# --- P1#2: a die() inside a menu action must NOT kill the whole session ---
# No install present here ⇒ Uninstall (2) hits die(); the loop must survive
# and still process the following Exit (0).
mo="$(printf '2\n0\n' | PORTUNUS_LANG=en bash "$script" --menu-stdin 2>&1)" || true
[ "$(printf '%s\n' "$mo" | grep -c 'Portunus Manager')" -ge 2 ] || fail "menu died after a failing action (P1#2)"

# --- P2#4: invalid menu choice gives explicit feedback ---
io="$(printf '99\n0\n' | PORTUNUS_LANG=en bash "$script" --menu-stdin 2>&1)" || true
printf '%s\n' "$io" | grep -qi 'invalid option' || fail "no invalid-option feedback"

# --- wizard: IP detection seam, offline path never hits network ---
di="$(PORTUNUS_SKIP_IP_PROBE=1 bash "$script" --detect-ip)" || fail "--detect-ip exit"
echo "$di" | grep -Eq '^[0-9a-fA-F.:]+ prov_(nic|loopback)$' || fail "skip-probe must yield NIC/loopback ($di)"

# --- shellcheck (skipped if not installed, but must pass if present) ---
if command -v shellcheck >/dev/null 2>&1; then
  shellcheck -s bash -S warning "$script" || fail "shellcheck warnings"
else
  echo "note: shellcheck not installed; skipping lint gate" >&2
fi

echo "PASS"
