# Client enrollment install UX — design

Date: 2026-05-17
Status: approved (pending spec review)

## Problem

The enrollment flow that replaced the credential-bundle flow ships a
minimal UI: `ClientProvision`/`ClientDetail` show a single
`portunus-client enroll '<uri>'` command in a `<pre>` with a copy
button. There is no guidance on getting the binary onto the host, no
platform-specific path (shell vs systemd vs Docker), and the docs install
instructions are scattered and inconsistent across pages. Operators must
assemble the install themselves.

Goal: a stepped install experience in the WebUI plus a one-line install
script, with docs that tell the same story across every deployment page
(EN + ZH).

Out of scope / intentional: no backward compatibility with the old
credential-bundle flow (already removed); no new frontend dependencies;
no speckit `specs/NNN-*` workflow (lightweight design doc only).

## Deliverables

1. `scripts/install.sh` — new POSIX install script (role-parameterised).
2. WebUI `EnrollmentInstallGuide` — stepped wizard component replacing
   `EnrollmentCommandCard` / `ReEnrollmentCommandCard`.
3. Docs rewrite — `configuration/client` as the single source of truth;
   `deployment/{systemd,docker,railway}`, `cli/walkthrough`, `README.md`
   updated to reference it; ZH mirrors kept 1:1.
4. Tests — shellcheck + dry-run smoke for the script; updated WebUI unit
   tests for the new component.

## 1. `scripts/install.sh`

Committed to the repo; consumed via raw URL on `main` (no release asset,
no `release.yml` change):

```
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- <role> [flags]
```

- **Positional `role`** (required): `client` | `server`. Installs only
  the matching binary from the release tarball.
- **Flags**:
  - `--version vX.Y.Z` — default: resolve latest via
    `https://api.github.com/repos/ZingerLittleBee/Portunus/releases/latest`.
  - `--bin-dir DIR` — default `/usr/local/bin`; if not writable, re-exec
    the install step via `sudo`.
  - `--systemd` — also generate, `daemon-reload`, and `enable --now` a
    systemd unit for the role (see below). Requires root/sudo.
  - `--yes` — non-interactive (assume yes on prompts).
  - `--dry-run` — print resolved `os/arch/target`, version, download URL,
    and the install/unit actions; download nothing, write nothing.
- **Resolution**: `uname -s` → `linux|darwin`; `uname -m` →
  `x86_64|aarch64` (map `arm64`→`aarch64`). Compose
  `target = <arch>-unknown-linux-gnu` (linux) or `<arch>-apple-darwin`
  (darwin). Asset: `portunus-${version}-${target}.tar.gz`; also fetch
  `portunus-${version}-checksums.txt`.
- **Integrity**: verify the tarball sha256 against the checksums file
  before extracting; abort on mismatch. Extract to a temp dir, copy only
  `portunus-${role}` into `--bin-dir`, `chmod 0755`.
- **Errors**: unsupported OS/arch, checksum mismatch, network failure,
  missing `sudo` when needed → clear message, non-zero exit, no silent
  fallback.
- **Output**: on success print the next step for the role:
  - client → the `portunus-client enroll '...'` reminder + `portunus-client`.
  - server → `portunus-server --data-dir <dir> serve` hint.

### `--systemd` unit generation

Generated unit path: `/etc/systemd/system/portunus-<role>.service`.

- **client** (`portunus-client --systemd`):
  - Create system user `portunus` (`useradd --system --no-create-home
    --shell /usr/sbin/nologin portunus`) if absent.
  - Expected bundle path: `/etc/portunus/client.bundle.json` (the wizard
    and docs instruct `enroll --out /etc/portunus/client.bundle.json`,
    `chown portunus:`, `chmod 0600`).
  - Unit:
    ```ini
    [Unit]
    Description=Portunus edge client
    After=network-online.target
    Wants=network-online.target

    [Service]
    User=portunus
    Environment=PORTUNUS_CLIENT_BUNDLE=/etc/portunus/client.bundle.json
    ExecStart=<bin-dir>/portunus-client
    Restart=on-failure
    RestartSec=5

    [Install]
    WantedBy=multi-user.target
    ```
- **server** (`portunus-server --systemd`):
  - Data dir `/var/lib/portunus` (`--data-dir`), owned by `portunus`.
  - `ExecStart=<bin-dir>/portunus-server --data-dir /var/lib/portunus serve`.
  - Same `[Unit]`/`Restart` shape; `WantedBy=multi-user.target`.

`--systemd` is a no-op-with-warning on non-Linux / no `systemctl`.
Bundle/data provisioning is the operator's step (script does not enroll).

## 2. WebUI `EnrollmentInstallGuide`

New component `webui/src/components/EnrollmentInstallGuide.tsx`, used by
both `ClientProvision` (mode `provision`) and `ClientDetail` (mode
`reenroll`). Replaces `EnrollmentCommandCard` and
`ReEnrollmentCommandCard` (both removed).

Props: `{ enrollment: ClientEnrollmentResponse; mode: "provision" |
"reenroll" }`.

Layout:
- Header: client name + **live countdown** derived from
  `enrollment.expires_at` (native `setInterval`, 1 s tick, cleared on
  unmount). At/after expiry: red state + "create a new command" hint;
  the raw command stays visible but marked stale.
- Tabs (shadcn `Tabs`): **Shell / systemd / Docker**.
  - **Shell** — step 1 `curl … install.sh | sh -s -- client`; step 2 the
    `enrollment.command`; step 3 `portunus-client`.
  - **systemd** — step 1 `curl … install.sh | sh -s -- client --systemd`;
    step 2 `enrollment.command` with
    `--out /etc/portunus/client.bundle.json` then
    `chown portunus: /etc/portunus/client.bundle.json`; step 3
    `systemctl status portunus-client`.
  - **Docker** — step 1 one-shot
    `docker run --rm … ghcr.io/zingerlittlebee/portunus-client … enroll …`
    (note: install.sh not used in container mode); step 2 long-running
    `docker run -d … portunus-client`.
- Each step: numbered, short description, command in a copy-able block
  with the existing copy/copied affordance (per-step copy state).
- `reenroll` mode: step 1 (install binary) rendered collapsed with an
  "already installed? skip" note; steps 2–3 expanded.

The `enrollment.command` string is taken verbatim from the API
(`portunus-client enroll '<uri>'`); the component only wraps/derives the
surrounding steps. The systemd `--out` variant is presentational — it
appends `--out …` to the displayed command, the server-issued command is
unchanged.

i18n: add keys under `clientProvision.guide.*` (steps, tab labels,
countdown, expired, skip note) in `en.json` and `zh-CN.json`; remove now
unused `clientProvision.enrollment.*` / `clientDetail.reenrollHint` keys
that the old cards used (keep keys still referenced elsewhere).

## 3. Docs

`docs/content/docs/configuration/client.mdx` is the **single source of
truth** for the install snippets (install.sh usage, enroll, bundle
resolution, systemd unit, Docker). Other pages give the context and link
to it rather than duplicating command text (prevents drift):

| Page (+ `docs/content/docs/zh/...` mirror) | Change |
|---|---|
| `configuration/client` | Authoritative: install.sh flags, enroll, bundle resolution chain, systemd + Docker variants |
| `deployment/systemd` | `install.sh client --systemd` walkthrough; link to client config for bundle provisioning |
| `deployment/docker` | Container enroll/run; explicitly note install.sh is host-only |
| `deployment/railway` | Container path, adapted from the Docker section |
| `cli/walkthrough` | End-to-end quickstart: install server → bootstrap → issue enrollment → install client → enroll → run |
| `README.md` | Top-of-readme quickstart switched to the install.sh one-liner |

ZH mirrors updated 1:1 with the EN pages (same structure, translated
prose, identical command blocks).

## 4. Testing

- `scripts/install.sh`: passes `shellcheck`; a shell smoke test invoking
  `sh scripts/install.sh client --dry-run` asserts it prints the correct
  resolved target and asset URL and exits 0 without network/writes.
  (Place under `scripts/` or `crates/portunus-e2e` shell harness — pick
  the lighter of the two during planning.)
- WebUI: rewrite `webui/tests/unit/client-provision-enrollment.test.ts`
  and `webui/tests/unit/client-detail-reenrollment.test.ts` to cover the
  new component — tab switching, per-step copy, countdown render, expired
  state, reenroll collapsed step 1. No new deps; countdown uses native
  timers (fake timers in tests).
- Existing Rust suites unaffected (no server/proto changes in this work).

## Risks / notes

- Scope expansion vs the original "update UI + docs" ask: a new
  `install.sh` is now a first-class deliverable. Accepted by the user.
- `install.sh --systemd` adds real complexity (user creation, unit
  generation, root). Kept minimal and Linux-only; non-Linux warns.
- raw-URL-on-`main` means the script is unversioned; acceptable per user
  decision. A breaking script change would affect old docs — mitigated by
  keeping the script's CLI contract stable.
- One-source-of-truth docs: if a command must appear on multiple pages,
  it is copied from `configuration/client` and a comment notes the
  canonical location to reduce drift.
