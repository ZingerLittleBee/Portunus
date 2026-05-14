# 用户中心化配额管理（User-Centric Quotas）— 设计

- **状态**：草案（待用户审阅）
- **日期**：2026-05-14
- **范围**：仅前端（webui/）。后端 API 不变。
- **依赖基线**：v0.5.0 RBAC（grants）、v0.11.0 rate-limit/QoS（owner caps）

## 1. 背景与问题

当前 UI 把「用户能在哪台 client 上工作」（grant）与「用户在该 client 上的流量/并发限额」（owner cap）拆成两个独立入口：

- **Grants**：`/grants`、`/grants/new`（以授权对象为中心）
- **Owner Quotas**：`/clients/:id` 详情页内的"Owner quotas"区块（以 client 为中心）

操作者的真实工作流是「以用户为中心」——为某个 user 配置「他在哪些 client 上能用、各能用多少」。当前 UI 强迫操作者来回切换两个不同视角的页面，且无法在 UserCreate 时一次性完成初始化。

## 2. 目标

1. 以 **user** 为中心组织配额管理入口（UserCreate 可选、UserDetail 主战场）。
2. 在 UI 层把 grant + cap 合并成一个统一概念「**用户配额**」（User Quota），列名通过端口段/协议/带宽/并发自解释，避免操作者认知二元化。
3. 不增加后端接口，纯前端编排。
4. 现有 ClientDetail 的"Owner quotas"区块改为只读 + 跳转，作为辅助入口；`/grants` 页从导航移除。

### 非目标

- 不修改任何后端 Rust 代码、SQLite schema、wire protocol。
- 不迁移老数据；老数据归一化在编辑保存时按"合并到一条"策略发生。
- 不重构既有 UserCreate.tsx 既存表单（保持受控 state 模式）；新增组件使用 react-hook-form + zod。

## 3. 概念模型

### 3.1 「用户配额」（User Quota）

UI 层引入复合对象：

```
UserQuota = {
  user_id:       string                 // RBAC user
  client_name:   string                 // 目标 client
  port_range:    { start, end }         // grant 的端口段
  protocols:     ("TCP" | "UDP")[]      // grant 的协议
  unlimited:     boolean                // true → 不创建 cap；false → 必须有非空 cap
  cap?: {                               // 当 unlimited=false 时存在
    bandwidth_in_bps?:        number
    bandwidth_out_bps?:       number
    new_connections_per_sec?: number
    concurrent_connections?:  number
    bandwidth_in_burst?:      number
    bandwidth_out_burst?:     number
    new_connections_burst?:   number
  }
}
```

**唯一约束（UI 层）**：每 `(user_id, client_name)` 至多 1 条 UserQuota。

**`unlimited` 字段语义**：在数据加载阶段从后端派生（`cap` 不存在 ⇔ `unlimited = true`）；在表单中作为受控输入，用户切换会驱动 cap 字段显隐与提交时的 API 选择（PUT vs DELETE cap）。

### 3.2 与后端对象的映射

| UserQuota 字段 | 后端来源 |
|---|---|
| `user_id`, `client_name`, `port_range`, `protocols` | `Grant`（`/v1/grants`） |
| `cap.*` | `OwnerRateLimit`（`/v1/clients/{c}/owners/{u}/rate-limit`） |
| `unlimited` | 派生：`cap` 不存在 ⇔ `unlimited = true` |

## 4. UI 结构

### 4.1 路由与页面

```
保留并增强：
  /users/new           UserCreate    + 可选「初始用户配额」区块（0..1 条）
  /users/:id           UserDetail    + 「用户配额」区块（主战场）

修改：
  /clients/:id         ClientDetail  - 原「Owner quotas」改为只读列表；
                                       「编辑/删除」按钮跳 /users/:owner_id

移除导航入口（保留路由并重定向）：
  /grants              → 301 重定向到 /users + toast "已迁移到用户配额"
  /grants/new          → 301 重定向到 /users
```

`Nav.tsx` 删除 Grants 项。

### 4.2 新增组件（按文件）

```
webui/src/components/UserQuota/
  ├── UserQuotaTable.tsx       表格 + 展开行容器
  ├── UserQuotaRow.tsx         单行：collapsed 视图 + 展开后的编辑面板
  ├── UserQuotaForm.tsx        access entry 表单（client + 端口 + 协议 + cap）
  ├── ClientCombobox.tsx       shadcn Command + Popover，搜索式 client 选择
  ├── UnlimitedToggle.tsx      shadcn Switch；切换 cap 表单显隐
  └── useAccessEntries.ts      join grants + caps、CRUD、乐观更新
```

复用 `RateLimitForm.tsx`（既有，4 维 + burst 字段）嵌入 `UserQuotaForm` 的 cap 部分。

### 4.3 表格列设计（collapsed 视图）

| Client | 端口段 | 协议 | 带宽 in | 带宽 out | 并发 | 新建/s | 状态 | 操作 |
|---|---|---|---|---|---|---|---|---|
| edge-tokyo | 20000-29999 | TCP, UDP | 100 MB/s | 100 MB/s | 500 | 50 | 在线 | 展开 / 删除 |
| edge-sg | 30000-39999 | TCP | **Unlimited** | **Unlimited** | 200 | — | 离线 | 展开 / 删除 |

- 带宽显示 human-friendly 单位（MB/s、GB/s）；不展示原始 bps。
- `Unlimited` 用 Badge 高亮。
- "状态"列复用 ClientsList 的在线/离线 Badge。
- burst 列隐藏；仅展开行可见。

展开行内嵌完整 `UserQuotaForm`：端口段输入 + 协议 multi-checkbox + Unlimited Switch + 4 维 cap + burst 折叠区 + Save / Cancel。

**新建条目交互**：表格右上角 "+ 添加" 按钮 → 在表格底部追加一条 collapsed 占位行（带"未保存"badge）并自动展开；client 下拉中已存在 entry 的 client 被禁用。Save 成功后占位行变为正式行；Cancel 移除占位行。

### 4.4 shadcn 组件清单

通过 `shadcn:` skill 走 registry 添加（当前 `webui/src/components/ui/` 已有 badge、button、card、dialog、input、label、scroll-area、separator、skeleton、tabs）：

- `table` — 表格主体
- `command` + `popover` — ClientCombobox
- `collapsible` — 行展开/收起
- `switch` — UnlimitedToggle
- `checkbox` — 协议多选
- `form` — react-hook-form 包装
- `tooltip` — 字段解释（burst 等）
- `alert` — 警告条（老数据多 grant、编辑失败）

### 4.5 表单技术栈

新组件统一使用 **react-hook-form + zod** + shadcn `form` 组件。既有的 UserCreate / GrantCreate 等表单保持受控 state 不重构（避免无关 churn；后续可单独 PR 统一）。

新增依赖：
- `react-hook-form`
- `zod`
- `@hookform/resolvers`

## 5. 数据流与 API 编排

**复用现有 4 个后端接口**，由前端做客户端补偿（方案 A）。

### 5.1 操作映射

| UI 操作 | 后端调用顺序 | 失败回滚 |
|---|---|---|
| 创建 entry | ① `POST /v1/grants` → ② 若 `!unlimited`：`PUT .../rate-limit` | ② 失败时 `DELETE` ①；回滚也失败 → "状态不一致"行级 badge |
| 编辑 entry（端口/协议变更） | `DELETE` 旧 grant + `POST` 新 grant | 失败：尝试恢复旧 grant；不行就 banner |
| 编辑 entry（仅 cap 变更） | `PUT .../rate-limit` 或 `DELETE`（切到 unlimited） | 自然幂等 |
| 删除 entry | ① `DELETE .../rate-limit`（404 忽略）→ ② `DELETE /v1/grants/{id}` | ② 失败 → 行 badge "状态不一致"，提供"重试"按钮 |

### 5.2 列表加载

进入 UserDetail 时并发拉取：

```
GET /v1/grants?user_id={u}                               // user 的全部 grants
GET /v1/clients                                          // client 列表 + 在线状态
对每条 grant 对应的 client：
  GET /v1/clients/{c}/owners/{u}/rate-limit              // 各自的 cap
```

并发数 = grant 数（典型 < 50）。React Query 缓存按 (client, owner) 失效。

### 5.3 客户端补偿失败的展示

- 失败 toast 红条 + "正在回滚..."
- 回滚成功 → toast 转黄 + "已撤销，请重试"
- 回滚失败 → 行级红色感叹号 badge "状态不一致"，展开行顶部显示最后失败的请求与状态码，提供"手动重试"与"手动删除孤儿"按钮

## 6. 边缘情况

### 6.1 老数据归一化

`useAccessEntries` 在 join 时检测：若 `(user, client)` 有 >1 条 grant：

- 选 `port_range` 最宽的一条作为展示主条目，其余记入 `row.legacy_duplicates`
- 表格行右侧加 `alert` badge "存在 N 条历史 grant"
- 展开行顶部 Alert："检测到 N 条额外 grant。保存编辑会合并为 1 条，端口段取并集 / 协议取并集。"

合并仅在保存时执行：`DELETE` 所有旧 grants + `POST` 1 条合并后的新 grant。未编辑的行不动后端任何数据。

### 6.2 表单校验（zod schema 要点）

- `port_range`: `start <= end`，`1..=65535`，且不与同 user 同 client 其他 entry 重叠（前端用已加载列表自查）
- `protocols`: 至少 1 个（TCP 或 UDP）
- 当 `unlimited == false`：4 维 cap 至少 1 个 > 0
- `burst` 字段：仅在对应 `*_per_sec` > 0 时可填（对齐后端 `validation.rate_limit_burst_without_rate`）
- 服务端返回的 `validation.*` 错误码在表单顶部 Alert 渲染

### 6.3 权限

- 操作者无 `grants:write` 或 `rate_limit:write` → UserQuota 区块只读，按钮显示禁用态（复用 `PermissionDenied.tsx` 文案模式）
- 401/403 走全局 axios interceptor（既有）

### 6.4 离线 client

- Combobox 中离线 client 标灰但可选（后端不阻止配置离线 client）
- 行"状态"列显 Badge "离线" + tooltip "client 离线，配额已保存但暂未生效"

### 6.5 删除二次确认

复用 `ConfirmDialog.tsx`。文案：

> 删除 alice 在 edge-tokyo 上的配额？该用户将立即失去访问权，**已创建的转发规则也会被后端级联清理**（与 `delete_grant` 现有行为一致，见 `crates/portunus-server/src/operator/grants.rs:182`）。

## 7. UserCreate 集成

`/users/new` 表单底部新增可选区块「初始用户配额」：

- 默认折叠，标题 "为该用户立即分配配额（可选）"
- 展开后嵌入 `UserQuotaForm` 单例（仅 1 条，不可加多）
- 提交时若有配额：先 `POST /v1/users`，成功后再走 §5.1 的"创建 entry"流程
- 用户创建成功但配额失败：toast 黄条提示，跳转 UserDetail 并自动滚动到配额区块，把刚才填的内容预填回展开行

## 8. 测试策略

### 8.1 单元 / 组件测试（vitest + Testing Library）

- `useAccessEntries.test.ts`：join 逻辑、老数据多 grant 合并、乐观更新回滚
- `UserQuotaForm.test.tsx`：端口重叠校验、协议必选、Unlimited 切换、burst 联动
- `ClientCombobox.test.tsx`：搜索过滤、键盘导航、离线标记
- `UserQuotaTable.test.tsx`：展开/收起、删除二次确认、"状态不一致"badge

### 8.2 集成测试（vitest + MSW 或 Playwright）

- 主路径：UserCreate 提交带初始配额 → UserDetail 看到 1 行 → 展开编辑 → 切 Unlimited → 删除
- 反向：mock `PUT .../rate-limit` 返回 500 → 验证 grant 回滚 + 红条
- 老数据：mock `GET /v1/grants` 返回同 (user, client) 的 2 条 → 验证 Alert badge + 编辑保存合并

### 8.3 后端契约

后端无改动。spec 显式声明依赖以下既有合约测试：
- `crates/portunus-server/tests/http_grants_contract.rs`
- `crates/portunus-server/tests/rate_limit_owner_contract.rs`

### 8.4 视觉验证

- ClientDetail 修改后跑一次 `ui-test` skill，确认无回归
- 新建 UserDetail 配额区块的截图基线（如果项目已有截图工具；否则手测）

## 9. 风险与缓解

| 风险 | 影响 | 缓解 |
|---|---|---|
| 客户端补偿失败时数据不一致 | 罕见，可能产生孤儿 grant 或孤儿 cap | 行级"状态不一致"badge + 手动重试 + 详细错误展示 |
| 老数据多 grant 合并丢失端口段精度 | 中：合并后端口段取并集可能放宽授权 | 编辑前 Alert 警告；操作者可在 Combobox 中手动收窄端口段后再保存 |
| 隐藏 `/grants` 后老操作者迷失 | 低 | 路由保留 + 重定向 `/grants` → `/users` 并 toast 提示"已迁移到用户配额，请进入对应用户" |
| 引入 react-hook-form 增加 bundle 大小 | 低 | 检查 size-limit；react-hook-form + zod 合计约 12 KB gz，预期可控 |
| ClientDetail 改为只读后操作者抱怨 | 低 | 文案明确"在用户页编辑"+ 一键跳转，不破坏可发现性 |

## 10. 实现切片建议

- **PR 1**：shadcn 组件注册（registry add: table、command、popover、collapsible、switch、checkbox、form、tooltip、alert）+ react-hook-form/zod 依赖
- **PR 2**：`UserQuota/*` 组件套 + `useAccessEntries` hook + 单测
- **PR 3**：UserDetail 集成 + UserCreate 可选初始配额
- **PR 4**：ClientDetail 改为只读跳转 + 删除 Grants 导航 + `/grants` 重定向
- **PR 5**：集成测试 + ui-test 回归

具体顺序在后续 implementation plan 中由 writing-plans 决定。
