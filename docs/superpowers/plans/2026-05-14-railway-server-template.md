# Railway Server Template Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Railway-ready Portunus server template that serves the Web UI over Railway HTTP and exposes the gRPC control plane through Railway TCP Proxy.

**Architecture:** Keep the existing release Dockerfiles untouched. Add a Railway-specific source-build Dockerfile, a tiny start script that binds operator HTTP to Railway's `$PORT`, and docs that require a persistent volume plus TCP Proxy for port `7443`.

**Tech Stack:** Rust 1.88, Node 20, pnpm 9.12.3, Docker, Railway config-as-code.

---

## Chunk 1: Railway Build And Runtime

**Files:**
- Create: `deploy/railway/Dockerfile`
- Create: `deploy/railway/start-server.sh`
- Create: `railway.json`
- Modify: `.dockerignore`

- [x] Add a source-build Dockerfile that builds `webui/dist/` before `cargo build --release -p portunus-server`.
- [x] Use `tsc -b && vite build` in the Dockerfile instead of `pnpm build`; `pnpm build` includes the CI-only size-limit Chrome check, which fails under arm64 Docker because the downloaded Chrome binary is x86_64.
- [x] Add a runtime start script that writes `/var/lib/portunus/server.toml`, binds HTTP to `0.0.0.0:${PORT}`, and keeps gRPC on `0.0.0.0:7443`.
- [x] Pass `--advertised-endpoint` from `PORTUNUS_ADVERTISED_ENDPOINT` so Web UI-provisioned client bundles point at the Railway TCP Proxy instead of `127.0.0.1:7443`.
- [x] Derive `operator_http_public_origin` from `RAILWAY_PUBLIC_DOMAIN`, with `PORTUNUS_OPERATOR_HTTP_PUBLIC_ORIGIN` as a custom-domain override.
- [x] Run the Railway runtime image as root because Railway volumes are mounted as root and non-root images need extra service configuration.
- [x] Add `railway.json` pointing Railway at `deploy/railway/Dockerfile`.
- [x] Expand `.dockerignore` so Railway source builds receive Cargo, proto, crate, Web UI, and Railway deployment files.

## Chunk 2: Deployment Documentation

**Files:**
- Create: `docs/content/docs/deployment/railway.mdx`
- Create: `docs/content/docs/zh/deployment/railway.mdx`
- Modify: `docs/content/docs/deployment/meta.json`
- Modify: `docs/content/docs/zh/deployment/meta.json`

- [x] Document the required Railway volume at `/var/lib/portunus`.
- [x] Document HTTP/Web UI vs TCP Proxy endpoint separation.
- [x] Document first-run onboarding from logs.
- [x] Add Railway pages to the English and Chinese deployment navigation.

## Chunk 3: Verification

**Commands:**
- [x] `sh -n deploy/railway/start-server.sh`
- [x] `node -e 'JSON.parse(...)'` for `railway.json` and deployment nav metadata.
- [x] `cargo fmt`
- [x] `docker build --target webui-builder -f deploy/railway/Dockerfile -t portunus-railway-webui:verify .`
- [ ] `docker build -f deploy/railway/Dockerfile -t portunus-railway:verify .`

Full Docker build is the meaningful end-to-end check because it proves `.dockerignore`, Web UI build, Rust build, and runtime copy all agree. Local full build was stopped after the 5-minute repository timeout rule while Debian `apt-get update` was still downloading packages; the failure mode was network slowness, not a Dockerfile syntax or application build error.
