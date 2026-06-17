# Docs restructure: split Standalone vs Server+Client behind a Fumadocs root toggle

- **Date:** 2026-06-17
- **Status:** Approved (design); ready for implementation plan
- **Area:** `docs/` (Fumadocs + TanStack Start documentation site)
- **Scope:** Full refactor — English + `zh/` mirror, all internal links rewritten, 301 redirects for every old URL.

## Problem

The docs (`docs/content/docs/`) are a single flat tree rendered into one
sidebar. That tree mixes two genuinely different audiences and deployment
models:

- **Standalone** — a single-binary, TOML-driven forwarder with no control
  plane (`portunus-standalone`). The memory note records that this is the
  primary install path the docs should lead with.
- **Server + Client** — the control-plane deployment (`portunus-server`
  pushing rules to `portunus-client` edges) with RBAC, Web UI, metrics, audit
  log, rate limiting, etc.

A standalone user has to wade past server/client/RBAC/Prometheus content that
does not apply to them, and vice versa. The data-plane forwarding primitives
(TCP, UDP, port ranges, DNS targets, multi-target failover, PROXY) are shared
by both modes, but the existing feature pages document them only through the
`portunus-server push-rule …` CLI, which is Server+Client-only — so they read
as Server+Client docs even though the underlying behavior is shared.

## Goals

1. Split the docs into mode-specific sections selectable via the **Fumadocs
   root toggle** (the sidebar-top dropdown: `RootToggle` driven by folders
   whose `meta.json` sets `"root": true`).
2. Give the shared data-plane forwarding primitives a **mode-neutral concept
   page** (the "how it works"), and a **per-mode usage page** (TOML for
   standalone, `push-rule`/operator for Server+Client).
3. Keep an **Overview** root that lands new users and routes them to the right
   mode ("which one should I use").
4. Lead with Standalone (it is the primary install path) in toggle order and in
   the picker.
5. Preserve every old URL via a 301 redirect; keep en and `zh/` in lockstep.

## Non-goals

- Not rewriting content for accuracy beyond what the concept/usage split
  requires. Prose is relocated and lightly adapted, not re-researched.
- No change to product behavior, the data plane, or the operator surface.
- No change to the search backend (Orama re-indexes from the new tree
  automatically) or the marketing landing route (`/`), beyond fixing any
  hardcoded docs links it contains.
- Not introducing a docs versioning scheme.

## Decisions (resolved during brainstorming)

- **Three roots:** Overview, Standalone, Server + Client. Toggle order
  Overview → Standalone → Server + Client.
- **Shared forwarding primitives are split** into a neutral concept page
  (Overview) plus a per-mode usage page in each mode root. The six shared
  primitives: TCP, UDP, port-range, DNS targets, multi-target failover, PROXY.
- **Server+Client-only features stay whole** (concept + usage together) inside
  the Server+Client root: TLS-SNI routing, rate limiting/QoS, RBAC, Web UI,
  advertised endpoint, SQLite storage.
- **Per-mode usage granularity:** one `forwarding-rules` page per mode, with a
  section per primitive (the TOML and `push-rule` snippets are compact). Not one
  page per primitive.
- **`installation.mdx` is split** into a standalone-only install page, a
  server+client install page, and a picker that lives on the Overview landing.
- **deployment / observability / operations move wholesale to Server+Client**
  (Prometheus, audit log, SQLite backup, server upgrade are CS-only). Standalone
  gets its own small `deployment` page.
- **Redirect canonical targets:** a feature page that splits redirects to its
  concept page (the stable anchor); a page that splits across modes (install,
  troubleshooting) redirects to the Overview picker.

## Writing style

All prose — new pages, concept extracts, the picker — is written in plain,
direct language. Short sentences. Concrete over abstract. No buzzwords, no
marketing fluff, no filler. When relocating existing prose, keep it as-is unless
it carries that tone, in which case tighten it. Write like a person explaining
to a peer, not like a brochure.

## Target structure

Per language (English shown; `zh/` mirrors it exactly). Each top-level folder is
a root (`meta.json` → `"root": true`, with `title` + lucide `icon`).

```
content/docs/
  index.mdx                         # global /docs landing = mode picker (see note below)

  overview/            (root, "Overview",        icon: Compass)
    meta.json
    architecture.mdx                # from getting-started/architecture.mdx (already covers both modes)
    performance.mdx                 # from getting-started/performance.mdx (standalone vs client compare)
    performance-history.mdx         # from getting-started/performance-history.mdx
    concepts/                       # neutral "how it works", no push-rule/TOML
      meta.json
      tcp-forwarding.mdx
      udp-forwarding.mdx
      port-range.mdx
      dns-targets.mdx
      multi-target-failover.mdx
      proxy-protocol.mdx            # PROXY concept only; SNI-peek-histogram carved to Server+Client

  standalone/          (root, "Standalone",      icon: Box)
    meta.json
    installation.mdx                # standalone section of old getting-started/installation.mdx
    configuration.mdx               # from configuration/standalone.mdx (TOML schema reference)
    forwarding-rules.mdx            # TOML usage for the 6 primitives; each section links to overview/concepts/*
    stats.mdx                       # portunus-standalone stats TUI (extracted from the standalone config page)
    deployment.mdx                  # standalone systemd / docker (extracted from install + config operational notes)
    troubleshooting.mdx             # standalone subset of operations/troubleshooting.mdx

  server-client/       (root, "Server + Client", icon: Network)
    meta.json
    installation.mdx                # server + client sections of old getting-started/installation.mdx
    configuration/
      meta.json
      server.mdx                    # from configuration/server.mdx
      client.mdx                    # from configuration/client.mdx
    forwarding-rules.mdx            # push-rule/operator usage for the 6 primitives; links to overview/concepts/*
    features/
      meta.json
      tls-sni-routing.mdx           # CS-only, kept whole
      rate-limiting.mdx
      rbac.mdx
      web-ui.mdx
      advertised-endpoint.mdx
      sqlite-storage.mdx
    cli/                            # from cli/* unchanged
      meta.json
      portunus-server.mdx
      portunus-client.mdx
      installer.mdx
      walkthrough.mdx
    api/
      meta.json
      operator-http.mdx
    deployment/                     # from deployment/* (CS)
      meta.json
      systemd.mdx
      docker.mdx
      railway.mdx
    observability/                  # from observability/*
      meta.json
      metrics.mdx
      audit-log.mdx
      logging.mdx
    operations/                     # from operations/* (minus standalone troubleshooting)
      meta.json
      backup-restore.mdx
      upgrade.mdx
      runbook-traffic-quotas.mdx
      troubleshooting.mdx
```

**Root toggle mechanic / the bare `/docs` page.** Fumadocs shows the
`RootToggle` from folders marked `"root": true`; the active root is inferred
from the current page's path. The bare `/docs` index (the picker) sits outside
the three roots. During implementation, verify how Fumadocs renders the toggle
on a non-root page: if a non-root `/docs` index renders without a usable toggle
state, fold the picker into `overview/index.mdx`, make Overview the default
root, and add `/docs → /docs/overview` to the redirect set. This is the one
mechanic the plan must confirm against the installed Fumadocs version
(`fumadocs-ui` 16.8.9) rather than assume.

## Content mapping (old → new)

`gs` = `getting-started`. Applies identically under `zh/`.

| Old page | New home | Action |
| --- | --- | --- |
| `index.mdx` | `index.mdx` (picker) | rewrite: mode-picker table + two routing Cards |
| `gs/installation.mdx` | `standalone/installation.mdx` + `server-client/installation.mdx` + picker on landing | split by mode |
| `gs/architecture.mdx` | `overview/architecture.mdx` | move |
| `gs/performance.mdx` | `overview/performance.mdx` | move |
| `gs/performance-history.mdx` | `overview/performance-history.mdx` | move |
| `features/tcp-forwarding.mdx` | `overview/concepts/tcp-forwarding.mdx` (concept) + a TCP section in each `forwarding-rules.mdx` | split concept/usage |
| `features/udp-forwarding.mdx` | `overview/concepts/udp-forwarding.mdx` + per-mode usage sections | split |
| `features/port-range.mdx` | `overview/concepts/port-range.mdx` + per-mode usage sections | split |
| `features/dns-targets.mdx` | `overview/concepts/dns-targets.mdx` + per-mode usage sections | split |
| `features/multi-target-failover.mdx` | `overview/concepts/multi-target-failover.mdx` + per-mode usage sections | split |
| `features/proxy-protocol.mdx` | `overview/concepts/proxy-protocol.mdx` (PROXY) + per-mode usage; SNI-peek histogram → Server+Client (next to TLS-SNI) | split + carve |
| `features/tls-sni-routing.mdx` | `server-client/features/tls-sni-routing.mdx` | move (CS-only) |
| `features/rate-limiting.mdx` | `server-client/features/rate-limiting.mdx` | move |
| `features/rbac.mdx` | `server-client/features/rbac.mdx` | move |
| `features/web-ui.mdx` | `server-client/features/web-ui.mdx` | move |
| `features/advertised-endpoint.mdx` | `server-client/features/advertised-endpoint.mdx` | move |
| `features/sqlite-storage.mdx` | `server-client/features/sqlite-storage.mdx` | move |
| `configuration/standalone.mdx` | `standalone/configuration.mdx` (+ stats split to `standalone/stats.mdx`) | move + carve |
| `configuration/server.mdx` | `server-client/configuration/server.mdx` | move |
| `configuration/client.mdx` | `server-client/configuration/client.mdx` | move |
| `cli/*` | `server-client/cli/*` | move |
| `api/operator-http.mdx` | `server-client/api/operator-http.mdx` | move |
| `deployment/*` | `server-client/deployment/*` (+ new `standalone/deployment.mdx`) | move + new |
| `observability/*` | `server-client/observability/*` | move |
| `operations/backup-restore,upgrade,runbook-traffic-quotas` | `server-client/operations/*` | move |
| `operations/troubleshooting.mdx` | `server-client/operations/troubleshooting.mdx` + `standalone/troubleshooting.mdx` | split by mode |

### New / heavily-rewritten pages (the content work, not pure moves)

1. `index.mdx` — the mode picker. Reuse the "Which one should I install?" table
   that already exists in `gs/installation.mdx` plus two large Cards routing to
   `standalone/` and `server-client/`. Standalone first.
2. `overview/concepts/*.mdx` (×6) — lift the `## How it works` / behavior prose
   from each feature page; strip every `push-rule` command. Mode-agnostic only.
3. `standalone/forwarding-rules.mdx` — TOML expression of each primitive
   (`[[rule]]` blocks: protocol/listen_port/target, range syntax, `targets[]`
   for failover, `proxy_protocol`). Source: `configuration/standalone.mdx` rule
   schema. Each section opens with a link to its `overview/concepts/*` page.
4. `server-client/forwarding-rules.mdx` — the `push-rule`/operator usage lifted
   from the six feature pages (the `## Push a rule` sections), each linking back
   to its concept page.
5. `standalone/installation.mdx` / `server-client/installation.mdx` — the two
   halves of the current install page.
6. `standalone/stats.mdx`, `standalone/deployment.mdx`,
   `standalone/troubleshooting.mdx` — extracted standalone operational content.

## meta.json plan

- Each root folder gets `{ "root": true, "title": …, "icon": … }`. Titles:
  "Overview" / "Standalone" / "Server + Client" (localized in `zh/`: e.g.
  "概览" / "独立部署" / "服务端 + 客户端" — final zh wording confirmed at
  implementation). Icons: `Compass` / `Box` / `Network`.
- Inner folders (`overview/concepts`, `server-client/configuration`,
  `server-client/features`, `server-client/cli`, `server-client/api`,
  `server-client/deployment`, `server-client/observability`,
  `server-client/operations`) keep ordinary `meta.json` with `title` + `pages`.
- The old top-level `content/docs/meta.json` (with its `---Section---`
  separators) is replaced by the per-root structure; if a top-level `meta.json`
  is still needed to order the roots / pin the picker, it lists `index` plus the
  three root folders.

## Internal links + redirects

- **Internal links:** ~175 `](/en/docs/…)` / `](/zh/docs/…)` references across
  ~38 MDX files all point at old paths and must be rewritten to the new paths.
  Also audit `docs/src/` (e.g. `components/landing.tsx`, `lib/layout.shared.tsx`,
  the `llms.txt`/`llms-full.txt` route builders) for any hardcoded docs links.
- **Redirects:** add 301s for every old URL under both `/en/docs/` and
  `/zh/docs/`. Mechanism: Nitro `routeRules` (the site is hosted via the Nitro
  vite plugin; `serve.json` is empty and `crawlLinks` prerender only follows
  live links, so old URLs need explicit rules). If Nitro `routeRules` redirects
  prove awkward under the TanStack Start prerender, fall back to a catch-all
  route that 301s known old slugs. Redirect targets follow the canonical rule:
  - feature split → its `overview/concepts/*` page
  - install / troubleshooting split → the Overview picker (`/{lang}/docs`)
  - everything else → its single new location (table above)

## Risks & mitigations

- **Broken links after the move.** Mitigation: rewrite all internal links in the
  same change; add redirects; after build, grep the prerendered output for any
  remaining `/docs/getting-started|/docs/features|/docs/configuration/standalone`
  style old paths; rely on `crawlLinks` prerender to surface 404s.
- **en/zh drift.** Mitigation: move/split both trees in the same pass; diff the
  two trees' file lists for parity before finishing.
- **Concept/usage split losing nuance.** Some "how it works" prose is entangled
  with CLI examples. Mitigation: keep concept pages behavior-only; when in
  doubt, the usage page may restate a one-line "what it does" before the
  command, but the canonical explanation lives once in the concept page.
- **Root-toggle/bare-`/docs` mechanic.** Verify against Fumadocs 16.8.9 before
  finalizing the picker's home (see note above).

## Success criteria

1. The sidebar shows a root toggle with Overview / Standalone / Server + Client,
   in that order, in both locales.
2. A standalone reader, after picking "Standalone" on the landing, sees only
   standalone-relevant pages in the sidebar; likewise for Server + Client.
3. The six shared primitives each have exactly one concept page (Overview) and a
   usage section in each mode's `forwarding-rules` page, cross-linked.
4. `pnpm build` succeeds (incl. size-limit) and prerender reports no broken
   internal links.
5. Every old `/en/docs/*` and `/zh/docs/*` URL 301-redirects to a valid new page.
6. en and `zh/` file trees are structurally identical.
