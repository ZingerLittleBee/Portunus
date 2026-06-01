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
window. To change the template's services/variables themselves, edit the published
template in the Railway dashboard.
