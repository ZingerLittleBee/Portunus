# portunus-standalone Documentation & Deployment Gap-Fill — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fill the only remaining user-facing gaps for `portunus-standalone`: production deployment templates (Docker / docker-compose / hardened systemd unit / Kubernetes manifest), `scripts/install.sh` support for `standalone` as a third role alongside `client`/`server`, release CI packaging for the new binary, and matching docs site additions (en + zh).

**Architecture:** No runtime code changes. New files live under `crates/portunus-standalone/contrib/`. `install.sh` is extended by adding role-specific arms to its existing parameterized functions (the single `install_binary`/`install_systemd_unit`/`install_docker`/`lifecycle_*` family). `release.yml` is patched to add `-p portunus-standalone` to its Linux and macOS build matrices and to copy the binary into the existing tarball. Docs additions extend `operations/standalone.mdx` rather than creating new pages.

**Tech Stack:** Bash 4+ (install.sh), GitHub Actions (release.yml), Docker (multi-stage Dockerfile, distroless runtime), Kubernetes YAML manifests, systemd unit files, Fumadocs MDX.

**Spec:** `docs/superpowers/specs/2026-05-26-standalone-docs-and-deploy-design.md`

---

## File Structure

**New files:**
- `crates/portunus-standalone/contrib/README.md` — index
- `crates/portunus-standalone/contrib/portunus.example.toml` — runnable example
- `crates/portunus-standalone/contrib/portunus-standalone.service` — hardened systemd unit
- `crates/portunus-standalone/contrib/Dockerfile` — multi-stage build → distroless
- `crates/portunus-standalone/contrib/docker-compose.yml`
- `crates/portunus-standalone/contrib/k8s/configmap.yaml`
- `crates/portunus-standalone/contrib/k8s/deployment.yaml`
- `crates/portunus-standalone/contrib/k8s/README.md`

**Modified files:**
- `.github/workflows/release.yml` — Linux & macOS build matrices (`-p portunus-standalone`); both `cp` blocks; Docker bin staging (no GHCR push)
- `scripts/install.sh` — i18n keys, role parser, lifecycle dispatch, systemd/docker special-cases, useradd & dir bootstrap, config get/set guard
- `scripts/install.test.sh` — standalone parallel cases
- `docs/content/docs/operations/standalone.mdx` — append "Production deployment" section; rewrite inline systemd block to link contrib/
- `docs/content/docs/zh/operations/standalone.mdx` — mirror EN
- `CHANGELOG.md` — Unreleased entry
- `CLAUDE.md` — crate list, tooling pointers

---

## Task 1: Bootstrap & baseline verification

**Files:**
- Read-only: `scripts/install.sh`, `scripts/install.test.sh`, `.github/workflows/release.yml`, `docs/content/docs/operations/standalone.mdx`, `docs/content/docs/zh/operations/standalone.mdx`, `crates/portunus-standalone/tests/fixtures/valid_full.toml`, `Makefile` (the `standalone` and `standalone-check` targets)

- [ ] **Step 1: Verify standalone builds and tests pass on this branch**

```bash
cargo build -p portunus-standalone --release
cargo test -p portunus-standalone
make standalone-check
```

Expected: All commands exit 0. `make standalone-check` validates every `tests/fixtures/valid_*.toml`.

- [ ] **Step 2: Verify install.test.sh currently passes**

```bash
bash scripts/install.test.sh
```

Expected: exit 0. Capture output for later diff.

- [ ] **Step 3: Confirm release.yml's current artifact layout**

```bash
grep -n "cargo build --release -p portunus" .github/workflows/release.yml
grep -n "cp \"target/" .github/workflows/release.yml
```

Expected: only `portunus-server` and `portunus-client` mentioned (lines around 160/225/172-173/237-238). This confirms standalone is not yet packaged — needed for Task 8.

- [ ] **Step 4: No commit — bootstrap is informational only**

---

## Task 2: `contrib/portunus.example.toml` + `contrib/README.md`

**Files:**
- Create: `crates/portunus-standalone/contrib/README.md`
- Create: `crates/portunus-standalone/contrib/portunus.example.toml`

- [ ] **Step 1: Create the example TOML**

Write `crates/portunus-standalone/contrib/portunus.example.toml`:

```toml
# portunus-standalone — example configuration
#
# Replace the `target` values with your real upstream hosts/ports.
# All fields here are documented in:
#   docs/content/docs/operations/standalone.mdx
#
# To validate this file before starting the service:
#   portunus-standalone --check --config /etc/portunus/standalone.toml

[global]
label              = "edge-1"
log_level          = "info"
log_format         = "json"
shutdown_drain_secs = 30

[defaults]
udp_max_flows      = 1024
udp_flow_idle_secs = 60
prefer_ipv6        = false

[[rule]]
name        = "ssh-tunnel"
protocol    = "tcp"
listen_port = 2222
target      = "10.0.0.5:22"

[[rule]]
name         = "web-range"
protocol     = "tcp"
listen_ports = "8000-8009"
target       = "10.0.0.10:8000-8009"

[[rule]]
name        = "ha-https"
protocol    = "tcp"
listen_port = 8443
targets = [
  { host = "primary.internal",   port = 443, priority = 0,  proxy_protocol = "v2" },
  { host = "secondary.internal", port = 443, priority = 10, proxy_protocol = "v2" },
]

[[rule]]
name               = "game-udp"
protocol           = "udp"
listen_port        = 27015
target             = "10.0.0.20:27015"
udp_flow_idle_secs = 120
```

- [ ] **Step 2: Create the contrib README index**

Write `crates/portunus-standalone/contrib/README.md`:

```markdown
# portunus-standalone — Deployment templates

Reference templates for running `portunus-standalone` in production.
None of these files are required by the binary itself; they exist as
copy-pasteable starting points.

| File | Purpose |
| --- | --- |
| `portunus.example.toml`         | Runnable TOML config with TCP/UDP/range/multi-target/PROXY-protocol examples |
| `portunus-standalone.service`   | Hardened systemd unit (CAP_NET_BIND_SERVICE, NoNewPrivileges, ProtectSystem) |
| `Dockerfile`                    | Multi-stage build → distroless runtime image |
| `docker-compose.yml`            | Single-service compose using host networking |
| `k8s/configmap.yaml`            | Kubernetes ConfigMap holding `standalone.toml` |
| `k8s/deployment.yaml`           | Kubernetes Deployment (hostNetwork: true, 1 replica) |
| `k8s/README.md`                 | hostNetwork vs DaemonSet vs NodePort notes |

Full usage and `[global]/[defaults]/[[rule]]` schema reference is in
[`docs/content/docs/operations/standalone.mdx`](../../../docs/content/docs/operations/standalone.mdx).
```

- [ ] **Step 3: Verify files were written**

```bash
ls crates/portunus-standalone/contrib/
```

Expected output includes `README.md` and `portunus.example.toml`.

- [ ] **Step 4: Validate the example config parses**

```bash
cargo run --release -p portunus-standalone -- --check \
  --config crates/portunus-standalone/contrib/portunus.example.toml
```

Expected: prints `ok` and exits 0.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-standalone/contrib/README.md \
        crates/portunus-standalone/contrib/portunus.example.toml
git commit -m "docs(standalone): add contrib index and example portunus.toml"
```

---

## Task 3: `contrib/portunus-standalone.service`

**Files:**
- Create: `crates/portunus-standalone/contrib/portunus-standalone.service`

- [ ] **Step 1: Write the hardened systemd unit**

Write `crates/portunus-standalone/contrib/portunus-standalone.service`:

```ini
[Unit]
Description=Portunus standalone TCP/UDP forwarder
Documentation=https://github.com/ZingerLittleBee/Portunus
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/portunus-standalone --config /etc/portunus/standalone.toml
# Current binary has no SIGHUP reload — SIGHUP triggers graceful shutdown.
# Modifying the config requires `systemctl restart portunus-standalone`.
ExecReload=/bin/kill -TERM $MAINPID
Restart=on-failure
RestartSec=2
StandardOutput=journal
StandardError=journal

# Resource limits — main runtime warns when nofile < 4096.
LimitNOFILE=65535

# User & filesystem hardening
User=portunus
Group=portunus
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
NoNewPrivileges=true
ReadOnlyPaths=/etc/portunus

# Network capabilities — allows binding privileged ports (<1024)
# without running as root. Required for ports such as 22, 53, 80, 443.
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 2: Syntax-check the unit with systemd-analyze (if available)**

```bash
command -v systemd-analyze >/dev/null \
  && systemd-analyze verify crates/portunus-standalone/contrib/portunus-standalone.service \
  || echo "systemd-analyze not available — skipping (OK on macOS)"
```

Expected: either passes with no output or skipped on macOS.

- [ ] **Step 3: Commit**

```bash
git add crates/portunus-standalone/contrib/portunus-standalone.service
git commit -m "docs(standalone): add hardened systemd unit template"
```

---

## Task 4: `contrib/Dockerfile`

**Files:**
- Create: `crates/portunus-standalone/contrib/Dockerfile`

- [ ] **Step 1: Write the multi-stage Dockerfile**

Write `crates/portunus-standalone/contrib/Dockerfile`:

```dockerfile
# syntax=docker/dockerfile:1.7
#
# Reference Dockerfile for portunus-standalone — not the published image.
# Build from the repo root:
#   docker build -t portunus-standalone:dev \
#     -f crates/portunus-standalone/contrib/Dockerfile .
#
# Runtime image is distroless `nonroot` (UID 65532). Binding privileged
# ports (<1024) inside the container requires the host to grant the
# NET_BIND_SERVICE capability — see contrib/docker-compose.yml or pass
# `--cap-add NET_BIND_SERVICE` to `docker run`.

FROM rust:1.88-bookworm AS builder
WORKDIR /usr/src/portunus
# Cache dependencies via the full workspace first; subsequent rebuilds
# only re-link portunus-standalone when its sources change.
COPY . .
RUN cargo build --release -p portunus-standalone \
 && strip target/release/portunus-standalone

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /usr/src/portunus/target/release/portunus-standalone \
     /usr/local/bin/portunus-standalone
USER nonroot
ENTRYPOINT ["/usr/local/bin/portunus-standalone"]
CMD ["--config", "/etc/portunus/standalone.toml"]
```

- [ ] **Step 2: Commit**

```bash
git add crates/portunus-standalone/contrib/Dockerfile
git commit -m "docs(standalone): add multi-stage Dockerfile (distroless runtime)"
```

---

## Task 5: `contrib/docker-compose.yml`

**Files:**
- Create: `crates/portunus-standalone/contrib/docker-compose.yml`

- [ ] **Step 1: Write the compose file**

Write `crates/portunus-standalone/contrib/docker-compose.yml`:

```yaml
# portunus-standalone — reference docker-compose configuration.
#
# Uses host networking by default so port-range and arbitrary-port
# forwarding rules work without per-port `ports:` declarations. On
# Docker Desktop (macOS/Windows), host networking is limited — switch
# to the commented `ports:` block and enumerate the listen ports your
# rules use.

services:
  portunus-standalone:
    build:
      context: ../../..
      dockerfile: crates/portunus-standalone/contrib/Dockerfile
    # Or, when consuming a locally-built tag:
    # image: portunus-standalone:dev
    container_name: portunus-standalone
    restart: unless-stopped
    network_mode: host
    # ── Alternative for Docker Desktop / when host networking is unavailable:
    # network_mode: bridge
    # ports:
    #   - "2222:2222/tcp"
    #   - "8000-8009:8000-8009/tcp"
    #   - "27015:27015/udp"
    volumes:
      - ./portunus.example.toml:/etc/portunus/standalone.toml:ro
    cap_add:
      - NET_BIND_SERVICE
    ulimits:
      nofile:
        soft: 65535
        hard: 65535
    # Logs go to stderr in JSON; collect with Docker's json-file driver
    # or a sidecar log shipper. Adjust `--log-format pretty` while
    # iterating locally.
    # command: ["--config", "/etc/portunus/standalone.toml", "--log-format", "pretty"]
```

- [ ] **Step 2: Commit**

```bash
git add crates/portunus-standalone/contrib/docker-compose.yml
git commit -m "docs(standalone): add docker-compose reference (host networking)"
```

---

## Task 6: `contrib/k8s/` manifests

**Files:**
- Create: `crates/portunus-standalone/contrib/k8s/configmap.yaml`
- Create: `crates/portunus-standalone/contrib/k8s/deployment.yaml`
- Create: `crates/portunus-standalone/contrib/k8s/README.md`

- [ ] **Step 1: Write the ConfigMap**

Write `crates/portunus-standalone/contrib/k8s/configmap.yaml`:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: portunus-standalone-config
  namespace: default
data:
  standalone.toml: |
    [global]
    log_level  = "info"
    log_format = "json"

    [[rule]]
    name        = "ssh-tunnel"
    protocol    = "tcp"
    listen_port = 2222
    target      = "10.0.0.5:22"
```

- [ ] **Step 2: Write the Deployment**

Write `crates/portunus-standalone/contrib/k8s/deployment.yaml`:

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: portunus-standalone
  namespace: default
  labels:
    app: portunus-standalone
spec:
  # hostNetwork pins this Pod to a single node's port space. Replicas > 1
  # would collide on the same listen ports — keep this at 1, and use a
  # DaemonSet (see contrib/k8s/README.md) if you need every node to forward.
  replicas: 1
  strategy:
    type: Recreate
  selector:
    matchLabels:
      app: portunus-standalone
  template:
    metadata:
      labels:
        app: portunus-standalone
    spec:
      hostNetwork: true
      dnsPolicy: ClusterFirstWithHostNet
      containers:
        - name: portunus-standalone
          # Replace with your own registry — no GHCR image is published
          # for portunus-standalone yet.
          image: portunus-standalone:dev
          imagePullPolicy: IfNotPresent
          args:
            - "--config"
            - "/etc/portunus/standalone.toml"
          securityContext:
            allowPrivilegeEscalation: false
            readOnlyRootFilesystem: true
            runAsNonRoot: true
            capabilities:
              drop: ["ALL"]
              add: ["NET_BIND_SERVICE"]
          resources:
            requests:
              cpu: "100m"
              memory: "64Mi"
            limits:
              cpu: "500m"
              memory: "256Mi"
          volumeMounts:
            - name: config
              mountPath: /etc/portunus
              readOnly: true
      volumes:
        - name: config
          configMap:
            name: portunus-standalone-config
            items:
              - key: standalone.toml
                path: standalone.toml
```

- [ ] **Step 3: Write the K8s README**

Write `crates/portunus-standalone/contrib/k8s/README.md`:

```markdown
# portunus-standalone on Kubernetes

These manifests run `portunus-standalone` as a single-replica Deployment
with `hostNetwork: true`. This is intentional — `portunus-standalone`
forwards arbitrary TCP/UDP ports (including ranges), which the
ClusterIP / NodePort / LoadBalancer service abstractions cannot express.

## Topology choices

| Topology | When to use |
| --- | --- |
| Deployment, replicas=1 (this template) | One forwarder pinned to a chosen node. Most common. |
| DaemonSet                              | One forwarder per node — fan out from each edge node. Change `kind: Deployment` → `kind: DaemonSet` and remove `replicas`/`strategy`. |
| Multiple replicas with bridge networking | Not supported — Service abstractions cannot route arbitrary listen ports. |

## Why `hostNetwork: true`

The Pod must bind on the node's actual network interfaces; otherwise
upstream targets cannot reach the listener and source-address-based
flow tracking (UDP) is broken by NAT.

**Cluster restrictions**: many production clusters reject `hostNetwork`
via PodSecurityPolicy / Pod Security Admission. Coordinate with your
platform team before applying.

## Apply

```sh
kubectl apply -f configmap.yaml
kubectl apply -f deployment.yaml
kubectl logs -l app=portunus-standalone --tail=50
```

## Updating the config

Editing the ConfigMap does **not** trigger a Pod restart — the standalone
binary reads the file once at startup. After `kubectl apply -f
configmap.yaml`, also run:

```sh
kubectl rollout restart deployment/portunus-standalone
```
```

- [ ] **Step 4: Dry-run validate the manifests**

```bash
command -v kubectl >/dev/null \
  && kubectl apply --dry-run=client -f crates/portunus-standalone/contrib/k8s/configmap.yaml \
                                    -f crates/portunus-standalone/contrib/k8s/deployment.yaml \
  || echo "kubectl not available — skipping"
```

Expected: both objects report `created (dry run)`, or skipped.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-standalone/contrib/k8s/
git commit -m "docs(standalone): add Kubernetes manifest (hostNetwork Deployment)"
```

---

## Task 7: Update `operations/standalone.mdx` (EN)

**Files:**
- Modify: `docs/content/docs/operations/standalone.mdx`

- [ ] **Step 1: Replace the existing `## systemd unit example` section with a pointer**

Find the existing `## systemd unit example` heading (use `grep -n "## systemd unit example" docs/content/docs/operations/standalone.mdx` to locate). Replace from that heading through the closing triple-backtick (the inline ini block) with the new "Production deployment" section below.

New text to insert at the **end of the file** (replacing the systemd-only block):

````markdown
## Production deployment

Reference templates for all four deployment styles live in
[`crates/portunus-standalone/contrib/`](https://github.com/ZingerLittleBee/Portunus/tree/main/crates/portunus-standalone/contrib).

### Installer (binary + systemd) — recommended

The lifecycle installer (`scripts/install.sh`) treats `standalone` as a
first-class role alongside `client` and `server`:

```sh
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- standalone
```

The installer downloads the release tarball, installs the binary to
`/usr/local/bin/portunus-standalone`, creates the `portunus` system
user/group, writes a hardened unit file to
`/etc/systemd/system/portunus-standalone.service` (sourced from
[`contrib/portunus-standalone.service`](https://github.com/ZingerLittleBee/Portunus/blob/main/crates/portunus-standalone/contrib/portunus-standalone.service)),
and leaves a starter config at `/etc/portunus/standalone.toml`. Edit
that file, then `systemctl start portunus-standalone`.

To uninstall: `bash install.sh uninstall standalone` (purges the unit
and binary; pass `--purge` to also delete `/etc/portunus`).

### Docker / docker-compose

The reference [`Dockerfile`](https://github.com/ZingerLittleBee/Portunus/blob/main/crates/portunus-standalone/contrib/Dockerfile)
is a two-stage build that produces a distroless runtime image.
[`docker-compose.yml`](https://github.com/ZingerLittleBee/Portunus/blob/main/crates/portunus-standalone/contrib/docker-compose.yml)
defaults to host networking so port ranges work without per-port
mapping:

```sh
cd crates/portunus-standalone/contrib
docker compose up -d
docker compose logs -f
```

No GHCR image is published yet — build locally or push to your own
registry.

### Kubernetes

The manifests in
[`contrib/k8s/`](https://github.com/ZingerLittleBee/Portunus/tree/main/crates/portunus-standalone/contrib/k8s)
run a single-replica `Deployment` with `hostNetwork: true` (required
for arbitrary-port forwarding):

```sh
kubectl apply -f crates/portunus-standalone/contrib/k8s/configmap.yaml
kubectl apply -f crates/portunus-standalone/contrib/k8s/deployment.yaml
```

See [`contrib/k8s/README.md`](https://github.com/ZingerLittleBee/Portunus/blob/main/crates/portunus-standalone/contrib/k8s/README.md)
for the Deployment-vs-DaemonSet decision and PodSecurityPolicy caveats.

### Hardened systemd unit

The unit file shipped in
[`contrib/portunus-standalone.service`](https://github.com/ZingerLittleBee/Portunus/blob/main/crates/portunus-standalone/contrib/portunus-standalone.service)
sets `LimitNOFILE=65535`, runs as `User=portunus`, adds
`AmbientCapabilities=CAP_NET_BIND_SERVICE` for binding privileged
ports, and applies `ProtectSystem=strict` / `PrivateTmp=true` /
`NoNewPrivileges=true`. The binary does not implement SIGHUP reload —
edit the TOML, then `systemctl restart portunus-standalone`.
````

- [ ] **Step 2: Verify file looks coherent**

```bash
grep -c "## " docs/content/docs/operations/standalone.mdx
```

Expected: 9 or 10 second-level headings (Quick start, Config file lookup, Config schema, CLI flags, Signals, Observability, Full example config, Differences from portunus-client, Production deployment — plus any subsections that use `## `).

- [ ] **Step 3: Commit**

```bash
git add docs/content/docs/operations/standalone.mdx
git commit -m "docs(standalone): add Production deployment section linking contrib/ (EN)"
```

---

## Task 8: Update `zh/operations/standalone.mdx` (ZH)

**Files:**
- Modify: `docs/content/docs/zh/operations/standalone.mdx`

- [ ] **Step 1: Mirror the Task 7 changes in Chinese**

Locate the existing `## systemd unit example`-equivalent heading in the zh file (likely `## systemd unit 示例`). Replace from that heading to file end with the Chinese counterpart of the Production deployment section:

````markdown
## 生产环境部署

四种部署方式的参考模板均位于
[`crates/portunus-standalone/contrib/`](https://github.com/ZingerLittleBee/Portunus/tree/main/crates/portunus-standalone/contrib)。

### Installer（二进制 + systemd，推荐）

生命周期安装器（`scripts/install.sh`）把 `standalone` 当作与 `client`
和 `server` 平级的 role：

```sh
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- standalone
```

安装器会下载 release tarball、将二进制装到 `/usr/local/bin/portunus-standalone`、
创建 `portunus` 系统用户/组、把加固版 unit 文件写到
`/etc/systemd/system/portunus-standalone.service`（源文件位于
[`contrib/portunus-standalone.service`](https://github.com/ZingerLittleBee/Portunus/blob/main/crates/portunus-standalone/contrib/portunus-standalone.service)），
并在 `/etc/portunus/standalone.toml` 放一份起步配置。编辑该文件后执行
`systemctl start portunus-standalone` 即可启动。

卸载：`bash install.sh uninstall standalone`（清理 unit 与二进制；加
`--purge` 一并删除 `/etc/portunus`）。

### Docker / docker-compose

参考
[`Dockerfile`](https://github.com/ZingerLittleBee/Portunus/blob/main/crates/portunus-standalone/contrib/Dockerfile)
是一个两阶段构建，运行时镜像基于 distroless。
[`docker-compose.yml`](https://github.com/ZingerLittleBee/Portunus/blob/main/crates/portunus-standalone/contrib/docker-compose.yml)
默认使用 host 网络，端口范围转发因此无需逐端口 mapping：

```sh
cd crates/portunus-standalone/contrib
docker compose up -d
docker compose logs -f
```

目前未发布 GHCR 镜像 —— 请本地构建或推送到自有 registry。

### Kubernetes

[`contrib/k8s/`](https://github.com/ZingerLittleBee/Portunus/tree/main/crates/portunus-standalone/contrib/k8s)
中的 manifest 运行一个单副本 `Deployment`，启用 `hostNetwork: true`
（任意端口转发的硬性要求）：

```sh
kubectl apply -f crates/portunus-standalone/contrib/k8s/configmap.yaml
kubectl apply -f crates/portunus-standalone/contrib/k8s/deployment.yaml
```

Deployment 与 DaemonSet 的取舍以及 PodSecurityPolicy 注意事项见
[`contrib/k8s/README.md`](https://github.com/ZingerLittleBee/Portunus/blob/main/crates/portunus-standalone/contrib/k8s/README.md)。

### 加固版 systemd unit

[`contrib/portunus-standalone.service`](https://github.com/ZingerLittleBee/Portunus/blob/main/crates/portunus-standalone/contrib/portunus-standalone.service)
设置了 `LimitNOFILE=65535`、以 `User=portunus` 运行、新增
`AmbientCapabilities=CAP_NET_BIND_SERVICE` 以便绑定特权端口，并启用
`ProtectSystem=strict` / `PrivateTmp=true` / `NoNewPrivileges=true`。
二进制不支持 SIGHUP 热加载 —— 修改 TOML 后请执行
`systemctl restart portunus-standalone`。
````

- [ ] **Step 2: Commit**

```bash
git add docs/content/docs/zh/operations/standalone.mdx
git commit -m "docs(standalone): add 生产环境部署 section linking contrib/ (ZH)"
```

---

## Task 9: `release.yml` — package portunus-standalone

**Files:**
- Modify: `.github/workflows/release.yml` (Linux matrix ~L160-180, macOS matrix ~L225-246, Docker bin staging ~L273-280)

- [ ] **Step 1: Patch the Linux build command**

Find:
```yaml
        run: cargo build --release -p portunus-server -p portunus-client --target "${{ matrix.target }}"
```
(Linux block, around L160.)

Replace with:
```yaml
        run: cargo build --release -p portunus-server -p portunus-client -p portunus-standalone --target "${{ matrix.target }}"
```

- [ ] **Step 2: Patch the Linux staging cp block**

Find:
```yaml
          cp "target/${target}/release/portunus-server" "${staging}/"
          cp "target/${target}/release/portunus-client" "${staging}/"
```
(Linux block, around L172-173.)

Replace with:
```yaml
          cp "target/${target}/release/portunus-server" "${staging}/"
          cp "target/${target}/release/portunus-client" "${staging}/"
          cp "target/${target}/release/portunus-standalone" "${staging}/"
```

- [ ] **Step 3: Patch the macOS build command**

Same change as Step 1 but in the macOS block (around L225):
```yaml
        run: cargo build --release -p portunus-server -p portunus-client -p portunus-standalone --target "${{ matrix.target }}"
```

- [ ] **Step 4: Patch the macOS staging cp block**

Same change as Step 2 but in the macOS block (around L237-238):
```yaml
          cp "target/${target}/release/portunus-server" "${staging}/"
          cp "target/${target}/release/portunus-client" "${staging}/"
          cp "target/${target}/release/portunus-standalone" "${staging}/"
```

- [ ] **Step 5: Leave the Docker bin staging block (L273-280) unchanged**

We are **not** publishing a GHCR image for portunus-standalone — spec §2
excludes that. The Docker job continues to only stage server/client.

- [ ] **Step 6: Sanity check the YAML**

```bash
grep -n "portunus-standalone" .github/workflows/release.yml
```

Expected: 4 hits (2 cargo build lines + 2 cp lines). No hits in the Docker job (L260-330 area).

```bash
if command -v yq >/dev/null; then
  yq '.' .github/workflows/release.yml >/dev/null && echo "yaml ok"
elif command -v python3 >/dev/null && python3 -c "import yaml" 2>/dev/null; then
  python3 -c "import yaml; yaml.safe_load(open('.github/workflows/release.yml'))" && echo "yaml ok"
else
  echo "no YAML validator available — skipping (CI will catch syntax errors)"
fi
```

Expected: `yaml ok` or the explicit skip message.

- [ ] **Step 7: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci(release): package portunus-standalone in linux/macos tarballs"
```

---

## Task 10: `install.sh` — add standalone i18n strings

**Files:**
- Modify: `scripts/install.sh` (MSG_EN around L88-150, MSG_ZH around L155-220)

- [ ] **Step 1: Update `ask_role` for both languages to include standalone**

Find in MSG_EN:
```bash
  [ask_role]="Install which role?\n  [1] server\n  [2] client"
```

Replace with:
```bash
  [ask_role]="Install which role?\n  [1] server\n  [2] client\n  [3] standalone"
```

Find in MSG_ZH:
```bash
  [ask_role]="请选择要安装的角色\n  [1] server（服务端）\n  [2] client（客户端）"
```

Replace with:
```bash
  [ask_role]="请选择要安装的角色\n  [1] server（服务端）\n  [2] client（客户端）\n  [3] standalone（独立转发器）"
```

- [ ] **Step 2: Update `need_role` for both languages**

Find in MSG_EN:
```bash
  [need_role]="role required: client or server"
```

Replace with:
```bash
  [need_role]="role required: client, server, or standalone"
```

Find in MSG_ZH:
```bash
  [need_role]="请指定角色：client 或 server"
```

Replace with:
```bash
  [need_role]="请指定角色：client、server 或 standalone"
```

- [ ] **Step 3: Add the standalone deploy-form prompt key**

In MSG_EN, after the line containing `[ask_deploy_client]=...`, add:
```bash
  [ask_deploy_standalone]="Deploy form? (Enter = recommended)\n  [1] binary + systemd  (recommended)\n  [2] docker compose"
```

In MSG_ZH, after the corresponding `[ask_deploy_client]` line, add:
```bash
  [ask_deploy_standalone]="选择部署方式？（回车 = 推荐）\n  [1] binary + systemd  （推荐）\n  [2] docker compose"
```

- [ ] **Step 4: Add the "config not applicable" message**

In MSG_EN add (anywhere in the MSG_EN block):
```bash
  [config_na_standalone]="config get/set is not applicable for the standalone role — edit /etc/portunus/standalone.toml directly"
```

In MSG_ZH add:
```bash
  [config_na_standalone]="standalone 角色不支持 config get/set —— 请直接编辑 /etc/portunus/standalone.toml"
```

- [ ] **Step 5: Quick sanity test**

```bash
bash -c 'source <(sed -n "/^declare -A MSG_EN/,/^)/p" scripts/install.sh); echo "${MSG_EN[ask_deploy_standalone]:?missing}"'
```

Expected: prints the standalone deploy-form prompt without "missing" error.

- [ ] **Step 6: Commit**

```bash
git add scripts/install.sh
git commit -m "install.sh: add i18n strings for standalone role (en + zh)"
```

---

## Task 11: `install.sh` — parse `standalone` role and dispatch

**Files:**
- Modify: `scripts/install.sh` (parse_args around L502-547, ask flow around L580-650)

- [ ] **Step 1: Extend the role parser**

Find (around L505):
```bash
      client|server) ROLE="$1"; [ -z "$VERB" ] && VERB="install" ;;
```

Replace with:
```bash
      client|server|standalone) ROLE="$1"; [ -z "$VERB" ] && VERB="install" ;;
```

- [ ] **Step 2: Extend the `--domain` guard**

Find (around L566):
```bash
  [ -n "$DOMAIN" ] && [ "$ROLE" = client ] && die "--domain is server-only"
```

Replace with:
```bash
  [ -n "$DOMAIN" ] && [ "$ROLE" != server ] && die "--domain is server-only"
```

- [ ] **Step 3: Add the standalone deploy-form ask branch**

Find the block where the script asks `ask_deploy_client` vs `ask_deploy_server`. In `wizard_install()` around L814 (search for `ask_deploy_`). After the `client` branch add a `standalone` branch:

```bash
    standalone)
      reply="$(ask ask_deploy_standalone)"
      case "$reply" in
        2) DEPLOY="docker" ;;
        *) DEPLOY="binary"; WANT_SYSTEMD="yes" ;;
      esac
      ;;
```

Exact line numbers vary — use `grep -n "ask_deploy_client" scripts/install.sh` to find the existing client branch and mirror it.

- [ ] **Step 4: Verify role parsing**

```bash
bash scripts/install.sh standalone --help 2>&1 | head -5 \
  || bash -n scripts/install.sh
```

Expected: either the script reaches its plan-print/help path without "role required" / "unknown command" errors, or `bash -n` reports no syntax errors.

- [ ] **Step 5: Commit**

```bash
git add scripts/install.sh
git commit -m "install.sh: accept 'standalone' as a third role"
```

---

## Task 12: `install.sh` — systemd unit content for standalone

**Files:**
- Modify: `scripts/install.sh` (`install_systemd_unit` around L389-408)

- [ ] **Step 1: Locate the systemd unit assembly**

```bash
sed -n '389,410p' scripts/install.sh
```

You'll see a function that builds a unit per `$ROLE`. It currently has special-casing for `client` vs `server`.

- [ ] **Step 2: Add a standalone branch that sources the contrib unit**

Inside `install_systemd_unit()`, after the existing role branches, add:

```bash
  elif [ "$ROLE" = "standalone" ]; then
    # Use the hardened contrib unit verbatim; user edits live in
    # /etc/portunus/standalone.toml, not in the unit file.
    if [ -r "${SELF_DIR:-}/../crates/portunus-standalone/contrib/portunus-standalone.service" ]; then
      cp "${SELF_DIR}/../crates/portunus-standalone/contrib/portunus-standalone.service" "$tmp/$unit"
    else
      # Network-resolved (curl|bash) invocation: fetch the unit from the repo.
      curl -fsSL "${RAW_BASE}/crates/portunus-standalone/contrib/portunus-standalone.service" -o "$tmp/$unit" \
        || die "failed to fetch portunus-standalone.service"
    fi
  fi
```

(`SELF_DIR` is set by `SELF_SCRIPT` at the top of install.sh — if not, derive it: add at the top of the function `local SELF_DIR; SELF_DIR="$(dirname "${SELF_SCRIPT:-$0}" 2>/dev/null || echo /tmp)"`.)

- [ ] **Step 3: Ensure the bootstrap user/group + config dir exist**

In `install_binary()` (around L364) or wherever the binary is installed, after the binary copy line, add a role-specific block:

```bash
  if [ "$ROLE" = "standalone" ]; then
    # Create the system user (idempotent) and seed the config dir.
    if ! id -u portunus >/dev/null 2>&1; then
      ${SUDO:-} useradd --system --no-create-home --shell /usr/sbin/nologin portunus \
        || die "failed to create portunus user"
    fi
    ${SUDO:-} mkdir -p /etc/portunus
    if [ ! -f /etc/portunus/standalone.toml ]; then
      if [ -r "${SELF_DIR:-}/../crates/portunus-standalone/contrib/portunus.example.toml" ]; then
        ${SUDO:-} cp "${SELF_DIR}/../crates/portunus-standalone/contrib/portunus.example.toml" /etc/portunus/standalone.toml
      else
        ${SUDO:-} curl -fsSL "${RAW_BASE}/crates/portunus-standalone/contrib/portunus.example.toml" -o /etc/portunus/standalone.toml \
          || die "failed to fetch starter standalone.toml"
      fi
      ${SUDO:-} chown root:portunus /etc/portunus/standalone.toml
      ${SUDO:-} chmod 0640 /etc/portunus/standalone.toml
    fi
  fi
```

- [ ] **Step 4: Commit**

```bash
git add scripts/install.sh
git commit -m "install.sh: bootstrap portunus user + ship contrib unit for standalone"
```

---

## Task 13: `install.sh` — docker-compose adaptation for standalone

**Files:**
- Modify: `scripts/install.sh` (`write_compose_file` around L456-486)

- [ ] **Step 1: Locate the compose writer**

```bash
sed -n '456,490p' scripts/install.sh
```

The current writer hard-codes `image: ghcr.io/.../portunus-${ROLE}` and assumes server/client.

- [ ] **Step 2: Special-case standalone**

Inside `write_compose_file()`, before the existing `cat >` heredoc that writes the compose file, add:

```bash
  if [ "$ROLE" = "standalone" ]; then
    # No GHCR image is published for standalone — fetch the reference
    # compose file from contrib/ and the user builds locally.
    if [ -r "${SELF_DIR:-}/../crates/portunus-standalone/contrib/docker-compose.yml" ]; then
      cp "${SELF_DIR}/../crates/portunus-standalone/contrib/docker-compose.yml" "$dir/docker-compose.yml"
    else
      curl -fsSL "${RAW_BASE}/crates/portunus-standalone/contrib/docker-compose.yml" -o "$dir/docker-compose.yml" \
        || die "failed to fetch contrib/docker-compose.yml"
    fi
    if [ ! -f "$dir/portunus.toml" ]; then
      if [ -r "${SELF_DIR:-}/../crates/portunus-standalone/contrib/portunus.example.toml" ]; then
        cp "${SELF_DIR}/../crates/portunus-standalone/contrib/portunus.example.toml" "$dir/portunus.toml"
      else
        curl -fsSL "${RAW_BASE}/crates/portunus-standalone/contrib/portunus.example.toml" -o "$dir/portunus.toml" \
          || die "failed to fetch contrib/portunus.example.toml"
      fi
    fi
    return 0
  fi
```

(The `return 0` short-circuits the server/client compose-writer that follows.)

- [ ] **Step 3: Commit**

```bash
git add scripts/install.sh
git commit -m "install.sh: standalone docker-compose uses contrib templates verbatim"
```

---

## Task 14: `install.sh` — lifecycle (status/service/upgrade) for standalone

**Files:**
- Modify: `scripts/install.sh` (`lifecycle_status` L989, `lifecycle_service` L1002, `lifecycle_upgrade` L1011, `lifecycle_service_restart_quiet` L1022, `dispatch_verb` L935, `validate_config_key` L961)

- [ ] **Step 1: Inspect the lifecycle dispatch**

```bash
sed -n '935,1025p' scripts/install.sh
```

Most lifecycle functions already use `"portunus-${ROLE}"` — they will work for standalone without code changes once role parsing accepts it.

- [ ] **Step 2: Guard `config get`/`config set` for standalone**

Find `validate_config_key()` (L961). Add at the top of `menu_config` (L880) or the `config` dispatch arm in `dispatch_verb`:

```bash
  if [ "$ROLE" = "standalone" ]; then
    echo "$(t config_na_standalone)" >&2
    return 2
  fi
```

Use `grep -n "config)" scripts/install.sh` to find the exact dispatch arm in `dispatch_verb`.

- [ ] **Step 3: Verify all lifecycle paths are reachable for standalone**

```bash
bash -n scripts/install.sh
grep -n "portunus-\${ROLE}" scripts/install.sh
```

Expected: no syntax errors; every `portunus-${ROLE}` line implicitly supports standalone.

- [ ] **Step 4: Commit**

```bash
git add scripts/install.sh
git commit -m "install.sh: guard config get/set for standalone; lifecycle reuses ROLE-parameterized paths"
```

---

## Task 15: `install.test.sh` — standalone cases

**Files:**
- Modify: `scripts/install.test.sh`

- [ ] **Step 1: Inspect existing test structure**

```bash
cat scripts/install.test.sh
```

Identify how existing tests assert on `install.sh install client` / `install server` (dry-run, plan, exit code). Mirror that structure.

- [ ] **Step 2: Add standalone parallel cases**

For each existing block that tests `client` and/or `server`, add a matching `standalone` block. Examples (adapt to match the actual test framework used in the file):

```bash
# ── standalone: role parsing accepts the new role ──────────────────────
output="$(bash scripts/install.sh standalone --help 2>&1 || true)"
case "$output" in
  *"role required"*) fail "standalone role rejected" ;;
esac

# ── standalone: print_plan reports the standalone artifact ─────────────
output="$(bash scripts/install.sh install standalone 2>&1 || true)"
case "$output" in
  *portunus-standalone*) ;;
  *) fail "install plan missed portunus-standalone artifact: $output" ;;
esac

# ── standalone: config get is rejected ─────────────────────────────────
output="$(bash scripts/install.sh config get foo standalone 2>&1 || true)"
case "$output" in
  *"not applicable"*|*"不支持"*) ;;
  *) fail "config get on standalone should be rejected: $output" ;;
esac
```

(Adjust to exact existing patterns — the test script may use a `it` / `assert_contains` helper that you should reuse.)

- [ ] **Step 3: Run install.test.sh and confirm all cases pass**

```bash
bash scripts/install.test.sh
```

Expected: exit 0. If a case fails, fix install.sh — do not relax the test.

- [ ] **Step 4: Commit**

```bash
git add scripts/install.test.sh
git commit -m "install.test.sh: cover standalone role"
```

---

## Task 16: `CHANGELOG.md` Unreleased entry

**Files:**
- Modify: `CHANGELOG.md` (Unreleased section near top)

- [ ] **Step 1: Inspect the existing Unreleased structure**

```bash
sed -n '1,30p' CHANGELOG.md
```

There are already "Unreleased" subsections for v014. Add a new "Documentation" subsection.

- [ ] **Step 2: Insert under `## [Unreleased]`**

Add this block directly after the `## [Unreleased]` heading line (and any blank line that follows), **above** the existing `## Unreleased — UDP runtime correction (014)` block. Do not modify or move the v014 entries:

```markdown
### Documentation & deployment

- **`crates/portunus-standalone/contrib/`** — production templates:
  hardened systemd unit (`AmbientCapabilities=CAP_NET_BIND_SERVICE`,
  `ProtectSystem=strict`, `LimitNOFILE=65535`), multi-stage Dockerfile
  with distroless runtime, host-networking `docker-compose.yml`,
  single-replica `hostNetwork` Kubernetes manifest, and a runnable
  `portunus.example.toml`.
- **`scripts/install.sh`** — accepts `standalone` as a third role
  alongside `client` and `server`. Installs the binary, creates the
  `portunus` system user, seeds `/etc/portunus/standalone.toml` from
  `contrib/portunus.example.toml`, and installs the hardened unit file.
  `config get/set` is not applicable for standalone and exits 2 with
  a descriptive message.
- **`.github/workflows/release.yml`** — Linux and macOS release
  tarballs now include the `portunus-standalone` binary alongside
  `portunus-server` and `portunus-client`. No GHCR image is published
  for standalone yet.
- **`docs/content/docs/operations/standalone.mdx`** (and `zh/`) — new
  "Production deployment" section covering installer, Docker, K8s,
  and the hardened systemd unit, all linking into `contrib/`.
```

- [ ] **Step 3: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs(changelog): record standalone deployment templates + installer support"
```

---

## Task 17: `CLAUDE.md` — refresh crate list & tooling pointers

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Update the "Architecture" section's crate count**

Find:
```markdown
Rust workspace, edition 2024, MSRV 1.88. Six crates under `crates/`:
```

Replace with:
```markdown
Rust workspace, edition 2024, MSRV 1.88. Eight crates under `crates/`:
```

- [ ] **Step 2: Insert the two missing crates in the list**

Find the block listing the six crates (starting with `- \`portunus-proto\``). After the `portunus-core` bullet, insert:

```markdown
- `portunus-forwarder` — shared data-plane library (TCP/UDP forwarders,
  resolver, shutdown). Consumed by both `portunus-client` and
  `portunus-standalone`. No `tonic` / `prost` / `portunus-proto`
  dependencies — proto-free.
```

After the `portunus-client` bullet, insert:

```markdown
- `portunus-standalone` — TOML-driven TCP/UDP forwarder binary with no
  gRPC control plane. Reuses `portunus-forwarder` end-to-end. See
  `crates/portunus-standalone/contrib/` for deployment templates and
  `docs/content/docs/operations/standalone.mdx` for the user guide.
```

- [ ] **Step 3: Add Makefile pointers**

Find the `make help` block in the "Common commands" section. After `make test-csrf` add:

```markdown
make standalone        # build portunus-standalone binary
make standalone-check  # validate every tests/fixtures/valid_*.toml
```

- [ ] **Step 4: Verify**

```bash
grep -n "portunus-forwarder\|portunus-standalone" CLAUDE.md
```

Expected: hits for both new bullets, plus the Makefile lines.

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md
git commit -m "docs(claude.md): reflect 8 crates, add standalone + forwarder pointers"
```

---

## Task 18: Final integration check

**Files:**
- Read-only

- [ ] **Step 1: Full workspace test + clippy**

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all tests pass; no clippy warnings.

- [ ] **Step 2: install.test.sh full pass**

```bash
bash scripts/install.test.sh
```

Expected: exit 0.

- [ ] **Step 3: Standalone smoke + check_mode (specific)**

```bash
cargo test -p portunus-standalone
```

Expected: all tests pass — verifies the contrib additions did not break the crate.

- [ ] **Step 4: Docker build smoke (optional, requires Docker)**

```bash
if command -v docker >/dev/null; then
  docker build -t portunus-standalone:plan-test \
    -f crates/portunus-standalone/contrib/Dockerfile . \
    && docker run --rm portunus-standalone:plan-test --check \
        --config /etc/portunus/standalone.toml 2>&1 \
    | tail -3
fi
```

Expected if Docker is present: build succeeds; `--check` fails with `error: ...` because no config is mounted — that's the correct behavior (proves the binary runs).

- [ ] **Step 5: K8s manifest dry-run (optional, requires kubectl)**

```bash
if command -v kubectl >/dev/null; then
  kubectl apply --dry-run=client \
    -f crates/portunus-standalone/contrib/k8s/configmap.yaml \
    -f crates/portunus-standalone/contrib/k8s/deployment.yaml
fi
```

Expected if kubectl is present: both objects report `created (dry run)`.

- [ ] **Step 6: Confirm the commit history is clean**

```bash
git log --oneline -25
```

Expected: roughly 16 commits added since the plan started (Tasks 2–17 each commit once; Tasks 1 and 18 do not commit). Visually confirm each new commit subject line corresponds to a plan task. If the count looks off, identify the missed task.

- [ ] **Step 7: No final commit (this task is verification only)**

---

## Summary

After all 18 tasks:

- `crates/portunus-standalone/contrib/` contains 7 production-ready files.
- `scripts/install.sh` supports `standalone` as a first-class role.
- `.github/workflows/release.yml` packages the standalone binary on next release.
- Fumadocs site has a "Production deployment" section in both languages.
- `CHANGELOG.md` and `CLAUDE.md` are up to date.
- Zero runtime code changes; `portunus-standalone` and `portunus-forwarder` are untouched.
- The change is push-ready when the user explicitly asks; the spec & plan are committed locally only.
