# One-command client onboarding

**Date:** 2026-06-09
**Status:** Approved design, pending implementation plan
**Scope:** `scripts/install.sh`, `crates/portunus-client`, `deploy/docker/Dockerfile.client`, `webui/src/components/EnrollmentInstallGuide.tsx`

## Problem

The "接入客户端" (Connect Client) dialog walks an operator through a
three-step binary flow and a two-step Docker flow. The binary flow's
middle step is the friction point:

```
portunus-client enroll '<uri>' --out ./client.bundle.json
sudo install -o root -g portunus-client -m 0640 ./client.bundle.json /etc/portunus/client.bundle.json
```

Those `install` flags are not knobs to tune — they are a symptom.
`portunus-client enroll` defaults `--out` to the **user-centric** XDG
path (`$XDG_CONFIG_HOME/portunus/client.bundle.json`, see
`crates/portunus-client/src/enroll.rs:146`), but the **service** reads
the **system** path `/etc/portunus/client.bundle.json` (the systemd unit's
`--bundle` arg / the OpenRC `confd`). The verbose `sudo install …` line
exists solely to bridge that user→system gap and fix ownership so the
non-root `portunus-client` service user can read the bundle.

## Goal

Collapse the binary flow **and** the Docker flow each into a single
copy-paste command. Eliminate the `--out` flag, the `sudo install …`
placement line, and the standalone `systemctl enable --now`.

## End-state UX

**Binary tab — one command:**

```
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh \
  | sh -s -- client --enroll 'portunus://HOST:7443/enroll?pin=sha256:…&code=…'
```

Performs, in order: download + verify the release binary → create the
`portunus-client` user and `/etc/portunus` → install the systemd/OpenRC
unit → **enroll, writing `/etc/portunus/client.bundle.json` as
`root:portunus-client`, mode `0640`** → enable + start. The service comes
up immediately because the bundle is in place before first start.

**Docker tab — one command:**

```
docker run -d --name portunus-client --network host \
  -e PORTUNUS_ENROLL_URI='portunus://HOST:7443/enroll?pin=sha256:…&code=…' \
  -v portunus-client:/etc/portunus \
  ghcr.io/zingerlittlebee/portunus-client
```

The container self-enrolls into the named volume on first boot, then
runs. The persisted bundle survives restarts (and the one-time code is
spent), so subsequent boots skip enrollment.

## Design

Three changes, no new wire protocol, no change to the one-time-code/TTL
model. The binary/systemd path and the Docker path use **different**
placement mechanisms because the runtime image is `distroless/static`
(`deploy/docker/Dockerfile.client:13`) and has no shell.

### 1. `install.sh --enroll '<uri>'` (binary/systemd path)

A new optional flag. Parsed in `parse_args` (`scripts/install.sh:742`)
into an `ENROLL_URI` variable. In `dispatch_verb`'s `install` branch
(`scripts/install.sh:1253`), **binary** deploy only, after
`install_binary` + `detect_init` + `svc install` (which already creates
the user and `/etc/portunus` as `root:portunus-client 0750` via
`ensure_svc_user`, `scripts/install.sh:357`) and **before**
`svc enable_start`:

1. Run `"${BIN_DIR}/portunus-client" enroll "$ENROLL_URI" --out <staging>`.
2. Place it with the existing idiom:
   `${SUDO:-} install -o root -g portunus-client -m 0640 <staging> /etc/portunus/client.bundle.json`.
3. Remove the staging file.

The chown/chmod logic stays in `install.sh` (it already owns this idiom
for every config it places). When `--enroll` is absent, behavior is
**exactly** as today: install-only, print next-step hints. Fully
backward compatible.

Ordering guarantees the bundle exists before `enable_start`, so the
service never crash-loops on a missing bundle.

### 2. Self-bootstrap run mode (Docker path)

Because distroless has no `/bin/sh`, the placement cannot live in an
entrypoint wrapper — it lives in the binary. In normal run mode
(`crates/portunus-client/src/main.rs`, the no-subcommand path at
`main.rs:95`), before loading the bundle:

- If the resolved bundle path does **not** exist **and**
  `PORTUNUS_ENROLL_URI` is set in the environment → call the existing
  `enroll::enroll(uri, Some(resolved_path))`, then continue to load and
  run.
- If the bundle exists → ignore `PORTUNUS_ENROLL_URI` (idempotent; the
  one-time code is already spent).
- If enrollment is needed but `PORTUNUS_ENROLL_URI` is unset → today's
  "bundle not found" error path is unchanged.

No chown is needed: inside the container the bundle is written by the
running user into a user-owned volume. `enroll`'s existing `0600` write
mode is correct there.

This mechanism is general (it activates whenever a bundle is missing and
the env var is present), but it is only *exercised* by the Docker image —
the systemd/OpenRC path always has the bundle placed by `install.sh`
first.

### 3. Dockerfile: writable bundle directory

For self-bootstrap to write into the named volume mounted at
`/etc/portunus`, the directory must be writable by the image's `nonroot`
user (`deploy/docker/Dockerfile.client:22`). The Dockerfile must create
`/etc/portunus` owned by `nonroot` (or otherwise ensure the
named-volume mount inherits a `nonroot`-writable owner). The exact path
and ownership mechanics are finalized in the plan; the default bundle
path stays `/etc/portunus/client.bundle.json` (the current CMD).

### 4. Web UI — one command per tab

`webui/src/components/EnrollmentInstallGuide.tsx`:

- **Binary tab** → a single `Step` whose command embeds
  `--enroll '${enrollment.uri}'`.
- **Docker tab** → a single `docker run -d … -e
  PORTUNUS_ENROLL_URI='${enrollment.uri}' -v portunus-client:/etc/portunus
  …` command.
- Remove the multi-step `StepList`/manual fallback entirely (no
  collapsible disclosure).
- **Re-enroll mode** (`mode === "reenroll"`): show the same single
  command with a fresh URI. Re-running the binary one-liner is
  idempotent (re-installs the binary, re-enrolls, restarts). For Docker,
  re-enroll recreates the container against a fresh volume; the precise
  Docker re-enroll wording is finalized in the plan.
- A one-line note that the command carries a single-use, short-TTL code
  (it may land in shell history; harmless once spent).

## Out of scope

- No change to the enrollment gRPC schema, the join-code lifecycle, or
  the pinned-TLS handshake.
- No change to the server or standalone install/enroll flows.
- No new `enroll --system` subcommand flag (the chown lives in
  `install.sh`, per the resolved decision).

## Risks & open details (for the plan)

- **Shell history exposure:** the one-time `code` is embedded in the
  command piped to `sh`. Acceptable — single-use and short-lived. Note
  it in the UI; do not engineer around it.
- **`install.sh` staging vs. direct write:** enroll-to-temp + `install`
  (atomic, matches existing idiom) vs. enroll-direct-to-final +
  `chown`/`chmod`. Plan picks one; the temp+`install` path is the
  existing pattern.
- **`SUDO` handling:** placement into `/etc` and chown require root;
  `install.sh` already prefixes privileged ops with `${SUDO:-}`. The
  `enroll` network call itself does not need root.
- **Docker named-volume ownership:** confirm a fresh `nonroot`-writable
  volume at `/etc/portunus`; otherwise fall back to a `nonroot`-owned
  default path (e.g. under `$HOME`).
- **Docker re-enroll ergonomics:** first-boot onboarding is the headline
  win; re-enroll for Docker (stale persisted bundle) needs either a
  documented recreate-with-fresh-volume step or an optional
  `PORTUNUS_ENROLL_FORCE` env. Decide in the plan.

## Success criteria

- SC-1: A fresh host is fully onboarded (binary installed, service
  running, traffic flowing) by a **single** `curl … | sh -s -- client
  --enroll '<uri>'` command, with no manual bundle placement.
- SC-2: A fresh host is fully onboarded by a **single** `docker run`
  command carrying `PORTUNUS_ENROLL_URI`.
- SC-3: `install.sh client` with no `--enroll` behaves byte-for-byte as
  today (install-only).
- SC-4: The placed bundle is `root:portunus-client`, mode `0640`, and
  the non-root service reads it successfully on first start (no
  crash-loop).
- SC-5: The Web UI binary and Docker tabs each render exactly one
  command.
