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

echo "PASS"
