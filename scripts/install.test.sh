#!/usr/bin/env bash
# Network-free smoke test for scripts/install.sh.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
script="$here/install.sh"

# Interpreter under test (default bash). The HARNESS runs under bash; the
# SCRIPT UNDER TEST runs under $SH so we can prove POSIX-sh compatibility.
# $SH is intentionally unquoted so multi-word values ("busybox sh") split.
SH="${TEST_SH:-bash}"
$SH -n "$script" || { echo "FAIL: parse error under $SH" >&2; exit 1; }

fail() { echo "FAIL: $1" >&2; exit 1; }

# --- existing dry-run contract (now under bash) ---
out="$($SH "$script" client --version 1.4.1 --dry-run)" || fail "exit non-zero"
echo "$out" | grep -q '^role:[[:space:]]*client$' || fail "role line"
echo "$out" | grep -q '^tag:[[:space:]]*v1.4.1$' || fail "tag line"
echo "$out" | grep -q '^artifact_version:[[:space:]]*1.4.1$' || fail "artifact_version line"
echo "$out" | grep -q 'releases/download/v1.4.1/portunus-1.4.1-.*\.tar\.gz' || fail "download_url"
echo "$out" | grep -q 'portunus-1.4.1-checksums\.txt' || fail "checksums_url"

out2="$($SH "$script" server --version v2.0.0 --dry-run)" || fail "v-prefixed exit"
echo "$out2" | grep -q '^role:[[:space:]]*server$' || fail "server role"
echo "$out2" | grep -q '^tag:[[:space:]]*v2.0.0$' || fail "v-normalised tag"
echo "$out2" | grep -q '^artifact_version:[[:space:]]*2.0.0$' || fail "v-normalised artifact"

if $SH "$script" bogus --dry-run >/dev/null 2>&1; then fail "bogus role accepted"; fi
$SH "$script" client --version 1.0.0 --yes --dry-run >/dev/null 2>&1 || fail "--yes flag rejected"

# --- i18n key coverage: every EN key exists in ZH and vice-versa ---
keys_en="$($SH "$script" --print-i18n-keys en | sort)"
keys_zh="$($SH "$script" --print-i18n-keys zh | sort)"
[ -n "$keys_en" ] || fail "no EN i18n keys"
[ "$keys_en" = "$keys_zh" ] || fail "i18n EN/ZH key sets differ"

# --- explicit lang override wins ---
o="$($SH "$script" --lang zh --print-i18n menu_title)"; printf '%s\n' "$o" | grep -q '管理' || fail "zh menu_title"
o="$(PORTUNUS_LANG=en $SH "$script" --print-i18n menu_title)"; printf '%s\n' "$o" | grep -qi 'manager' || fail "en menu_title"

# --- new flags accepted in dry-run plan ---
o="$($SH "$script" server --deploy docker --advertised-endpoint h.example:7443 --data-dir /srv/p --operator-http-listen 0.0.0.0:7080 --version 1.0.0 --dry-run)" || fail "new flags exit"
echo "$o" | grep -q '^deploy:[[:space:]]*docker$' || fail "deploy docker"
echo "$o" | grep -q '^advertised:[[:space:]]*h.example:7443$' || fail "advertised line"

# --- bare role implies install verb; explicit verb parsed ---
$SH "$script" install client --version 1.0.0 --dry-run >/dev/null 2>&1 || fail "install verb"
$SH "$script" status --help >/dev/null 2>&1 || fail "status+help"

# --- non-interactive when no tty and no args: helpful error, non-zero ---
if echo "" | $SH "$script" </dev/null >/dev/null 2>&1; then fail "no-arg no-tty should error"; fi

# --- meta round-trip via test seam ---
tmpm="$(mktemp -d)"
$SH "$script" --meta-write "$tmpm/.install-meta" role=server deploy=docker version=1.2.3 lang=en >/dev/null || fail "meta write"
val="$($SH "$script" --meta-read "$tmpm/.install-meta" version)" || fail "meta read"
[ "$val" = "1.2.3" ] || fail "meta round-trip ($val)"
rm -rf "$tmpm"

# --- deploy-form detection from a compose fixture ---
tmpd="$(mktemp -d)"
printf 'services:\n  server:\n    image: portunus-server\n' > "$tmpd/compose.yml"
[ "$($SH "$script" --detect-deploy "$tmpd")" = "docker" ] || fail "detect docker"
rm -rf "$tmpd"

# --- server binary dry-run mentions drop-in target, writes nothing ---
sentinel="$(mktemp -d)"
o="$($SH "$script" server --version 1.0.0 --systemd --advertised-endpoint h.example:7443 --data-dir "$sentinel/data" --dry-run)" || fail "server dry-run"
echo "$o" | grep -q 'drop-in:.*portunus-server.service.d/10-portunus.conf' || fail "drop-in plan line"
[ -z "$(ls -A "$sentinel" 2>/dev/null)" ] || fail "dry-run wrote files"
rm -rf "$sentinel"

sb="$(mktemp -d)"
o="$($SH "$script" server --deploy docker --compose-dir "$sb" --advertised-endpoint d.example:7443 --version 1.0.0 --dry-run)" || fail "docker dry-run"
echo "$o" | grep -q '^deploy:[[:space:]]*docker$' || fail "docker deploy plan"
echo "$o" | grep -q "compose_dir:.*$sb" || fail "compose_dir plan"
[ -z "$(ls -A "$sb" 2>/dev/null)" ] || fail "docker dry-run wrote files"
rm -rf "$sb"

# config rejects unknown key
if $SH "$script" config set bogus x --dry-run >/dev/null 2>&1; then fail "bogus config key accepted"; fi
# config accepts a scoped key in dry-run (no meta ⇒ defaults to server)
$SH "$script" config get advertised-endpoint --dry-run >/dev/null 2>&1 || fail "config get scoped key"
# --dry-run config mirrors the real path's server-only role guard
for _role in client standalone; do
  _rt="$(mktemp -d)"; printf 'role=%s\ndeploy=binary\nversion=2.2.0\nlang=en\n' "$_role" > "$_rt/.install-meta"
  if $SH "$script" --compose-dir "$_rt" config get advertised-endpoint --dry-run >/dev/null 2>&1; then
    fail "--dry-run config must reject the $_role role"
  fi
  rm -rf "$_rt"
done
# uninstall dry-run performs nothing and exits 0
$SH "$script" uninstall server --dry-run >/dev/null 2>&1 || fail "uninstall dry-run"
# status dry-run exits 0
$SH "$script" status --dry-run >/dev/null 2>&1 || fail "status dry-run"

# --- drop-in is an ExecStart= override carrying the flags (env is inert) ---
dr="$($SH "$script" server --systemd --advertised-endpoint h:7443 --data-dir /srv/p --operator-http-listen 0.0.0.0:7080 --render-dropin)" || fail "render-dropin exit"
echo "$dr" | grep -qx 'ExecStart=' || fail "dropin missing ExecStart= clear line"
echo "$dr" | grep -qx 'ExecStart=/usr/local/bin/portunus-server --data-dir /srv/p serve --operator-http-listen 0.0.0.0:7080 --advertised-endpoint h:7443' || fail "dropin ExecStart override"
if echo "$dr" | grep -q 'Environment=PORTUNUS_ADVERTISED_ENDPOINT='; then fail "inert Environment line still emitted"; fi

# --- Fix 2: explicit --compose-dir never falls back to a system meta ---
cdtmp="$(mktemp -d)"            # empty: no .install-meta here
if $SH "$script" --compose-dir "$cdtmp" --resolve-meta >/dev/null 2>&1; then
  fail "Fix 2: empty --compose-dir must NOT resolve a fallback meta"
fi
printf 'role=server\ndeploy=docker\n' > "$cdtmp/.install-meta"
[ "$($SH "$script" --compose-dir "$cdtmp" --resolve-meta)" = "$cdtmp/.install-meta" ] || fail "Fix 2: scoped meta not resolved"
rm -rf "$cdtmp"

# --- Fix 4: next-step i18n keys exist in both languages ---
o="$($SH "$script" --lang en --print-i18n next_systemd)"; printf '%s\n' "$o" | grep -qi 'systemctl' || fail "en next_systemd"
o="$($SH "$script" --lang zh --print-i18n next_docker)"; printf '%s\n' "$o" | grep -q 'docker compose' || fail "zh next_docker"

# --- new enroll i18n keys resolve (not echoed back as the bare key) ---
for key in enroll_placed enroll_failed; do
  en="$($SH "$script" --lang en --print-i18n "$key")"; [ "$en" != "$key" ] || fail "en i18n missing: $key"
  zh="$($SH "$script" --lang zh --print-i18n "$key")"; [ "$zh" != "$key" ] || fail "zh i18n missing: $key"
done

# --- P2: light host:port validation ---
$SH "$script" --valid-endpoint "host.example:7443" || fail "valid endpoint rejected"
$SH "$script" --valid-endpoint "" || fail "blank endpoint must be allowed (auto)"
if $SH "$script" --valid-endpoint "no-port" 2>/dev/null; then fail "missing port accepted"; fi
if $SH "$script" --valid-endpoint "bad host:7443" 2>/dev/null; then fail "space in host accepted"; fi
if $SH "$script" --valid-endpoint "h:99999x" 2>/dev/null; then fail "non-numeric port accepted"; fi

# --- P1/P2: new interactive i18n keys present in both languages ---
o="$($SH "$script" --lang en --print-i18n ask_config_key)"; printf '%s\n' "$o" | grep -qi 'advertised-endpoint' || fail "en ask_config_key"
o="$($SH "$script" --lang zh --print-i18n ask_service_action)"; printf '%s\n' "$o" | grep -q '启动' || fail "zh ask_service_action"
o="$($SH "$script" --lang en --print-i18n menu_invalid bogus)"; printf '%s\n' "$o" | grep -qi 'invalid' || fail "en menu_invalid"

# --- wizard: IP detection seam, offline path never hits network ---
di="$(PORTUNUS_SKIP_IP_PROBE=1 $SH "$script" --detect-ip)" || fail "--detect-ip exit"
echo "$di" | grep -Eq '^[0-9a-fA-F.:]+ prov_(nic|loopback)$' || fail "skip-probe must yield NIC/loopback ($di)"

# --- --reset-lang clears the cached language preference ---
fakehome="$(mktemp -d)"
mkdir -p "$fakehome/.config/portunus"; printf 'zh' > "$fakehome/.config/portunus/installer-lang"
HOME="$fakehome" XDG_CONFIG_HOME="$fakehome/.config" $SH "$script" --reset-lang >/dev/null 2>&1 || fail "--reset-lang exit"
[ ! -e "$fakehome/.config/portunus/installer-lang" ] || fail "--reset-lang did not remove the cache"
rm -rf "$fakehome"

# --- Caddy: FQDN validation ---
$SH "$script" --valid-fqdn serverbee-test.900040.xyz || fail "valid fqdn rejected"
if $SH "$script" --valid-fqdn no_dot 2>/dev/null; then fail "fqdn without dot accepted"; fi
if $SH "$script" --valid-fqdn "bad host.com" 2>/dev/null; then fail "fqdn with space accepted"; fi
if $SH "$script" --valid-fqdn "-lead.com" 2>/dev/null; then fail "fqdn leading dash accepted"; fi
if $SH "$script" --valid-fqdn "" 2>/dev/null; then fail "empty fqdn accepted"; fi

# --- Caddy: managed block render ---
cb="$($SH "$script" --render-caddy serverbee-test.900040.xyz 7080)" || fail "render-caddy exit"
printf '%s\n' "$cb" | grep -qx '# >>> portunus >>>' || fail "missing start marker"
printf '%s\n' "$cb" | grep -qx '# <<< portunus <<<' || fail "missing end marker"
printf '%s\n' "$cb" | grep -qx 'serverbee-test.900040.xyz {' || fail "missing site line"
printf '%s\n' "$cb" | grep -q 'reverse_proxy 127.0.0.1:7080' || fail "missing reverse_proxy"

# --- Caddy: server dry-run plan includes role; client+--domain errors ---
od="$(PORTUNUS_SKIP_IP_PROBE=1 $SH "$script" server --domain serverbee-test.900040.xyz --skip-dns-check --dry-run 2>&1)" || fail "server --domain dry-run exit"
printf '%s\n' "$od" | grep -q '^role:[[:space:]]*server$' || fail "domain dry-run role"
if PORTUNUS_SKIP_IP_PROBE=1 $SH "$script" client --domain x.example.com --dry-run >/dev/null 2>&1; then fail "client --domain must error"; fi

# --- Caddy: domain verb dry-run writes nothing, exits 0 ---
PORTUNUS_SKIP_IP_PROBE=1 $SH "$script" domain serverbee-test.900040.xyz --skip-dns-check --dry-run >/dev/null 2>&1 || fail "domain verb dry-run"

# --- Caddy: EN/ZH i18n parity for the new keys ---
diff <($SH "$script" --print-i18n-keys en | sort) <($SH "$script" --print-i18n-keys zh | sort) >/dev/null || fail "EN/ZH i18n key parity broken"
# The key-parity check above compares the same $I18N_KEYS string twice, so it
# cannot catch a missing `zh:`/`*:` case arm (it falls through to the wildcard).
# Assert each renders, and that EN and ZH actually differ, for the config keys
# this work renamed/added.
for _k in config_server_only bad_config_value; do
  _en="$($SH "$script" --lang en --print-i18n "$_k")"
  _zh="$($SH "$script" --lang zh --print-i18n "$_k")"
  [ "$_en" != "$_k" ] || fail "i18n: en arm missing for $_k"
  [ "$_zh" != "$_k" ] || fail "i18n: zh arm missing for $_k"
  [ "$_en" != "$_zh" ] || fail "i18n: en/zh identical for $_k (a case arm is likely missing)"
done

# --- advertised precedence: domain derives, explicit wins ---
ea1="$(PORTUNUS_SKIP_IP_PROBE=1 $SH "$script" server --domain d.example.com --effective-advertised 2>/dev/null)"
[ "$ea1" = "d.example.com:7443" ] || fail "domain should derive advertised d.example.com:7443 (got '$ea1')"
ea2="$(PORTUNUS_SKIP_IP_PROBE=1 $SH "$script" server --domain d.example.com --advertised-endpoint x.host:7443 --effective-advertised 2>/dev/null)"
[ "$ea2" = "x.host:7443" ] || fail "explicit advertised must win (got '$ea2')"
ea3="$(PORTUNUS_SKIP_IP_PROBE=1 $SH "$script" server --effective-advertised 2>/dev/null)"
[ -z "$ea3" ] || fail "no domain/explicit => empty effective advertised (got '$ea3')"

# --- dry-run plan shows the derived advertised ---
dp="$(PORTUNUS_SKIP_IP_PROBE=1 $SH "$script" server --domain d.example.com --skip-dns-check --dry-run 2>&1)"
echo "$dp" | grep -q 'advertised:[[:space:]]*d.example.com:7443' || fail "dry-run plan missing derived advertised"

# --- render-dropin minimal (no flags) carries no advertised flag ---
dm="$($SH "$script" server --render-dropin)" || fail "render-dropin minimal exit"
echo "$dm" | grep -qx 'ExecStart=/usr/local/bin/portunus-server --data-dir /var/lib/portunus serve' || fail "minimal ExecStart unexpected"
if echo "$dm" | grep -q -- '--advertised-endpoint'; then fail "minimal drop-in must not advertise"; fi

# --- standalone role: dry-run plan accepted, artifact name correct ---
out_sa="$($SH "$script" standalone --version 1.4.1 --dry-run)" || fail "standalone dry-run exit non-zero"
echo "$out_sa" | grep -q '^role:[[:space:]]*standalone$' || fail "standalone: role line missing"
echo "$out_sa" | grep -Eq '^service:[[:space:]]*install \+ start' || fail "standalone: default install must start the service"
echo "$out_sa" | grep -Eq '^init:[[:space:]]*(systemd|openrc|none)$' || fail "standalone: plan must report detected init"
echo "$out_sa" | grep -q 'portunus-standalone' || fail "standalone: portunus-standalone not in plan"
echo "$out_sa" | grep -q '^config:' || fail "standalone: plan must show the config line"

# --- standalone --config: a relative path is absolutized in the plan ---
oc="$(cd /tmp && $SH "$script" standalone --version 1.4.1 --config rel.toml --dry-run)" || fail "relative --config dry-run"
printf '%s\n' "$oc" | grep -Eq '^config:[[:space:]]*/' || fail "relative --config must absolutize to an absolute path"

# --- --no-service flips the plan to install-only ---
out_ns="$($SH "$script" standalone --version 1.4.1 --no-service --dry-run)" || fail "--no-service dry-run exit"
echo "$out_ns" | grep -Eq '^service:[[:space:]]*install only' || fail "--no-service: plan must say install only"

# --- --systemd is accepted as a back-compat no-op ---
$SH "$script" standalone --version 1.4.1 --systemd --dry-run >/dev/null 2>&1 || fail "--systemd back-compat no-op rejected"

# --- --config is standalone-only; rejected for client/server ---
$SH "$script" standalone --version 1.4.1 --config /etc/portunus/my.toml --dry-run >/dev/null 2>&1 || fail "standalone --config rejected"
if $SH "$script" server --config /etc/portunus/x.toml --dry-run >/dev/null 2>&1; then fail "server --config must error"; fi
if $SH "$script" client --config /etc/portunus/x.toml --dry-run >/dev/null 2>&1; then fail "client --config must error"; fi

# --- --enroll: client dry-run plan surfaces a redacted enroll_uri ---
o="$($SH "$script" client --enroll 'portunus://example.com:7443/enroll?pin=sha256:abc&code=secret' --version 1.0.0 --dry-run)" || fail "--enroll dry-run exit"
echo "$o" | grep -q '^enroll_uri:[[:space:]]*portunus://example.com:7443/enroll' || fail "enroll_uri line in plan"
echo "$o" | grep -q 'code=secret' && fail "enroll_uri must NOT leak the code"

# --- --enroll: rejected for non-client roles ---
if $SH "$script" server --enroll 'portunus://x:1/enroll?code=y' --version 1.0.0 --dry-run >/dev/null 2>&1; then
  fail "--enroll must error for non-client roles"
fi

# --- --enroll: rejected with --deploy docker (Docker uses PORTUNUS_ENROLL_URI) ---
if $SH "$script" client --enroll 'portunus://x:1/enroll?code=y' --deploy docker --version 1.0.0 --dry-run >/dev/null 2>&1; then
  fail "--enroll must error with --deploy docker"
fi

# --- --enroll: requires a value ---
if $SH "$script" client --enroll >/dev/null 2>&1; then
  fail "--enroll with no value must error"
fi

# --- --detect-init prints one of the known init systems ---
oi="$($SH "$script" --detect-init)" || fail "--detect-init exit"
case "$oi" in systemd|openrc|none) : ;; *) fail "--detect-init bad value '$oi'" ;; esac

# --- --render-openrc emits a valid openrc-run service for each role ---
for role in standalone client server; do
  orc="$($SH "$script" --render-openrc "$role")" || fail "--render-openrc $role exit"
  printf '%s\n' "$orc" | grep -q '^#!/sbin/openrc-run' || fail "openrc $role: missing shebang"
  printf '%s\n' "$orc" | grep -q "^command=\"/usr/local/bin/portunus-$role\"" || fail "openrc $role: command line"
  printf '%s\n' "$orc" | grep -q '^supervisor=supervise-daemon' || fail "openrc $role: supervisor"
done

# --- --render-confd reflects the role's config knob ---
cf_sa="$($SH "$script" --render-confd standalone /etc/portunus/custom.toml)" || fail "--render-confd standalone exit"
printf '%s\n' "$cf_sa" | grep -q 'cfgfile="/etc/portunus/custom.toml"' || fail "confd standalone: cfgfile not honored"
cf_cl="$($SH "$script" --render-confd client)" || fail "--render-confd client exit"
printf '%s\n' "$cf_cl" | grep -q '^bundle=' || fail "confd client: bundle knob missing"
cf_sv="$($SH "$script" --render-confd server)" || fail "--render-confd server exit"
printf '%s\n' "$cf_sv" | grep -q '^datadir=' || fail "confd server: datadir knob missing"

# --- --render-config-dropin (systemd custom config path) for standalone ---
cd_sa="$($SH "$script" --render-config-dropin standalone /etc/portunus/custom.toml)" || fail "--render-config-dropin exit"
printf '%s\n' "$cd_sa" | grep -q '/etc/portunus/custom.toml' || fail "config drop-in: custom path missing"

# --- standalone role: explicit install verb ---
$SH "$script" install standalone --version 1.0.0 --dry-run >/dev/null 2>&1 || fail "standalone: install verb rejected"

# --- standalone role: --help does not error with 'role required' ---
out_sh="$($SH "$script" standalone --help 2>&1 || true)"
case "$out_sh" in
  *"role required"*) fail "standalone --help printed 'role required'" ;;
esac

# --- standalone role: bogus-role test still rejects unknown roles ---
if $SH "$script" bogus_standalone --dry-run >/dev/null 2>&1; then fail "bogus_standalone role accepted"; fi

# --- standalone: config get/set is rejected (non-applicable role) ---
sa_tmp="$(mktemp -d)"
printf 'role=standalone\ndeploy=binary\nversion=1.0.0\nlang=en\n' > "$sa_tmp/.install-meta"
sa_out="$($SH "$script" --compose-dir "$sa_tmp" config get advertised-endpoint 2>&1 || true)"
echo "$sa_out" | grep -qi 'standalone' || fail "standalone config: rejection message missing 'standalone'"
if $SH "$script" --compose-dir "$sa_tmp" config get advertised-endpoint >/dev/null 2>&1; then
  fail "standalone config get must exit non-zero"
fi
rm -rf "$sa_tmp"

# --- config get/set reads & writes the compose `command:` array (docker) ---
# The server consumes advertised/op-http as CLI flags, so config must target the
# compose command (NOT an inert .env line). Network-free: no `up -d` is run
# (the restart confirm defaults to no under a non-TTY stdin).
dk_tmp="$(mktemp -d)"
printf 'role=server\ndeploy=docker\nversion=2.2.0\nlang=en\n' > "$dk_tmp/.install-meta"
cat > "$dk_tmp/compose.yml" <<'YAML'
services:
  server:
    image: ghcr.io/zingerlittlebee/portunus-server:2.2.0
    container_name: portunus-server
    env_file: [ .env ]
    command: ["--data-dir", "/var/lib/portunus", "serve", "--operator-http-listen", "0.0.0.0:7080", "--advertised-endpoint", "old.example:7443"]
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
# get reads the value out of the command array
g="$($SH "$script" --compose-dir "$dk_tmp" config get advertised-endpoint 2>/dev/null)" || fail "docker config get exit"
[ "$g" = "old.example:7443" ] || fail "docker config get advertised-endpoint: got '$g'"
# set rewrites the command array (and preserves the image tag). Without
# --restart no `up -d` runs, so this stays network-free / docker-less.
$SH "$script" --compose-dir "$dk_tmp" config set advertised-endpoint new.example:7443 >/dev/null 2>&1 \
  || fail "docker config set exit"
grep -q '"--advertised-endpoint", "new.example:7443"' "$dk_tmp/compose.yml" || fail "docker config set did not update command"
grep -q 'portunus-server:2.2.0' "$dk_tmp/compose.yml" || fail "docker config set must preserve the pinned image tag"
# re-reading reflects the new value (round-trip through the rewritten compose)
g2="$($SH "$script" --compose-dir "$dk_tmp" config get advertised-endpoint 2>/dev/null)" || fail "docker config get (post-set) exit"
[ "$g2" = "new.example:7443" ] || fail "docker config get after set: got '$g2'"
# data-dir is not settable on docker (fixed volume mount) — must reject
if $SH "$script" --compose-dir "$dk_tmp" config set data-dir /tmp/x >/dev/null 2>&1; then
  fail "docker config set data-dir must be rejected"
fi
# operator-http-listen set must re-sync the PUBLISHED port too, not just the
# command flag, and must preserve the advertised-endpoint set above.
$SH "$script" --compose-dir "$dk_tmp" config set operator-http-listen 0.0.0.0:9090 >/dev/null 2>&1 \
  || fail "docker config set operator-http-listen exit"
grep -q '"--operator-http-listen", "0.0.0.0:9090"' "$dk_tmp/compose.yml" || fail "docker op-http: command flag not updated"
grep -q '127.0.0.1:9090:9090' "$dk_tmp/compose.yml" || fail "docker op-http: published port not re-synced"
grep -q '"--advertised-endpoint", "new.example:7443"' "$dk_tmp/compose.yml" || fail "docker op-http set must preserve advertised-endpoint"
# a backup of the prior compose is left behind (recoverable hand-edits)
ls "$dk_tmp"/compose.yml.portunus.*.bak >/dev/null 2>&1 || fail "docker config set must leave a timestamped .bak"
# injection-bearing values are rejected before any write (JSON breakout)
if $SH "$script" --compose-dir "$dk_tmp" config set advertised-endpoint 'x", "evil' >/dev/null 2>&1; then
  fail "docker config set must reject a JSON-breakout value"
fi
rm -rf "$dk_tmp"

# --- config get/set is init-aware for a binary server (systemd & openrc) ---
# PORTUNUS_TEST_CONFIG_ROOT redirects the root-owned drop-in / conf.d under a
# temp dir and disables sudo + systemctl, so the binary path (the primary fix)
# is exercisable unprivileged. systemd writes the ExecStart override:
bs_tmp="$(mktemp -d)"; bs_root="$(mktemp -d)"
printf 'role=server\ndeploy=binary\nversion=2.2.0\nlang=en\ninit=systemd\n' > "$bs_tmp/.install-meta"
[ "$($SH "$script" --compose-dir "$bs_tmp" config get advertised-endpoint 2>/dev/null)" = "<unset>" ] \
  || fail "binary/systemd: fresh advertised should be <unset>"
PORTUNUS_TEST_CONFIG_ROOT="$bs_root" $SH "$script" --compose-dir "$bs_tmp" config set advertised-endpoint h.example:7443 >/dev/null 2>&1 \
  || fail "binary/systemd config set advertised exit"
PORTUNUS_TEST_CONFIG_ROOT="$bs_root" $SH "$script" --compose-dir "$bs_tmp" config set operator-http-listen 127.0.0.1:7080 >/dev/null 2>&1 \
  || fail "binary/systemd config set op-http exit"
dropin="$bs_root/etc/systemd/system/portunus-server.service.d/10-portunus.conf"
grep -q -- '--advertised-endpoint h.example:7443' "$dropin" || fail "binary/systemd: ExecStart missing advertised"
grep -q -- '--operator-http-listen 127.0.0.1:7080' "$dropin" || fail "binary/systemd: ExecStart missing op-http (sibling not preserved)"
[ "$(PORTUNUS_TEST_CONFIG_ROOT="$bs_root" $SH "$script" --compose-dir "$bs_tmp" config get advertised-endpoint 2>/dev/null)" = "h.example:7443" ] \
  || fail "binary/systemd: get advertised round-trip"
[ "$(PORTUNUS_TEST_CONFIG_ROOT="$bs_root" $SH "$script" --compose-dir "$bs_tmp" config get data-dir 2>/dev/null)" = "/var/lib/portunus" ] \
  || fail "binary/systemd: get data-dir reads the ExecStart flag (regression: old code returned <unset>)"
rm -rf "$bs_tmp" "$bs_root"

# openrc keeps the flags in /etc/conf.d/portunus-server's server_args=/datadir=.
# Seed a HAND-ANNOTATED custom datadir (trailing comment) to prove a non-default
# value survives an unrelated set rather than reverting to /var/lib/portunus.
or_tmp="$(mktemp -d)"; or_root="$(mktemp -d)"
printf 'role=server\ndeploy=binary\nversion=2.2.0\nlang=en\ninit=openrc\n' > "$or_tmp/.install-meta"
mkdir -p "$or_root/etc/conf.d"
printf 'datadir="/mnt/ssd/portunus"   # fast disk\nserver_args="--operator-http-listen 0.0.0.0:7080"\n' > "$or_root/etc/conf.d/portunus-server"
[ "$(PORTUNUS_TEST_CONFIG_ROOT="$or_root" $SH "$script" --compose-dir "$or_tmp" config get operator-http-listen 2>/dev/null)" = "0.0.0.0:7080" ] \
  || fail "binary/openrc: get op-http from server_args"
[ "$(PORTUNUS_TEST_CONFIG_ROOT="$or_root" $SH "$script" --compose-dir "$or_tmp" config get data-dir 2>/dev/null)" = "/mnt/ssd/portunus" ] \
  || fail "binary/openrc: get data-dir tolerates a trailing comment"
PORTUNUS_TEST_CONFIG_ROOT="$or_root" $SH "$script" --compose-dir "$or_tmp" config set advertised-endpoint h.example:7443 >/dev/null 2>&1 \
  || fail "binary/openrc config set advertised exit"
grep -q -- '--operator-http-listen 0.0.0.0:7080 --advertised-endpoint h.example:7443' "$or_root/etc/conf.d/portunus-server" \
  || fail "binary/openrc: server_args must carry both flags (sibling preserved)"
grep -q 'datadir="/mnt/ssd/portunus"' "$or_root/etc/conf.d/portunus-server" \
  || fail "binary/openrc: custom datadir must survive an unrelated set (not revert to default)"
ls "$or_root/etc/conf.d/portunus-server.portunus."*.bak >/dev/null 2>&1 \
  || fail "binary/openrc: config set must leave a timestamped conf.d .bak"
# data-dir IS settable on openrc (unlike docker): updates datadir=, leaves server_args
PORTUNUS_TEST_CONFIG_ROOT="$or_root" $SH "$script" --compose-dir "$or_tmp" config set data-dir /srv/portunus >/dev/null 2>&1 \
  || fail "binary/openrc config set data-dir exit"
grep -q 'datadir="/srv/portunus"' "$or_root/etc/conf.d/portunus-server" || fail "binary/openrc: data-dir set did not update datadir="
grep -q -- '--advertised-endpoint h.example:7443' "$or_root/etc/conf.d/portunus-server" || fail "binary/openrc: data-dir set must preserve server_args"
rm -rf "$or_tmp" "$or_root"

# docker config set preserves an operator's compose.yaml FILENAME (not just .yml)
dy_tmp="$(mktemp -d)"
printf 'role=server\ndeploy=docker\nversion=2.2.0\nlang=en\n' > "$dy_tmp/.install-meta"
cat > "$dy_tmp/compose.yaml" <<'YAML'
services:
  server:
    image: ghcr.io/zingerlittlebee/portunus-server:2.2.0
    command: ["--data-dir", "/var/lib/portunus", "serve", "--operator-http-listen", "0.0.0.0:7080"]
volumes:
  portunus-data:
    name: portunus-data
YAML
$SH "$script" --compose-dir "$dy_tmp" config set advertised-endpoint y.example:7443 >/dev/null 2>&1 \
  || fail "docker config set (.yaml) exit"
[ -f "$dy_tmp/compose.yaml" ] || fail "docker config set must keep the operator's compose.yaml filename"
[ ! -f "$dy_tmp/compose.yml" ] || fail "docker config set must not leave a stray compose.yml"
grep -q '"--advertised-endpoint", "y.example:7443"' "$dy_tmp/compose.yaml" || fail "docker .yaml: advertised not updated"
ls "$dy_tmp/compose.yaml.portunus."*.bak >/dev/null 2>&1 || fail "docker .yaml: backup must be named compose.yaml.*.bak"
rm -rf "$dy_tmp"

# docker config set with BOTH compose.yml and compose.yaml present: edit the
# file compose v2 actually uses (compose.yaml), and back up EVERY file so the
# prior unbacked-deletion bug cannot recur (and a regression would be caught).
db_tmp="$(mktemp -d)"
printf 'role=server\ndeploy=docker\nversion=2.2.0\nlang=en\n' > "$db_tmp/.install-meta"
cat > "$db_tmp/compose.yml" <<'YAML'
services:
  server:
    image: ghcr.io/zingerlittlebee/portunus-server:2.2.0
    command: ["--data-dir", "/var/lib/portunus", "serve", "--operator-http-listen", "0.0.0.0:7080"]
YAML
cat > "$db_tmp/compose.yaml" <<'YAML'
services:
  server:
    image: ghcr.io/zingerlittlebee/portunus-server:2.2.0
    command: ["--data-dir", "/var/lib/portunus", "serve", "--operator-http-listen", "0.0.0.0:7080"]
  sidecar:
    image: nginx
YAML
$SH "$script" --compose-dir "$db_tmp" config set advertised-endpoint y2.example:7443 >/dev/null 2>&1 \
  || fail "docker config set (both files) exit"
grep -q '"--advertised-endpoint", "y2.example:7443"' "$db_tmp/compose.yaml" || fail "docker both-files: effective compose.yaml not updated"
[ ! -f "$db_tmp/compose.yml" ] || fail "docker both-files: stale compose.yml should be removed"
ls "$db_tmp/compose.yaml.portunus."*.bak >/dev/null 2>&1 || fail "docker both-files: compose.yaml backup missing"
ls "$db_tmp/compose.yml.portunus."*.bak >/dev/null 2>&1 || fail "docker both-files: compose.yml deleted UNBACKED (the prior bug)"
grep -q sidecar "$db_tmp/compose.yaml.portunus."*.bak || fail "docker both-files: operator sidecar must be recoverable from .bak"
rm -rf "$db_tmp"

# --- config get/set rejects the client role (keys are server-only) ---
cl_tmp="$(mktemp -d)"
printf 'role=client\ndeploy=binary\nversion=2.2.0\nlang=en\ninit=systemd\n' > "$cl_tmp/.install-meta"
cl_out="$($SH "$script" --compose-dir "$cl_tmp" config get advertised-endpoint 2>&1 || true)"
echo "$cl_out" | grep -qi 'server' || fail "client config: rejection message should mention server-only"
if $SH "$script" --compose-dir "$cl_tmp" config get advertised-endpoint >/dev/null 2>&1; then
  fail "client config get must exit non-zero"
fi
rm -rf "$cl_tmp"

# --- acme-email validation (security: Caddyfile directive injection) ---
# A well-formed single-line email passes the predicate hook.
$SH "$script" --valid-email "ops@example.com" || fail "valid acme-email rejected"
$SH "$script" --valid-email "first.last+tag@sub.example.co" || fail "valid plus-tagged acme-email rejected"
# Injection vectors are rejected: a newline (carrying extra Caddy directives),
# embedded whitespace, Caddy metacharacters, and a dotless / malformed domain.
inj="$(printf 'x@y.com\n    reverse_proxy 127.0.0.1:9999')"
if $SH "$script" --valid-email "$inj" >/dev/null 2>&1; then fail "newline-injection acme-email accepted"; fi
if $SH "$script" --valid-email "a b@example.com" >/dev/null 2>&1; then fail "whitespace acme-email accepted"; fi
if $SH "$script" --valid-email 'a@b{}.com' >/dev/null 2>&1; then fail "brace acme-email accepted"; fi
if $SH "$script" --valid-email "a@b" >/dev/null 2>&1; then fail "dotless-domain acme-email accepted"; fi
if $SH "$script" --valid-email "a@@b.com" >/dev/null 2>&1; then fail "double-at acme-email accepted"; fi
if $SH "$script" --valid-email "" >/dev/null 2>&1; then fail "empty acme-email accepted by predicate"; fi
# Enforcement: a malicious --acme-email aborts before any side effect, on the
# same dry-run path a clean value passes.
if $SH "$script" server install --domain example.com --acme-email "$inj" --version 1.0.0 --dry-run >/dev/null 2>&1; then
  fail "injection acme-email survived validation"
fi
$SH "$script" server install --domain example.com --acme-email "ops@example.com" --version 1.0.0 --dry-run >/dev/null 2>&1 \
  || fail "clean acme-email rejected by validation"

# --- layered help: short --help stays terse; --help-all carries the CI seams ---
h="$($SH "$script" --help 2>&1)" || fail "--help exit"
printf '%s\n' "$h" | grep -qi 'interactive wizard' || fail "--help should mention the interactive wizard"
if printf '%s\n' "$h" | grep -q -- '--meta-write'; then fail "--help must not expose CI seams"; fi
ha="$($SH "$script" --help-all 2>&1)" || fail "--help-all exit"
printf '%s\n' "$ha" | grep -q -- '--meta-write' || fail "--help-all should list CI seams"
printf '%s\n' "$ha" | grep -q -- '--render-caddy' || fail "--help-all should list render seams"

# --- shellcheck (skipped if not installed, but must pass if present) ---
if command -v shellcheck >/dev/null 2>&1; then
  shellcheck -s sh -S warning "$script" || fail "shellcheck warnings"
else
  echo "note: shellcheck not installed; skipping lint gate" >&2
fi

echo "PASS"
