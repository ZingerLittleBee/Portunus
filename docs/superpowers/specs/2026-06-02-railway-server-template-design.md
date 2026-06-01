# Railway 一键部署模板：Portunus Server

**日期**：2026-06-02
**状态**：设计已确认，待落实施工计划
**目标读者**：维护者 / 实现 agent

## 1. 目标与非目标

### 目标
将 `portunus-server` 发布为 **Railway 模板市场**里的公开一键部署模板（带 "Deploy on Railway" 按钮）。
部署者点一下即可拿到：

- 一个可用的 Web 管理界面（operator HTTP / Web UI）。
- 一个可供**公网远程 `portunus-client`** 连接的 gRPC 控制平面。
- 持久化的状态与自签 TLS 证书。

### 非目标
- 不为 `portunus-client` / `portunus-standalone` 单独做模板。
- 不改动核心转发数据平面、认证模型（仍是 TLS + bearer token，非 mTLS）。
- 不在 Railway 上从源码编译镜像。

## 2. 关键事实（决定设计的约束）

1. **镜像由 GitHub Actions 预构建并推送到 GHCR**：`release.yml` 在 tag `v*` 上编译 musl 静态二进制，`docker` job 把二进制塞进 `deploy/docker/Dockerfile.server`（仅 COPY，不编译），推到
   `ghcr.io/zingerlittlebee/portunus-server:{version,latest}`（多架构 amd64/arm64）。
   模板**直接拉这个镜像**，不在 Railway 编译。
2. **发布镜像基于 `distroless/static-debian12:nonroot`**：无 shell、无 openssl、nonroot 运行，
   `ENTRYPOINT` 为 server 二进制，`CMD ["--data-dir","/var/lib/portunus","serve"]`。
3. **Railway 对镜像服务的启动命令是 exec-form，不展开变量**；distroless 无 `/bin/sh` 可包裹。
   因此 `$VAR` 无法在启动命令里展开 → **配置必须经环境变量进入二进制**。
4. **`${{...}}` 引用变量在"环境变量值"这一层被 Railway 解析**（与启动命令的 exec-form 是两回事）。
5. **server 二进制已能自签 TLS 证书**（`crates/portunus-server/src/tls.rs` `ServerTlsMaterial::load_or_generate`，
   rcgen）：把 advertised host 写进 SAN，advertised host 变化时还会自动重新生成（`.portunus-autocert` 标记）。
   → `deploy/railway/start-server.sh` 里的 openssl 证书生成**是冗余的**。
6. **CSRF 有同源回退**（`crates/portunus-server/src/operator/csrf.rs`）：`expected_origin = None` 时比较
   `Origin` 头 authority 与 `Host` 头（不比 scheme）。Railway HTTPS 直连时两者一致 →
   `operator_http_public_origin` **非硬性必需**，仅为加固项。
7. **首次登录 onboarding 流程前后端齐全**：
   - 后端 `GET /v1/auth/status`（`onboarding_required`）、`POST /v1/auth/onboarding`（`web_auth.rs`）。
   - 前端 `webui/src/auth/OnboardingPage.tsx` + `AuthGate` 自动路由全新库到引导页。
   - server 启动发现无超管时 `eprintln!("Portunus onboarding setup token: <raw>")`（`serve.rs:161`），打到部署日志。
8. **Railway 原生支持 GHCR 镜像 Image Auto Updates**：轮询 registry，`:latest` 同 tag 新 SHA → 自动重部署
   （维护窗口内，先备份卷，带卷服务有 <2 分钟短暂停机）。
9. **`RAILWAY_TCP_PROXY_DOMAIN` / `RAILWAY_TCP_PROXY_PORT`** 是 Railway 系统变量，**仅当该服务启用 TCP Proxy 时才有值**
   （"if applicable"）；未分配时为空 → 存在首次部署竞态。配套有 `RAILWAY_TCP_APPLICATION_PORT`（内部目标端口）。
10. 仓库 `ZingerLittleBee/Portunus` 为 **PUBLIC**，满足模板从公开来源构建/引用的前提。

## 3. 架构：单个 Railway 服务

从 GHCR 镜像部署的**单服务**，同时暴露两个公网入口 + 一个卷：

| 资源 | Railway 机制 | 指向 | 用途 |
|---|---|---|---|
| HTTP 公网域名 | 自动 HTTPS 域名，target port = `7080` | operator HTTP（`0.0.0.0:7080`） | Web UI 管理界面 |
| TCP Proxy（模板预置） | 裸 TCP 透传 | 内部 `7443` gRPC 控制平面 | 公网远程客户端连接 |
| 持久卷 | Volume 挂 `/var/lib/portunus` | — | `state.db` + 自签证书 |

> gRPC 为端到端自签 TLS + 客户端证书 pinning，**必须**走 TCP Proxy（裸 TCP），
> 不能走会终止 TLS 的 HTTP 边缘——否则破坏 pinning，客户端连不上。这是双入口的根本原因。

### 镜像与自动更新
- 服务镜像：`ghcr.io/zingerlittlebee/portunus-server:latest`（写**完整 `ghcr.io` 路径**，否则 Railway 去 Docker Hub 找）。
- 开启 **Image Auto Updates** 跟踪 `:latest`：GitHub Actions 推新镜像后 Railway 自动重部署。
- （可选/拉伸）release workflow 末尾加 `railway redeploy` 实现即时更新，免等轮询。

## 4. 配置注入（Option A）：让二进制读环境变量

### 4.1 server 代码改动（小，通用，非 Railway 耦合）
对以下 CLI 参数增加环境变量兜底（`crates/portunus-server/src/main.rs`）：

- 全局 `--advertised-endpoint` ← `PORTUNUS_ADVERTISED_ENDPOINT`
- serve `--operator-http-listen` ← `PORTUNUS_OPERATOR_HTTP_LISTEN`

实现方式二选一（施工计划定夺）：
- clap `env` feature：`clap = { features = ["derive", "env"] }` + 参数加 `env = "..."`；或
- 手动 `std::env::var(...)` 兜底，免加 feature。

**健壮性要求（必须）**：当 `PORTUNUS_ADVERTISED_ENDPOINT` 为空、纯空白、或仅为 `:`（TCP Proxy 变量尚未分配的首部署竞态）时，
二进制须将其视作"无 advertised endpoint"**优雅跳过**——不报错、不生成残缺证书。待 proxy 分配后下次启动再生成正确证书
（既有 `.portunus-autocert` 重生成逻辑天然支持）。

> 变量名保持 `PORTUNUS_*` 通用语义，不读 `RAILWAY_*`——Railway 专属拼接放在模板的引用变量里（见 4.2）。

### 4.2 模板环境变量（在 Railway 模板/服务里设）
```
PORTUNUS_ADVERTISED_ENDPOINT = ${{RAILWAY_TCP_PROXY_DOMAIN}}:${{RAILWAY_TCP_PROXY_PORT}}
PORTUNUS_OPERATOR_HTTP_LISTEN = 0.0.0.0:7080
```
- 镜像默认 `CMD serve` 不变；上述全局/子命令参数经 env 兜底生效。
- 卷挂载点 `/var/lib/portunus` 与镜像默认 `--data-dir` 一致。

### 4.3 不在范围内（同源回退已够用）
- `operator_http_public_origin`（CSRF）默认**不设**，依赖同源回退。
  - 如后续发现 Railway 反代下同源校验有边界问题，可作为加固再加 `PORTUNUS_OPERATOR_HTTP_PUBLIC_ORIGIN`
    env 支持并设 `https://${{RAILWAY_PUBLIC_DOMAIN}}`——列为可选拉伸项，非首版必需。

## 5. 部署者首次使用流程（全用现成功能）

1. 点 "Deploy on Railway" → Railway 拉 GHCR 镜像起服务（预置卷 + TCP Proxy + env）。
2. 打开 Railway **Deploy Logs**，抄一行 `Portunus onboarding setup token: <token>`。
3. 访问 HTTP 公网域名 → `AuthGate` 检测到 `onboarding_required` → 自动进引导页。
4. 填 setup token + 设超管用户名/密码 → 进入 Web UI。
5. 在 UI/CLI `provision-client` 生成客户端 bundle（已内嵌 advertised endpoint = TCP Proxy 地址 + 钉好的证书指纹）。
6. 公网机器跑 `portunus-client --bundle ...` → 经 TCP Proxy 连上控制平面 → 接收转发规则。

## 6. 仓库侧改动清单

1. **`crates/portunus-server/src/main.rs`**：为 `advertised_endpoint`、`operator_http_listen` 加 env 兜底 + 空值健壮性处理（见 4.1）。
2. **移除源码构建的 Railway 残留**：删 `deploy/railway/Dockerfile`、`deploy/railway/start-server.sh`，
   以及 `railway.json`（DOCKERFILE builder，已不适用于镜像部署）。
3. **新增 `deploy/railway/README.md`**：模板维护文档——服务配置（镜像、HTTP target port 7080、TCP Proxy→7443、卷）、
   env 引用变量、如何更新发布、如何实测 TCP Proxy 变量。
4. **`README.md` / `README.zh-CN.md`**：加 "Deploy on Railway" 按钮（模板 URL）+ 部署后三步说明
   （抄 token → 引导设密码 → provision client）。
5. **（可选）`.github/workflows/release.yml`**：末尾加 `railway redeploy` 步骤实现即时更新。

> server 的核心 Rust 逻辑、webui 不动；改动集中在 CLI 入参与文档/部署资产。

## 7. Railway 后台施工与发布（用已登录的 Chrome）

1. New Project → **Deploy from Docker Image** → `ghcr.io/zingerlittlebee/portunus-server:latest`。
2. 配置服务：
   - 挂卷 `/var/lib/portunus`。
   - 启用 **TCP Proxy**，目标内部端口 `7443`。
   - HTTP 公网域名 target port = `7080`。
   - 设 4.2 的 env 引用变量。
   - 开启 Image Auto Updates 跟踪 `:latest`。
3. **实测**：确认 TCP Proxy 启用后 `RAILWAY_TCP_PROXY_DOMAIN/PORT` 真在该服务环境、`${{}}` 拼接被解析进 `PORTUNUS_ADVERTISED_ENDPOINT`。
4. **端到端验证（发布前必做）**：抄 token → web 引导设密码 → provision 一个 bundle → 在**本地 Mac**（模拟公网客户端）跑 client 连上 → push 一条规则 → 实打一笔流量验证转发通。
5. 验证通过 → 项目设置 **Create/Publish Template**（捕获镜像、变量提示、卷、TCP Proxy、网络）→ 填图标/描述/分类 → 提交市场。
6. 把生成的模板 URL 回填到 README 的 Deploy 按钮。

## 8. 风险与注意点

- **TCP Proxy 变量首部署竞态**：proxy 未分配时 `RAILWAY_TCP_PROXY_*` 为空 → `PORTUNUS_ADVERTISED_ENDPOINT` 拼成 `:`。
  由 4.1 的空值健壮性处理兜住；证书在 proxy 分配后的后续启动自动重生成。
- **Image Auto Updates 短暂停机**：带卷服务重部署有 <2 分钟停机，可接受。
- **完整 `ghcr.io` 路径**：短名会被 Railway 误指向 Docker Hub。
- **CSRF**：依赖同源回退；若 Railway 反代下边界异常，回退到 4.3 的可选 `public_origin` env 加固。
- **验证依赖**：需一台能跑 `portunus-client` 的公网侧机器（本地 Mac 即可）。

## 9. 验收标准

- 从模板一键部署后，**未做任何手动改文件**即可：抄 token → web 引导 → provision → 远程客户端连上 → 转发通。
- 服务镜像为 GHCR distroless 原镜像，无 Railway 端编译、无 shell wrapper。
- GitHub Actions 推新 `:latest` 后，Railway 经 Image Auto Updates 自动重部署。
- 卷持久化：重部署后 `state.db`、证书、已 onboard 的超管仍在。
