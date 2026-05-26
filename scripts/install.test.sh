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

# --- drop-in is an ExecStart= override carrying the flags (env is inert) ---
dr="$(bash "$script" server --systemd --advertised-endpoint h:7443 --data-dir /srv/p --operator-http-listen 0.0.0.0:7080 --render-dropin)" || fail "render-dropin exit"
echo "$dr" | grep -qx 'ExecStart=' || fail "dropin missing ExecStart= clear line"
echo "$dr" | grep -qx 'ExecStart=/usr/local/bin/portunus-server --data-dir /srv/p serve --operator-http-listen 0.0.0.0:7080 --advertised-endpoint h:7443' || fail "dropin ExecStart override"
if echo "$dr" | grep -q 'Environment=PORTUNUS_ADVERTISED_ENDPOINT='; then fail "inert Environment line still emitted"; fi

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

# --- minimal wizard: server+binary asks only role/deploy/endpoint ---
wo="$(printf '1\n1\n-\nn\n0\n' | PORTUNUS_SKIP_IP_PROBE=1 PORTUNUS_LANG=en bash "$script" --menu-stdin 2>&1)" || true
printf '%s\n' "$wo" | grep -q 'About to install:' || fail "no summary block"
printf '%s\n' "$wo" | grep -q 'data dir:.*\/var\/lib\/portunus' || fail "summary missing data-dir default"
printf '%s\n' "$wo" | grep -q 'operator http:.*127\.0\.0\.1:7080' || fail "summary missing op-http default"
printf '%s\n' "$wo" | grep -qi 'loopback' || fail "'-' input should mark loopback"
if printf '%s\n' "$wo" | grep -q 'Version (blank = latest)'; then fail "wizard still asks version"; fi
if printf '%s\n' "$wo" | grep -q 'Server data dir'; then fail "wizard still asks data-dir"; fi

# client: only role+deploy, no endpoint/summary advertised line
co="$(printf '1\n2\n1\nn\n0\n' | PORTUNUS_SKIP_IP_PROBE=1 PORTUNUS_LANG=en bash "$script" --menu-stdin 2>&1)" || true
printf '%s\n' "$co" | grep -q 'About to install:' || fail "client no summary"
if printf '%s\n' "$co" | grep -q 'advertised endpoint:'; then fail "client must not show advertised line"; fi

# --- recommended deploy default differs by role (Enter accepts) ---
so="$(printf '1\n\n\n-\nn\n0\n' | PORTUNUS_SKIP_IP_PROBE=1 PORTUNUS_LANG=en bash "$script" --menu-stdin 2>&1)" || true
printf '%s\n' "$so" | grep -Eq 'deploy: +docker' || fail "server Enter must default to docker (recommended)"
printf '%s\n' "$so" | grep -q 'compose dir:' || fail "server docker default missing compose dir line"
ro="$(printf '1\n2\n\nn\n0\n' | PORTUNUS_SKIP_IP_PROBE=1 PORTUNUS_LANG=en bash "$script" --menu-stdin 2>&1)" || true
printf '%s\n' "$ro" | grep -Eq 'deploy: +binary \+ systemd' || fail "client Enter must default to binary (recommended)"

# --- --reset-lang clears the cached language preference ---
fakehome="$(mktemp -d)"
mkdir -p "$fakehome/.config/portunus"; printf 'zh' > "$fakehome/.config/portunus/installer-lang"
HOME="$fakehome" XDG_CONFIG_HOME="$fakehome/.config" bash "$script" --reset-lang >/dev/null 2>&1 || fail "--reset-lang exit"
[ ! -e "$fakehome/.config/portunus/installer-lang" ] || fail "--reset-lang did not remove the cache"
rm -rf "$fakehome"

# --- Caddy: FQDN validation ---
bash "$script" --valid-fqdn serverbee-test.900040.xyz || fail "valid fqdn rejected"
if bash "$script" --valid-fqdn no_dot 2>/dev/null; then fail "fqdn without dot accepted"; fi
if bash "$script" --valid-fqdn "bad host.com" 2>/dev/null; then fail "fqdn with space accepted"; fi
if bash "$script" --valid-fqdn "-lead.com" 2>/dev/null; then fail "fqdn leading dash accepted"; fi
if bash "$script" --valid-fqdn "" 2>/dev/null; then fail "empty fqdn accepted"; fi

# --- Caddy: managed block render ---
cb="$(bash "$script" --render-caddy serverbee-test.900040.xyz 7080)" || fail "render-caddy exit"
printf '%s\n' "$cb" | grep -qx '# >>> portunus >>>' || fail "missing start marker"
printf '%s\n' "$cb" | grep -qx '# <<< portunus <<<' || fail "missing end marker"
printf '%s\n' "$cb" | grep -qx 'serverbee-test.900040.xyz {' || fail "missing site line"
printf '%s\n' "$cb" | grep -q 'reverse_proxy 127.0.0.1:7080' || fail "missing reverse_proxy"

# --- Caddy: server dry-run plan includes role; client+--domain errors ---
od="$(PORTUNUS_SKIP_IP_PROBE=1 bash "$script" server --domain serverbee-test.900040.xyz --skip-dns-check --dry-run 2>&1)" || fail "server --domain dry-run exit"
printf '%s\n' "$od" | grep -q '^role:[[:space:]]*server$' || fail "domain dry-run role"
if PORTUNUS_SKIP_IP_PROBE=1 bash "$script" client --domain x.example.com --dry-run >/dev/null 2>&1; then fail "client --domain must error"; fi

# --- Caddy: domain verb dry-run writes nothing, exits 0 ---
PORTUNUS_SKIP_IP_PROBE=1 bash "$script" domain serverbee-test.900040.xyz --skip-dns-check --dry-run >/dev/null 2>&1 || fail "domain verb dry-run"

# --- Caddy: EN/ZH i18n parity for the new keys ---
diff <(bash "$script" --print-i18n-keys en | sort) <(bash "$script" --print-i18n-keys zh | sort) >/dev/null || fail "EN/ZH i18n key parity broken"

# --- advertised precedence: domain derives, explicit wins ---
ea1="$(PORTUNUS_SKIP_IP_PROBE=1 bash "$script" server --domain d.example.com --effective-advertised 2>/dev/null)"
[ "$ea1" = "d.example.com:7443" ] || fail "domain should derive advertised d.example.com:7443 (got '$ea1')"
ea2="$(PORTUNUS_SKIP_IP_PROBE=1 bash "$script" server --domain d.example.com --advertised-endpoint x.host:7443 --effective-advertised 2>/dev/null)"
[ "$ea2" = "x.host:7443" ] || fail "explicit advertised must win (got '$ea2')"
ea3="$(PORTUNUS_SKIP_IP_PROBE=1 bash "$script" server --effective-advertised 2>/dev/null)"
[ -z "$ea3" ] || fail "no domain/explicit => empty effective advertised (got '$ea3')"

# --- dry-run plan shows the derived advertised ---
dp="$(PORTUNUS_SKIP_IP_PROBE=1 bash "$script" server --domain d.example.com --skip-dns-check --dry-run 2>&1)"
echo "$dp" | grep -q 'advertised:[[:space:]]*d.example.com:7443' || fail "dry-run plan missing derived advertised"

# --- render-dropin minimal (no flags) carries no advertised flag ---
dm="$(bash "$script" server --render-dropin)" || fail "render-dropin minimal exit"
echo "$dm" | grep -qx 'ExecStart=/usr/local/bin/portunus-server --data-dir /var/lib/portunus serve' || fail "minimal ExecStart unexpected"
if echo "$dm" | grep -q -- '--advertised-endpoint'; then fail "minimal drop-in must not advertise"; fi

# --- standalone role: dry-run plan accepted, artifact name correct ---
out_sa="$(bash "$script" standalone --version 1.4.1 --dry-run)" || fail "standalone dry-run exit non-zero"
echo "$out_sa" | grep -q '^role:[[:space:]]*standalone$' || fail "standalone: role line missing"
echo "$out_sa" | grep -q 'portunus-standalone' || fail "standalone: portunus-standalone not in plan"

# --- standalone role: explicit install verb ---
bash "$script" install standalone --version 1.0.0 --dry-run >/dev/null 2>&1 || fail "standalone: install verb rejected"

# --- standalone role: --help does not error with 'role required' ---
out_sh="$(bash "$script" standalone --help 2>&1 || true)"
case "$out_sh" in
  *"role required"*) fail "standalone --help printed 'role required'" ;;
esac

# --- standalone role: bogus-role test still rejects unknown roles ---
if bash "$script" bogus_standalone --dry-run >/dev/null 2>&1; then fail "bogus_standalone role accepted"; fi

# --- standalone: config get/set is rejected (non-applicable role) ---
sa_tmp="$(mktemp -d)"
printf 'role=standalone\ndeploy=binary\nversion=1.0.0\nlang=en\n' > "$sa_tmp/.install-meta"
sa_out="$(bash "$script" --compose-dir "$sa_tmp" config get advertised-endpoint 2>&1 || true)"
echo "$sa_out" | grep -qi 'standalone' || fail "standalone config: rejection message missing 'standalone'"
if bash "$script" --compose-dir "$sa_tmp" config get advertised-endpoint >/dev/null 2>&1; then
  fail "standalone config get must exit non-zero"
fi
rm -rf "$sa_tmp"

# --- shellcheck (skipped if not installed, but must pass if present) ---
if command -v shellcheck >/dev/null 2>&1; then
  shellcheck -s bash -S warning "$script" || fail "shellcheck warnings"
else
  echo "note: shellcheck not installed; skipping lint gate" >&2
fi

echo "PASS"
