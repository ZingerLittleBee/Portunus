# Docs Root-Toggle Split Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Split the Portunus docs into three Fumadocs root-toggle sections — Overview, Standalone, Server + Client — with shared forwarding primitives written once as neutral concept pages and once as per-mode usage pages, keeping English and `zh/` in lockstep and 301-redirecting every old URL.

**Architecture:** Reorganize `docs/content/docs/` into three top-level folders, each marked `"root": true` in `meta.json` so Fumadocs renders sidebar tabs automatically (no component wiring needed — the existing `DocsLayout tree={pageTree}` already drives it). Forwarding feature pages are carved into concept (Overview) + usage (per mode). Old URLs redirect via a slug map consulted in the existing `$lang/docs/$` route loader, the same `redirect()` mechanism the repo already uses for `/docs → /en/docs`.

**Tech Stack:** Fumadocs UI 16.8.9 + fumadocs-mdx 15 + fumadocs-core 16, TanStack Start/Router, Vite + Nitro, MDX content, Bun (the `docs/` package has `bun.lock`).

**Spec:** `docs/superpowers/specs/2026-06-17-docs-root-toggle-split-design.md`

**Branch:** Work on `docs/root-toggle-split` (already checked out). Do **not** push.

---

## Conventions used throughout this plan

- **Run docs commands from `docs/`.** Package manager is Bun. Type/link check:
  `bun run types:check` (= `fumadocs-mdx && tsc --noEmit`). Full build +
  prerender + size-limit: `bun run build`. If `bun install` has not run yet in
  this clone, run it once first.
- **Move files with `git mv`** so history is preserved and the working tree stays
  clean for the en/zh parity check.
- **Every page move is done for BOTH languages in the same task** — the en file
  under `content/docs/...` and its mirror under `content/docs/zh/...`. The two
  trees must stay structurally identical (Task: Final parity check enforces it).
- **Writing style (from the spec):** plain, direct language. Short sentences.
  No buzzwords, no marketing fluff, no filler. When relocating existing prose,
  keep it verbatim unless it carries that tone, then tighten. New prose
  (picker, cross-links) follows the same rule.
- **Cross-link snippet** placed at the top of every concept page:

  ```mdx
  > **How to set it up:** [Standalone (TOML)](/en/docs/standalone/forwarding-rules#ANCHOR) · [Server + Client (operator)](/en/docs/server-client/forwarding-rules#ANCHOR)
  ```

  and at the top of every usage section in a `forwarding-rules` page:

  ```mdx
  *How it works: [<Primitive> concept](/en/docs/overview/concepts/<slug>).*
  ```

  (In `zh/` files use the `/zh/docs/...` prefix.)
- **Canonical slug map** (language-agnostic; drives both link rewrites and
  redirects). `__PICKER__` = the Overview landing at `/{lang}/docs`.

  | Old slug | New slug |
  | --- | --- |
  | `getting-started/installation` | `__PICKER__` |
  | `getting-started/architecture` | `overview/architecture` |
  | `getting-started/performance` | `overview/performance` |
  | `getting-started/performance-history` | `overview/performance-history` |
  | `features/tcp-forwarding` | `overview/concepts/tcp-forwarding` |
  | `features/udp-forwarding` | `overview/concepts/udp-forwarding` |
  | `features/port-range` | `overview/concepts/port-range` |
  | `features/dns-targets` | `overview/concepts/dns-targets` |
  | `features/multi-target-failover` | `overview/concepts/multi-target-failover` |
  | `features/proxy-protocol` | `overview/concepts/proxy-protocol` |
  | `features/tls-sni-routing` | `server-client/features/tls-sni-routing` |
  | `features/rate-limiting` | `server-client/features/rate-limiting` |
  | `features/rbac` | `server-client/features/rbac` |
  | `features/web-ui` | `server-client/features/web-ui` |
  | `features/advertised-endpoint` | `server-client/features/advertised-endpoint` |
  | `features/sqlite-storage` | `server-client/features/sqlite-storage` |
  | `configuration/standalone` | `standalone/configuration` |
  | `configuration/server` | `server-client/configuration/server` |
  | `configuration/client` | `server-client/configuration/client` |
  | `cli/installer` | `server-client/cli/installer` |
  | `cli/portunus-server` | `server-client/cli/portunus-server` |
  | `cli/portunus-client` | `server-client/cli/portunus-client` |
  | `cli/walkthrough` | `server-client/cli/walkthrough` |
  | `api/operator-http` | `server-client/api/operator-http` |
  | `deployment/systemd` | `server-client/deployment/systemd` |
  | `deployment/docker` | `server-client/deployment/docker` |
  | `deployment/railway` | `server-client/deployment/railway` |
  | `observability/metrics` | `server-client/observability/metrics` |
  | `observability/audit-log` | `server-client/observability/audit-log` |
  | `observability/logging` | `server-client/observability/logging` |
  | `operations/backup-restore` | `server-client/operations/backup-restore` |
  | `operations/upgrade` | `server-client/operations/upgrade` |
  | `operations/runbook-traffic-quotas` | `server-client/operations/runbook-traffic-quotas` |
  | `operations/troubleshooting` | `server-client/operations/troubleshooting` |

- **Anchor re-points** (links that carried a `#fragment` whose section moved):

  | Old link (with anchor) | New link |
  | --- | --- |
  | `getting-started/installation#standalone-install` | `standalone/installation` |
  | `getting-started/installation#server-install` | `server-client/installation` |
  | `getting-started/installation#client-install` | `server-client/installation#client` |
  | `getting-started/installation#release-binaries` | `server-client/installation#release-binaries` |
  | `configuration/client#enroll` | `server-client/configuration/client#enroll` |
  | `configuration/client#docker` | `server-client/configuration/client#docker` |

  (Plain path moves keep any anchor unchanged — only re-point when the section
  itself relocated to a different page.)

---

## Task 1: Scaffold the three roots and prove the toggle (English only)

De-risk the Fumadocs mechanic before any mass move: create the root folders with
real `meta.json` (en **and** zh), a throwaway page in each, wire both top-level
`meta.json` files, and confirm the sidebar tabs render in order.

Note: a `pages` entry that names a not-yet-existing page is silently excluded by
Fumadocs (it does not error), so the root `meta.json` can carry its final
`pages` order now even though most pages land in later tasks. The toggle proof
only checks that the three tabs render and switch — the per-root sidebar fills in
as pages arrive.

**Files:**
- Create: `docs/content/docs/{overview,standalone,server-client}/meta.json` (en)
- Create: `docs/content/docs/zh/{overview,standalone,server-client}/meta.json` (zh)
- Create: `docs/content/docs/{overview,standalone,server-client}/_tmp.mdx` (en only — removed in Task 8)
- Modify: `docs/content/docs/meta.json` and `docs/content/docs/zh/meta.json`

- [ ] **Step 1: Create the three English root `meta.json` files**

`overview/meta.json`:
```json
{ "root": true, "title": "Overview", "description": "Concepts shared by both deployment modes", "icon": "Compass", "pages": ["architecture", "performance", "performance-history", "concepts"] }
```
`standalone/meta.json`:
```json
{ "root": true, "title": "Standalone", "description": "Single-host TOML forwarder", "icon": "Box", "pages": ["installation", "configuration", "forwarding-rules", "stats", "deployment", "troubleshooting"] }
```
`server-client/meta.json`:
```json
{ "root": true, "title": "Server + Client", "description": "Central control plane with clients", "icon": "Network", "pages": ["installation", "configuration", "forwarding-rules", "features", "cli", "api", "deployment", "observability", "operations"] }
```

- [ ] **Step 2: Create the three zh root `meta.json` files (localized title/description, same keys)**

`zh/overview/meta.json`:
```json
{ "root": true, "title": "概览", "description": "两种部署模式共享的原理", "icon": "Compass", "pages": ["architecture", "performance", "performance-history", "concepts"] }
```
`zh/standalone/meta.json`:
```json
{ "root": true, "title": "独立部署", "description": "单机 TOML 转发", "icon": "Box", "pages": ["installation", "configuration", "forwarding-rules", "stats", "deployment", "troubleshooting"] }
```
`zh/server-client/meta.json`:
```json
{ "root": true, "title": "服务端 + 客户端", "description": "中心控制面 + 客户端", "icon": "Network", "pages": ["installation", "configuration", "forwarding-rules", "features", "cli", "api", "deployment", "observability", "operations"] }
```

- [ ] **Step 3: Add a throwaway English page in each root**

Each `_tmp.mdx` (overview/standalone/server-client) — same shape, change the title:
```mdx
---
title: TMP
---
placeholder
```

- [ ] **Step 4: Rewrite both top-level `meta.json` files to order the tabs**

`docs/content/docs/meta.json`:
```json
{ "title": "Documentation", "pages": ["index", "overview", "standalone", "server-client"] }
```
`docs/content/docs/zh/meta.json`:
```json
{ "title": "文档", "pages": ["index", "overview", "standalone", "server-client"] }
```

- [ ] **Step 5: Run the dev server and verify the toggle**

Run: `cd docs && bun run dev` then open `http://localhost:3000/en/docs`.
Expected: the sidebar shows a root-toggle dropdown listing **Overview**,
**Standalone**, **Server + Client** in that order; selecting one shows only that
root's pages. Confirm the bare `/en/docs` (the index) still renders. If the
toggle looks broken specifically on the bare index page, note it — Task 9 sets
the picker; the fallback (fold picker into `overview/index.mdx` + redirect
`/docs → /docs/overview`) is recorded there.

- [ ] **Step 6: Commit**

```bash
cd docs && git add content/docs/overview content/docs/standalone content/docs/server-client content/docs/meta.json content/docs/zh/overview content/docs/zh/standalone content/docs/zh/server-client content/docs/zh/meta.json
git commit -m "docs(restructure): scaffold overview/standalone/server-client roots"
```

---

## Task 2: Move Server + Client pages that relocate wholesale (en + zh)

The bulk of CS content moves unchanged. Pure `git mv` + inner `meta.json`.

**Files (each line is the same move under `content/docs/` and `content/docs/zh/`):**
- `configuration/server.mdx`, `configuration/client.mdx` → `server-client/configuration/`
- `cli/*` → `server-client/cli/`
- `api/operator-http.mdx` → `server-client/api/`
- `deployment/*` → `server-client/deployment/`
- `observability/*` → `server-client/observability/`
- `operations/{backup-restore,upgrade,runbook-traffic-quotas}.mdx` → `server-client/operations/`
- CS-only features `features/{tls-sni-routing,rate-limiting,rbac,web-ui,advertised-endpoint,sqlite-storage}.mdx` → `server-client/features/`

- [ ] **Step 1: Create the inner folders and move (run for en, then repeat with `zh/` prefix)**

```bash
cd docs/content/docs
mkdir -p server-client/configuration server-client/cli server-client/api server-client/deployment server-client/observability server-client/operations server-client/features
git mv configuration/server.mdx server-client/configuration/server.mdx
git mv configuration/client.mdx server-client/configuration/client.mdx
git mv cli/installer.mdx cli/portunus-server.mdx cli/portunus-client.mdx cli/walkthrough.mdx server-client/cli/
git mv api/operator-http.mdx server-client/api/operator-http.mdx
git mv deployment/systemd.mdx deployment/docker.mdx deployment/railway.mdx server-client/deployment/
git mv observability/metrics.mdx observability/audit-log.mdx observability/logging.mdx server-client/observability/
git mv operations/backup-restore.mdx operations/upgrade.mdx operations/runbook-traffic-quotas.mdx server-client/operations/
git mv features/tls-sni-routing.mdx features/rate-limiting.mdx features/rbac.mdx features/web-ui.mdx features/advertised-endpoint.mdx features/sqlite-storage.mdx server-client/features/
```
Then repeat the exact block prefixed with `zh/` (e.g. `git mv zh/configuration/server.mdx zh/server-client/configuration/server.mdx`, `mkdir -p zh/server-client/...`).

- [ ] **Step 2: Move the inner `meta.json` files and adjust `pages`**

`git mv cli/meta.json server-client/cli/meta.json` (and zh). The CLI meta's
`pages` array is unchanged. Do the same for `api/meta.json`,
`deployment/meta.json`, `observability/meta.json`. For
`server-client/configuration/meta.json`, `server-client/features/meta.json`,
`server-client/operations/meta.json`: create/move and set `pages` to the
remaining members only:
```json
// server-client/configuration/meta.json
{ "title": "Configuration", "icon": "Settings", "pages": ["server", "client"] }
```
```json
// server-client/features/meta.json
{ "title": "Features", "icon": "Sparkles", "pages": ["tls-sni-routing", "rate-limiting", "rbac", "web-ui", "advertised-endpoint", "sqlite-storage"] }
```
```json
// server-client/operations/meta.json
{ "title": "Operations", "icon": "Wrench", "pages": ["backup-restore", "upgrade", "runbook-traffic-quotas", "troubleshooting"] }
```
(`operations/troubleshooting.mdx` is moved in Task 5; listing it now is fine —
Fumadocs ignores names with no file until it exists, and Task 5 lands before the
build gate in Task 11.)

- [ ] **Step 3: Type-check**

Run: `cd docs && bun run types:check`
Expected: passes (moves don't break the MDX source map; broken *links* are fixed
in Task 10, not caught here).

- [ ] **Step 4: Commit**

```bash
cd docs && git add -A content/docs
git commit -m "docs(restructure): move server+client pages under server-client root"
```

---

## Task 3: Move Overview pages and split the 6 concept pages (en + zh)

Architecture + performance move wholesale. The six shared forwarding pages are
carved: concept half → `overview/concepts/`, usage half → held for Task 6.

**Files:**
- `getting-started/{architecture,performance,performance-history}.mdx` → `overview/` (en + zh)
- Create `overview/concepts/meta.json` + 6 concept pages (en + zh)
- Keep the 6 old `features/*.mdx` in place for now (Task 6 extracts their usage, then deletes them)

- [ ] **Step 1: Move architecture + performance (en, then zh)**

```bash
cd docs/content/docs
git mv getting-started/architecture.mdx overview/architecture.mdx
git mv getting-started/performance.mdx overview/performance.mdx
git mv getting-started/performance-history.mdx overview/performance-history.mdx
# repeat with zh/ prefix
```

- [ ] **Step 2: Create `overview/concepts/meta.json` (en + zh)**

```json
{ "title": "Forwarding concepts", "icon": "Workflow", "pages": ["tcp-forwarding", "udp-forwarding", "port-range", "dns-targets", "multi-target-failover", "proxy-protocol"] }
```
(zh title: "转发原理".)

- [ ] **Step 3: Create the 6 concept pages by extraction**

For each, create `overview/concepts/<slug>.mdx` (and the zh mirror from the zh
source). Recipe per page — copy from the matching `features/<slug>.mdx`:

- `tcp-forwarding`: frontmatter (keep `title`, reword `description` to behavior-only) + the intro + `## How it works`. **Drop** `## Push a rule` and everything after.
- `udp-forwarding`: intro + `## How it works`. **Drop** `## Push a UDP rule` and the config-knob table.
- `port-range`: intro + `## How it works`. **Drop** `## Push a range rule` and `## Limits` (the `server.toml` cap is CS usage).
- `dns-targets`: intro + `## How it works` + `## Hot path`. **Drop** `## Push a DNS-target rule`.
- `multi-target-failover`: intro + `## How it works`. **Drop** `## Push a multi-target rule`.
- `proxy-protocol`: keep the PROXY **Why it exists** + **How it works** prose only. **Drop** the per-target enablement examples (→ usage). The **SNI peek-duration histogram** section is CS-only — it is appended to `server-client/features/tls-sni-routing.mdx` in this step (read the full source file first to lift it cleanly).

Add the cross-link snippet (from Conventions) at the top of each concept page,
with `ANCHOR` = the slug's section anchor in the forwarding-rules pages (e.g.
`#tcp-forwarding`, `#udp-forwarding`, `#port-range`, `#dns-targets`,
`#multi-target-failover`, `#proxy-protocol`).

- [ ] **Step 4: Append the SNI peek histogram to tls-sni-routing**

Paste the carved SNI-histogram section from `proxy-protocol.mdx` into
`server-client/features/tls-sni-routing.mdx` (en + zh) under a `## SNI peek
duration histogram` heading near the end.

- [ ] **Step 5: Type-check**

Run: `cd docs && bun run types:check`  Expected: passes.

- [ ] **Step 6: Commit**

```bash
cd docs && git add -A content/docs
git commit -m "docs(restructure): add overview root + neutral forwarding concept pages"
```

---

## Task 4: Carve the standalone config page into the Standalone root (en + zh)

`configuration/standalone.mdx` is one large page covering install, schema, CLI,
signals, examples, stats, observability, ops, client-diff, upgrade. Split it.

**Files (en + zh):**
- Create: `standalone/installation.mdx` — from the standalone page's `## Install` (Docker + one-click installer) sections + the standalone half of `getting-started/installation.mdx`.
- Create: `standalone/configuration.mdx` — `## Config schema`, `## CLI flags`, `## Signals`, `## Example configs`, `## Verifying the installation`, `## Observability`, `## Operational notes`, `## Differences from portunus-client`.
- Create: `standalone/stats.mdx` — the `## Live stats dashboard` section (rename heading to a page title "Live stats").
- Create: `standalone/deployment.mdx` — `## Upgrade and uninstall` + the Docker/systemd operational specifics (capabilities, ulimits, snap caveat) relevant to deploying.
- Create: `standalone/troubleshooting.mdx` — the permission/`os error 13`, snap bind-mount, and `--check` failure notes gathered as a short troubleshooting page.
- Delete: `configuration/standalone.mdx` after extraction (en + zh).

- [ ] **Step 1: Create `standalone/configuration.mdx`**

Move the schema/CLI/signals/examples/verifying/observability/operational-notes/
client-diff sections verbatim. Keep frontmatter `title: Configuration`,
`icon: Settings`. Update the in-page `<Callout>` link that pointed at
`/en/docs/getting-started/installation#standalone-install` → `/en/docs/standalone/installation`.

- [ ] **Step 2: Create `standalone/installation.mdx`, `standalone/stats.mdx`, `standalone/deployment.mdx`, `standalone/troubleshooting.mdx`** per the file list above. Reword the cross-section links to their new homes (e.g. the stats page link `#live-stats-dashboard` → `/en/docs/standalone/stats`).

- [ ] **Step 3: Delete the old standalone config page**

```bash
cd docs/content/docs && git rm configuration/standalone.mdx zh/configuration/standalone.mdx
```

- [ ] **Step 4: Type-check**

Run: `cd docs && bun run types:check`  Expected: passes.

- [ ] **Step 5: Commit**

```bash
cd docs && git add -A content/docs
git commit -m "docs(restructure): split standalone config page into standalone root pages"
```

---

## Task 5: Split operations/troubleshooting by mode (en + zh)

`operations/troubleshooting.mdx` mixes CS and general troubleshooting.

**Files (en + zh):**
- Move CS troubleshooting → `server-client/operations/troubleshooting.mdx`.
- Standalone-relevant items already live in `standalone/troubleshooting.mdx` (Task 4); if any standalone item only exists in the old ops page, copy it there.

- [ ] **Step 1: Read `operations/troubleshooting.mdx`; move it to `server-client/operations/troubleshooting.mdx` (en + zh)**

```bash
cd docs/content/docs
git mv operations/troubleshooting.mdx server-client/operations/troubleshooting.mdx
git mv zh/operations/troubleshooting.mdx zh/server-client/operations/troubleshooting.mdx
```

- [ ] **Step 2: Lift any standalone-only entry into `standalone/troubleshooting.mdx`** (if present). Remove now-empty `operations/`, `configuration/`, `cli/`, `api/`, `deployment/`, `observability/`, `features/`, `getting-started/` dirs (en + zh) once empty: `cd docs/content/docs && find . -type d -empty -delete`.

- [ ] **Step 3: Type-check + commit**

```bash
cd docs && bun run types:check
git add -A content/docs
git commit -m "docs(restructure): move troubleshooting under server-client, drop empty dirs"
```

---

## Task 6: Author the two `forwarding-rules` usage pages, delete old feature pages (en + zh)

Each mode gets one task-oriented page with a section per primitive, linking back
to its concept page.

**Files (en + zh):**
- Create: `standalone/forwarding-rules.mdx`
- Create: `server-client/forwarding-rules.mdx`
- Delete: `features/{tcp-forwarding,udp-forwarding,port-range,dns-targets,multi-target-failover,proxy-protocol}.mdx`
- Delete: `features/meta.json` (the old features folder is now empty)

- [ ] **Step 1: Create `server-client/forwarding-rules.mdx`**

Frontmatter:
```mdx
---
title: Forwarding rules
description: Set up TCP, UDP, port-range, DNS-target, failover, and PROXY-protocol rules from the operator CLI.
icon: ArrowRightLeft
---
```
One `## <Primitive>` section per primitive (anchors: `tcp-forwarding`,
`udp-forwarding`, `port-range`, `dns-targets`, `multi-target-failover`,
`proxy-protocol`). Each section = the cross-link line (Conventions) + the
`push-rule` examples and CS-side config knobs lifted from that feature page's
usage half (the `## Push a … rule` / `## Limits` / config-table content removed
in Task 3). Keep examples verbatim.

- [ ] **Step 2: Create `standalone/forwarding-rules.mdx`**

Frontmatter:
```mdx
---
title: Forwarding rules
description: Express TCP, UDP, port-range, DNS-target, failover, and PROXY-protocol rules in TOML.
icon: ArrowRightLeft
---
```
Same six sections, but the body is the TOML form lifted from the standalone
config page's `### Port ranges`, `### Multi-target failover`, `### PROXY
protocol` subsections plus the basic single TCP/UDP and DNS `[[rule]]` examples
from its `## Config schema` / `## Example configs`. Each section opens with the
concept cross-link.

- [ ] **Step 3: Delete the old shared feature pages**

```bash
cd docs/content/docs
git rm features/tcp-forwarding.mdx features/udp-forwarding.mdx features/port-range.mdx features/dns-targets.mdx features/multi-target-failover.mdx features/proxy-protocol.mdx features/meta.json
git rm zh/features/tcp-forwarding.mdx zh/features/udp-forwarding.mdx zh/features/port-range.mdx zh/features/dns-targets.mdx zh/features/multi-target-failover.mdx zh/features/proxy-protocol.mdx zh/features/meta.json
find . -type d -empty -delete
```

- [ ] **Step 4: Type-check + commit**

```bash
cd docs && bun run types:check
git add -A content/docs
git commit -m "docs(restructure): add per-mode forwarding-rules pages, remove split feature pages"
```

---

## Task 7: Split the installation page into the two roots (en + zh)

`getting-started/installation.mdx` has standalone + server + client sections.

**Files (en + zh):**
- Create: `server-client/installation.mdx` — `## Server install`, `## Client install`, `## Release binaries`, `## Next steps` (CS parts). Ensure the anchor `client` exists (heading `## Client install` → anchor `client-install`; if Task `Anchor re-points` referenced `#client`, use the real heading anchor `#client-install` instead — verify the generated anchor and make the link match).
- `standalone/installation.mdx` already created in Task 4; merge in any standalone-only prose from the old install page's `## Standalone install` + the "Which one should I install?" intro is reused by the picker (Task 9).
- Delete: `getting-started/installation.mdx` (en + zh).

- [ ] **Step 1: Create `server-client/installation.mdx`** from the CS sections; fix in-page links (e.g. `#standalone-install` → `/en/docs/standalone/installation`).

- [ ] **Step 2: Fold remaining standalone install prose into `standalone/installation.mdx`** (avoid duplication — prefer one canonical set of steps).

- [ ] **Step 3: Delete old install page**

```bash
cd docs/content/docs && git rm getting-started/installation.mdx zh/getting-started/installation.mdx && find . -type d -empty -delete
```

- [ ] **Step 4: Type-check + commit**

```bash
cd docs && bun run types:check
git add -A content/docs
git commit -m "docs(restructure): split installation into standalone and server-client roots"
```

---

## Task 8: Remove the throwaway scaffolding pages

- [ ] **Step 1: Delete the `_tmp.mdx` pages from all three roots (en + zh have none for tmp — tmp was en-only)**

```bash
cd docs/content/docs && git rm overview/_tmp.mdx standalone/_tmp.mdx server-client/_tmp.mdx
```

- [ ] **Step 2: Commit**

```bash
cd docs && git add -A content/docs && git commit -m "docs(restructure): drop scaffolding placeholder pages"
```

---

## Task 9: Rewrite the index as the mode picker (en + zh)

`content/docs/index.mdx` stays at `/{lang}/docs` and becomes the picker.

**Files:** Modify `content/docs/index.mdx` and `content/docs/zh/index.mdx`.

- [ ] **Step 1: Rewrite the index**

Keep frontmatter `title: Portunus`. Body, in plain language: one short paragraph
("Portunus forwards TCP/UDP ports. Run it standalone from a TOML file, or as a
server that pushes rules to clients."), the "Which one should I install?"
comparison table lifted from the old install page, then two routing cards
(Standalone first):
```mdx
<Cards>
  <Card title="Standalone" href="/en/docs/standalone/installation" description="One host, rules from a local TOML file. Most users start here." />
  <Card title="Server + Client" href="/en/docs/server-client/installation" description="Many hosts, central control: RBAC, Web UI, metrics, quotas." />
</Cards>
```
Replace the old feature-highlight card grid links with the new concept/feature
paths (Task 10 will catch any stragglers).

- [ ] **Step 2: Verify the toggle on the index**

Run: `cd docs && bun run dev`, open `/en/docs`. Expected: picker renders, toggle
present. If the toggle has no usable active state on this non-root page and that
looks wrong, apply the fallback: move this content to `overview/index.mdx`, add
`overview` `index` to its `pages`, and add `__PICKER__` redirect target
`overview` instead of `''` in Task 12's map. Record which path was taken.

- [ ] **Step 3: Commit**

```bash
cd docs && git add content/docs/index.mdx content/docs/zh/index.mdx
git commit -m "docs(restructure): turn docs index into the standalone/server-client picker"
```

---

## Task 10: Rewrite every internal docs link (en + zh)

~175 `](/{en,zh}/docs/...)` references point at old slugs. Rewrite per the
canonical slug map + anchor re-points (Conventions).

**Files:** every `.mdx` under `content/docs/` (38 files contained links pre-move).

- [ ] **Step 1: Apply path rewrites**

For each `old → new` row in the slug map, replace `/en/docs/<old>` with
`/en/docs/<new>` and `/zh/docs/<old>` with `/zh/docs/<new>` across all MDX.
Apply the anchor re-points first (they are more specific), then the plain path
rows. Do this with a reviewed script or per-file edits — not a blind global
sed that could partial-match (e.g. `configuration/server` is a prefix of
nothing new, but verify). For `__PICKER__` rows, the link becomes `/en/docs`
(or `/en/docs/overview` if Task 9 took the fallback).

- [ ] **Step 2: Verify no old slugs remain**

Run:
```bash
cd docs/content/docs
grep -rnE '/(en|zh)/docs/(getting-started|features|configuration/(standalone|server|client)|cli|api|deployment|observability|operations)/' . ; echo "exit:$?"
```
Expected: no matches (grep exit 1). Any hit is a stale link — fix it.

- [ ] **Step 3: Audit `src/` for hardcoded docs links**

Run: `cd docs && grep -rnE '/docs/(getting-started|features|configuration/(standalone|server|client)|cli|api|deployment|observability|operations)' src` — fix any hits in `src/components/landing.tsx`, `src/lib/layout.shared.tsx`, and the `llms.txt`/`llms-full.txt` route builders.

- [ ] **Step 4: Commit**

```bash
cd docs && git add -A
git commit -m "docs(restructure): repoint all internal docs links to new paths"
```

---

## Task 11: Build + prerender gate

- [ ] **Step 1: Full build**

Run: `cd docs && bun run build`
Expected: build succeeds; `size-limit` passes (≤ 500 KB gz); the `crawlLinks`
prerender completes without reporting unreachable internal links. If prerender
flags a 404, it is a missed link or a page that did not land — fix and rebuild.

- [ ] **Step 2: Grep prerendered output for old paths (belt-and-suspenders)**

Run: `cd docs && grep -rlE 'docs/(getting-started|/features/|configuration/standalone)' .output/public 2>/dev/null | head` — expected: empty.

- [ ] **Step 3: Commit any fixes**

```bash
cd docs && git add -A && git commit -m "docs(restructure): fix links/pages surfaced by build" --allow-empty
```

---

## Task 12: Add 301 redirects for every old URL

Old URLs must 301 to their new home via the existing TanStack Router redirect
seam in the catch-all docs loader.

**Files:**
- Create: `docs/src/lib/redirects.ts`
- Modify: `docs/src/routes/$lang/docs/$.tsx` (add a `beforeLoad` to the Route — the same seam the repo already uses in `src/routes/docs/$.tsx`)

- [ ] **Step 1: Create the redirect map**

`docs/src/lib/redirects.ts` — export `OLD_TO_NEW: Record<string, string>` keyed
by old page slug (no language prefix, no leading slash, no `#fragment`), value =
new page slug (`''` for the picker at bare `/{lang}/docs`). Keys are exactly the
left column of the canonical slug map; anchored MDX links were already repointed
in Task 10, so the map is page-level only.
```ts
// Old docs slug -> new docs slug. '' means the Overview picker at /{lang}/docs.
export const OLD_TO_NEW: Record<string, string> = {
  'getting-started/installation': '',
  'getting-started/architecture': 'overview/architecture',
  // ...every other row from the canonical slug map...
  'operations/troubleshooting': 'server-client/operations/troubleshooting',
};
```

- [ ] **Step 2: Redirect in the route `beforeLoad`**

In `docs/src/routes/$lang/docs/$.tsx`, add a `beforeLoad` to the `Route` options
(alongside `component`, `loader`, `head`). `beforeLoad` runs before the loader,
has `params` ({ lang, _splat }), and is where the repo already throws redirects:
```ts
import { createFileRoute, Link, notFound, redirect } from '@tanstack/react-router';
import { OLD_TO_NEW } from '@/lib/redirects';
// ...inside createFileRoute('/$lang/docs/$')({ ... })
  beforeLoad: ({ params }) => {
    const oldSlug = params._splat ?? '';
    if (oldSlug in OLD_TO_NEW) {
      throw redirect({
        to: '/$lang/docs/$',
        params: { lang: params.lang, _splat: OLD_TO_NEW[oldSlug] },
        statusCode: 301,
      });
    }
  },
```
(`_splat: ''` resolves to `/{lang}/docs`, the picker. `redirect` is already
imported in sibling routes; add it to this file's import from
`@tanstack/react-router`.)

- [ ] **Step 3: Verify a sample of redirects in dev**

Run: `cd docs && bun run dev`. Visit `/en/docs/getting-started/architecture`
(→ `/en/docs/overview/architecture`), `/en/docs/features/tcp-forwarding`
(→ `/en/docs/overview/concepts/tcp-forwarding`), `/zh/docs/configuration/standalone`
(→ `/zh/docs/standalone/configuration`), `/en/docs/getting-started/installation`
(→ `/en/docs`). Each lands on the new page.

- [ ] **Step 4: Commit**

```bash
cd docs && git add src/lib/redirects.ts 'src/routes/$lang/docs/$.tsx'
git commit -m "docs(restructure): 301-redirect old docs URLs to new paths"
```

---

## Task 13: Final en/zh parity + clean-build verification

- [ ] **Step 1: Structural parity between en and zh**

Run:
```bash
cd docs/content/docs
comm -3 \
  <(find . -name '*.mdx' -not -path './zh/*' | sed 's#^\./##' | sort) \
  <(find ./zh -name '*.mdx' | sed 's#^\./zh/##' | sort)
```
Expected: empty output (every en page has a zh mirror and vice versa). Do the
same for `meta.json` files. Fix any asymmetry.

- [ ] **Step 2: Final build**

Run: `cd docs && bun run build`  Expected: success, no broken-link warnings.

- [ ] **Step 3: Visual smoke test (dev)**

Run: `cd docs && bun run dev`. Confirm in both `/en/docs` and `/zh/docs`:
toggle shows Overview → Standalone → Server + Client; each root's sidebar shows
only its own pages; a concept page links to both usage pages; a usage page links
back to its concept; search returns results from the new tree.

- [ ] **Step 4: Final commit**

```bash
cd docs && git add -A
git commit -m "docs(restructure): finalize root-toggle split, en/zh parity verified" --allow-empty
```

---

## Self-review notes (author)

- **Spec coverage:** 3 roots (Task 1) · concept/usage split (Tasks 3, 6) ·
  CS-only features whole (Task 2) · single forwarding-rules page per mode
  (Task 6) · installation split (Task 7) · deployment/observability/operations
  → CS (Tasks 2, 5) · picker (Task 9) · all internal links (Task 10) ·
  redirects with picker/​concept canonical targets (Task 12) · en/zh parity
  (every move task + Task 13) · writing style (Conventions). Success criteria
  SC-1..SC-6 map to Tasks 1, 9/13, 3+6, 11, 12, 13.
- **Open mechanic:** the bare-`/docs` toggle render is verified in Task 1 Step 4
  and Task 9 Step 2, with a recorded fallback. This is the one item that may
  shift the picker into `overview/index.mdx`; everything downstream (redirect
  `__PICKER__` target) has the fallback value called out.
- **Anchors:** Task 7 Step 1 flags that `#client` vs the real generated anchor
  (`#client-install`) must be reconciled against the actual heading.
