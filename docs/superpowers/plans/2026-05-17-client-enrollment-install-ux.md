# Client Enrollment Install UX Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give operators a stepped install experience (WebUI wizard + one-line `install.sh`) for the new client enrollment flow, with a single-source-of-truth docs rewrite (EN + ZH).

**Architecture:** Add a bare `uri` to the operator-HTTP enrollment response only (no proto/gRPC change). Build a shared `EnrollmentInstallGuide` React component with Shell/systemd/Docker tabs and a live countdown, used by both provision and re-enroll. Add `scripts/install.sh` (role-parameterised, downloads release binary + the existing hardened systemd unit, per-op `sudo`). Rewrite docs so `configuration/client` is authoritative and other pages reference it.

**Tech Stack:** Rust (axum operator HTTP, `crates/portunus-server`), React + Vite + TypeScript + vitest + @testing-library/react (`webui/`), POSIX `sh`, Fumadocs MDX (`docs/`).

**Spec:** `docs/superpowers/specs/2026-05-17-client-enrollment-install-ux-design.md`
**Branch:** `feat/client-enrollment-install-ux` (no worktree).

---

## File Structure

**Created:**
- `scripts/install.sh` — curl-able installer (role param, `--systemd`, `--dry-run`).
- `scripts/install.test.sh` — POSIX dry-run smoke test (network-free).
- `webui/src/components/EnrollmentInstallGuide.tsx` — shared stepped wizard.
- `webui/tests/unit/enrollment-install-guide-render.test.tsx` — render test for the new component.

**Modified (Rust):**
- `crates/portunus-server/src/operator/cli.rs` — split URI builder; add `uri` to `EnrollmentCommand`.
- `crates/portunus-server/src/operator/http.rs` — add `uri` to `EnrollmentResponse` + both construction sites.
- `crates/portunus-server/tests/http_client_enrollments_contract.rs` — assert `uri`.

**Modified (WebUI):**
- `webui/src/api/types.ts` — add `uri` to `ClientEnrollmentResponse`.
- `webui/src/pages/ClientProvision.tsx` — use `EnrollmentInstallGuide`, delete `EnrollmentCommandCard`.
- `webui/src/pages/ClientDetail.tsx` — use `EnrollmentInstallGuide`, delete `ReEnrollmentCommandCard`.
- `webui/src/i18n/en.json`, `webui/src/i18n/zh-CN.json` — add `clientProvision.guide.*`, drop unused keys.
- `webui/tests/unit/client-provision-enrollment.test.ts`, `webui/tests/unit/client-detail-reenrollment.test.ts` — update grep assertions.

**Modified (docs + tooling):**
- `deploy/systemd/install.sh` — drop stale `provision-client` reference.
- `docs/content/docs/configuration/client.mdx` (authoritative) + `deployment/{systemd,docker,railway}.mdx` + `cli/walkthrough.mdx` + `README.md`, and the `docs/content/docs/zh/...` mirrors.

**Unchanged (explicitly):** `proto/portunus.proto`, gRPC `ClientEnrollment`, `crates/portunus-proto/tests/enrollment_wire_compat.rs`.

---

## Task 1: Add `uri` to the operator-HTTP enrollment response

**Files:**
- Modify: `crates/portunus-server/src/operator/cli.rs:459-551`
- Modify: `crates/portunus-server/src/operator/http.rs:178-216`
- Modify: `webui/src/api/types.ts:315-319`
- Test: `crates/portunus-server/tests/http_client_enrollments_contract.rs`

- [ ] **Step 1: Add failing `uri` assertions to the contract test**

In `crates/portunus-server/tests/http_client_enrollments_contract.rs`, in `create_enrollment_returns_one_time_client_command_without_issuing_token`, after the existing `command` assertions and before `assert!(tokens.list()...)`, insert:

```rust
    let uri = body["uri"].as_str().expect("uri");
    assert!(uri.starts_with("portunus://control.example.com:7443/enroll?"));
    assert!(uri.contains("pin=sha256:"));
    assert!(uri.contains("code="));
    assert!(uri.contains("cert="));
    assert_eq!(command, format!("portunus-client enroll '{uri}'"));
```

In `existing_client_enrollment_does_not_rotate_until_redeemed`, after the `command` assertion and before the `tokens.verify` assertion, insert:

```rust
    let uri = body["uri"].as_str().expect("uri");
    assert!(uri.starts_with("portunus://control.example.com:7443/enroll?"));
    assert_eq!(command, format!("portunus-client enroll '{uri}'"));
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test http_client_enrollments_contract 2>&1 | tail -20`
Expected: FAIL — `uri` is `Null` so `.as_str().expect("uri")` panics in both tests.

- [ ] **Step 3: Split the URI builder and extend `EnrollmentCommand` (cli.rs)**

In `crates/portunus-server/src/operator/cli.rs`, replace the struct (lines 459-463):

```rust
pub struct EnrollmentCommand {
    pub client_name: ClientName,
    pub expires_at: chrono::DateTime<Utc>,
    pub command: String,
}
```

with:

```rust
pub struct EnrollmentCommand {
    pub client_name: ClientName,
    pub expires_at: chrono::DateTime<Utc>,
    pub command: String,
    pub uri: String,
}
```

In `create_enrollment_command`, replace:

```rust
    let command = enrollment_command(state, &created.code);
    info!(
        event,
        client_name = %created.client_name,
        expires_at = %created.expires_at,
    );
    Ok(EnrollmentCommand {
        client_name: name,
        expires_at: created.expires_at,
        command,
    })
}
```

with:

```rust
    let uri = enrollment_uri(state, &created.code);
    let command = format!("portunus-client enroll '{uri}'");
    info!(
        event,
        client_name = %created.client_name,
        expires_at = %created.expires_at,
    );
    Ok(EnrollmentCommand {
        client_name: name,
        expires_at: created.expires_at,
        command,
        uri,
    })
}
```

Replace the whole `enrollment_command` fn:

```rust
fn enrollment_command(state: &AppState, code: &str) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let cert = URL_SAFE_NO_PAD.encode(state.server_cert_pem.as_bytes());
    let uri = format!(
        "portunus://{}/enroll?pin=sha256:{}&code={}&cert={}",
        state.server_endpoint, state.server_cert_sha256, code, cert
    );
    format!("portunus-client enroll '{uri}'")
}
```

with:

```rust
fn enrollment_uri(state: &AppState, code: &str) -> String {
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let cert = URL_SAFE_NO_PAD.encode(state.server_cert_pem.as_bytes());
    format!(
        "portunus://{}/enroll?pin=sha256:{}&code={}&cert={}",
        state.server_endpoint, state.server_cert_sha256, code, cert
    )
}
```

- [ ] **Step 4: Add `uri` to the HTTP response struct + both sites (http.rs)**

In `crates/portunus-server/src/operator/http.rs`, replace:

```rust
#[derive(Debug, serde::Serialize)]
struct EnrollmentResponse {
    client_name: String,
    expires_at: String,
    command: String,
}
```

with:

```rust
#[derive(Debug, serde::Serialize)]
struct EnrollmentResponse {
    client_name: String,
    expires_at: String,
    command: String,
    uri: String,
}
```

In `post_client_enrollments` and `post_client_reenrollment`, both contain this block:

```rust
        Json(EnrollmentResponse {
            client_name: enrollment.client_name.to_string(),
            expires_at: enrollment.expires_at.to_rfc3339(),
            command: enrollment.command,
        }),
```

Replace **both** occurrences with:

```rust
        Json(EnrollmentResponse {
            client_name: enrollment.client_name.to_string(),
            expires_at: enrollment.expires_at.to_rfc3339(),
            command: enrollment.command,
            uri: enrollment.uri,
        }),
```

- [ ] **Step 5: Run the contract test, verify it passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test http_client_enrollments_contract 2>&1 | tail -20`
Expected: PASS (6 passed).

- [ ] **Step 6: Add `uri` to the TS type**

In `webui/src/api/types.ts`, replace:

```ts
export interface ClientEnrollmentResponse {
  client_name: string;
  expires_at: string;
  command: string;
}
```

with:

```ts
export interface ClientEnrollmentResponse {
  client_name: string;
  expires_at: string;
  command: string;
  uri: string;
}
```

- [ ] **Step 7: Verify clippy is clean for the crate**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo clippy -p portunus-server --all-targets 2>&1 | tail -15`
Expected: no warnings (CI gates on `-D warnings`).

- [ ] **Step 8: Commit**

```bash
git add crates/portunus-server/src/operator/cli.rs crates/portunus-server/src/operator/http.rs crates/portunus-server/tests/http_client_enrollments_contract.rs webui/src/api/types.ts
git commit -m "feat(server): expose bare enrollment uri on operator HTTP response"
```

---

## Task 2: `scripts/install.sh` + dry-run smoke test

**Files:**
- Create: `scripts/install.sh`
- Test: `scripts/install.test.sh`

- [ ] **Step 1: Write the failing smoke test**

Create `scripts/install.test.sh`:

```sh
#!/bin/sh
# Network-free smoke test for scripts/install.sh --dry-run.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
script="$here/install.sh"

fail() { echo "FAIL: $1" >&2; exit 1; }

out="$(sh "$script" client --version 1.4.1 --dry-run)" || fail "exit non-zero"

echo "$out" | grep -q '^role:[[:space:]]*client$' || fail "role line"
echo "$out" | grep -q '^tag:[[:space:]]*v1.4.1$' || fail "tag line"
echo "$out" | grep -q '^artifact_version:[[:space:]]*1.4.1$' || fail "artifact_version line"
echo "$out" | grep -q 'releases/download/v1.4.1/portunus-1.4.1-.*\.tar\.gz' || fail "download_url"
echo "$out" | grep -q 'portunus-1.4.1-checksums\.txt' || fail "checksums_url"

# Accepts a leading-v version identically.
out2="$(sh "$script" server --version v2.0.0 --dry-run)" || fail "v-prefixed exit"
echo "$out2" | grep -q '^role:[[:space:]]*server$' || fail "server role"
echo "$out2" | grep -q '^tag:[[:space:]]*v2.0.0$' || fail "v-normalised tag"
echo "$out2" | grep -q '^artifact_version:[[:space:]]*2.0.0$' || fail "v-normalised artifact"

# Unknown role is rejected non-zero.
if sh "$script" bogus --dry-run >/dev/null 2>&1; then fail "bogus role accepted"; fi

echo "PASS"
```

Make it executable:

```bash
chmod +x scripts/install.test.sh
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `sh scripts/install.test.sh 2>&1 | tail -5`
Expected: FAIL (`install.sh` does not exist → `sh: ... install.sh: No such file or directory`, test prints `FAIL: exit non-zero`).

- [ ] **Step 3: Write `scripts/install.sh`**

Create `scripts/install.sh`:

```sh
#!/bin/sh
# Portunus installer. Downloads a release binary (and optionally installs
# the hardened systemd unit) for one role.
#
#   curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client
#   curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sudo sh -s -- server --systemd
#
# Flags: --version <X.Y.Z|vX.Y.Z>  --bin-dir DIR  --systemd  --yes  --dry-run
set -eu

REPO="ZingerLittleBee/Portunus"
RAW_BASE="https://raw.githubusercontent.com/${REPO}/main"
ROLE=""
VERSION=""
BIN_DIR="/usr/local/bin"
WANT_SYSTEMD="no"
ASSUME_YES="no"
DRY_RUN="no"

die() { echo "error: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    client|server) ROLE="$1" ;;
    --version) shift; [ $# -gt 0 ] || die "--version needs a value"; VERSION="$1" ;;
    --bin-dir) shift; [ $# -gt 0 ] || die "--bin-dir needs a value"; BIN_DIR="$1" ;;
    --systemd) WANT_SYSTEMD="yes" ;;
    --yes) ASSUME_YES="yes" ;;
    --dry-run) DRY_RUN="yes" ;;
    -h|--help) echo "usage: install.sh <client|server> [--version V] [--bin-dir DIR] [--systemd] [--yes] [--dry-run]"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
  shift
done

[ -n "$ROLE" ] || die "role required: client or server"

# Platform.
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

# Version: tag always has a leading v; artifact_version never does.
if [ -n "$VERSION" ]; then
  case "$VERSION" in
    v*) tag="$VERSION"; artifact_version="${VERSION#v}" ;;
    *)  tag="v$VERSION"; artifact_version="$VERSION" ;;
  esac
  resolved_version="$artifact_version"
else
  resolved_version="<latest, resolved at run time>"
  tag=""
  artifact_version=""
fi

rel() {
  # echo the release download URL for asset $1; requires resolved version.
  echo "https://github.com/${REPO}/releases/download/${tag}/$1"
}

print_plan() {
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
  echo "bin_dir:          ${BIN_DIR}"
  echo "systemd:          ${WANT_SYSTEMD}"
  echo "actions:          download+verify+install portunus-${ROLE} -> ${BIN_DIR}$( [ "$WANT_SYSTEMD" = yes ] && echo ' + install systemd unit' )"
}

# --dry-run short-circuits BEFORE any network call (incl. latest resolution).
if [ "$DRY_RUN" = "yes" ]; then
  print_plan
  exit 0
fi

need() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }
need curl
need tar
need uname

# Resolve latest if no explicit --version.
if [ -z "$tag" ]; then
  need sed
  tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
  [ -n "$tag" ] || die "could not resolve latest release tag"
  artifact_version="${tag#v}"
  resolved_version="$artifact_version"
fi

asset="portunus-${artifact_version}-${target}.tar.gz"
checksums="portunus-${artifact_version}-checksums.txt"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "→ downloading ${asset} (${tag})"
curl -fsSL "$(rel "$asset")" -o "$tmp/$asset" || die "download failed: $asset"
curl -fsSL "$(rel "$checksums")" -o "$tmp/$checksums" || die "download failed: $checksums"

echo "→ verifying sha256"
expected="$(grep " ${asset}\$" "$tmp/$checksums" | awk '{print $1}')"
[ -n "$expected" ] || die "no checksum entry for $asset"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $asset"

tar -xzf "$tmp/$asset" -C "$tmp"
src="$tmp/portunus-${artifact_version}-${target}/portunus-${ROLE}"
[ -f "$src" ] || die "binary not found in archive: portunus-${ROLE}"

maybe_sudo() {
  if [ -w "$1" ] || [ "$(id -u)" = "0" ]; then sudo_cmd=""; else sudo_cmd="sudo"; fi
}
maybe_sudo "$BIN_DIR"
echo "→ installing portunus-${ROLE} to ${BIN_DIR}"
${sudo_cmd:-} install -m 0755 "$src" "${BIN_DIR}/portunus-${ROLE}"

if [ "$WANT_SYSTEMD" = "yes" ]; then
  if [ "$os" != "linux" ] || ! command -v systemctl >/dev/null 2>&1; then
    echo "warning: --systemd ignored (not Linux or systemctl missing)" >&2
  else
    unit="portunus-${ROLE}.service"
    echo "→ fetching hardened unit ${unit}"
    curl -fsSL "${RAW_BASE}/deploy/systemd/${unit}" -o "$tmp/$unit" || die "unit download failed"
    if [ "$ROLE" = "client" ]; then
      id portunus-client >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin portunus-client
      sudo install -d -o root -g portunus-client -m 0750 /etc/portunus
    else
      id portunus-server >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin portunus-server
      sudo install -d -o portunus-server -g portunus-server -m 0750 /var/lib/portunus
    fi
    sudo install -m 0644 "$tmp/$unit" "/etc/systemd/system/$unit"
    sudo systemctl daemon-reload
    echo "→ installed /etc/systemd/system/$unit"
  fi
fi

echo
echo "Done. Next steps:"
if [ "$ROLE" = "client" ]; then
  echo "  1. Get an enrollment command from the operator (Web UI Clients page)."
  echo "  2. portunus-client enroll 'portunus://...' --out ./client.bundle.json"
  if [ "$WANT_SYSTEMD" = "yes" ]; then
    echo "  3. sudo install -o root -g portunus-client -m 0640 ./client.bundle.json /etc/portunus/client.bundle.json"
    echo "  4. sudo systemctl enable --now portunus-client"
  else
    echo "  3. portunus-client"
  fi
else
  if [ "$WANT_SYSTEMD" = "yes" ]; then
    echo "  sudo systemctl enable --now portunus-server"
  else
    echo "  portunus-server --data-dir /var/lib/portunus serve"
  fi
fi
```

Make it executable:

```bash
chmod +x scripts/install.sh
```

- [ ] **Step 4: Run the smoke test, verify it passes**

Run: `sh scripts/install.test.sh 2>&1 | tail -5`
Expected: `PASS`.

- [ ] **Step 5: Lint with shellcheck**

Run: `shellcheck -s sh scripts/install.sh scripts/install.test.sh 2>&1 | tail -20`
Expected: no output (clean). If `shellcheck` is not installed, run `command -v shellcheck || echo "shellcheck not installed — skipping"` and note it; do not fail the task on a missing linter, but fix any reported issues if it is present.

- [ ] **Step 6: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(scripts): add role-parameterised install.sh with dry-run smoke test"
```

---

## Task 3: De-stale `deploy/systemd/install.sh`

**Files:**
- Modify: `deploy/systemd/install.sh:61-65`

- [ ] **Step 1: Replace the `provision-client` block**

In `deploy/systemd/install.sh`, replace exactly:

```sh
  if [[ ! -f /etc/portunus/client.bundle.json ]]; then
    echo "→ /etc/portunus/client.bundle.json not present yet. Provision one on the server:"
    echo "    portunus-server --data-dir /var/lib/portunus provision-client <name> --out client.bundle.json"
    echo "  scp it here, then: install -o root -g portunus-client -m 0640 client.bundle.json /etc/portunus/"
  fi
```

with:

```sh
  if [[ ! -f /etc/portunus/client.bundle.json ]]; then
    echo "→ /etc/portunus/client.bundle.json not present yet. Enroll this host:"
    echo "    1. Operator creates an enrollment command (Web UI Clients page,"
    echo "       or: portunus-server --data-dir /var/lib/portunus enroll-client <name>)."
    echo "    2. On this host, redeem it to a local file:"
    echo "       portunus-client enroll 'portunus://...' --out ./client.bundle.json"
    echo "    3. install -o root -g portunus-client -m 0640 ./client.bundle.json /etc/portunus/client.bundle.json"
  fi
```

- [ ] **Step 2: Sanity-check the script still parses**

Run: `bash -n deploy/systemd/install.sh && echo OK`
Expected: `OK`.

- [ ] **Step 3: Commit**

```bash
git add deploy/systemd/install.sh
git commit -m "fix(deploy): point systemd install.sh client hint at the enroll flow"
```

---

## Task 4: i18n keys for the install guide

**Files:**
- Modify: `webui/src/i18n/en.json:308-339`
- Modify: `webui/src/i18n/zh-CN.json:308-339`

- [ ] **Step 1: Replace the `clientProvision` block in `en.json`**

In `webui/src/i18n/en.json`, replace the entire existing `"clientProvision": { ... },` block (the one ending `"backToList": "Back to clients"`) with:

```json
  "clientProvision": {
    "title": "Enroll client",
    "submit": "Create enrollment",
    "address": "Client entry address",
    "addressHint": "Public IP address or DNS name users connect to for this client's forwarding rules. Do not include a URL scheme or port.",
    "backToList": "Back to clients",
    "guide": {
      "heading": "Install {{name}}",
      "expiresIn": "Expires in {{remaining}}",
      "expired": "This command has expired — create a new one.",
      "tabShell": "Shell",
      "tabSystemd": "systemd",
      "tabDocker": "Docker",
      "copy": "Copy",
      "copied": "Copied",
      "skipNote": "Already installed on this host? Skip to step {{step}}.",
      "stepInstall": "Install the binary",
      "stepEnroll": "Redeem the one-time enrollment command",
      "stepRun": "Run the forwarder",
      "stepEnrollSystemd": "Redeem the command, then place the bundle",
      "stepEnableSystemd": "Enable and start the service",
      "stepEnrollDocker": "Enroll once into a host volume",
      "stepRunDocker": "Run the long-lived container",
      "dockerNote": "install.sh is host-only; in Docker both commands run as your host user so the 0600 bundle is writable and readable by one identity."
    }
  },
```

- [ ] **Step 2: Trim the unused `clientDetail` re-enroll hint in `en.json`**

In `webui/src/i18n/en.json`, in the `clientDetail` block, remove the line:

```json
    "reenrollHint": "Run this once on {{name}}. The existing token is replaced only after this command is redeemed."
```

and ensure the preceding line (`"reenrollConfirmAction": "Create command",`) is now the last entry before the closing `}` (i.e. it must NOT have a trailing comma). The block tail should read:

```json
    "reenrollConfirmBody": "The current token keeps working until the client redeems the new one-time command. Redemption rotates the token and disconnects the old live session.",
    "reenrollConfirmAction": "Create command"
  },
```

- [ ] **Step 3: Replace the `clientProvision` block in `zh-CN.json`**

In `webui/src/i18n/zh-CN.json`, replace the entire existing `"clientProvision": { ... },` block with:

```json
  "clientProvision": {
    "title": "接入客户端",
    "submit": "创建接入命令",
    "address": "客户端入口地址",
    "addressHint": "用户访问该客户端转发规则时连接的公网 IP 或域名。不要填写 URL 协议或端口。",
    "backToList": "返回客户端列表",
    "guide": {
      "heading": "安装 {{name}}",
      "expiresIn": "{{remaining}} 后过期",
      "expired": "该命令已过期 —— 请重新创建。",
      "tabShell": "Shell",
      "tabSystemd": "systemd",
      "tabDocker": "Docker",
      "copy": "复制",
      "copied": "已复制",
      "skipNote": "该主机已安装？直接跳到第 {{step}} 步。",
      "stepInstall": "安装二进制",
      "stepEnroll": "兑换一次性接入命令",
      "stepRun": "运行转发器",
      "stepEnrollSystemd": "兑换命令，并放置 bundle",
      "stepEnableSystemd": "启用并启动服务",
      "stepEnrollDocker": "一次性接入到宿主卷",
      "stepRunDocker": "运行长驻容器",
      "dockerNote": "install.sh 仅用于宿主机；Docker 下两条命令都以你的宿主用户身份运行，使 0600 的 bundle 由同一身份写入与读取。"
    }
  },
```

- [ ] **Step 4: Trim the unused `clientDetail` re-enroll hint in `zh-CN.json`**

In `webui/src/i18n/zh-CN.json`, in the `clientDetail` block, remove the line:

```json
    "reenrollHint": "在 {{name}} 上执行一次。只有这条命令兑换成功后,现有 token 才会被替换。"
```

and drop the trailing comma on the now-last entry so the tail reads:

```json
    "reenrollConfirmBody": "当前 token 会继续可用,直到客户端兑换新的一次性命令。兑换成功后才会轮换 token 并断开旧的在线会话。",
    "reenrollConfirmAction": "创建命令"
  },
```

- [ ] **Step 5: Verify both JSON files parse**

Run: `node -e "JSON.parse(require('fs').readFileSync('webui/src/i18n/en.json'));JSON.parse(require('fs').readFileSync('webui/src/i18n/zh-CN.json'));console.log('OK')"`
Expected: `OK`.

- [ ] **Step 6: Commit**

```bash
git add webui/src/i18n/en.json webui/src/i18n/zh-CN.json
git commit -m "feat(webui): add install-guide i18n keys, drop unused enrollment keys"
```

---

## Task 5: `EnrollmentInstallGuide` component

**Files:**
- Create: `webui/src/components/EnrollmentInstallGuide.tsx`
- Test: `webui/tests/unit/enrollment-install-guide-render.test.tsx`

- [ ] **Step 1: Write the failing render test**

Create `webui/tests/unit/enrollment-install-guide-render.test.tsx`:

```tsx
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "@/i18n";
import { EnrollmentInstallGuide } from "@/components/EnrollmentInstallGuide";
import type { ClientEnrollmentResponse } from "@/api/types";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

beforeEach(() => {
  Object.assign(navigator, {
    clipboard: { writeText: vi.fn().mockResolvedValue(undefined) },
  });
});

function mk(overrides: Partial<ClientEnrollmentResponse> = {}): ClientEnrollmentResponse {
  return {
    client_name: "edge-01",
    expires_at: new Date(Date.now() + 600_000).toISOString(),
    command: "portunus-client enroll 'portunus://host:7443/enroll?code=abc'",
    uri: "portunus://host:7443/enroll?code=abc",
    ...overrides,
  };
}

describe("EnrollmentInstallGuide", () => {
  it("renders the three platform tabs and the shell command verbatim", () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="provision" />);
    expect(screen.getByRole("tab", { name: "Shell" })).toBeDefined();
    expect(screen.getByRole("tab", { name: "systemd" })).toBeDefined();
    expect(screen.getByRole("tab", { name: "Docker" })).toBeDefined();
    expect(
      screen.getByText("portunus-client enroll 'portunus://host:7443/enroll?code=abc'"),
    ).toBeDefined();
  });

  it("uses the bare uri (not the wrapped command) in the Docker tab", () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="provision" />);
    fireEvent.click(screen.getByRole("tab", { name: "Docker" }));
    const docker = screen.getByTestId("guide-step-docker-enroll").textContent ?? "";
    expect(docker).toContain("enroll 'portunus://host:7443/enroll?code=abc'");
    expect(docker).toContain('--user "$(id -u):$(id -g)"');
    expect(docker).not.toContain("portunus-client enroll 'portunus-client");
  });

  it("shows a live countdown that reaches the expired state", () => {
    vi.useFakeTimers();
    const past = new Date(Date.now() - 1_000).toISOString();
    render(<EnrollmentInstallGuide enrollment={mk({ expires_at: past })} mode="provision" />);
    vi.advanceTimersByTime(1_100);
    expect(screen.getByText(/expired/i)).toBeDefined();
  });

  it("collapses the install step in reenroll mode", () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="reenroll" />);
    expect(screen.getByText(/Already installed on this host/i)).toBeDefined();
  });

  it("copies a step command to the clipboard", () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="provision" />);
    const [firstCopy] = screen.getAllByRole("button", { name: /copy/i });
    fireEvent.click(firstCopy);
    expect(navigator.clipboard.writeText).toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cd webui && npx vitest run tests/unit/enrollment-install-guide-render.test.tsx 2>&1 | tail -15; cd ..`
Expected: FAIL — `Cannot find module '@/components/EnrollmentInstallGuide'`.

- [ ] **Step 3: Implement the component**

Create `webui/src/components/EnrollmentInstallGuide.tsx`:

```tsx
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Clock, Copy, Terminal } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import type { ClientEnrollmentResponse } from "@/api/types";

const INSTALL_URL =
  "https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh";
const IMAGE = "ghcr.io/zingerlittlebee/portunus-client";

type Mode = "provision" | "reenroll";

interface Step {
  key: string;
  title: string;
  command: string;
}

function useCountdown(expiresAt: string) {
  const target = useMemo(() => new Date(expiresAt).getTime(), [expiresAt]);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1_000);
    return () => clearInterval(id);
  }, []);
  const ms = target - now;
  const expired = ms <= 0;
  const total = Math.max(0, Math.floor(ms / 1_000));
  const mm = String(Math.floor(total / 60)).padStart(2, "0");
  const ss = String(total % 60).padStart(2, "0");
  return { expired, remaining: `${mm}:${ss}` };
}

function CommandBlock({ testId, command }: { testId: string; command: string }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      setTimeout(() => setCopied(false), 2_000);
    } catch {
      /* ignore */
    }
  }
  return (
    <div className="space-y-2">
      <div className="flex items-center justify-end">
        <Button variant="outline" size="sm" onClick={copy}>
          {copied ? <Check className="mr-1 h-4 w-4" /> : <Copy className="mr-1 h-4 w-4" />}
          {copied ? t("clientProvision.guide.copied") : t("clientProvision.guide.copy")}
        </Button>
      </div>
      <ScrollArea className="rounded-md bg-muted">
        <pre data-testid={testId} className="p-3 text-xs leading-relaxed">
          {command}
        </pre>
      </ScrollArea>
    </div>
  );
}

function StepList({
  steps,
  startIndex,
}: {
  steps: Step[];
  startIndex: number;
}) {
  return (
    <ol className="space-y-4">
      {steps.map((s, i) => (
        <li key={s.key} className="space-y-2">
          <Label>
            {startIndex + i}. {s.title}
          </Label>
          <CommandBlock testId={`guide-step-${s.key}`} command={s.command} />
        </li>
      ))}
    </ol>
  );
}

export function EnrollmentInstallGuide({
  enrollment,
  mode,
}: {
  enrollment: ClientEnrollmentResponse;
  mode: Mode;
}) {
  const { t } = useTranslation();
  const { expired, remaining } = useCountdown(enrollment.expires_at);
  const reenroll = mode === "reenroll";

  const installStep: Step = {
    key: "shell-install",
    title: t("clientProvision.guide.stepInstall"),
    command: `curl -fsSL ${INSTALL_URL} | sh -s -- client`,
  };
  const shellSteps: Step[] = [
    installStep,
    {
      key: "shell-enroll",
      title: t("clientProvision.guide.stepEnroll"),
      command: enrollment.command,
    },
    {
      key: "shell-run",
      title: t("clientProvision.guide.stepRun"),
      command: "portunus-client",
    },
  ];
  const systemdSteps: Step[] = [
    {
      key: "systemd-install",
      title: t("clientProvision.guide.stepInstall"),
      command: `curl -fsSL ${INSTALL_URL} | sudo sh -s -- client --systemd`,
    },
    {
      key: "systemd-enroll",
      title: t("clientProvision.guide.stepEnrollSystemd"),
      command: `${enrollment.command} --out ./client.bundle.json
sudo install -o root -g portunus-client -m 0640 ./client.bundle.json /etc/portunus/client.bundle.json`,
    },
    {
      key: "systemd-enable",
      title: t("clientProvision.guide.stepEnableSystemd"),
      command: "sudo systemctl enable --now portunus-client",
    },
  ];
  const dockerSteps: Step[] = [
    {
      key: "docker-enroll",
      title: t("clientProvision.guide.stepEnrollDocker"),
      command: `docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/work" ${IMAGE} enroll '${enrollment.uri}' --out /work/client.bundle.json`,
    },
    {
      key: "docker-run",
      title: t("clientProvision.guide.stepRunDocker"),
      command: `docker run -d --name portunus-client --network host --user "$(id -u):$(id -g)" -v "$PWD/client.bundle.json:/etc/portunus/client.bundle.json:ro" ${IMAGE}`,
    },
  ];

  const visibleShell = reenroll ? shellSteps.slice(1) : shellSteps;
  const shellStart = reenroll ? 2 : 1;

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center justify-between gap-2">
          <span className="flex items-center gap-2">
            <Terminal className="h-5 w-5" />
            {t("clientProvision.guide.heading", { name: enrollment.client_name })}
          </span>
          <span
            className={`flex items-center gap-1 text-sm ${expired ? "text-destructive" : "text-muted-foreground"}`}
          >
            <Clock className="h-4 w-4" />
            {expired
              ? t("clientProvision.guide.expired")
              : t("clientProvision.guide.expiresIn", { remaining })}
          </span>
        </CardTitle>
      </CardHeader>
      <CardContent>
        {reenroll && (
          <p className="mb-4 text-xs text-muted-foreground">
            {t("clientProvision.guide.skipNote", { step: 2 })}
          </p>
        )}
        <Tabs defaultValue="shell">
          <TabsList>
            <TabsTrigger value="shell">{t("clientProvision.guide.tabShell")}</TabsTrigger>
            <TabsTrigger value="systemd">{t("clientProvision.guide.tabSystemd")}</TabsTrigger>
            <TabsTrigger value="docker">{t("clientProvision.guide.tabDocker")}</TabsTrigger>
          </TabsList>
          <TabsContent value="shell" className="pt-4">
            <StepList steps={visibleShell} startIndex={shellStart} />
          </TabsContent>
          <TabsContent value="systemd" className="pt-4">
            <StepList steps={systemdSteps} startIndex={1} />
          </TabsContent>
          <TabsContent value="docker" className="space-y-3 pt-4">
            <p className="text-xs text-muted-foreground">
              {t("clientProvision.guide.dockerNote")}
            </p>
            <StepList steps={dockerSteps} startIndex={1} />
          </TabsContent>
        </Tabs>
      </CardContent>
    </Card>
  );
}
```

- [ ] **Step 4: Run the render test, verify it passes**

Run: `cd webui && npx vitest run tests/unit/enrollment-install-guide-render.test.tsx 2>&1 | tail -15; cd ..`
Expected: PASS (5 passed). If the `expired` test is flaky under fake timers, it is acceptable for the countdown effect to also compute `expired` from the initial render (it does: `useCountdown` derives `expired` from `target - now` on first render, so a past `expires_at` is expired immediately without waiting for the interval).

- [ ] **Step 5: Commit**

```bash
git add webui/src/components/EnrollmentInstallGuide.tsx webui/tests/unit/enrollment-install-guide-render.test.tsx
git commit -m "feat(webui): add EnrollmentInstallGuide stepped wizard component"
```

---

## Task 6: Wire the guide into ClientProvision and ClientDetail

**Files:**
- Modify: `webui/src/pages/ClientProvision.tsx`
- Modify: `webui/src/pages/ClientDetail.tsx`
- Modify: `webui/tests/unit/client-provision-enrollment.test.ts`
- Modify: `webui/tests/unit/client-detail-reenrollment.test.ts`

- [ ] **Step 1: Update the two grep tests (failing first)**

Replace the body of `webui/tests/unit/client-provision-enrollment.test.ts` with:

```ts
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

describe("ClientProvision enrollment flow", () => {
  it("creates an enrollment command and renders the install guide", () => {
    const source = readFileSync(resolve(__dirname, "../../src/pages/ClientProvision.tsx"), "utf8");

    expect(source).toContain("useCreateClientEnrollment");
    expect(source).not.toContain("useProvisionClient");
    expect(source).toContain("EnrollmentInstallGuide");
    expect(source).not.toContain("EnrollmentCommandCard");
  });
});
```

Replace the body of `webui/tests/unit/client-detail-reenrollment.test.ts` with:

```ts
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

describe("ClientDetail re-enrollment flow", () => {
  it("generates an enrollment command and renders the install guide", () => {
    const source = readFileSync(resolve(__dirname, "../../src/pages/ClientDetail.tsx"), "utf8");

    expect(source).toContain("useCreateClientReEnrollment");
    expect(source).not.toContain("useReissueClient");
    expect(source).not.toContain("CredentialBundleCard");
    expect(source).not.toContain("ClientInstallSteps");
    expect(source).toContain("EnrollmentInstallGuide");
    expect(source).not.toContain("ReEnrollmentCommandCard");
  });
});
```

- [ ] **Step 2: Run both grep tests, verify they fail**

Run: `cd webui && npx vitest run tests/unit/client-provision-enrollment.test.ts tests/unit/client-detail-reenrollment.test.ts 2>&1 | tail -15; cd ..`
Expected: FAIL — source still contains `EnrollmentCommandCard` / `ReEnrollmentCommandCard` and lacks `EnrollmentInstallGuide`.

- [ ] **Step 3: Rewrite `ClientProvision.tsx`**

Replace the entire file `webui/src/pages/ClientProvision.tsx` with:

```tsx
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";

import { useCreateClientEnrollment } from "@/api/clients";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { EnrollmentInstallGuide } from "@/components/EnrollmentInstallGuide";
import type { ClientEnrollmentResponse } from "@/api/types";

export function ClientProvision() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const enrollmentMutation = useCreateClientEnrollment();
  const [name, setName] = useState("");
  const [address, setAddress] = useState("");
  const [enrollment, setEnrollment] = useState<ClientEnrollmentResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    try {
      const res = await enrollmentMutation.mutateAsync({ name, address });
      setEnrollment(res);
    } catch (err) {
      if (err instanceof ApiError) {
        setError(`${err.code}: ${err.message}`);
      } else if (err instanceof Error) {
        setError(err.message);
      } else {
        setError(String(err));
      }
    }
  }

  return (
    <div className="max-w-3xl space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>{t("clientProvision.title")}</CardTitle>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSubmit} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="name">{t("clients.name")}</Label>
              <Input
                id="name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="client-a"
                required
                disabled={!!enrollment}
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="address">{t("clientProvision.address")}</Label>
              <Input
                id="address"
                value={address}
                onChange={(e) => setAddress(e.target.value)}
                placeholder="68.77.201.69 or edge.example.com"
                required
                disabled={!!enrollment}
              />
              <p className="text-xs text-muted-foreground">
                {t("clientProvision.addressHint")}
              </p>
            </div>
            {error && <p className="text-sm text-destructive">{error}</p>}
            {!enrollment && (
              <div className="flex gap-2">
                <Button type="submit" disabled={enrollmentMutation.isPending}>
                  {enrollmentMutation.isPending ? t("confirm.busy") : t("clientProvision.submit")}
                </Button>
                <Button type="button" variant="outline" onClick={() => navigate(-1)}>
                  {t("confirm.cancel")}
                </Button>
              </div>
            )}
          </form>
        </CardContent>
      </Card>

      {enrollment && (
        <>
          <EnrollmentInstallGuide enrollment={enrollment} mode="provision" />
          <Button variant="link" onClick={() => navigate("/clients")}>
            {t("clientProvision.backToList")}
          </Button>
        </>
      )}
    </div>
  );
}
```

- [ ] **Step 4: Rewrite the enrollment card usage in `ClientDetail.tsx`**

In `webui/src/pages/ClientDetail.tsx`:

(a) Replace the import line:

```tsx
import { ArrowLeft, Check, Clock, Copy, RefreshCw, Terminal } from "lucide-react";
```

with:

```tsx
import { ArrowLeft, RefreshCw } from "lucide-react";
```

(b) Add this import alongside the other `@/components` imports (e.g. directly after the `ConfirmDialog` import line):

```tsx
import { EnrollmentInstallGuide } from "@/components/EnrollmentInstallGuide";
```

(c) Remove the now-unused imports `Label` and `ScrollArea` (the lines `import { Label } from "@/components/ui/label";` and `import { ScrollArea } from "@/components/ui/scroll-area";`) — `ReEnrollmentCommandCard` was their only consumer.

(d) Replace the render block:

```tsx
          {reenrollment && (
            <ReEnrollmentCommandCard reenrollment={reenrollment} />
          )}
```

with:

```tsx
          {reenrollment && (
            <EnrollmentInstallGuide enrollment={reenrollment} mode="reenroll" />
          )}
```

(e) Delete the entire `ReEnrollmentCommandCard` function definition (from `function ReEnrollmentCommandCard({` through its closing `}` before `function Row(`).

- [ ] **Step 5: Run the grep tests, verify they pass**

Run: `cd webui && npx vitest run tests/unit/client-provision-enrollment.test.ts tests/unit/client-detail-reenrollment.test.ts 2>&1 | tail -15; cd ..`
Expected: PASS (2 files, 2 passed).

- [ ] **Step 6: Typecheck and run the full unit suite**

Run: `cd webui && npx tsc -b 2>&1 | tail -15 && npx vitest run 2>&1 | tail -20; cd ..`
Expected: `tsc` clean (no errors); vitest all green. Fix any unused-import/type errors surfaced by `tsc` (e.g. a leftover `Terminal`/`Clock` import) before continuing.

- [ ] **Step 7: Commit**

```bash
git add webui/src/pages/ClientProvision.tsx webui/src/pages/ClientDetail.tsx webui/tests/unit/client-provision-enrollment.test.ts webui/tests/unit/client-detail-reenrollment.test.ts
git commit -m "feat(webui): use EnrollmentInstallGuide in provision and detail pages"
```

---

## Task 7: Docs rewrite (EN) — single source of truth

**Files:**
- Modify: `docs/content/docs/configuration/client.mdx`
- Modify: `docs/content/docs/deployment/systemd.mdx`
- Modify: `docs/content/docs/deployment/docker.mdx`
- Modify: `docs/content/docs/deployment/railway.mdx`
- Modify: `docs/content/docs/cli/walkthrough.mdx`
- Modify: `README.md`

- [ ] **Step 1: Make `configuration/client.mdx` authoritative**

In `docs/content/docs/configuration/client.mdx`, replace everything from the first body line (`` `portunus-client` is configured by redeeming a short-lived enrollment URI. ``) through the end of the bundle-resolution `sh` block (the block ending with `portunus-client`, originally around line 66) with:

````mdx
`portunus-client` is configured by redeeming a short-lived **enrollment
command**. There is no `client.toml`. An operator issues a one-time
command (Web UI **Clients** page, or `portunus-server enroll-client`),
the edge host redeems it, and the client writes a local bundle that
holds the bearer token.

This page is the canonical reference for installing and enrolling a
client. Deployment guides (systemd, Docker, Railway) link here.

## Install the binary

```sh
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client
```

`install.sh <client|server>` detects your OS/arch, downloads the
matching release binary, verifies its SHA-256 against the published
checksums, and installs it to `/usr/local/bin` (`--bin-dir` to change;
it uses `sudo` only for the privileged steps).

| Flag | Effect |
| --- | --- |
| `--version <X.Y.Z\|vX.Y.Z>` | Pin a release (default: latest). Either form works. |
| `--bin-dir DIR` | Install location (default `/usr/local/bin`). |
| `--systemd` | Also install + enable the hardened unit (Linux only; see systemd guide). |
| `--dry-run` | Print the resolved target/URL and planned actions; do nothing. |

For an already-root context: `curl -fsSL … | sudo sh -s -- client --systemd`.

## Enroll

```sh
# Operator side (or use the Web UI Clients page):
portunus-server enroll-client edge-01 --ttl-secs 600

# Edge host: paste the printed command
portunus-client enroll 'portunus://host-A.example.com:7443/enroll?...'
```

`portunus-client enroll` verifies the pinned certificate and writes the
bundle with mode `0600` to the default location unless `--out` is
supplied.

## The bundle

```json
{
  "version": 1,
  "client_name": "edge-01",
  "server_endpoint": "host-A.example.com:7443",
  "server_cert_sha256": "f5e7c2a1...",
  "token": "<bearer>"
}
```

<Callout type="warn">
  Edit `server_endpoint` if the address the edge host reaches the
  server on differs from what the server advertised by default.
</Callout>

## Run the client

### Explicit `--bundle`

```sh
portunus-client --bundle ./edge-01.bundle.json
```

### Bundle resolution (since v0.8)

When `--bundle` is omitted, the client searches in this order:

1. `$PORTUNUS_CLIENT_BUNDLE`
2. `$XDG_CONFIG_HOME/portunus/client.bundle.json`
3. `$HOME/.config/portunus/client.bundle.json`
4. `./client.bundle.json`

If none resolve the client exits `1` listing every attempted path.

```sh
mkdir -p ~/.config/portunus
mv edge-01.bundle.json ~/.config/portunus/client.bundle.json
portunus-client
```

## systemd

`install.sh client --systemd` installs the hardened
`portunus-client.service` (runs as the `portunus-client` user,
`--bundle /etc/portunus/client.bundle.json`). Provision the bundle:

```sh
portunus-client enroll 'portunus://...' --out ./client.bundle.json
sudo install -o root -g portunus-client -m 0640 \
  ./client.bundle.json /etc/portunus/client.bundle.json
sudo systemctl enable --now portunus-client
```

See the [systemd deployment guide](/en/docs/deployment/systemd).

## Docker

The client image entrypoint is already `portunus-client`. Run both the
one-shot enroll and the long-lived container as your host user so the
`0600` bundle is written and read by one identity:

```sh
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/work" \
  ghcr.io/zingerlittlebee/portunus-client \
  enroll 'portunus://...' --out /work/client.bundle.json

docker run -d --name portunus-client --network host \
  --user "$(id -u):$(id -g)" \
  -v "$PWD/client.bundle.json:/etc/portunus/client.bundle.json:ro" \
  ghcr.io/zingerlittlebee/portunus-client
```

See the [Docker deployment guide](/en/docs/deployment/docker).
````

Leave the existing `## Resource limits` section (and anything after it) unchanged.

- [ ] **Step 2: Point `deployment/systemd.mdx` at the new flow**

In `docs/content/docs/deployment/systemd.mdx`, replace the `## One-shot install` block (the fenced `sh` block `cd deploy/systemd` / `sudo ./install.sh` and the bullet list through `Installs portunus-server.service and portunus-client.service.`) with:

````mdx
## One-shot install

On the edge host, install the client binary and the hardened unit in one
step:

```sh
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh \
  | sudo sh -s -- client --systemd
```

This downloads + checksum-verifies the release binary, creates the
`portunus-client` service user, lays out `/etc/portunus` (mode 0750),
and installs the hardened `portunus-client.service`. For the server,
use `… | sudo sh -s -- server --systemd` (creates `portunus-server`,
`/var/lib/portunus`). Binary acquisition and flags are documented once
in [Client Configuration](/en/docs/configuration/client#install-the-binary).

From a source checkout you can instead install only the units with
`cd deploy/systemd && sudo ./install.sh client` (the binary must already
be on `PATH`).
````

Then replace the `## First-boot procedure` `sh` block's client tail. Specifically replace these lines inside that block:

```mdx
# 5. Create the first client from the Web UI Clients page, or print a
#    one-time enrollment command with the CLI.
portunus-server enroll-client edge-01

# 6. Run the printed `portunus-client enroll 'portunus://...'` command
#    on the edge host. It writes /etc/portunus/client.bundle.json.

# 7. Enable the client unit on the edge host
sudo systemctl enable --now portunus-client
```

with:

```mdx
# 5. Create the first client (Web UI Clients page, or the CLI):
portunus-server enroll-client edge-01

# 6. On the edge host: install + enroll, then place the bundle
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh \
  | sudo sh -s -- client --systemd
portunus-client enroll 'portunus://...' --out ./client.bundle.json
sudo install -o root -g portunus-client -m 0640 \
  ./client.bundle.json /etc/portunus/client.bundle.json

# 7. Enable the client unit on the edge host
sudo systemctl enable --now portunus-client
```

Leave the `## client.service` ini block unchanged (it documents the unit shape).

- [ ] **Step 3: Update `deployment/docker.mdx`**

In `docs/content/docs/deployment/docker.mdx`, replace the `## Client compose` section (from the `## Client compose` heading through the end of the "Host networking is the least surprising mode…" paragraph) with:

````mdx
## Client

The client image's entrypoint is already `portunus-client`, so pass
`enroll …` as arguments to override the default `CMD`. Run **both** the
one-shot enroll and the long-lived container as your host user — the
image defaults to `nonroot` (UID 65532), which cannot write a
host-owned bind mount nor read a host-UID `0600` bundle:

```sh
# One-shot: redeem the enrollment command into a host file.
docker run --rm --user "$(id -u):$(id -g)" -v "$PWD:/work" \
  ghcr.io/zingerlittlebee/portunus-client \
  enroll 'portunus://...' --out /work/client.bundle.json

# Long-lived forwarder.
docker run -d --name portunus-client --network host \
  --user "$(id -u):$(id -g)" \
  -v "$PWD/client.bundle.json:/etc/portunus/client.bundle.json:ro" \
  ghcr.io/zingerlittlebee/portunus-client
```

Compose equivalent (`compose.client.yaml`), after the bundle exists:

```yaml
services:
  client:
    image: ghcr.io/zingerlittlebee/portunus-client:latest
    container_name: portunus-client
    network_mode: host
    user: "${HOST_UID}:${HOST_GID}"
    volumes:
      - ./client.bundle.json:/etc/portunus/client.bundle.json:ro
    restart: unless-stopped
```

Start it with `HOST_UID=$(id -u) HOST_GID=$(id -g) docker compose -f
compose.client.yaml up -d`. Host networking is the least surprising mode
for `portunus-client`: pushed rules bind listeners on the edge host and
Docker cannot know those ports before the operator creates the rules.
`install.sh` is host-only — containers use the image directly. Binary
and enroll details: [Client Configuration](/en/docs/configuration/client).
````

Leave the operator-container `enroll-client` block earlier in the file unchanged.

- [ ] **Step 4: Update `deployment/railway.mdx`**

In `docs/content/docs/deployment/railway.mdx`, replace the `## Client endpoint` section through the end of the `## Notes` list with:

````mdx
## Client endpoint

After creating a TCP Proxy for application port `7443`, set
`PORTUNUS_ADVERTISED_ENDPOINT` to the proxy `host:port` and redeploy.
Then enroll clients from the Web UI **Clients** page; the enrollment
command makes `portunus-client` dial the TCP Proxy endpoint. Install and
run the edge client with Docker (see
[Client Configuration](/en/docs/configuration/client#docker)) or the
host installer
([`install.sh`](/en/docs/configuration/client#install-the-binary)).

## Notes

- Railway's HTTP domain should be used only for the Web UI and `/v1/*`
  operator API.
- `portunus-client` must dial the TCP Proxy endpoint, not the HTTP
  domain.
````

- [ ] **Step 5: Update `cli/walkthrough.mdx`**

In `docs/content/docs/cli/walkthrough.mdx`, replace the `## 3. Provision a forwarding client` and `## 4. Start the client (Host B)` sections (from the `## 3.` heading through the closing fence of the `portunus-client --bundle ./edge-01.bundle.json` block) with:

````mdx
## 3. Enroll a forwarding client

```sh
portunus-server --data-dir ./srv enroll-client edge-01
```

On Host B, install the client then redeem the printed command:

```sh
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client
portunus-client enroll 'portunus://...' --out ./edge-01.bundle.json
```

It verifies the pinned certificate and writes the bundle with mode 0600.

## 4. Start the client (Host B)

```sh
portunus-client --bundle ./edge-01.bundle.json
```
````

- [ ] **Step 6: Update `README.md`**

In `README.md`, in the `## Install` section, replace the opening paragraph + the two `docker pull` lines:

```md
Docker Compose is the recommended install path. Published images default to
the newest stable release via `:latest`:

```sh
docker pull ghcr.io/zingerlittlebee/portunus-server:latest
docker pull ghcr.io/zingerlittlebee/portunus-client:latest
```
```

with:

````md
The fastest install is the one-line script (detects OS/arch, verifies
the release checksum):

```sh
# Edge host
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client
# Control plane host
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- server
```

Docker Compose is also supported; published images default to `:latest`:

```sh
docker pull ghcr.io/zingerlittlebee/portunus-server:latest
docker pull ghcr.io/zingerlittlebee/portunus-client:latest
```
````

In the `## Basic flow` `sh` block, replace the two lines:

```md
# Host B — redeem the enrollment URI, then start from the written bundle
./target/release/portunus-client enroll 'portunus://host:7443/enroll?...'
./target/release/portunus-client
```

with:

```md
# Host B — redeem the enrollment URI (writes the bundle), then start
./target/release/portunus-client enroll 'portunus://host:7443/enroll?...' --out ./client.bundle.json
./target/release/portunus-client --bundle ./client.bundle.json
```

- [ ] **Step 7: Build the docs site to verify MDX is valid**

Run: `cd docs && (pnpm build 2>&1 | tail -25); cd ..`
Expected: build succeeds (no MDX parse/compile error for the edited pages). If `docs/` has no build script or deps aren't installed, instead run a syntax sanity check: `cd docs && npx --yes @mdx-js/mdx@latest --version >/dev/null 2>&1; cd ..` and visually confirm each edited file's fences are balanced. Record which check was used.

- [ ] **Step 8: Commit**

```bash
git add docs/content/docs/configuration/client.mdx docs/content/docs/deployment/systemd.mdx docs/content/docs/deployment/docker.mdx docs/content/docs/deployment/railway.mdx docs/content/docs/cli/walkthrough.mdx README.md
git commit -m "docs: single-source-of-truth enrollment install (EN)"
```

---

## Task 8: Docs ZH mirror

**Files:**
- Modify: `docs/content/docs/zh/configuration/client.mdx`
- Modify: `docs/content/docs/zh/deployment/systemd.mdx`
- Modify: `docs/content/docs/zh/deployment/docker.mdx`
- Modify: `docs/content/docs/zh/deployment/railway.mdx`
- Modify: `docs/content/docs/zh/cli/walkthrough.mdx`

- [ ] **Step 1: Diff each EN page against its ZH mirror to locate the mirrored blocks**

Run for each page:

```bash
for p in configuration/client deployment/systemd deployment/docker deployment/railway cli/walkthrough; do
  echo "=== $p ==="; diff <(sed -n '1,40p' "docs/content/docs/$p.mdx") <(sed -n '1,40p' "docs/content/docs/zh/$p.mdx") || true
done
```

Expected: the ZH files have the same structure with translated prose; identify the same sections changed in Task 7.

- [ ] **Step 2: Apply the same structural edits to each ZH page**

For each ZH page, make the **same** sections changes as Task 7, keeping every fenced code block **byte-identical** to the EN version (commands, URLs, YAML, ini are not translated) and translating only the surrounding prose. Use these Chinese renderings for the headings/prose introduced in Task 7:

- "Install the binary" → "安装二进制"
- "Enroll" → "接入(enroll)"
- "The bundle" → "bundle 文件"
- "Run the client" → "运行客户端"
- "systemd" → "systemd"
- "Docker" → "Docker"
- Authoritative-page sentence → "本页是安装与接入客户端的权威参考，部署指南(systemd、Docker、Railway)均链接至此。"
- install.sh description → "`install.sh <client|server>` 会检测系统架构,下载对应发行版二进制,校验 SHA-256,并安装到 `/usr/local/bin`(用 `--bin-dir` 修改;仅特权步骤使用 `sudo`)。"
- Docker identity sentence → "客户端镜像入口点已是 `portunus-client`;一次性 enroll 与长驻容器都要以你的宿主用户身份运行,使 0600 的 bundle 由同一身份写入与读取(镜像默认 nonroot/UID 65532,无法写入宿主属主的挂载,也无法读取宿主 UID 的 0600 文件)。"
- systemd one-shot intro → "在边缘主机上,一步安装客户端二进制与加固单元:"
- Cross-links: keep the same paths but use the `/zh/` locale prefix (e.g. `/zh/docs/configuration/client#install-the-binary`).

Concretely, for `docs/content/docs/zh/configuration/client.mdx` replace the body from the first prose line through the end of the bundle-resolution `sh` block with the Task 7 Step 1 content, Chinese prose + identical code blocks + `/zh/` links. Repeat the analogous replacement for the other four ZH pages mirroring Task 7 Steps 2–5. (README has no ZH mirror — skip.)

- [ ] **Step 3: Build/verify ZH MDX**

Run: `cd docs && (pnpm build 2>&1 | tail -25); cd ..`
Expected: same outcome/criteria as Task 7 Step 7 (use the same fallback check if no build script). Record which check was used.

- [ ] **Step 4: Commit**

```bash
git add docs/content/docs/zh/configuration/client.mdx docs/content/docs/zh/deployment/systemd.mdx docs/content/docs/zh/deployment/docker.mdx docs/content/docs/zh/deployment/railway.mdx docs/content/docs/zh/cli/walkthrough.mdx
git commit -m "docs: single-source-of-truth enrollment install (ZH mirror)"
```

---

## Task 9: Full-workspace verification

**Files:** none (verification only).

- [ ] **Step 1: Rust workspace tests + clippy**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server 2>&1 | tail -15`
Expected: all server tests pass (including `http_client_enrollments_contract`).

Run: `PORTUNUS_SKIP_WEBUI=1 cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -15`
Expected: no warnings.

- [ ] **Step 2: WebUI typecheck + tests + bundle budget**

Run: `cd webui && npx tsc -b 2>&1 | tail -10 && npx vitest run 2>&1 | tail -15 && pnpm build 2>&1 | tail -10; cd ..`
Expected: tsc clean, vitest all green, `pnpm build` succeeds and stays within the size-limit (≤ 500 KB gz).

- [ ] **Step 3: Script lint + smoke**

Run: `sh scripts/install.test.sh && (command -v shellcheck >/dev/null && shellcheck -s sh scripts/install.sh scripts/install.test.sh || echo "shellcheck absent")`
Expected: `PASS` then clean shellcheck (or the absent note).

- [ ] **Step 4: Final review request**

Use superpowers:requesting-code-review to dispatch a code-reviewer over `git diff main...HEAD` for the whole feature branch before declaring done. Address Critical/Important findings.

---

## Self-Review (filled by plan author)

**Spec coverage:**
- §"API change" (uri on operator HTTP only, no proto) → Task 1.
- §1 `scripts/install.sh` (role, version split, sudo model, --systemd reusing repo unit, --dry-run) → Task 2.
- §5 stale `deploy/systemd/install.sh` → Task 3.
- §2 `EnrollmentInstallGuide` (tabs, countdown, per-step copy, reenroll collapse, Docker uses `uri`) → Tasks 4–6.
- §3 docs single-source-of-truth EN → Task 7; ZH mirror → Task 8.
- §4 testing (HTTP contract `uri`, dry-run smoke, shellcheck, WebUI render + grep tests, no proto/wire-compat change) → Tasks 1,2,5,6,9.
- Risk "uri must land before WebUI/docs" → enforced by task order (Task 1 first).

**Placeholder scan:** no TBD/TODO; every code/edit step has literal content. The only deferred decision (docs build vs syntax-fallback) has an explicit either/or with a recorded-outcome instruction, not a placeholder.

**Type consistency:** `EnrollmentCommand.uri` (Rust) ↔ `EnrollmentResponse.uri` (Rust) ↔ `ClientEnrollmentResponse.uri` (TS) ↔ `enrollment.uri` (component) ↔ `body["uri"]` (contract test) — consistent. Component prop `mode: "provision" | "reenroll"` used identically in Tasks 5 & 6. i18n namespace `clientProvision.guide.*` defined in Task 4, consumed in Task 5.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-17-client-enrollment-install-ux.md`. Per the active goal, execution proceeds via **superpowers:subagent-driven-development** — a fresh subagent per task with two-stage review between tasks.
