# Railway Server Template Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish `portunus-server` as a public Railway template that deploys the prebuilt GHCR distroless image, configured entirely through environment variables, with no Railway-side build and no shell wrapper.

**Architecture:** Single Railway service from `ghcr.io/zingerlittlebee/portunus-server:latest` (Image Auto Updates). HTTP public domain → operator HTTP on `0.0.0.0:7080`; TCP Proxy → gRPC `7443`; volume → `/var/lib/portunus`. The server binary already self-signs its TLS cert (advertised host in SAN) and the CSRF layer has a same-origin fallback, so the only code change is letting two CLI flags read from environment variables and hardening the advertised-endpoint guard against the empty `:` produced before Railway assigns the TCP proxy.

**Tech Stack:** Rust 2024 / clap 4 (server CLI), GitHub Actions release pipeline (GHCR), Railway (image deploy + template), Fumadocs MDX (docs site).

**Source spec:** `docs/superpowers/specs/2026-06-02-railway-server-template-design.md`

---

## Phase 1 — Repository changes (code + docs)

### Task 1: Harden advertised-endpoint guard + add operator-HTTP-listen env override

**Files:**
- Modify: `crates/portunus-server/src/main.rs` (`advertised_seed` at lines 436-441; `Cmd::Serve` arm at lines 448-455; tests module near line 876)

Context: `advertised_seed` already reads `PORTUNUS_ADVERTISED_ENDPOINT` but only filters fully-empty strings — it lets through `:` and `:7443` (what `${{RAILWAY_TCP_PROXY_DOMAIN}}:${{RAILWAY_TCP_PROXY_PORT}}` resolves to before Railway assigns the proxy). `operator_http_listen` has no env override, so the distroless image (no shell to expand `$PORT`/vars) cannot be told to bind `0.0.0.0:7080`. We refactor both into pure, unit-testable helpers.

- [ ] **Step 1: Write the failing tests**

Add to the `#[cfg(test)] mod tests` block in `crates/portunus-server/src/main.rs`:

```rust
#[test]
fn resolve_advertised_rejects_hostless_values() {
    // Flag wins when present.
    assert_eq!(
        resolve_advertised(Some("flag.host:7443".into()), Some("env.host:7443".into())),
        Some("flag.host:7443".into())
    );
    // Env used when no flag.
    assert_eq!(
        resolve_advertised(None, Some("d.example.com:7443".into())),
        Some("d.example.com:7443".into())
    );
    // The Railway "proxy not yet assigned" cases must be dropped.
    assert_eq!(resolve_advertised(None, Some(":".into())), None);
    assert_eq!(resolve_advertised(None, Some(":7443".into())), None);
    assert_eq!(resolve_advertised(None, Some("   ".into())), None);
    assert_eq!(resolve_advertised(None, Some(String::new())), None);
    assert_eq!(resolve_advertised(None, None), None);
    // A bare host (no port) is still valid.
    assert_eq!(
        resolve_advertised(None, Some("bare.host".into())),
        Some("bare.host".into())
    );
}

#[test]
fn resolve_operator_http_listen_prefers_flag_then_env() {
    let flag: SocketAddr = "127.0.0.1:9999".parse().unwrap();
    let env: SocketAddr = "0.0.0.0:7080".parse().unwrap();
    // Flag wins.
    assert_eq!(
        resolve_operator_http_listen(Some(flag), Some("0.0.0.0:7080".into())),
        Ok(Some(flag))
    );
    // Env parsed when no flag.
    assert_eq!(
        resolve_operator_http_listen(None, Some("0.0.0.0:7080".into())),
        Ok(Some(env))
    );
    // Empty / whitespace env → None (fall back to ServeOptions default).
    assert_eq!(resolve_operator_http_listen(None, Some("   ".into())), Ok(None));
    assert_eq!(resolve_operator_http_listen(None, None), Ok(None));
    // Malformed env is a hard error (don't silently bind loopback on Railway).
    assert!(resolve_operator_http_listen(None, Some("not-an-addr".into())).is_err());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib resolve_ -- --nocapture`
Expected: FAIL — `cannot find function resolve_advertised` / `resolve_operator_http_listen`.

- [ ] **Step 3: Write the helpers and wire them in**

Replace `advertised_seed` (lines 436-441) with the pure helper plus a thin env reader, and add the operator-listen resolver. Place these next to `advertised_seed`:

```rust
/// True when `ep` carries a non-empty host portion. Mirrors
/// `serve::advertised_host` so the cert-SAN host and the bundle endpoint
/// never disagree. Guards against the `:` / `:7443` that
/// `${{RAILWAY_TCP_PROXY_DOMAIN}}:${{RAILWAY_TCP_PROXY_PORT}}` resolves to
/// before Railway assigns the TCP proxy.
fn advertised_has_host(ep: &str) -> bool {
    let ep = ep.trim();
    if ep.is_empty() {
        return false;
    }
    let host = ep.rsplit_once(':').map_or(ep, |(h, _)| h);
    !host.trim().is_empty()
}

/// Pure resolution: flag overrides env; host-less values are dropped.
fn resolve_advertised(flag: Option<String>, env_value: Option<String>) -> Option<String> {
    flag.or(env_value).filter(|s| advertised_has_host(s))
}

fn advertised_seed(cli: &Cli) -> Option<String> {
    resolve_advertised(
        cli.advertised_endpoint.clone(),
        std::env::var("PORTUNUS_ADVERTISED_ENDPOINT").ok(),
    )
}

/// Pure resolution for the operator HTTP bind address: explicit `--operator-http-listen`
/// flag wins; otherwise parse `PORTUNUS_OPERATOR_HTTP_LISTEN`. Empty/whitespace env →
/// `None` (caller keeps its default). A malformed env value is a hard error so a Railway
/// deploy fails loudly instead of silently binding loopback and 502-ing.
fn resolve_operator_http_listen(
    flag: Option<SocketAddr>,
    env_value: Option<String>,
) -> Result<Option<SocketAddr>, String> {
    if let Some(addr) = flag {
        return Ok(Some(addr));
    }
    match env_value.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s
            .parse::<SocketAddr>()
            .map(Some)
            .map_err(|e| format!("invalid PORTUNUS_OPERATOR_HTTP_LISTEN '{s}': {e}")),
        None => Ok(None),
    }
}
```

Then update the `Cmd::Serve` arm (lines 448-455) to consult the env override:

```rust
        Cmd::Serve {
            operator_http_listen,
        } => {
            let operator_http_listen = resolve_operator_http_listen(
                operator_http_listen,
                std::env::var("PORTUNUS_OPERATOR_HTTP_LISTEN").ok(),
            )
            .map_err(|msg| {
                eprintln!("error: {msg}");
                2u8
            })?;
            let opts = serve::ServeOptions {
                data_dir: data_dir.clone(),
                advertised_endpoint: seed.clone(),
                operator_http_listen,
            };
```

(Leave the rest of the arm — runtime build, crypto provider install, `serve::run` — unchanged.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib resolve_ -- --nocapture`
Expected: PASS (both tests).

- [ ] **Step 5: Lint + full server lib tests**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo clippy -p portunus-server --lib -- -D warnings && PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib`
Expected: no warnings; all tests pass (the pre-existing `serve_accepts_operator_http_listen_override` still passes because the flag path is unchanged).

- [ ] **Step 6: Commit**

```bash
git add crates/portunus-server/src/main.rs
git commit -m "feat(server): env override for operator-http-listen + harden advertised guard"
```

---

### Task 2: Remove source-build Railway artifacts

**Files:**
- Delete: `deploy/railway/Dockerfile`
- Delete: `deploy/railway/start-server.sh`
- Delete: `railway.json`

Context: the template now deploys the prebuilt GHCR image and is configured in the Railway dashboard, so the source-build Dockerfile, the shell wrapper (its openssl cert-gen is redundant — the binary self-signs), and the `DOCKERFILE`-builder `railway.json` are all dead. CHANGELOG references to them are historical and stay.

- [ ] **Step 1: Confirm nothing else references them**

Run: `grep -rn "deploy/railway/Dockerfile\|start-server.sh\|railway.json" --include="*.yml" --include="*.yaml" --include="Makefile" --include="*.toml" --include="*.sh" --include="*.rs" . | grep -v docs/superpowers`
Expected: no live references (CHANGELOG/docs prose is fine; if a workflow or Makefile target references them, stop and fold its removal into this task).

- [ ] **Step 2: Delete the files**

```bash
git rm deploy/railway/Dockerfile deploy/railway/start-server.sh railway.json
```

- [ ] **Step 3: Commit**

```bash
git commit -m "chore(railway): drop source-build Dockerfile, shell wrapper, and railway.json"
```

---

### Task 3: Add the template maintenance README

**Files:**
- Create: `deploy/railway/README.md`

- [ ] **Step 1: Write the maintenance doc**

```markdown
# Railway template (portunus-server)

This directory documents the **Railway marketplace template** for `portunus-server`.
There is no Dockerfile here: the template deploys the prebuilt multi-arch GHCR image
built by `.github/workflows/release.yml`, configured entirely through environment
variables. Nothing is compiled on Railway.

## Service configuration

| Setting | Value |
|---|---|
| Image | `ghcr.io/zingerlittlebee/portunus-server:latest` (full `ghcr.io` path is required) |
| Image Auto Updates | Enabled, tracking `:latest` |
| Volume mount | `/var/lib/portunus` |
| HTTP domain target port | `7080` (operator HTTP / Web UI) |
| TCP Proxy | Enabled, internal target port `7443` (gRPC control plane) |

## Environment variables

```
PORTUNUS_ADVERTISED_ENDPOINT = ${{RAILWAY_TCP_PROXY_DOMAIN}}:${{RAILWAY_TCP_PROXY_PORT}}
PORTUNUS_OPERATOR_HTTP_LISTEN = 0.0.0.0:7080
```

- `PORTUNUS_ADVERTISED_ENDPOINT` is baked into client bundles and the gRPC cert SAN.
  Railway resolves the `${{ }}` references into a single `host:port` value. The server
  drops a host-less value (`:` / `:7443`) gracefully if the TCP proxy is not yet
  assigned — the correct endpoint is picked up on the next start.
- `PORTUNUS_OPERATOR_HTTP_LISTEN` makes the operator HTTP listener bind `0.0.0.0` so
  Railway's HTTP edge can reach it (the default is loopback-pinned).

The server self-signs its TLS cert (advertised host in SAN, regenerated when the host
changes), and CSRF uses a same-origin fallback, so no `openssl`, no shell wrapper, and
no `operator_http_public_origin` are required.

## First login (operator)

1. Open the service **Deploy Logs** and copy the line
   `Portunus onboarding setup token: <token>`.
2. Visit the HTTP public domain → the Web UI routes to the onboarding page.
3. Paste the setup token, choose a superadmin username + password.

## Connecting a client

In the Web UI (or via CLI), `provision-client` produces a bundle that already embeds the
advertised endpoint (the TCP proxy `host:port`) and the pinned cert fingerprint. Run
`portunus-client --bundle <file>` on any public host; it connects through the TCP proxy.

## Updating the template

Push a new image to `:latest` (cut a release tag, or run the Release workflow via
`workflow_dispatch`). Railway's Image Auto Updates redeploys within the maintenance
window. To make changes to the template's services/variables themselves, edit the
published template in the Railway dashboard.
```

- [ ] **Step 2: Commit**

```bash
git add deploy/railway/README.md
git commit -m "docs(railway): template maintenance README"
```

---

### Task 4: Rewrite the deployment docs page (en + zh) for the image-based template

**Files:**
- Modify: `docs/content/docs/deployment/railway.mdx`
- Modify: `docs/content/docs/zh/deployment/railway.mdx`

Context: both pages currently describe the deleted source-build flow. Rewrite them to the image-based template flow. Read the current files first to preserve frontmatter shape (title/description) and any `<Callout>`/component imports the docs site uses.

- [ ] **Step 1: Read both current pages**

Run: `sed -n '1,60p' docs/content/docs/deployment/railway.mdx`
Note the frontmatter keys and any MDX component imports in use; reuse the same components.

- [ ] **Step 2: Rewrite `docs/content/docs/deployment/railway.mdx`**

Keep the existing frontmatter block (same `title`/`description` keys). Replace the body with content matching `deploy/railway/README.md` from Task 3, adapted to the docs voice:
- Intro: one-click template, deploys the prebuilt GHCR image, nothing built on Railway.
- "What the template provisions": single service, HTTP domain (target 7080) for the Web UI, TCP Proxy (→7443) for the gRPC control plane, volume at `/var/lib/portunus`, Image Auto Updates on `:latest`.
- "Environment variables": the two `PORTUNUS_*` vars with the `${{RAILWAY_TCP_PROXY_*}}` references, and why each is needed.
- "First login": setup token from Deploy Logs → onboarding page → set superadmin password.
- "Connect a client": `provision-client` → `portunus-client --bundle` through the TCP proxy.
- A note on the TCP-proxy-not-yet-assigned race being handled gracefully.

- [ ] **Step 3: Mirror the rewrite into the zh page**

Apply the same structure, translated, to `docs/content/docs/zh/deployment/railway.mdx`, preserving its frontmatter.

- [ ] **Step 4: Commit**

```bash
git add docs/content/docs/deployment/railway.mdx docs/content/docs/zh/deployment/railway.mdx
git commit -m "docs(railway): rewrite deployment page for image-based template"
```

---

### Task 5: README deploy button + post-deploy steps (en + zh)

**Files:**
- Modify: `README.md`
- Modify: `README.zh-CN.md`

Context: add a "Deploy on Railway" button and a short post-deploy section. The template URL is unknown until Phase 2 publishes the template, so use a placeholder constant `RAILWAY_TEMPLATE_URL` and a tracking note; fill it in during Phase 2, Task 8.

- [ ] **Step 1: Add the deploy button near the top badges of `README.md`**

After the existing badge block (the line with the Rust badge, around line 7), add:

```markdown
[![Deploy on Railway](https://railway.com/button.svg)](RAILWAY_TEMPLATE_URL)
<!-- TODO(railway-template): replace RAILWAY_TEMPLATE_URL after the template is published (Phase 2) -->
```

- [ ] **Step 2: Add a "Deploy the control plane on Railway" subsection**

Under an appropriate deployment area of `README.md`, add:

```markdown
### Deploy the control plane on Railway

One-click deploy of `portunus-server` (Web UI + gRPC control plane) from the prebuilt
GHCR image — no build on Railway:

1. Click **Deploy on Railway** above and create the service.
2. Open the service **Deploy Logs** and copy `Portunus onboarding setup token: <token>`.
3. Visit the generated HTTPS domain → onboarding page → paste the token, set a
   superadmin username + password.
4. `provision-client` a bundle and run `portunus-client --bundle <file>` on any public
   host; it connects through the Railway TCP proxy.

See [`deploy/railway/README.md`](deploy/railway/README.md) for the template internals.
```

- [ ] **Step 3: Mirror both additions into `README.zh-CN.md`** (translated, same placeholder URL).

- [ ] **Step 4: Commit**

```bash
git add README.md README.zh-CN.md
git commit -m "docs(readme): add Railway deploy button and post-deploy steps"
```

---

## Phase 2 — Publish image, configure Railway, verify, publish template

> Phase 2 is gated on Phase 1 being merged (or at least the Task 1 code reaching `:latest`):
> the live verification needs a GHCR image that honors `PORTUNUS_OPERATOR_HTTP_LISTEN` and
> `PORTUNUS_ADVERTISED_ENDPOINT`. Phase 2 is performed in the Railway dashboard via the
> browser (the user is logged in to Railway in Chrome) and is not a code-edit task.

### Task 6: Publish an updated `:latest` image

- [ ] **Step 1:** Land Task 1 on `main` (merge the branch), or build/push a one-off
  pre-release image to a test tag for verification.
- [ ] **Step 2:** Trigger `.github/workflows/release.yml` (a `v*` tag, or `workflow_dispatch`)
  so the `docker` job rebuilds and pushes `ghcr.io/zingerlittlebee/portunus-server:latest`
  with the env support.
- [ ] **Step 3:** Confirm the new digest is on GHCR.

### Task 7: Configure the Railway service (Chrome)

- [ ] **Step 1:** New Project → **Deploy from Docker Image** → `ghcr.io/zingerlittlebee/portunus-server:latest`.
- [ ] **Step 2:** Attach a volume at `/var/lib/portunus`.
- [ ] **Step 3:** Enable **TCP Proxy** with internal target port `7443`.
- [ ] **Step 4:** Set the HTTP domain **target port** to `7080`.
- [ ] **Step 5:** Set env vars:
  `PORTUNUS_ADVERTISED_ENDPOINT = ${{RAILWAY_TCP_PROXY_DOMAIN}}:${{RAILWAY_TCP_PROXY_PORT}}`
  and `PORTUNUS_OPERATOR_HTTP_LISTEN = 0.0.0.0:7080`.
- [ ] **Step 6:** Enable **Image Auto Updates** tracking `:latest`.
- [ ] **Step 7 (verify the variable assumption):** after the TCP proxy is enabled, confirm
  `RAILWAY_TCP_PROXY_DOMAIN`/`PORT` are present in the service env and that
  `PORTUNUS_ADVERTISED_ENDPOINT` resolved to a real `host:port` (not `:`). If they are
  empty, redeploy once so the proxy assignment is picked up.

### Task 8: End-to-end verification + publish

- [ ] **Step 1:** Copy the onboarding setup token from Deploy Logs; complete onboarding in the Web UI (set superadmin password).
- [ ] **Step 2:** `provision-client` a bundle; download it.
- [ ] **Step 3:** On the local Mac (a stand-in for a public client), run `portunus-client --bundle <file>` and confirm it connects through the TCP proxy (control-plane session established).
- [ ] **Step 4:** Push a forwarding rule and drive one real packet of traffic end-to-end; confirm it forwards.
- [ ] **Step 5:** Verify volume persistence: redeploy and confirm `state.db`, the cert, and the onboarded superadmin survive.
- [ ] **Step 6:** Project settings → **Create/Publish Template** (captures image, variable prompts, volume, TCP proxy, networking). Fill in icon/description/category and submit to the marketplace.
- [ ] **Step 7:** Copy the generated template URL and replace `RAILWAY_TEMPLATE_URL` in `README.md` + `README.zh-CN.md` (remove the TODO comment); commit `docs(readme): link published Railway template`.

### Task 9 (OPTIONAL): Instant redeploy step in the release workflow

**Files:**
- Modify: `.github/workflows/release.yml` (append a step to the `docker` job)

Context: spec §6.5 lists this as optional. Image Auto Updates already redeploys within
the maintenance window; this only removes the polling delay. **Skip unless the user wants
instant deploys and is willing to add a `RAILWAY_TOKEN` repo secret** (and the service id).

- [ ] **Step 1:** Add `RAILWAY_TOKEN` (and the target service id) as repo secrets.
- [ ] **Step 2:** After the "Build and push server image" step in the `docker` job, add:

```yaml
      - name: Trigger Railway redeploy
        if: ${{ secrets.RAILWAY_TOKEN != '' }}
        env:
          RAILWAY_TOKEN: ${{ secrets.RAILWAY_TOKEN }}
        run: |
          npx -y @railway/cli redeploy --service "${{ secrets.RAILWAY_SERVICE_ID }}" --yes
```

- [ ] **Step 3:** Commit `ci(release): optional Railway redeploy after image push`.

---

## Acceptance criteria (from spec §9)

- From the published template, a one-click deploy needs **no manual file edits**: copy token → web onboard → provision → remote client connects → traffic forwards.
- The service runs the GHCR distroless image directly — no Railway-side build, no shell wrapper.
- Pushing a new `:latest` triggers Railway Image Auto Updates to redeploy.
- Volume persists `state.db`, cert, and the onboarded superadmin across redeploys.
