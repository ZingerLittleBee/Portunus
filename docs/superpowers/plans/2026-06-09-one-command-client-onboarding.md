# One-command client onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Collapse the client onboarding flow so the binary path is a single `curl … | sh -s -- client --enroll '<uri>'` command and the Docker path is a single `docker run -d -e PORTUNUS_ENROLL_URI='<uri>' …` command, eliminating the manual `sudo install -o … -g … -m …` bundle placement and the separate enroll/start steps.

**Architecture:** Three independent mechanisms over one shared core (`enroll::enroll`). (1) `install.sh --enroll '<uri>'` orchestrates enroll-then-place-then-start for the systemd/OpenRC binary path, reusing the existing `install -o root -g portunus-client -m 0640` idiom. (2) The `portunus-client` binary gains a **self-bootstrap** run-mode: when the resolved bundle is absent and `PORTUNUS_ENROLL_URI` is set, it enrolls into the resolved path before connecting — this is how the Docker image (distroless, no shell) onboards. (3) `Dockerfile.client` makes `/etc/portunus` nonroot-writable so the self-bootstrap can write into a mounted named volume. The Web UI renders one command per tab.

**Tech Stack:** Rust (edition 2024, tokio, tracing), POSIX `sh` (`scripts/install.sh`, tested with bash+dash), Docker (distroless multi-stage), React + Vite + TypeScript + i18next + vitest.

**Reference spec:** `docs/superpowers/specs/2026-06-09-one-command-client-onboarding-design.md`

> **Note on line numbers:** all line numbers below are anchors from a snapshot; locate the quoted code by its surrounding context (function name / adjacent lines) before editing, since earlier edits in the same file shift later numbers.

---

## File Structure

**Modified:**
- `crates/portunus-client/src/main.rs` — add `self_enroll_uri` helper + unit tests; wire self-bootstrap into the run path.
- `deploy/docker/Dockerfile.client` — create nonroot-owned `/etc/portunus`.
- `scripts/install.sh` — `ENROLL_URI` global, `--enroll` parse arm, help text, client-only validation, `enroll_uri` plan line, `place_client_bundle` helper, dispatch wiring, two i18n keys.
- `scripts/install.test.sh` — dry-run + validation + i18n assertions for `--enroll`.
- `webui/src/i18n/en.json`, `webui/src/i18n/zh-CN.json` — remove 7 step keys, add 2 keys.
- `webui/src/components/EnrollmentInstallGuide.tsx` — single command per tab.
- `webui/tests/unit/enrollment-install-guide-render.test.tsx` — rewrite for new testids/commands.
- `crates/portunus-e2e/tests/common/mod.rs` — `create_enrollment_uri` + `spawn_client_self_enroll` helpers; refactor `provision_client_http`.
- `crates/portunus-e2e/tests/happy_path.rs` — self-bootstrap e2e test.
- `CHANGELOG.md`, docs — note the new flow.

**Unchanged (verified, no edits needed):**
- `deploy/systemd/portunus-client.service`, `deploy/openrc/portunus-client.*` — already read `/etc/portunus/client.bundle.json`; `install.sh` places it.
- `webui/tests/unit/client-provision-enrollment.test.ts`, `client-detail-reenrollment.test.ts` — only assert `EnrollmentInstallGuide` source presence (still true).

---

## Task 1: Rust — `self_enroll_uri` decision helper (pure, TDD)

**Files:**
- Modify: `crates/portunus-client/src/main.rs` (add a free fn near the top-level fns; add a `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Append to `crates/portunus-client/src/main.rs` (end of file):

```rust
#[cfg(test)]
mod tests {
    use super::self_enroll_uri;

    #[test]
    fn skips_when_bundle_present() {
        assert_eq!(self_enroll_uri(true, Some("portunus://x".into())), None);
    }

    #[test]
    fn returns_uri_when_absent_and_set() {
        assert_eq!(
            self_enroll_uri(false, Some("portunus://x".into())),
            Some("portunus://x".to_string())
        );
    }

    #[test]
    fn none_when_absent_and_unset() {
        assert_eq!(self_enroll_uri(false, None), None);
    }

    #[test]
    fn trims_and_rejects_blank() {
        assert_eq!(self_enroll_uri(false, Some("   ".into())), None);
        assert_eq!(
            self_enroll_uri(false, Some("  portunus://x  ".into())),
            Some("portunus://x".to_string())
        );
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p portunus-client self_enroll_uri 2>&1 | tail -20`
Expected: FAIL to compile — `cannot find function self_enroll_uri in this scope` / `super::self_enroll_uri` unresolved.

- [ ] **Step 3: Write the minimal implementation**

Add this free function to `crates/portunus-client/src/main.rs`, immediately above `fn main() -> ExitCode {`:

```rust
/// Decide whether to self-enroll on startup. Returns the URI to redeem
/// only when no bundle is present yet AND a non-empty `PORTUNUS_ENROLL_URI`
/// was supplied. A bundle that already exists always wins — the one-time
/// enrollment code is spent after first use, so a present bundle is loaded
/// as-is. Used by the Docker image to onboard on first boot.
fn self_enroll_uri(bundle_present: bool, env_uri: Option<String>) -> Option<String> {
    if bundle_present {
        return None;
    }
    let uri = env_uri?;
    let trimmed = uri.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p portunus-client self_enroll_uri 2>&1 | tail -20`
Expected: PASS — `test result: ok. 4 passed`.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-client/src/main.rs
git commit -m "feat(client): add self-enroll decision helper"
```

---

## Task 2: Rust — wire self-bootstrap into the run path

**Files:**
- Modify: `crates/portunus-client/src/main.rs` (the run path in `fn main`, between `resolve_bundle_path` and `CredentialBundle::read_from`)

Context — the existing run path (around `main.rs:95`) reads:

```rust
    let bundle_path = match resolve_bundle_path(cli.bundle.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            error!(event = "client.bundle_search_failed", attempted = ?e.attempted);
            return ExitCode::from(1);
        }
    };
    let bundle = match CredentialBundle::read_from(&bundle_path) {
```

- [ ] **Step 1: Insert the self-bootstrap block**

Insert the following **between** the closing `};` of the `let bundle_path = match …` block and the `let bundle = match CredentialBundle::read_from(&bundle_path) {` line:

```rust
    // Self-bootstrap (Docker first boot): when the resolved bundle is
    // absent but PORTUNUS_ENROLL_URI is set, redeem it once into the
    // resolved path before loading. The Docker image always passes
    // `--bundle /etc/portunus/client.bundle.json`, so the target path is
    // known; on subsequent boots the persisted bundle wins.
    if let Some(uri) = self_enroll_uri(
        bundle_path.is_file(),
        std::env::var("PORTUNUS_ENROLL_URI").ok(),
    ) {
        info!(event = "client.self_bootstrap", path = %bundle_path.display());
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                error!(event = "client.runtime_failed", error = %e);
                return ExitCode::from(1);
            }
        };
        if let Err(e) = rt.block_on(enroll::enroll(&uri, Some(bundle_path.clone()))) {
            eprintln!("error: {e}");
            error!(
                event = "client.self_bootstrap_failed",
                error = %e,
                path = %bundle_path.display()
            );
            return ExitCode::from(1);
        }
    }
```

- [ ] **Step 2: Build to verify it compiles**

Run: `cargo build -p portunus-client 2>&1 | tail -20`
Expected: builds cleanly (no errors). `enroll`, `info!`, `error!` are already imported in `main.rs`.

- [ ] **Step 3: Lint**

Run: `cargo clippy -p portunus-client --all-targets -- -D warnings 2>&1 | tail -20`
Expected: no warnings.

- [ ] **Step 4: Run the crate's unit tests**

Run: `cargo test -p portunus-client 2>&1 | tail -20`
Expected: PASS (the Task 1 tests plus existing bundle/enroll tests).

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-client/src/main.rs
git commit -m "feat(client): self-enroll on first boot from PORTUNUS_ENROLL_URI"
```

> End-to-end coverage of this path lands in Task 8.

---

## Task 3: Dockerfile — nonroot-writable `/etc/portunus`

**Files:**
- Modify: `deploy/docker/Dockerfile.client`

Current contents (verified):

```dockerfile
FROM debian:12-slim AS abi-check
ARG TARGETARCH
COPY docker-bin/${TARGETARCH}/portunus-client /usr/local/bin/portunus-client
RUN /usr/local/bin/portunus-client --version

FROM gcr.io/distroless/static-debian12:nonroot

ARG TARGETARCH
COPY --from=abi-check /usr/local/bin/portunus-client /usr/local/bin/portunus-client

# Bundle is mounted read-only at /etc/portunus/client.bundle.json.
# …
USER nonroot:nonroot

ENTRYPOINT ["/usr/local/bin/portunus-client"]
CMD ["--bundle", "/etc/portunus/client.bundle.json"]
```

The final stage is distroless (no shell), so the directory is prepared in the `abi-check` stage and copied with ownership. `nonroot` is uid/gid `65532` in `gcr.io/distroless/static-debian12` (same value already used in `Dockerfile.server`).

- [ ] **Step 1: Prepare the dir in the abi-check stage**

Edit the `RUN /usr/local/bin/portunus-client --version` line in the `abi-check` stage to also create the dir:

```dockerfile
RUN /usr/local/bin/portunus-client --version \
 && mkdir -p /etc/portunus && chown 65532:65532 /etc/portunus
```

- [ ] **Step 2: Copy it into the final stage with ownership**

Immediately after the `COPY --from=abi-check /usr/local/bin/portunus-client /usr/local/bin/portunus-client` line, add:

```dockerfile
# Self-enrollment (PORTUNUS_ENROLL_URI on first boot) writes the bundle
# here. A bind- or named-volume mounted at /etc/portunus inherits this
# nonroot ownership so the unprivileged process can create the bundle.
COPY --from=abi-check --chown=65532:65532 /etc/portunus /etc/portunus
```

- [ ] **Step 3: Update the stale "read-only" comment**

Replace the comment line `# Bundle is mounted read-only at /etc/portunus/client.bundle.json.` with:

```dockerfile
# The bundle lives at /etc/portunus/client.bundle.json. With
# `-e PORTUNUS_ENROLL_URI=…` and a writable volume at /etc/portunus, the
# container self-enrolls on first boot; otherwise mount a ready bundle.
```

- [ ] **Step 4: Build the image for the host arch to verify the Dockerfile parses and runs**

Run (requires Docker; stages the local binary into the build context):

```bash
cd /Users/zingerbee/Documents/forward-rs
./deploy/docker/prepare-build-context.sh 2>/dev/null || true
docker build -f deploy/docker/Dockerfile.client -t portunus-client:plan-check . \
  && docker run --rm portunus-client:plan-check --version
```

Expected: build succeeds and `--version` prints the client version.
If Docker is unavailable locally, skip the build (it is exercised in `.github/workflows/release.yml`); the directory-ownership change mirrors the verified `Dockerfile.server` pattern and the functional path is covered by Task 8.

- [ ] **Step 5: Commit**

```bash
git add deploy/docker/Dockerfile.client
git commit -m "feat(docker): make /etc/portunus nonroot-writable for self-enroll"
```

---

## Task 4: install.sh — `--enroll` flag, validation, and dry-run plan line (TDD)

**Files:**
- Modify: `scripts/install.sh` (global vars, `parse_args`, help, `main` validation, `print_plan`)
- Test: `scripts/install.test.sh`

- [ ] **Step 1: Write the failing tests**

Add to `scripts/install.test.sh` (near the other dry-run `client` assertions; follow the existing `out=$(…) || fail …; echo "$out" | grep …` style):

```sh
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `bash scripts/install.test.sh 2>&1 | tail -20`
Expected: FAIL — the `client --enroll … --dry-run` invocation errors with `unknown argument: --enroll` (so `|| fail "--enroll dry-run exit"` triggers).

- [ ] **Step 3: Add the global variable**

In the global init block (`scripts/install.sh:28–63`), after the `DRY_RUN="no"` line, add:

```sh
ENROLL_URI=""     # --enroll '<uri>' (client/binary only): one-time enrollment URI
```

- [ ] **Step 4: Add the parse_args arm**

In `parse_args()`, after the `--config) …` arm (`scripts/install.sh:762`), add:

```sh
      --enroll) shift; [ $# -gt 0 ] || die "--enroll needs a value"; ENROLL_URI="$1" ;;
```

- [ ] **Step 5: Update the help usage string**

In the `-h|--help)` arm (`scripts/install.sh:768`), insert `[--enroll '<uri>' (client)]` right after the `[--config PATH (standalone)]` token in the usage string.

- [ ] **Step 6: Add client-only / binary-only validation**

In `main()`, immediately after the line `[ -n "$DOMAIN" ] && [ -n "$ROLE" ] && [ "$ROLE" != server ] && die "--domain is server-only"` (`scripts/install.sh:814`), add:

```sh
  [ -n "$ENROLL_URI" ] && [ "$ROLE" != client ] && die "--enroll is client-only"
  [ -n "$ENROLL_URI" ] && [ "$DEPLOY" = docker ] && die "--enroll is binary-only (for Docker pass PORTUNUS_ENROLL_URI to the container)"
```

- [ ] **Step 7: Add the redacted plan line**

Find `print_plan()` (the function that emits `echo "role:             ${ROLE}"`, around `scripts/install.sh:482`). Immediately after that `role:` echo, add:

```sh
  [ "$ROLE" = client ] && [ -n "$ENROLL_URI" ] && echo "enroll_uri:       ${ENROLL_URI%%\?*} (code redacted)"
```

(`${ENROLL_URI%%\?*}` strips the `?pin=…&code=…` query, so the one-time code never appears in plan output. Verified to work under both bash and dash.)

- [ ] **Step 8: Run the tests to verify they pass (bash AND dash)**

Run:
```bash
bash scripts/install.test.sh 2>&1 | tail -5
TEST_SH=dash bash scripts/install.test.sh 2>&1 | tail -5
```
Expected: both print the suite's success line (no `FAIL`).

- [ ] **Step 9: Lint the script**

Run: `shellcheck -s sh -S warning scripts/install.sh && dash -n scripts/install.sh && bash -n scripts/install.sh`
Expected: no output (clean).

- [ ] **Step 10: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(install): add client --enroll flag (parse, validate, plan)"
```

---

## Task 5: install.sh — place the bundle and wire it into install

**Files:**
- Modify: `scripts/install.sh` (`place_client_bundle` helper, `dispatch_verb` wiring, i18n keys)
- Test: `scripts/install.test.sh` (i18n resolution)

- [ ] **Step 1: Write the failing i18n tests**

Add to `scripts/install.test.sh` (near the other `--print-i18n` assertions):

```sh
# --- new enroll i18n keys resolve (not echoed back as the bare key) ---
for key in enroll_placed enroll_failed; do
  en="$($SH "$script" --lang en --print-i18n "$key")"; [ "$en" != "$key" ] || fail "en i18n missing: $key"
  zh="$($SH "$script" --lang zh --print-i18n "$key")"; [ "$zh" != "$key" ] || fail "zh i18n missing: $key"
done
```

- [ ] **Step 2: Run to verify failure**

Run: `bash scripts/install.test.sh 2>&1 | tail -5`
Expected: FAIL — `en i18n missing: enroll_placed` (`--print-i18n` echoes the key back because no `t()` arm exists yet).

- [ ] **Step 3: Register the i18n keys**

In `I18N_KEYS` (`scripts/install.sh:26`), append the two keys to the end of the space-separated string:

```sh
… next_standalone_create enroll_placed enroll_failed"
```

- [ ] **Step 4: Add the `t()` arms (both languages)**

In `t()`, just before the final `*) _f="$_k" ;;` fallthrough, add the zh arms with the other `zh:` entries and the en arms with the other `*:` entries (place each in its language block to match file style):

```sh
    zh:enroll_placed) _f="已将注册凭据写入 %s" ;;
    zh:enroll_failed) _f="客户端注册失败；二进制与服务已安装，请用新的注册链接重试。" ;;
```

```sh
    *:enroll_placed) _f="Enrollment bundle placed at %s" ;;
    *:enroll_failed) _f="Enrollment failed; the binary and service are installed — retry with a fresh enroll link." ;;
```

- [ ] **Step 5: Add the `place_client_bundle` helper**

Add this function after `apply_config_path()` (`scripts/install.sh:~376`):

```sh
# Enroll the client and place its bundle where the service reads it.
# Runs after the binary + service unit are installed; dies on failure so we
# never enable a service that would crash-loop on a missing bundle.
# Re-enrollment: if the service is already active, restart it so the new
# credentials take effect (a fresh install is started by the caller).
place_client_bundle() {
  _uri="$1"
  ensure_svc_user client   # idempotent: guarantees portunus-client + /etc/portunus
  _tmp="$(mktemp)" || die "failed to create temp file for bundle"
  if ! "${BIN_DIR}/portunus-client" enroll "$_uri" --out "$_tmp"; then
    rm -f "$_tmp"
    die "$(t enroll_failed)"
  fi
  ${SUDO:-} install -o root -g portunus-client -m 0640 "$_tmp" /etc/portunus/client.bundle.json \
    || { rm -f "$_tmp"; die "failed to place client bundle"; }
  rm -f "$_tmp"
  if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet portunus-client 2>/dev/null; then
    ${SUDO:-} systemctl restart portunus-client || true
  elif command -v rc-service >/dev/null 2>&1 && rc-service portunus-client status >/dev/null 2>&1; then
    ${SUDO:-} rc-service portunus-client restart || true
  fi
  t enroll_placed "/etc/portunus/client.bundle.json"; echo
}
```

- [ ] **Step 6: Wire it into `dispatch_verb`**

In `dispatch_verb()`'s `install` branch, in the binary (`else`) sub-branch, the tail currently reads (`scripts/install.sh:1266–1267`):

```sh
        meta_write "$(meta_path_for)" "role=$ROLE" "deploy=$DEPLOY" "version=$resolved_version" "lang=${LANG_CODE:-en}" "init=$INIT" "advertised_endpoint_set=$([ -n "$ADVERTISED" ] && echo yes || echo no)"
        if service_should_start; then svc enable_start "$ROLE"; fi
```

Insert the enroll step **between** the `meta_write` line and the `if service_should_start` line:

```sh
        if [ "$ROLE" = client ] && [ -n "$ENROLL_URI" ]; then
          place_client_bundle "$ENROLL_URI"
        fi
```

(Placing after `meta_write` keeps the install recorded even if enrollment dies; placing before `enable_start` guarantees the bundle exists before first start.)

- [ ] **Step 7: Run the i18n tests + lint**

Run:
```bash
bash scripts/install.test.sh 2>&1 | tail -5
TEST_SH=dash bash scripts/install.test.sh 2>&1 | tail -5
shellcheck -s sh -S warning scripts/install.sh && dash -n scripts/install.sh && bash -n scripts/install.sh
```
Expected: tests pass under bash and dash; shellcheck/parse clean.

- [ ] **Step 8: Manual smoke (optional, requires a running server)**

On a throwaway Linux host (or container) with a reachable control plane, confirm the headline flow end-to-end:
```bash
curl -fsSL .../install.sh | sh -s -- client --enroll 'portunus://HOST:7443/enroll?pin=sha256:…&code=…'
# expect: "Enrollment bundle placed at /etc/portunus/client.bundle.json" and an active service
systemctl is-active portunus-client
sudo stat -c '%U:%G %a' /etc/portunus/client.bundle.json   # expect: root:portunus-client 640
```

- [ ] **Step 9: Commit**

```bash
git add scripts/install.sh scripts/install.test.sh
git commit -m "feat(install): place enrolled bundle and start service in one command"
```

---

## Task 6: Web UI — i18n key changes (en + zh-CN)

**Files:**
- Modify: `webui/src/i18n/en.json`, `webui/src/i18n/zh-CN.json`
- Test: `webui/tests/unit/i18n-coverage.test.ts` (existing; enforces en/zh parity)

The `clientProvision.guide` block currently holds: `heading`, `expiresIn`, `expired`, `tabBinary`, `tabDocker`, `copy`, `copied`, `skipNote`, `stepInstall`, `stepEnrollSystemd`, `stepEnableSystemd`, `stepEnrollDocker`, `stepRunDocker`, `dockerNote`.

- [ ] **Step 1: Edit `en.json`**

In `webui/src/i18n/en.json`, replace the `clientProvision.guide` object with (keep `heading`, `expiresIn`, `expired`, `tabBinary`, `tabDocker`, `copy`, `copied`; remove the 7 step/skip/docker keys; add `codeNote` + `reenrollNote`):

```json
    "guide": {
      "heading": "Install {{name}}",
      "expiresIn": "Expires in {{remaining}}",
      "expired": "This command has expired — create a new one.",
      "tabBinary": "Binary",
      "tabDocker": "Docker",
      "copy": "Copy",
      "copied": "Copied",
      "codeNote": "This command carries a single-use enrollment code that expires soon — run it once on the edge host.",
      "reenrollNote": "Re-running replaces this client's credentials. On Docker, recreate the container with a fresh volume first: docker rm -f portunus-client && docker volume rm portunus-client."
    }
```

- [ ] **Step 2: Edit `zh-CN.json`**

In `webui/src/i18n/zh-CN.json`, replace the matching `clientProvision.guide` object with the same key set in Chinese:

```json
    "guide": {
      "heading": "安装 {{name}}",
      "expiresIn": "{{remaining}} 后过期",
      "expired": "该命令已过期，请重新生成。",
      "tabBinary": "二进制",
      "tabDocker": "Docker",
      "copy": "复制",
      "copied": "已复制",
      "codeNote": "该命令包含一次性的注册码，且很快过期——请在边缘主机上执行一次。",
      "reenrollNote": "重新执行会替换该客户端的凭据。Docker 场景请先用新卷重建容器：docker rm -f portunus-client && docker volume rm portunus-client。"
    }
```

> Preserve the surrounding keys' existing translated values for `heading`/`expiresIn`/`expired`/etc. if they already differ from the above — only the key **set** must match en.json. If the current zh values are already correct, keep them and just drop the 7 removed keys + add the 2 new ones.

- [ ] **Step 3: Run the i18n coverage test**

Run: `cd webui && pnpm test i18n-coverage 2>&1 | tail -20`
Expected: PASS — "en and zh-CN expose identical key sets".

> This will FAIL if `EnrollmentInstallGuide.tsx` still references a removed key at runtime, but the coverage test only compares JSON key sets — the component is updated in Task 7. If `pnpm test` (full run) is used here it may fail on the render test until Task 7; run the focused `i18n-coverage` test for this task.

- [ ] **Step 4: Commit**

```bash
git add webui/src/i18n/en.json webui/src/i18n/zh-CN.json
git commit -m "feat(webui): swap multi-step enroll strings for single-command notes"
```

---

## Task 7: Web UI — rewrite EnrollmentInstallGuide to one command per tab

**Files:**
- Modify: `webui/src/components/EnrollmentInstallGuide.tsx`
- Test: `webui/tests/unit/enrollment-install-guide-render.test.tsx` (rewrite)

- [ ] **Step 1: Rewrite the render test first (TDD)**

Replace the whole body of `webui/tests/unit/enrollment-install-guide-render.test.tsx` with:

```tsx
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
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
  it("renders the binary tab as a single install.sh --enroll command", () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="provision" />);
    expect(screen.getByRole("tab", { name: "Binary" })).toBeDefined();
    expect(screen.getByRole("tab", { name: "Docker" })).toBeDefined();
    expect(screen.queryByRole("tab", { name: "systemd" })).toBeNull();
    const binary = screen.getByTestId("guide-command-binary").textContent ?? "";
    expect(binary).toContain("sh -s -- client --enroll 'portunus://host:7443/enroll?code=abc'");
  });

  it("renders the docker tab as a single docker run with PORTUNUS_ENROLL_URI", async () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="provision" />);
    await userEvent.click(screen.getByRole("tab", { name: "Docker" }));
    const docker = screen.getByTestId("guide-command-docker").textContent ?? "";
    expect(docker).toContain("PORTUNUS_ENROLL_URI='portunus://host:7443/enroll?code=abc'");
    expect(docker).toContain("-v portunus-client:/etc/portunus");
  });

  it("shows a live countdown that reaches the expired state", () => {
    vi.useFakeTimers();
    const past = new Date(Date.now() - 1_000).toISOString();
    render(<EnrollmentInstallGuide enrollment={mk({ expires_at: past })} mode="provision" />);
    vi.advanceTimersByTime(1_100);
    expect(screen.getByText(/expired/i)).toBeDefined();
  });

  it("shows a re-enroll note only in reenroll mode", () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="reenroll" />);
    expect(screen.getByText(/docker volume rm/i)).toBeDefined();
  });

  it("copies a command to the clipboard", () => {
    render(<EnrollmentInstallGuide enrollment={mk()} mode="provision" />);
    const firstCopy = screen.getAllByRole("button", { name: /copy/i })[0] as HTMLElement;
    fireEvent.click(firstCopy);
    expect((navigator.clipboard as Clipboard).writeText).toHaveBeenCalled();
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd webui && pnpm test enrollment-install-guide-render 2>&1 | tail -20`
Expected: FAIL — `guide-command-binary` testid not found (component still emits `guide-step-*`).

- [ ] **Step 3: Rewrite the component**

Replace the entire contents of `webui/src/components/EnrollmentInstallGuide.tsx` with:

```tsx
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Check, Clock, Copy, Terminal } from "lucide-react";

import { cn } from "@/lib/cn";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import type { ClientEnrollmentResponse } from "@/api/types";

const INSTALL_URL =
  "https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh";
const IMAGE = "ghcr.io/zingerlittlebee/portunus-client";

type Mode = "provision" | "reenroll";

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
    <div className="relative min-w-0">
      <pre
        data-testid={testId}
        className="overflow-hidden whitespace-pre-wrap break-all rounded-md bg-muted p-3 pr-24 font-mono text-xs leading-relaxed"
      >
        {command}
      </pre>
      <Button
        variant="outline"
        size="sm"
        onClick={copy}
        className="absolute right-1.5 top-1.5 h-7 px-2"
      >
        {copied ? <Check className="mr-1 h-3.5 w-3.5" /> : <Copy className="mr-1 h-3.5 w-3.5" />}
        {copied ? t("clientProvision.guide.copied") : t("clientProvision.guide.copy")}
      </Button>
    </div>
  );
}

export function EnrollmentInstallGuide({
  enrollment,
  mode,
  framed = true,
}: {
  enrollment: ClientEnrollmentResponse;
  mode: Mode;
  /** Wrap in a Card (standalone panel). Set false when already inside a
   * dialog or another card so we don't nest framed surfaces. */
  framed?: boolean;
}) {
  const { t } = useTranslation();
  const { expired, remaining } = useCountdown(enrollment.expires_at);
  const reenroll = mode === "reenroll";

  const binaryCommand = `curl -fsSL ${INSTALL_URL} | sh -s -- client --enroll '${enrollment.uri}'`;
  const dockerCommand = `docker run -d --name portunus-client --network host -e PORTUNUS_ENROLL_URI='${enrollment.uri}' -v portunus-client:/etc/portunus ${IMAGE}`;

  const header = (
    <div className="flex flex-wrap items-center justify-between gap-2">
      <span className="flex items-center gap-2 font-semibold">
        <Terminal className="h-5 w-5 shrink-0" />
        {t("clientProvision.guide.heading", { name: enrollment.client_name })}
      </span>
      <span
        className={cn(
          "flex items-center gap-1 text-sm",
          expired ? "text-destructive" : "text-muted-foreground",
        )}
      >
        <Clock className="h-4 w-4 shrink-0" />
        {expired
          ? t("clientProvision.guide.expired")
          : t("clientProvision.guide.expiresIn", { remaining })}
      </span>
    </div>
  );

  const body = (
    <>
      <Tabs defaultValue="binary" className="min-w-0">
        <TabsList>
          <TabsTrigger value="binary">{t("clientProvision.guide.tabBinary")}</TabsTrigger>
          <TabsTrigger value="docker">{t("clientProvision.guide.tabDocker")}</TabsTrigger>
        </TabsList>
        <TabsContent value="binary" className="pt-4">
          <CommandBlock testId="guide-command-binary" command={binaryCommand} />
        </TabsContent>
        <TabsContent value="docker" className="pt-4">
          <CommandBlock testId="guide-command-docker" command={dockerCommand} />
        </TabsContent>
      </Tabs>
      <p className="text-xs text-muted-foreground">{t("clientProvision.guide.codeNote")}</p>
      {reenroll && (
        <p className="text-xs text-muted-foreground">{t("clientProvision.guide.reenrollNote")}</p>
      )}
    </>
  );

  if (!framed) {
    return (
      <div className="flex min-w-0 flex-col gap-4">
        {header}
        {body}
      </div>
    );
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>{header}</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">{body}</CardContent>
    </Card>
  );
}
```

- [ ] **Step 4: Run the render test to verify it passes**

Run: `cd webui && pnpm test enrollment-install-guide-render 2>&1 | tail -20`
Expected: PASS (all 5 cases).

- [ ] **Step 5: Typecheck, lint, and full unit suite**

Run:
```bash
cd webui && pnpm exec tsc -b 2>&1 | tail -20 && pnpm test 2>&1 | tail -20
```
Expected: typecheck clean; full vitest run passes (including `i18n-coverage`, `client-provision-enrollment`, `client-detail-reenrollment`).

- [ ] **Step 6: Commit**

```bash
git add webui/src/components/EnrollmentInstallGuide.tsx webui/tests/unit/enrollment-install-guide-render.test.tsx
git commit -m "feat(webui): render one onboarding command per tab"
```

---

## Task 8: e2e — self-bootstrap enrolls and connects

**Files:**
- Modify: `crates/portunus-e2e/tests/common/mod.rs` (add `create_enrollment_uri`, `spawn_client_self_enroll`; refactor `provision_client_http`)
- Test: `crates/portunus-e2e/tests/happy_path.rs` (new test)

- [ ] **Step 1: Add the harness helpers**

In `crates/portunus-e2e/tests/common/mod.rs`, add a function that creates an enrollment and returns its URI without redeeming it. Replace the body of the existing `provision_client_http` to delegate to it (so the URI-extraction logic is not duplicated):

```rust
/// Create an enrollment via the operator HTTP API and return its one-time
/// `portunus://…` URI **without** redeeming it (so the caller can drive
/// self-bootstrap). Mirrors the URI extraction in `provision_client_http`.
pub fn create_enrollment_uri(operator_http_addr: &str, name: &str) -> String {
    let endpoint = format!("http://{operator_http_addr}/v1/client-enrollments");
    let (k, v) = auth_header();
    let resp = reqwest::blocking::Client::new()
        .post(&endpoint)
        .header(k, v)
        .json(&serde_json::json!({ "name": name, "address": "127.0.0.1" }))
        .send()
        .expect("POST /v1/client-enrollments");
    assert!(resp.status().is_success(), "enrollment HTTP failed: {resp:?}");
    let enrollment: serde_json::Value = resp.json().expect("parse enrollment JSON");
    let command = enrollment["command"].as_str().expect("enrollment command");
    command
        .split_once('\'')
        .and_then(|(_, rest)| rest.rsplit_once('\'').map(|(uri, _)| uri.to_string()))
        .expect("extract enrollment URI")
}
```

Then change `provision_client_http` to:

```rust
pub fn provision_client_http(operator_http_addr: &str, name: &str) -> PathBuf {
    let uri = create_enrollment_uri(operator_http_addr, name);
    let out_dir = fresh_tempdir("bundle out").keep();
    let path = out_dir.join(format!("{name}.bundle.json"));
    let status = cmd_for("portunus-client")
        .arg("enroll")
        .arg(uri)
        .arg("--out")
        .arg(&path)
        .status()
        .expect("run portunus-client enroll");
    assert!(status.success(), "portunus-client enroll failed: {status:?}");
    path
}
```

And add a self-enroll spawn variant next to `spawn_client`:

```rust
/// Launch `portunus-client --bundle <path>` with `PORTUNUS_ENROLL_URI` set
/// and no bundle on disk yet, exercising the first-boot self-bootstrap path.
pub fn spawn_client_self_enroll(bundle_path: &Path, enroll_uri: &str) -> ClientHandle {
    let mut cmd = cmd_for("portunus-client");
    cmd.arg("--bundle")
        .arg(bundle_path)
        .env("PORTUNUS_ENROLL_URI", enroll_uri)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUST_LOG", rust_log_env());
    let mut child = cmd.spawn().expect("spawn portunus-client");
    let stderr = child.stderr.take().expect("client stderr piped");
    let stderr_lines = capture_stderr(stderr);
    ClientHandle {
        child,
        stderr_lines,
    }
}
```

- [ ] **Step 2: Write the failing e2e test**

Add to `crates/portunus-e2e/tests/happy_path.rs`:

```rust
#[test]
fn test_self_bootstrap_enrolls_and_connects() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server should log listening event within 5s");

    // Create an enrollment but do NOT redeem it — hand the URI to the
    // client via PORTUNUS_ENROLL_URI and let it self-bootstrap.
    let uri = common::create_enrollment_uri(&http, "edge-boot");
    let dir = common::fresh_tempdir("self-bootstrap");
    let bundle_path = dir.path().join("client.bundle.json");
    assert!(!bundle_path.exists(), "bundle must be absent before boot");

    let _client = common::spawn_client_self_enroll(&bundle_path, &uri);

    let view = common::wait_for(Duration::from_secs(10), || {
        let arr = common::list_clients_http(&http);
        let edge = arr
            .as_array()?
            .iter()
            .find(|v| v.get("client_name").and_then(|n| n.as_str()) == Some("edge-boot"))?;
        if edge.get("connected")?.as_bool()? {
            Some(edge.clone())
        } else {
            None
        }
    });

    assert_eq!(view.get("client_name").and_then(|n| n.as_str()), Some("edge-boot"));
    assert!(
        bundle_path.is_file(),
        "self-bootstrap should have written the bundle"
    );
}
```

- [ ] **Step 3: Run the test to verify it passes**

Run: `cargo test -p portunus-e2e --test happy_path test_self_bootstrap_enrolls_and_connects 2>&1 | tail -30`
Expected: PASS — the client with no bundle on disk enrolls via `PORTUNUS_ENROLL_URI`, writes the bundle, and shows `connected: true`.

> If it fails, capture logs with `E2E_RUST_LOG_ENV=debug cargo test -p portunus-e2e --test happy_path test_self_bootstrap_enrolls_and_connects -- --nocapture` and look for the `client.self_bootstrap` / `client.self_bootstrap_failed` events.

- [ ] **Step 4: Run the full e2e suite to confirm the refactor didn't regress existing tests**

Run: `cargo test -p portunus-e2e 2>&1 | tail -30`
Expected: all tests pass (including `test_list_clients_after_connect`, which now routes through the refactored `provision_client_http`).

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-e2e/tests/common/mod.rs crates/portunus-e2e/tests/happy_path.rs
git commit -m "test(e2e): cover PORTUNUS_ENROLL_URI self-bootstrap"
```

---

## Task 9: Docs, CHANGELOG, and full-suite verification

**Files:**
- Modify: `CHANGELOG.md`
- Modify: any docs that show the old multi-step enroll (grep below)

- [ ] **Step 1: Find docs that show the old flow**

Run:
```bash
cd /Users/zingerbee/Documents/forward-rs
grep -rn --include='*.md' --include='*.mdx' -e "sudo install -o root -g portunus-client" -e "--out ./client.bundle.json" -e "enroll '.*' --out" docs README.md 2>/dev/null
```

- [ ] **Step 2: Update each hit**

For each file found, replace the three-step binary snippet with the single command, and the two-step Docker snippet with the single `docker run`:

```sh
# Binary
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client --enroll 'portunus://HOST:7443/enroll?pin=sha256:…&code=…'

# Docker
docker run -d --name portunus-client --network host \
  -e PORTUNUS_ENROLL_URI='portunus://HOST:7443/enroll?pin=sha256:…&code=…' \
  -v portunus-client:/etc/portunus \
  ghcr.io/zingerlittlebee/portunus-client
```

If no hits are returned, skip this step (the Web UI dialog is the primary surface and is handled in Tasks 6–7).

- [ ] **Step 3: Add a CHANGELOG entry**

In `CHANGELOG.md`, under the top "Unreleased" / next-version section (match the existing heading style), add:

```markdown
### Added
- One-command client onboarding: `install.sh --enroll '<uri>'` installs, enrolls, places the bundle (`/etc/portunus/client.bundle.json`, `root:portunus-client 0640`), and starts the service in a single command. The Docker image self-enrolls on first boot from `PORTUNUS_ENROLL_URI` into a mounted volume. The Web UI "Connect client" dialog now shows one command per tab.
```

- [ ] **Step 4: Full workspace verification**

Run each and confirm clean:
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -20
cargo test --workspace 2>&1 | tail -20
bash scripts/install.test.sh 2>&1 | tail -5
TEST_SH=dash bash scripts/install.test.sh 2>&1 | tail -5
shellcheck -s sh -S warning scripts/install.sh
( cd webui && pnpm exec tsc -b && pnpm test 2>&1 | tail -20 )
```
Expected: formatting clean, no clippy warnings, all Rust tests pass, install.sh tests pass under bash+dash, shellcheck clean, webui typecheck + vitest pass.

- [ ] **Step 5: Commit**

```bash
git add CHANGELOG.md docs README.md
git commit -m "docs: document one-command client onboarding"
```

---

## Self-Review (completed during planning)

**Spec coverage:**
- SC-1 (binary one command) → Tasks 4 + 5 (`--enroll` + `place_client_bundle`); manual smoke in Task 5 Step 8.
- SC-2 (docker one command) → Tasks 2 + 3 (self-bootstrap + writable dir); exercised by Task 8.
- SC-3 (no `--enroll` ⇒ unchanged) → Task 4 validation keeps the flag optional; `dispatch_verb` only enrolls `if [ -n "$ENROLL_URI" ]`; existing install.test.sh cases stay green (Task 4 Step 8 / Task 9).
- SC-4 (bundle `root:portunus-client 0640`, no crash-loop) → Task 5 `install -o root -g portunus-client -m 0640`, placement before `enable_start`; verified in Task 5 Step 8 smoke.
- SC-5 (UI one command per tab) → Task 7 + render test.

**Placeholder scan:** No TBD/TODO; every code step shows complete code. The two deferred spec items are resolved here: Docker re-enroll → `reenrollNote` recreate-with-fresh-volume (Task 6/7); install.sh placement → enroll-to-temp + `install` (Task 5).

**Type/name consistency:** `self_enroll_uri` (Tasks 1→2), `place_client_bundle` (Task 5), `ENROLL_URI` (Tasks 4→5), testids `guide-command-binary`/`guide-command-docker` (Task 7 component + test), i18n keys `codeNote`/`reenrollNote` (Tasks 6→7), e2e `create_enrollment_uri`/`spawn_client_self_enroll` (Task 8) are used consistently across tasks.
