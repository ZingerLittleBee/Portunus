# Client enrollment install UX — design

Date: 2026-05-17
Status: revised after spec review round 1 (pending re-review)

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

## Spec-review resolutions (round 1)

All six findings verified against the repo and accepted. How each is
resolved in this revision:

1. **Version naming** — release assets use the Cargo version *without*
   `v` (`portunus-1.4.1-<target>.tar.gz`), while `releases/latest`
   returns `tag_name=v1.4.1`. The script tracks two distinct values:
   `tag` (`vX.Y.Z`, used for the GitHub release URL path) and
   `artifact_version` (`X.Y.Z`, used in asset/checksum filenames).
   `--version` input is normalised (accepts `v1.4.1` or `1.4.1`).
   (§1, §1 "Version resolution")
2. **Docker / verbatim command** — the API gains an explicit `uri`
   field on `ClientEnrollmentResponse` (server already constructs the
   `portunus://…` URI; it is now returned alongside `command`). The UI
   never string-parses `command`. Docker steps pass `enroll <uri> --out
   /work/client.bundle.json` as container args (overriding the image
   `CMD`, entrypoint is already `portunus-client`) writing into a
   mounted volume that the long-running container then mounts.
   (§"API change", §2 Docker tab)
3. **systemd write privilege** — the new installer's `--systemd` path
   creates `/etc/portunus` as root (mirroring the existing
   `deploy/systemd/install.sh`). The operator runs `enroll --out
   ./client.bundle.json` as themselves, then a `sudo install -o root -g
   portunus-client -m 0640 ./client.bundle.json
   /etc/portunus/client.bundle.json` step. No step assumes a non-root
   shell can write `/etc/portunus`. (§1 "--systemd", §2 systemd tab)
4. **Reuse existing hardened units** — there are already hardened,
   role-specific units at `deploy/systemd/portunus-client.service` and
   `deploy/systemd/portunus-server.service` (users `portunus-client` /
   `portunus-server`, `--bundle`, `LimitNOFILE`, `CAP_NET_BIND_SERVICE`,
   full hardening block). The installer **downloads and installs those
   exact unit files** (raw URL on `main`, same mechanism as the script);
   it does NOT generate a new minimal unit. The stale
   `deploy/systemd/install.sh` (references the removed `provision-client`
   command) is updated to the enroll flow and kept as the
   "from-a-checkout" path; the new `scripts/install.sh` is the
   "curl one-liner" path. (§1 "--systemd", §3, §5)
5. **sudo model for `curl | sh`** — the script escalates *individual*
   privileged operations with `sudo` (`sudo install` for the binary,
   `sudo install -d` for dirs, `sudo install` for units, `sudo
   systemctl`). It never tries to re-exec itself. Docs also show the
   `curl … | sudo sh -s -- …` alternative for already-root contexts.
   (§1 "Privilege model")
6. **dry-run vs latest resolution** — `--dry-run` short-circuits
   *before any network call*, including latest-version resolution; it
   prints `version: <resolved at run time>` when `--version` is absent.
   The smoke test always passes an explicit `--version` so it is
   network-free and deterministic. (§1 "--dry-run", §4)

## Deliverables

1. `scripts/install.sh` — new POSIX install script (role-parameterised,
   downloads binary + hardened unit, per-op `sudo`).
2. Server/API: add `uri` to `ClientEnrollmentResponse` (proto + gRPC +
   operator HTTP + CLI struct + WebUI type + wire-compat test).
3. WebUI `EnrollmentInstallGuide` — stepped wizard component replacing
   `EnrollmentCommandCard` / `ReEnrollmentCommandCard`.
4. Docs rewrite — `configuration/client` as the single source of truth;
   `deployment/{systemd,docker,railway}`, `cli/walkthrough`, `README.md`
   updated to reference it; ZH mirrors kept 1:1.
5. Update stale `deploy/systemd/install.sh` (drop `provision-client`,
   point at the enroll flow).
6. Tests — shellcheck + deterministic dry-run smoke for the script;
   updated WebUI unit tests; updated proto wire-compat test.

## API change: `uri` on `ClientEnrollmentResponse`

The server already builds `portunus://…` in `enrollment_command()`
(`crates/portunus-server/src/operator/cli.rs`) and wraps it as
`portunus-client enroll '<uri>'`. Expose the bare URI too:

- `proto/portunus.proto`: add `string uri = N;` to the enrollment
  response message (next free field number; update
  `crates/portunus-proto/tests/enrollment_wire_compat.rs`).
- `EnrollmentCommand` struct + gRPC `enrollment.rs` + operator
  `http.rs`: populate `uri` (the value before the
  `portunus-client enroll '…'` wrapper).
- `webui/src/api/types.ts`: `ClientEnrollmentResponse.uri: string`.
- `command` stays for the plain Shell copy-paste; `uri` is used by the
  Docker tab and anywhere a bare URI is needed. No string parsing in
  the browser.

This touches the server/proto surface again (small, additive, field is
new — no compatibility concern since the flow itself is new).

## 1. `scripts/install.sh`

Committed to the repo; consumed via raw URL on `main` (no release asset,
no `release.yml` change):

```
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- <role> [flags]
```

- **Positional `role`** (required): `client` | `server`. Installs only
  the matching binary.
- **Flags**:
  - `--version <X.Y.Z|vX.Y.Z>` — optional; input normalised. Default:
    resolve latest (see below).
  - `--bin-dir DIR` — default `/usr/local/bin`.
  - `--systemd` — also install + enable the hardened unit (Linux only).
  - `--yes` — non-interactive.
  - `--dry-run` — print plan, do nothing, no network.

### Version resolution

- `releases/latest` API
  (`https://api.github.com/repos/ZingerLittleBee/Portunus/releases/latest`)
  returns `tag_name` like `v1.4.1`.
- `tag = v1.4.1` (used in the release download URL path:
  `…/releases/download/${tag}/…`).
- `artifact_version = ${tag#v}` = `1.4.1` (used in asset names).
- Asset: `portunus-${artifact_version}-${target}.tar.gz`; checksums:
  `portunus-${artifact_version}-checksums.txt`.
- `--version` accepts either form; internally split into the same two
  values (`tag` always has the `v`, `artifact_version` never does).

### Platform resolution

`uname -s` → `linux|darwin`; `uname -m` → `x86_64|aarch64` (map
`arm64`→`aarch64`). `target = <arch>-unknown-linux-gnu` (linux) or
`<arch>-apple-darwin` (darwin). Unsupported OS/arch → clear error,
non-zero exit.

### Integrity & install

Verify the tarball sha256 against the checksums file before extracting;
abort on mismatch. Extract to a temp dir; install only
`portunus-${role}` into `--bin-dir` (`sudo install -m 0755` when the
dir is not writable by the current user).

### Privilege model

The script runs unprivileged and escalates *individual* operations:

- binary: `install -m 0755 … <bin-dir>` or `sudo install …` if needed.
- `--systemd` dirs/units: `sudo install -d …`, `sudo install -m 0644
  <unit> /etc/systemd/system/…`, `sudo systemctl daemon-reload`,
  `sudo systemctl enable --now portunus-<role>`.

It never re-execs itself. Docs also document `curl … | sudo sh -s -- …`
for contexts that are already root.

### `--systemd` (Linux only; warn-and-skip elsewhere)

Installs the **existing repo unit verbatim** — fetched from
`https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/deploy/systemd/portunus-<role>.service`
(same raw-URL mechanism as the script). Steps:

- **client**:
  1. `id portunus-client || sudo useradd --system --no-create-home
     --shell /usr/sbin/nologin portunus-client`
  2. `sudo install -d -o root -g portunus-client -m 0750 /etc/portunus`
  3. `sudo install -m 0644 <fetched unit> /etc/systemd/system/portunus-client.service`
  4. `sudo systemctl daemon-reload`
  5. Print: enrollment is a separate operator step — run `enroll --out
     ./client.bundle.json`, then `sudo install -o root -g
     portunus-client -m 0640 ./client.bundle.json
     /etc/portunus/client.bundle.json`, then `sudo systemctl enable
     --now portunus-client`.
- **server**:
  1. `id portunus-server || sudo useradd --system …`
  2. `sudo install -d -o portunus-server -g portunus-server -m 0750
     /var/lib/portunus`
  3. install `portunus-server.service`, `daemon-reload`.
  4. Print: `sudo systemctl enable --now portunus-server`.

The unit's `ExecStart` already encodes `--bundle
/etc/portunus/client.bundle.json` / `--data-dir /var/lib/portunus`; the
script does not template `ExecStart`. The script does not enroll and
does not `enable --now` the client (bundle must exist first).

### `--dry-run`

Short-circuits before *any* network call (including latest resolution).
Prints: resolved `os/arch/target`, `version` (the explicit value, or
the literal `<latest, resolved at run time>` when `--version` absent),
the would-be download URL, and the install/unit actions. Exit 0.

### Errors

Unsupported OS/arch, checksum mismatch, network failure, missing `sudo`
when required → clear message, non-zero exit, no silent fallback.

## 2. WebUI `EnrollmentInstallGuide`

New component `webui/src/components/EnrollmentInstallGuide.tsx`, used by
both `ClientProvision` (mode `provision`) and `ClientDetail` (mode
`reenroll`). Replaces and removes `EnrollmentCommandCard` /
`ReEnrollmentCommandCard`.

Props: `{ enrollment: ClientEnrollmentResponse; mode: "provision" |
"reenroll" }` (`enrollment` now carries both `command` and `uri`).

Layout:
- Header: client name + **live countdown** from `enrollment.expires_at`
  (native `setInterval`, 1 s, cleared on unmount). At/after expiry: red
  state + "create a new command" hint; command stays visible but marked
  stale.
- Tabs (shadcn `Tabs`): **Shell / systemd / Docker**.
  - **Shell** — 1) `curl … install.sh | sh -s -- client`; 2)
    `enrollment.command` (verbatim); 3) `portunus-client`.
  - **systemd** — 1) `curl … install.sh | sudo sh -s -- client
    --systemd`; 2) `enrollment.command` with ` --out
    ./client.bundle.json` appended (display-only; the issued command is
    unchanged) then `sudo install -o root -g portunus-client -m 0640
    ./client.bundle.json /etc/portunus/client.bundle.json`; 3) `sudo
    systemctl enable --now portunus-client` + `systemctl status
    portunus-client`.
  - **Docker** — 1) one-shot enroll into a host volume:
    `docker run --rm -v "$PWD:/work" ghcr.io/zingerlittlebee/portunus-client \
    enroll '<enrollment.uri>' --out /work/client.bundle.json`
    (args override the image `CMD`; entrypoint is already
    `portunus-client`); 2) long-running:
    `docker run -d --name portunus-client --network host \
    -v "$PWD/client.bundle.json:/etc/portunus/client.bundle.json:ro" \
    ghcr.io/zingerlittlebee/portunus-client`.
- Each step: numbered, short description, copy-able command block with
  the existing copy/copied affordance (per-step copy state).
- `reenroll` mode: step 1 (install binary) collapsed with an "already
  installed? skip" note; steps 2–3 expanded.

i18n: add keys under `clientProvision.guide.*` (steps, tab labels,
countdown, expired, skip note) in `en.json` and `zh-CN.json`; remove the
now-unused `clientProvision.enrollment.*` / `clientDetail.reenrollHint`
keys (keep keys still referenced elsewhere).

## 3. Docs

`docs/content/docs/configuration/client.mdx` is the **single source of
truth** for install snippets (install.sh usage incl. version/sudo
model, enroll, bundle resolution, systemd via the hardened unit, Docker
volume layout). Other pages give context and link to it rather than
duplicating command text. When a command must appear on multiple pages
it is copied from `configuration/client` with a comment naming the
canonical location.

| Page (+ `docs/content/docs/zh/...` mirror) | Change |
|---|---|
| `configuration/client` | Authoritative: install.sh flags + version/sudo model, enroll, bundle resolution chain, systemd (hardened unit) + Docker (volume) variants |
| `deployment/systemd` | `install.sh client --systemd` walkthrough using the hardened unit; the privileged enroll→install→enable sequence |
| `deployment/docker` | Container enroll into a mounted volume + long-running mount; note install.sh is host-only |
| `deployment/railway` | Container path, adapted from the Docker section |
| `cli/walkthrough` | End-to-end quickstart: install server → bootstrap → issue enrollment → install client → enroll → run |
| `README.md` | Top-of-readme quickstart switched to the install.sh one-liner |

ZH mirrors updated 1:1 with the EN pages (same structure, translated
prose, identical command blocks).

## 4. Testing

- `scripts/install.sh`: passes `shellcheck`; a deterministic smoke test
  invoking `sh scripts/install.sh client --version 1.4.1 --dry-run`
  asserts the resolved target and asset URL
  (`…/releases/download/v1.4.1/portunus-1.4.1-<target>.tar.gz`) and
  exit 0 with no network and no writes. (Lives under `scripts/` or the
  `crates/portunus-e2e` shell harness — pick the lighter during
  planning.)
- WebUI: rewrite `webui/tests/unit/client-provision-enrollment.test.ts`
  and `webui/tests/unit/client-detail-reenrollment.test.ts` — tab
  switching, per-step copy, countdown render, expired state, reenroll
  collapsed step 1, Docker step uses `uri` (not parsed `command`). No
  new deps; fake timers for the countdown.
- Proto: extend `crates/portunus-proto/tests/enrollment_wire_compat.rs`
  for the new `uri` field number; existing Rust suites otherwise
  unaffected.

## Risks / notes

- Scope expanded twice: a new top-level `install.sh` and a new `uri`
  API field. The latter touches proto/gRPC/HTTP/CLI again — additive
  and low-risk because the enrollment surface is brand new (no
  compatibility concern), but it must land before the WebUI/docs work.
- raw-URL-on-`main` means the script *and* the fetched unit files are
  unversioned; acceptable per user decision. The script's CLI contract
  and the unit `ExecStart` paths must stay stable so older docs keep
  working.
- Two install entry points now coexist: `scripts/install.sh` (curl
  one-liner, downloads binary + unit) and `deploy/systemd/install.sh`
  (from a checkout, units only). The latter is updated to the enroll
  flow so they do not contradict each other; docs point at the former.
- `--systemd` complexity is contained by reusing the existing hardened
  unit verbatim instead of generating one.
