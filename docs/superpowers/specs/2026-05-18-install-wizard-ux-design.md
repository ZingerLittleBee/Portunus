# Install Wizard UX Refactor — Design

> Status: Draft
> Scope: interactive wizard/menu only. The non-interactive flag
> interface, CLI defaults, `curl … | bash` one-liner, and CI behavior
> are unchanged (back-compat).

## Problem

`wizard_install()` asks the server operator six questions (role, deploy
form, version, bin-dir, advertised-endpoint, data-dir,
operator-http-listen). Most have a sensible default the user should not
have to type, and the prompts demand knowledge (data-dir,
operator-http-listen, version pin) that a first-time user does not have.
The `blank = auto` advertised-endpoint resolves to loopback when the
server binds `0.0.0.0`, which is a silent footgun for remote installs.

## Goal

Reduce the wizard to the few decisions that genuinely need a human,
apply reasonable defaults for everything else, and list every effective
value in a summary block before the final confirm.

## Decisions (locked)

| Axis | Decision |
|------|----------|
| Strategy | Minimal prompts + sensible defaults + end-of-wizard summary; advanced overrides via CLI flags only (not surfaced in the wizard) |
| Wizard prompts | role; deploy form; (server only) advertised endpoint |
| advertised default | Probe public IP (external request) and pre-fill `<ip>:7443` |
| Deploy form | Stays a prompt (binary+systemd default, docker = option 2) |
| Scope | Interactive wizard/menu only; non-interactive path untouched |

## 1. Minimal wizard flow

`wizard_install()` prompts, in order:

1. **Role** — `[1] server [2] client` (default `1`, Enter accepts).
2. **Deploy form** — `[1] binary+systemd [2] docker` (default `1`,
   Enter accepts). Binary path sets `WANT_SYSTEMD=yes` as today.
3. **Advertised endpoint** — *server role only*. Prompt shows the
   detected default: `Public advertised endpoint [<detected>:7443]: `.
   - Enter ⇒ accept the detected value.
   - `host:port` ⇒ override (validated by the existing
     `valid_host_port`; re-ask on failure).
   - `-` ⇒ explicitly leave unset (runtime loopback); summary marks it
     `(loopback — local only)`.

`client` role asks only 1) and 2). No other prompts. All remaining
values come from defaults (§2). The menu's Status / Service / Config /
Env / Upgrade / Uninstall items are unchanged.

## 2. Defaults and the summary block

Values the wizard no longer asks, with their defaults:

| Item | Default | Applies |
|------|---------|---------|
| version | latest (resolved at run time) | all |
| bin-dir | `/usr/local/bin` | binary |
| systemd unit | enabled | binary |
| data-dir | `/var/lib/portunus` | server |
| operator-http-listen | `127.0.0.1:7080` | server |
| compose-dir | current dir (`$PWD`) | docker |
| language | cached / autodetected value | all |

Before the existing `confirm "$(t confirm_proceed)"`, print a summary
block listing every effective value, annotating non-default or detected
ones with their provenance, e.g.:

```
About to install:
  role:                 server
  deploy:               binary + systemd
  version:              latest (resolved at run time)
  bin dir:              /usr/local/bin
  data dir:             /var/lib/portunus
  operator http:        127.0.0.1:7080
  advertised endpoint:  203.0.113.7:7443  (detected public IP)
Proceed? [Y/n]:
```

Annotations: `(detected public IP)`, `(local NIC)`,
`(loopback — local only)`. The block is rendered by a dedicated
`print_install_summary()` and is i18n-keyed (EN/ZH parity enforced by
the existing key-coverage test).

The dry-run `print_plan` path is unchanged (separate output contract
the test harness asserts on).

## 3. Public-IP detection and fallback

Runs only in the interactive wizard, only for `server` role, only when
`--dry-run` is off (preserves the "dry-run performs zero network"
invariant). Implemented as `detect_public_ip()`:

1. **Public probe** — in order, `curl -fsS --max-time 3` against
   `https://api.ipify.org`, `https://ifconfig.me/ip`,
   `https://icanhazip.com`; take the first response that parses as a
   valid IPv4/IPv6 literal.
2. **Local NIC** (all probes failed) — `ip route get 1.1.1.1` src
   address, else first non-loopback from `hostname -I`.
3. **Loopback** — still nothing ⇒ `127.0.0.1`; summary annotates
   `(loopback — local only)`.

Runs at most once, ~9s worst case (3 × 3s). Failures are silent and
non-fatal (fall through the chain). `PORTUNUS_SKIP_IP_PROBE=1` skips
step 1 entirely (offline / test / CI-like environments) and starts at
step 2.

The detected IP only seeds the **default**; the server's runtime
resolution and the authoritative SAN/grammar check are unchanged.

## 4. Testing

Network-free harness additions (`scripts/install.test.sh`):

- `--detect-ip` seam: prints the resolved default IP and its provenance
  tag; with `PORTUNUS_SKIP_IP_PROBE=1` it must return a NIC or loopback
  value and never hit the network.
- Minimal-wizard drive via `--menu-stdin`: server+binary, server+docker,
  client+binary — assert the wizard asks only the expected prompts and
  the summary block contains every default key.
- `-` input ⇒ advertised unset and summary shows the loopback
  annotation.
- EN/ZH parity for all new i18n keys (existing coverage test).
- `shellcheck -s bash -S warning` clean.

VPS smoke (covers every point):

- server binary: public-IP probe pre-fills default; Enter accepts;
  summary block correct; real install; drop-in/meta reflect defaults.
- server docker: same via compose; compose-dir default = cwd.
- client binary: only role+deploy asked; installs with defaults.
- `-` input ⇒ loopback, documented annotation, install proceeds.
- `PORTUNUS_SKIP_IP_PROBE=1` ⇒ NIC/loopback path, no external call.
- Regression: non-interactive `install … --yes` flag path and
  `--dry-run` output unchanged; `install.test.sh` PASS on the VPS.

## Non-goals (YAGNI)

- No advanced/expert wizard tier; advanced values stay CLI-flag only.
- No change to non-interactive parsing, CLI defaults, or dry-run output.
- No public-IP probe outside the interactive server wizard.
- No new persisted config files beyond the existing language cache.

## Risks

- Public-IP probe latency / unreachable endpoints: bounded by
  `--max-time 3` and the NIC→loopback fallback; `PORTUNUS_SKIP_IP_PROBE`
  provides an escape hatch.
- Multi-NIC / NAT hosts: detected IP may be wrong; the value is only a
  pre-filled default the user can override, and the summary shows its
  provenance so a wrong guess is visible before confirm.
