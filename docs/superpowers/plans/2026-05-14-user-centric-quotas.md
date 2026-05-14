# 用户中心化配额（User-Centric Quotas）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 webui 中以用户为中心管理「访问 + 配额」，把后端独立的 grant + cap 在 UI 层合并成"用户配额"，UserDetail 是主战场、UserCreate 可选初始化、ClientDetail 改为只读跳转、`/grants` 入口下线。

**Architecture:** 后端零改动。前端引入 `UserQuota` 复合对象（grant 字段 + 可选 cap + `unlimited` 派生标志），写操作由 `useAccessEntries` hook 顺序调用 4 个现有 endpoint 并做客户端补偿。每 `(user, client)` UI 层强制唯一；老多 grant 数据在编辑时合并。

**Tech Stack:** React 18 + TypeScript + Vite + TanStack Query v5 + react-router 6 + Tailwind + shadcn/ui（new-york + slate）+ **新增** react-hook-form + zod + sonner（toast）；vitest + @testing-library/react + happy-dom。

**Spec reference:** `docs/superpowers/specs/2026-05-14-user-centric-quotas-design.md`

---

## 文件结构

```
webui/
├── package.json                                  # +react-hook-form, zod, @hookform/resolvers, sonner
├── src/
│   ├── api/
│   │   └── access-entries.ts                     # 新建：useAccessEntries + 复合 CRUD（客户端补偿）
│   ├── components/
│   │   ├── UserQuota/                            # 新建目录
│   │   │   ├── ClientCombobox.tsx                # shadcn Command + Popover
│   │   │   ├── UserQuotaForm.tsx                 # react-hook-form 表单
│   │   │   ├── UserQuotaRow.tsx                  # 单行 + 展开编辑面板
│   │   │   ├── UserQuotaTable.tsx                # 表格容器 + "+ 添加"
│   │   │   └── format.ts                         # bps→human-friendly + zod schema
│   │   └── ui/                                   # shadcn 新增：table, command, popover, collapsible,
│   │                                             #              switch, checkbox, form, tooltip, alert, sonner
│   ├── pages/
│   │   ├── UserCreate.tsx                        # 修改：+ 可选「初始用户配额」区块
│   │   ├── UserDetail.tsx                        # 修改：删 grants 卡片，加 UserQuotaTable
│   │   └── ClientDetail.tsx                      # 修改：OwnerQuotasTab 改只读 + 跳转
│   ├── components/Nav.tsx                        # 修改：删 Grants 导航
│   └── App.tsx                                   # 修改：/grants /grants/new 改 redirect
└── tests/unit/                                   # （或同目录 *.test.tsx）— 每个组件配测试
```

---

## Task 1: 安装 shadcn 组件与新依赖

**Files:**
- Modify: `webui/package.json`
- Modify: `webui/pnpm-lock.yaml`
- Create: `webui/src/components/ui/table.tsx`、`command.tsx`、`popover.tsx`、`collapsible.tsx`、`switch.tsx`、`checkbox.tsx`、`form.tsx`、`tooltip.tsx`、`alert.tsx`、`sonner.tsx`

- [ ] **Step 1: 安装 shadcn 组件**

```bash
cd webui && npx shadcn@latest add table command popover collapsible switch checkbox form tooltip alert sonner
```

逐个回答 prompt 时选默认覆盖（none should pre-exist）。如果命令交互式停住，把每个组件分批跑：
```bash
npx shadcn@latest add table
npx shadcn@latest add command
# ...
```

- [ ] **Step 2: 安装 react-hook-form + zod**

```bash
cd webui && pnpm add react-hook-form zod @hookform/resolvers
```

- [ ] **Step 3: 验证 ui/ 目录里新增 10 个文件**

```bash
ls webui/src/components/ui/ | grep -E "^(table|command|popover|collapsible|switch|checkbox|form|tooltip|alert|sonner)\.tsx$" | wc -l
```
Expected: `10`

- [ ] **Step 4: 在 main.tsx 挂载 Toaster**

打开 `webui/src/main.tsx`，在 `<App />` 兄弟位置加 `<Toaster />`：

```tsx
import { Toaster } from "@/components/ui/sonner";
// ...在 render 树最外层：
<>
  <App />
  <Toaster richColors closeButton position="top-right" />
</>
```

- [ ] **Step 5: 跑 typecheck**

```bash
cd webui && pnpm tsc -b --noEmit
```
Expected: no errors

- [ ] **Step 6: 提交**

```bash
git add webui/package.json webui/pnpm-lock.yaml webui/src/components/ui/ webui/src/main.tsx
git commit -m "chore(webui): add shadcn components + react-hook-form deps for user quotas"
```

---

## Task 2: 单位格式化 + zod schema

**Files:**
- Create: `webui/src/components/UserQuota/format.ts`
- Test: `webui/src/components/UserQuota/format.test.ts`

- [ ] **Step 1: 写失败测试**

```ts
// webui/src/components/UserQuota/format.test.ts
import { describe, expect, it } from "vitest";
import { formatBps, parseBpsInput, accessEntrySchema } from "./format";

describe("formatBps", () => {
  it("formats 0 as 0 bps", () => expect(formatBps(0)).toBe("0 bps"));
  it("formats 1500 as 1.5 KB/s", () => expect(formatBps(1500)).toBe("1.5 KB/s"));
  it("formats 12_500_000 as 12.5 MB/s", () => expect(formatBps(12_500_000)).toBe("12.5 MB/s"));
  it("formats 5_000_000_000 as 5.0 GB/s", () => expect(formatBps(5_000_000_000)).toBe("5.0 GB/s"));
});

describe("parseBpsInput", () => {
  it("parses '1.5 MB/s' to 1_500_000", () => expect(parseBpsInput("1.5 MB/s")).toBe(1_500_000));
  it("parses '100KB' to 100_000", () => expect(parseBpsInput("100KB")).toBe(100_000));
  it("parses bare number as raw bps", () => expect(parseBpsInput("42")).toBe(42));
  it("returns null on garbage", () => expect(parseBpsInput("abc")).toBeNull());
  it("returns null on empty", () => expect(parseBpsInput("")).toBeNull());
});

describe("accessEntrySchema", () => {
  it("requires at least one protocol", () => {
    const r = accessEntrySchema.safeParse({
      client_name: "c1",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: [],
      unlimited: true,
    });
    expect(r.success).toBe(false);
  });

  it("requires start <= end", () => {
    const r = accessEntrySchema.safeParse({
      client_name: "c1",
      listen_port_start: 2000,
      listen_port_end: 1000,
      protocols: ["tcp"],
      unlimited: true,
    });
    expect(r.success).toBe(false);
  });

  it("requires ports in 1..65535", () => {
    const r = accessEntrySchema.safeParse({
      client_name: "c1",
      listen_port_start: 0,
      listen_port_end: 100,
      protocols: ["tcp"],
      unlimited: true,
    });
    expect(r.success).toBe(false);
  });

  it("requires at least one cap > 0 when not unlimited", () => {
    const r = accessEntrySchema.safeParse({
      client_name: "c1",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: ["tcp"],
      unlimited: false,
      bandwidth_in_bps: null,
      bandwidth_out_bps: null,
      new_connections_per_sec: null,
      concurrent_connections: null,
    });
    expect(r.success).toBe(false);
  });

  it("accepts unlimited=true with all caps null", () => {
    const r = accessEntrySchema.safeParse({
      client_name: "c1",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: ["tcp", "udp"],
      unlimited: true,
    });
    expect(r.success).toBe(true);
  });

  it("rejects burst without matching rate", () => {
    const r = accessEntrySchema.safeParse({
      client_name: "c1",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: ["tcp"],
      unlimited: false,
      bandwidth_in_bps: null,
      bandwidth_in_burst: 1_000_000,
      bandwidth_out_bps: 500_000,
      new_connections_per_sec: null,
      concurrent_connections: null,
    });
    expect(r.success).toBe(false);
  });
});
```

- [ ] **Step 2: 运行测试验证失败**

```bash
cd webui && pnpm vitest run src/components/UserQuota/format.test.ts
```
Expected: FAIL — module not found

- [ ] **Step 3: 实现 format.ts**

```ts
// webui/src/components/UserQuota/format.ts
import { z } from "zod";

export function formatBps(n: number): string {
  if (n < 1_000) return `${n} bps`;
  if (n < 1_000_000) return `${(n / 1_000).toFixed(1)} KB/s`;
  if (n < 1_000_000_000) return `${(n / 1_000_000).toFixed(1)} MB/s`;
  return `${(n / 1_000_000_000).toFixed(1)} GB/s`;
}

export function parseBpsInput(raw: string): number | null {
  const s = raw.trim();
  if (!s) return null;
  const m = s.match(/^(\d+(?:\.\d+)?)\s*([KMG]?)B?\/?s?$/i);
  if (!m) {
    const n = Number(s);
    return Number.isFinite(n) && n >= 0 ? n : null;
  }
  const value = Number(m[1]);
  if (!Number.isFinite(value)) return null;
  const unit = m[2].toUpperCase();
  const mul = unit === "K" ? 1_000 : unit === "M" ? 1_000_000 : unit === "G" ? 1_000_000_000 : 1;
  return Math.round(value * mul);
}

const portInt = z.number().int().min(1).max(65535);
const positiveOrNull = z.number().int().positive().nullable().optional();

const baseShape = {
  client_name: z.string().min(1),
  listen_port_start: portInt,
  listen_port_end: portInt,
  protocols: z.array(z.enum(["tcp", "udp"])).min(1),
  note: z.string().optional(),
  unlimited: z.boolean(),
  bandwidth_in_bps: positiveOrNull,
  bandwidth_out_bps: positiveOrNull,
  new_connections_per_sec: positiveOrNull,
  concurrent_connections: positiveOrNull,
  bandwidth_in_burst: positiveOrNull,
  bandwidth_out_burst: positiveOrNull,
  new_connections_burst: positiveOrNull,
};

export const accessEntrySchema = z
  .object(baseShape)
  .refine((d) => d.listen_port_start <= d.listen_port_end, {
    message: "listen_port_start must be <= listen_port_end",
    path: ["listen_port_end"],
  })
  .refine(
    (d) => {
      if (d.unlimited) return true;
      return [
        d.bandwidth_in_bps,
        d.bandwidth_out_bps,
        d.new_connections_per_sec,
        d.concurrent_connections,
      ].some((v) => typeof v === "number" && v > 0);
    },
    {
      message: "at least one cap must be set when not unlimited",
      path: ["bandwidth_in_bps"],
    },
  )
  .refine((d) => !(d.bandwidth_in_burst && !d.bandwidth_in_bps), {
    message: "bandwidth_in_burst requires bandwidth_in_bps",
    path: ["bandwidth_in_burst"],
  })
  .refine((d) => !(d.bandwidth_out_burst && !d.bandwidth_out_bps), {
    message: "bandwidth_out_burst requires bandwidth_out_bps",
    path: ["bandwidth_out_burst"],
  })
  .refine((d) => !(d.new_connections_burst && !d.new_connections_per_sec), {
    message: "new_connections_burst requires new_connections_per_sec",
    path: ["new_connections_burst"],
  });

export type AccessEntryInput = z.infer<typeof accessEntrySchema>;
```

- [ ] **Step 4: 运行测试验证通过**

```bash
cd webui && pnpm vitest run src/components/UserQuota/format.test.ts
```
Expected: PASS — 13 tests passed

- [ ] **Step 5: 提交**

```bash
git add webui/src/components/UserQuota/format.ts webui/src/components/UserQuota/format.test.ts
git commit -m "feat(webui): add format helpers and zod schema for UserQuota"
```

---

## Task 3: `useAccessEntries` join hook（读路径）

**Files:**
- Create: `webui/src/api/access-entries.ts`
- Test: `webui/src/api/access-entries.test.ts`

- [ ] **Step 1: 写失败测试**

```ts
// webui/src/api/access-entries.test.ts
import { describe, expect, it } from "vitest";
import { joinAccessEntries } from "./access-entries";
import type { GrantView, OwnerRateLimitView } from "@/api/types";

const g = (overrides: Partial<GrantView> = {}): GrantView => ({
  grant_id: "g1",
  user_id: "alice",
  client: "edge-tokyo",
  listen_port_start: 1000,
  listen_port_end: 2000,
  protocols: ["tcp"],
  note: null,
  created_at: "2026-01-01T00:00:00Z",
  ...overrides,
});

const cap = (owner: string, client: string): OwnerRateLimitView => ({
  client_name: client,
  owner_id: owner,
  rate_limit: { bandwidth_in_bps: 1_000_000 },
  updated_at_unix_ms: 0,
});

describe("joinAccessEntries", () => {
  it("returns empty when no grants", () => {
    expect(joinAccessEntries([], [])).toEqual([]);
  });

  it("creates one entry per (user, client) with cap", () => {
    const res = joinAccessEntries([g()], [cap("alice", "edge-tokyo")]);
    expect(res).toHaveLength(1);
    expect(res[0].grant_id).toBe("g1");
    expect(res[0].unlimited).toBe(false);
    expect(res[0].cap?.bandwidth_in_bps).toBe(1_000_000);
  });

  it("marks unlimited when grant has no cap", () => {
    const res = joinAccessEntries([g()], []);
    expect(res[0].unlimited).toBe(true);
    expect(res[0].cap).toBeUndefined();
  });

  it("flags duplicates when same (user, client) has 2 grants", () => {
    const grants = [
      g({ grant_id: "g1", listen_port_start: 1000, listen_port_end: 2000 }),
      g({ grant_id: "g2", listen_port_start: 3000, listen_port_end: 9000 }),
    ];
    const res = joinAccessEntries(grants, []);
    expect(res).toHaveLength(1);
    expect(res[0].grant_id).toBe("g2"); // widest range wins
    expect(res[0].legacy_duplicates).toHaveLength(1);
    expect(res[0].legacy_duplicates![0].grant_id).toBe("g1");
  });

  it("keeps separate entries when same user different clients", () => {
    const grants = [g({ client: "edge-tokyo" }), g({ grant_id: "g2", client: "edge-sg" })];
    const res = joinAccessEntries(grants, []);
    expect(res).toHaveLength(2);
  });
});
```

- [ ] **Step 2: 运行测试验证失败**

```bash
cd webui && pnpm vitest run src/api/access-entries.test.ts
```
Expected: FAIL

- [ ] **Step 3: 实现 `joinAccessEntries`**

```ts
// webui/src/api/access-entries.ts
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch, ApiError } from "@/api/client";
import type {
  CreateGrantBody,
  DeleteGrantResponse,
  GrantView,
  OwnerRateLimitView,
  RateLimit,
} from "@/api/types";

export interface AccessEntry {
  grant_id: string;
  user_id: string;
  client_name: string;
  listen_port_start: number;
  listen_port_end: number;
  protocols: ("tcp" | "udp")[];
  unlimited: boolean;
  cap?: RateLimit;
  /// Set when the backend has >1 grant for this (user, client).
  legacy_duplicates?: GrantView[];
}

function rangeWidth(g: GrantView): number {
  return g.listen_port_end - g.listen_port_start;
}

export function joinAccessEntries(
  grants: GrantView[],
  caps: OwnerRateLimitView[],
): AccessEntry[] {
  const capByPair = new Map<string, OwnerRateLimitView>();
  for (const c of caps) {
    capByPair.set(`${c.owner_id}::${c.client_name}`, c);
  }

  // Group grants by (user, client)
  const groups = new Map<string, GrantView[]>();
  for (const g of grants) {
    const k = `${g.user_id}::${g.client}`;
    const arr = groups.get(k) ?? [];
    arr.push(g);
    groups.set(k, arr);
  }

  const out: AccessEntry[] = [];
  for (const [, gs] of groups) {
    const sorted = [...gs].sort((a, b) => rangeWidth(b) - rangeWidth(a));
    const primary = sorted[0];
    const cap = capByPair.get(`${primary.user_id}::${primary.client}`);
    out.push({
      grant_id: primary.grant_id,
      user_id: primary.user_id,
      client_name: primary.client,
      listen_port_start: primary.listen_port_start,
      listen_port_end: primary.listen_port_end,
      protocols: primary.protocols,
      unlimited: !cap,
      cap: cap?.rate_limit,
      legacy_duplicates: sorted.length > 1 ? sorted.slice(1) : undefined,
    });
  }
  return out.sort((a, b) => a.client_name.localeCompare(b.client_name));
}
```

- [ ] **Step 4: 运行测试验证通过**

```bash
cd webui && pnpm vitest run src/api/access-entries.test.ts
```
Expected: PASS — 5 tests

- [ ] **Step 5: 提交**

```bash
git add webui/src/api/access-entries.ts webui/src/api/access-entries.test.ts
git commit -m "feat(webui): join grants + owner caps into AccessEntry view"
```

---

## Task 4: `useAccessEntries` 查询 hook（含 caps 懒拉取）

**Files:**
- Modify: `webui/src/api/access-entries.ts`

- [ ] **Step 1: 在 `access-entries.ts` 末尾追加查询 hook**

```ts
// 在文件末尾追加

export const userAccessEntriesKey = (userId: string) =>
  ["access-entries", userId] as const;
export const userAccessCapKey = (userId: string, clientName: string) =>
  ["access-entries", userId, "cap", clientName] as const;

interface UseAccessEntriesResult {
  data: AccessEntry[] | undefined;
  isLoading: boolean;
  error: unknown;
}

export function useAccessEntries(userId: string): UseAccessEntriesResult {
  const grantsQ = useQuery({
    queryKey: ["grants", "user", userId],
    queryFn: () => apiFetch<GrantView[]>(`/v1/grants?user_id=${encodeURIComponent(userId)}`),
    enabled: userId.length > 0,
  });

  const grants = grantsQ.data ?? [];
  const uniquePairs = Array.from(
    new Set(grants.map((g) => `${g.user_id}::${g.client}`)),
  ).map((k) => {
    const [u, c] = k.split("::");
    return { user_id: u, client_name: c };
  });

  const capQueries = useQueries({
    queries: uniquePairs.map((p) => ({
      queryKey: userAccessCapKey(p.user_id, p.client_name),
      queryFn: async (): Promise<OwnerRateLimitView | null> => {
        try {
          return await apiFetch<OwnerRateLimitView>(
            `/v1/clients/${encodeURIComponent(p.client_name)}/owners/${encodeURIComponent(p.user_id)}/rate-limit`,
          );
        } catch (err) {
          if (err instanceof ApiError && err.status === 404) return null;
          throw err;
        }
      },
      enabled: userId.length > 0,
    })),
  });

  const capsLoading = capQueries.some((q) => q.isLoading);
  const caps = capQueries
    .map((q) => q.data)
    .filter((v): v is OwnerRateLimitView => v != null);
  const error = grantsQ.error ?? capQueries.find((q) => q.error)?.error;

  return {
    data: grantsQ.data ? joinAccessEntries(grants, caps) : undefined,
    isLoading: grantsQ.isLoading || (grants.length > 0 && capsLoading),
    error,
  };
}
```

- [ ] **Step 2: 跑 typecheck**

```bash
cd webui && pnpm tsc -b --noEmit
```
Expected: no errors

- [ ] **Step 3: 提交**

```bash
git add webui/src/api/access-entries.ts
git commit -m "feat(webui): useAccessEntries hook with lazy cap fetching"
```

---

## Task 5: CRUD mutations 与客户端补偿

**Files:**
- Modify: `webui/src/api/access-entries.ts`
- Test: `webui/src/api/access-entries-mutations.test.ts`

- [ ] **Step 1: 在 `access-entries.ts` 追加 mutation hooks**

```ts
// 在文件末尾追加

export interface CreateAccessEntryInput {
  user_id: string;
  client_name: string;
  listen_port_start: number;
  listen_port_end: number;
  protocols: ("tcp" | "udp")[];
  cap?: RateLimit;
}

export interface AccessEntryError extends Error {
  stage: "grant" | "cap" | "rollback";
  recoverable: boolean;
}

function makeError(
  stage: "grant" | "cap" | "rollback",
  cause: unknown,
  recoverable: boolean,
): AccessEntryError {
  const msg = cause instanceof Error ? cause.message : String(cause);
  const err = new Error(`[${stage}] ${msg}`) as AccessEntryError;
  err.stage = stage;
  err.recoverable = recoverable;
  return err;
}

export function useCreateAccessEntry(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (input: CreateAccessEntryInput): Promise<AccessEntry> => {
      const grantBody: CreateGrantBody = {
        user_id: input.user_id,
        client: input.client_name,
        listen_port_start: input.listen_port_start,
        listen_port_end: input.listen_port_end,
        protocols: input.protocols,
      };
      let grant: GrantView;
      try {
        grant = await apiFetch<GrantView>("/v1/grants", {
          method: "POST",
          body: JSON.stringify(grantBody),
        });
      } catch (err) {
        throw makeError("grant", err, false);
      }

      if (input.cap) {
        try {
          await apiFetch<OwnerRateLimitView>(
            `/v1/clients/${encodeURIComponent(input.client_name)}/owners/${encodeURIComponent(input.user_id)}/rate-limit`,
            { method: "PUT", body: JSON.stringify(input.cap) },
          );
        } catch (err) {
          // Compensation: roll back the grant we just made.
          try {
            await apiFetch<DeleteGrantResponse>(
              `/v1/grants/${encodeURIComponent(grant.grant_id)}`,
              { method: "DELETE" },
            );
            throw makeError("cap", err, true);
          } catch (rollbackErr) {
            if ((rollbackErr as AccessEntryError).stage === "cap") throw rollbackErr;
            throw makeError("rollback", rollbackErr, false);
          }
        }
      }

      return {
        grant_id: grant.grant_id,
        user_id: grant.user_id,
        client_name: grant.client,
        listen_port_start: grant.listen_port_start,
        listen_port_end: grant.listen_port_end,
        protocols: grant.protocols,
        unlimited: !input.cap,
        cap: input.cap,
      };
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: userAccessEntriesKey(userId) });
      void qc.invalidateQueries({ queryKey: ["grants"] });
      void qc.invalidateQueries({ queryKey: ["users"] });
    },
  });
}

export interface UpdateAccessEntryInput {
  user_id: string;
  client_name: string;
  /// The current grant id; replaced if port range or protocols change.
  grant_id: string;
  /// Old fields (to detect whether we need to delete+recreate the grant).
  old: Pick<AccessEntry, "listen_port_start" | "listen_port_end" | "protocols">;
  /// New fields.
  listen_port_start: number;
  listen_port_end: number;
  protocols: ("tcp" | "udp")[];
  cap?: RateLimit;
  /// Optional: if the backend already had multiple grants for this
  /// (user, client), they will be deleted as part of normalization.
  legacy_duplicate_ids?: string[];
}

function grantShapeChanged(input: UpdateAccessEntryInput): boolean {
  return (
    input.old.listen_port_start !== input.listen_port_start ||
    input.old.listen_port_end !== input.listen_port_end ||
    input.old.protocols.length !== input.protocols.length ||
    input.old.protocols.some((p) => !input.protocols.includes(p))
  );
}

export function useUpdateAccessEntry(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (input: UpdateAccessEntryInput): Promise<void> => {
      const duplicates = input.legacy_duplicate_ids ?? [];
      const reshape = grantShapeChanged(input) || duplicates.length > 0;

      if (reshape) {
        // Delete primary + duplicates, then create one merged grant.
        try {
          for (const id of [input.grant_id, ...duplicates]) {
            await apiFetch<DeleteGrantResponse>(
              `/v1/grants/${encodeURIComponent(id)}`,
              { method: "DELETE" },
            );
          }
        } catch (err) {
          throw makeError("grant", err, false);
        }
        try {
          await apiFetch<GrantView>("/v1/grants", {
            method: "POST",
            body: JSON.stringify({
              user_id: input.user_id,
              client: input.client_name,
              listen_port_start: input.listen_port_start,
              listen_port_end: input.listen_port_end,
              protocols: input.protocols,
            } satisfies CreateGrantBody),
          });
        } catch (err) {
          throw makeError("grant", err, false);
        }
      }

      // Cap: PUT if non-empty, DELETE if cap=undefined (unlimited).
      const capUrl = `/v1/clients/${encodeURIComponent(input.client_name)}/owners/${encodeURIComponent(input.user_id)}/rate-limit`;
      try {
        if (input.cap) {
          await apiFetch<OwnerRateLimitView>(capUrl, {
            method: "PUT",
            body: JSON.stringify(input.cap),
          });
        } else {
          try {
            await apiFetch<void>(capUrl, { method: "DELETE" });
          } catch (err) {
            if (!(err instanceof ApiError && err.status === 404)) throw err;
          }
        }
      } catch (err) {
        throw makeError("cap", err, true);
      }
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: userAccessEntriesKey(userId) });
      void qc.invalidateQueries({ queryKey: ["grants"] });
    },
  });
}

export function useDeleteAccessEntry(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (input: {
      grant_id: string;
      client_name: string;
      user_id: string;
      legacy_duplicate_ids?: string[];
    }): Promise<void> => {
      const capUrl = `/v1/clients/${encodeURIComponent(input.client_name)}/owners/${encodeURIComponent(input.user_id)}/rate-limit`;
      try {
        await apiFetch<void>(capUrl, { method: "DELETE" });
      } catch (err) {
        if (!(err instanceof ApiError && err.status === 404)) {
          throw makeError("cap", err, true);
        }
      }
      try {
        for (const id of [input.grant_id, ...(input.legacy_duplicate_ids ?? [])]) {
          await apiFetch<DeleteGrantResponse>(
            `/v1/grants/${encodeURIComponent(id)}`,
            { method: "DELETE" },
          );
        }
      } catch (err) {
        throw makeError("grant", err, false);
      }
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: userAccessEntriesKey(userId) });
      void qc.invalidateQueries({ queryKey: ["grants"] });
      void qc.invalidateQueries({ queryKey: ["users"] });
    },
  });
}
```

- [ ] **Step 2: 写 mutation 测试**

```ts
// webui/src/api/access-entries-mutations.test.ts
import { describe, expect, it, beforeEach, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { useCreateAccessEntry, useDeleteAccessEntry } from "./access-entries";
import type { ReactNode } from "react";

function wrapper(qc: QueryClient) {
  return ({ children }: { children: ReactNode }) => (
    <QueryClientProvider client={qc}>{children}</QueryClientProvider>
  );
}

const fetchMock = vi.fn();
beforeEach(() => {
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});

function jsonRes(body: unknown, init: number | ResponseInit = 200): Response {
  const ri = typeof init === "number" ? { status: init } : init;
  return new Response(JSON.stringify(body), {
    ...ri,
    headers: { "content-type": "application/json" },
  });
}

describe("useCreateAccessEntry", () => {
  it("creates grant + cap on happy path", async () => {
    fetchMock
      .mockResolvedValueOnce(
        jsonRes({
          grant_id: "g1",
          user_id: "alice",
          client: "edge",
          listen_port_start: 1000,
          listen_port_end: 2000,
          protocols: ["tcp"],
          note: null,
          created_at: "x",
        }),
      )
      .mockResolvedValueOnce(
        jsonRes({
          client_name: "edge",
          owner_id: "alice",
          rate_limit: { bandwidth_in_bps: 1000 },
          updated_at_unix_ms: 0,
        }),
      );

    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useCreateAccessEntry("alice"), { wrapper: wrapper(qc) });

    result.current.mutate({
      user_id: "alice",
      client_name: "edge",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: ["tcp"],
      cap: { bandwidth_in_bps: 1000 },
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[0][0]).toContain("/v1/grants");
    expect(fetchMock.mock.calls[1][0]).toContain("/rate-limit");
  });

  it("rolls back grant when cap PUT fails", async () => {
    fetchMock
      .mockResolvedValueOnce(
        jsonRes({
          grant_id: "g1",
          user_id: "alice",
          client: "edge",
          listen_port_start: 1000,
          listen_port_end: 2000,
          protocols: ["tcp"],
          note: null,
          created_at: "x",
        }),
      )
      .mockResolvedValueOnce(jsonRes({ error: { code: "x", message: "boom" } }, 500))
      .mockResolvedValueOnce(jsonRes({ grant_id: "g1" })); // DELETE rollback ok

    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useCreateAccessEntry("alice"), { wrapper: wrapper(qc) });
    result.current.mutate({
      user_id: "alice",
      client_name: "edge",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: ["tcp"],
      cap: { bandwidth_in_bps: 1000 },
    });

    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(fetchMock).toHaveBeenCalledTimes(3); // POST grant + PUT cap + DELETE rollback
    expect(fetchMock.mock.calls[2][1]?.method).toBe("DELETE");
  });
});

describe("useDeleteAccessEntry", () => {
  it("deletes cap (404 ignored) then grant", async () => {
    fetchMock
      .mockResolvedValueOnce(jsonRes({ error: { code: "not_found", message: "" } }, 404))
      .mockResolvedValueOnce(jsonRes({ grant_id: "g1" }));

    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useDeleteAccessEntry("alice"), { wrapper: wrapper(qc) });
    result.current.mutate({ grant_id: "g1", user_id: "alice", client_name: "edge" });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});
```

- [ ] **Step 3: 运行测试**

```bash
cd webui && pnpm vitest run src/api/access-entries-mutations.test.ts
```
Expected: PASS — 3 tests

- [ ] **Step 4: 提交**

```bash
git add webui/src/api/access-entries.ts webui/src/api/access-entries-mutations.test.ts
git commit -m "feat(webui): access entry CRUD with client-side compensation"
```

---

## Task 6: `ClientCombobox` 组件

**Files:**
- Create: `webui/src/components/UserQuota/ClientCombobox.tsx`
- Test: `webui/src/components/UserQuota/ClientCombobox.test.tsx`

- [ ] **Step 1: 写测试**

```tsx
// webui/src/components/UserQuota/ClientCombobox.test.tsx
import { describe, expect, it } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { ClientCombobox } from "./ClientCombobox";
import "@/i18n";

const clients = [
  { client_name: "edge-tokyo", connected: true },
  { client_name: "edge-sg", connected: false },
  { client_name: "edge-fra", connected: true },
];

describe("ClientCombobox", () => {
  it("renders placeholder when value empty", () => {
    render(
      <ClientCombobox
        clients={clients}
        value=""
        onChange={() => {}}
        disabledClientNames={new Set()}
      />,
    );
    expect(screen.getByRole("combobox")).toBeTruthy();
  });

  it("opens popover on click and lists clients", () => {
    render(
      <ClientCombobox
        clients={clients}
        value=""
        onChange={() => {}}
        disabledClientNames={new Set()}
      />,
    );
    fireEvent.click(screen.getByRole("combobox"));
    expect(screen.getByText("edge-tokyo")).toBeTruthy();
    expect(screen.getByText("edge-sg")).toBeTruthy();
  });

  it("disables clients in disabledClientNames", () => {
    render(
      <ClientCombobox
        clients={clients}
        value=""
        onChange={() => {}}
        disabledClientNames={new Set(["edge-sg"])}
      />,
    );
    fireEvent.click(screen.getByRole("combobox"));
    const sg = screen.getByText("edge-sg").closest("[role='option']");
    expect(sg?.getAttribute("aria-disabled")).toBe("true");
  });
});
```

- [ ] **Step 2: 运行测试验证失败**

```bash
cd webui && pnpm vitest run src/components/UserQuota/ClientCombobox.test.tsx
```
Expected: FAIL

- [ ] **Step 3: 实现 `ClientCombobox.tsx`**

```tsx
// webui/src/components/UserQuota/ClientCombobox.tsx
import { Check, ChevronsUpDown } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { cn } from "@/lib/cn";

export interface ClientLite {
  client_name: string;
  connected: boolean;
}

interface Props {
  clients: ClientLite[];
  value: string;
  onChange: (next: string) => void;
  disabledClientNames: Set<string>;
  disabled?: boolean;
}

export function ClientCombobox({
  clients,
  value,
  onChange,
  disabledClientNames,
  disabled,
}: Props) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);

  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger asChild>
        <Button
          variant="outline"
          role="combobox"
          aria-expanded={open}
          disabled={disabled}
          className="w-full justify-between"
        >
          {value || t("userQuota.combobox.placeholder")}
          <ChevronsUpDown className="ml-2 h-4 w-4 opacity-50" />
        </Button>
      </PopoverTrigger>
      <PopoverContent className="w-[--radix-popover-trigger-width] p-0">
        <Command>
          <CommandInput placeholder={t("userQuota.combobox.search")} />
          <CommandList>
            <CommandEmpty>{t("userQuota.combobox.empty")}</CommandEmpty>
            <CommandGroup>
              {clients.map((c) => {
                const isDisabled = disabledClientNames.has(c.client_name);
                return (
                  <CommandItem
                    key={c.client_name}
                    value={c.client_name}
                    disabled={isDisabled}
                    onSelect={() => {
                      onChange(c.client_name);
                      setOpen(false);
                    }}
                  >
                    <Check
                      className={cn(
                        "mr-2 h-4 w-4",
                        value === c.client_name ? "opacity-100" : "opacity-0",
                      )}
                    />
                    <span className={cn("flex-1 font-mono", !c.connected && "opacity-60")}>
                      {c.client_name}
                    </span>
                    {!c.connected && (
                      <span className="ml-2 text-xs text-muted-foreground">
                        {t("userQuota.combobox.offline")}
                      </span>
                    )}
                    {isDisabled && (
                      <span className="ml-2 text-xs text-muted-foreground">
                        {t("userQuota.combobox.alreadyAssigned")}
                      </span>
                    )}
                  </CommandItem>
                );
              })}
            </CommandGroup>
          </CommandList>
        </Command>
      </PopoverContent>
    </Popover>
  );
}
```

- [ ] **Step 4: 运行测试验证通过**

```bash
cd webui && pnpm vitest run src/components/UserQuota/ClientCombobox.test.tsx
```
Expected: PASS — 3 tests

- [ ] **Step 5: 提交**

```bash
git add webui/src/components/UserQuota/ClientCombobox.{tsx,test.tsx}
git commit -m "feat(webui): add ClientCombobox component"
```

---

## Task 7: `UserQuotaForm` 组件

**Files:**
- Create: `webui/src/components/UserQuota/UserQuotaForm.tsx`
- Test: `webui/src/components/UserQuota/UserQuotaForm.test.tsx`

- [ ] **Step 1: 写测试**

```tsx
// webui/src/components/UserQuota/UserQuotaForm.test.tsx
import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { UserQuotaForm } from "./UserQuotaForm";
import "@/i18n";

const clients = [
  { client_name: "edge-tokyo", connected: true },
  { client_name: "edge-sg", connected: true },
];

describe("UserQuotaForm", () => {
  it("renders with empty defaults", () => {
    render(
      <UserQuotaForm
        clients={clients}
        disabledClientNames={new Set()}
        defaultValues={undefined}
        onSubmit={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(screen.getByRole("combobox")).toBeTruthy();
  });

  it("blocks submission when ports inverted", async () => {
    const onSubmit = vi.fn();
    render(
      <UserQuotaForm
        clients={clients}
        disabledClientNames={new Set()}
        defaultValues={{
          client_name: "edge-tokyo",
          listen_port_start: 5000,
          listen_port_end: 1000,
          protocols: ["tcp"],
          unlimited: true,
        }}
        onSubmit={onSubmit}
        onCancel={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /save/i }));
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("hides cap fields when unlimited toggled on", () => {
    render(
      <UserQuotaForm
        clients={clients}
        disabledClientNames={new Set()}
        defaultValues={{
          client_name: "edge-tokyo",
          listen_port_start: 1000,
          listen_port_end: 2000,
          protocols: ["tcp"],
          unlimited: true,
        }}
        onSubmit={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(screen.queryByLabelText(/bandwidth in/i)).toBeFalsy();
  });
});
```

- [ ] **Step 2: 运行测试验证失败**

```bash
cd webui && pnpm vitest run src/components/UserQuota/UserQuotaForm.test.tsx
```
Expected: FAIL

- [ ] **Step 3: 实现 `UserQuotaForm.tsx`**

```tsx
// webui/src/components/UserQuota/UserQuotaForm.tsx
import { zodResolver } from "@hookform/resolvers/zod";
import { useForm, Controller } from "react-hook-form";
import { useTranslation } from "react-i18next";
import type { z } from "zod";

import type { RateLimit } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { ClientCombobox, type ClientLite } from "./ClientCombobox";
import { accessEntrySchema } from "./format";

export type FormValues = z.infer<typeof accessEntrySchema>;

export interface UserQuotaFormSubmitValue {
  client_name: string;
  listen_port_start: number;
  listen_port_end: number;
  protocols: ("tcp" | "udp")[];
  cap?: RateLimit;
}

interface Props {
  clients: ClientLite[];
  disabledClientNames: Set<string>;
  /// Lock the client picker (used when editing an existing entry).
  lockClient?: boolean;
  defaultValues?: Partial<FormValues>;
  onSubmit: (v: UserQuotaFormSubmitValue) => void | Promise<void>;
  onCancel: () => void;
  busy?: boolean;
  serverError?: string | null;
}

const DEFAULTS: FormValues = {
  client_name: "",
  listen_port_start: 10_000,
  listen_port_end: 19_999,
  protocols: ["tcp"],
  unlimited: false,
  bandwidth_in_bps: null,
  bandwidth_out_bps: null,
  new_connections_per_sec: null,
  concurrent_connections: null,
  bandwidth_in_burst: null,
  bandwidth_out_burst: null,
  new_connections_burst: null,
};

export function UserQuotaForm({
  clients,
  disabledClientNames,
  lockClient,
  defaultValues,
  onSubmit,
  onCancel,
  busy,
  serverError,
}: Props) {
  const { t } = useTranslation();
  const form = useForm<FormValues>({
    resolver: zodResolver(accessEntrySchema),
    defaultValues: { ...DEFAULTS, ...defaultValues },
  });
  const { register, handleSubmit, watch, control, formState } = form;
  const unlimited = watch("unlimited");

  async function submit(v: FormValues) {
    const cap: RateLimit | undefined = v.unlimited
      ? undefined
      : {
          bandwidth_in_bps: v.bandwidth_in_bps ?? undefined,
          bandwidth_out_bps: v.bandwidth_out_bps ?? undefined,
          new_connections_per_sec: v.new_connections_per_sec ?? undefined,
          concurrent_connections: v.concurrent_connections ?? undefined,
          bandwidth_in_burst: v.bandwidth_in_burst ?? undefined,
          bandwidth_out_burst: v.bandwidth_out_burst ?? undefined,
          new_connections_burst: v.new_connections_burst ?? undefined,
        };
    await onSubmit({
      client_name: v.client_name,
      listen_port_start: v.listen_port_start,
      listen_port_end: v.listen_port_end,
      protocols: v.protocols,
      cap,
    });
  }

  return (
    <form onSubmit={handleSubmit(submit)} className="space-y-4 p-4 border rounded-md bg-card">
      <div className="space-y-2">
        <Label>{t("userQuota.form.client")}</Label>
        <Controller
          name="client_name"
          control={control}
          render={({ field }) => (
            <ClientCombobox
              clients={clients}
              value={field.value}
              onChange={field.onChange}
              disabledClientNames={disabledClientNames}
              disabled={lockClient}
            />
          )}
        />
        {formState.errors.client_name && (
          <p className="text-sm text-destructive">{formState.errors.client_name.message}</p>
        )}
      </div>

      <div className="grid grid-cols-2 gap-3">
        <div className="space-y-1">
          <Label htmlFor="port-start">{t("userQuota.form.portStart")}</Label>
          <Input
            id="port-start"
            type="number"
            min={1}
            max={65535}
            {...register("listen_port_start", { valueAsNumber: true })}
          />
        </div>
        <div className="space-y-1">
          <Label htmlFor="port-end">{t("userQuota.form.portEnd")}</Label>
          <Input
            id="port-end"
            type="number"
            min={1}
            max={65535}
            {...register("listen_port_end", { valueAsNumber: true })}
          />
        </div>
      </div>
      {formState.errors.listen_port_end && (
        <p className="text-sm text-destructive">{formState.errors.listen_port_end.message}</p>
      )}

      <div className="space-y-2">
        <Label>{t("userQuota.form.protocols")}</Label>
        <Controller
          name="protocols"
          control={control}
          render={({ field }) => (
            <div className="flex gap-4">
              {(["tcp", "udp"] as const).map((p) => (
                <label key={p} className="flex items-center gap-2 text-sm">
                  <Checkbox
                    checked={field.value.includes(p)}
                    onCheckedChange={(checked) => {
                      const next = checked
                        ? Array.from(new Set([...field.value, p]))
                        : field.value.filter((x) => x !== p);
                      field.onChange(next);
                    }}
                  />
                  {p.toUpperCase()}
                </label>
              ))}
            </div>
          )}
        />
        {formState.errors.protocols && (
          <p className="text-sm text-destructive">{formState.errors.protocols.message}</p>
        )}
      </div>

      <div className="flex items-center justify-between border-t pt-4">
        <div>
          <Label htmlFor="unlimited">{t("userQuota.form.unlimited")}</Label>
          <p className="text-xs text-muted-foreground">{t("userQuota.form.unlimitedHelp")}</p>
        </div>
        <Controller
          name="unlimited"
          control={control}
          render={({ field }) => (
            <Switch id="unlimited" checked={field.value} onCheckedChange={field.onChange} />
          )}
        />
      </div>

      {!unlimited && (
        <div className="grid grid-cols-2 gap-3">
          <div className="space-y-1">
            <Label htmlFor="bw-in">{t("userQuota.form.bandwidthIn")}</Label>
            <Input
              id="bw-in"
              type="number"
              min={1}
              placeholder={t("userQuota.form.uncapped")}
              {...register("bandwidth_in_bps", { valueAsNumber: true, setValueAs: (v) => (v === "" || Number.isNaN(v) ? null : Number(v)) })}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="bw-out">{t("userQuota.form.bandwidthOut")}</Label>
            <Input
              id="bw-out"
              type="number"
              min={1}
              placeholder={t("userQuota.form.uncapped")}
              {...register("bandwidth_out_bps", { valueAsNumber: true, setValueAs: (v) => (v === "" || Number.isNaN(v) ? null : Number(v)) })}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="conc">{t("userQuota.form.concurrent")}</Label>
            <Input
              id="conc"
              type="number"
              min={1}
              placeholder={t("userQuota.form.uncapped")}
              {...register("concurrent_connections", { valueAsNumber: true, setValueAs: (v) => (v === "" || Number.isNaN(v) ? null : Number(v)) })}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="ncps">{t("userQuota.form.newConnPerSec")}</Label>
            <Input
              id="ncps"
              type="number"
              min={1}
              placeholder={t("userQuota.form.uncapped")}
              {...register("new_connections_per_sec", { valueAsNumber: true, setValueAs: (v) => (v === "" || Number.isNaN(v) ? null : Number(v)) })}
            />
          </div>
          {formState.errors.bandwidth_in_bps && (
            <p className="col-span-2 text-sm text-destructive">
              {formState.errors.bandwidth_in_bps.message}
            </p>
          )}
        </div>
      )}

      {serverError && <p className="text-sm text-destructive">{serverError}</p>}

      <div className="flex gap-2">
        <Button type="submit" disabled={busy}>
          {busy ? t("confirm.busy") : t("userQuota.form.save")}
        </Button>
        <Button type="button" variant="outline" onClick={onCancel}>
          {t("confirm.cancel")}
        </Button>
      </div>
    </form>
  );
}
```

- [ ] **Step 4: 运行测试**

```bash
cd webui && pnpm vitest run src/components/UserQuota/UserQuotaForm.test.tsx
```
Expected: PASS — 3 tests

- [ ] **Step 5: 提交**

```bash
git add webui/src/components/UserQuota/UserQuotaForm.{tsx,test.tsx}
git commit -m "feat(webui): UserQuotaForm with react-hook-form + zod"
```

---

## Task 8: `UserQuotaRow` 与 `UserQuotaTable`

**Files:**
- Create: `webui/src/components/UserQuota/UserQuotaRow.tsx`
- Create: `webui/src/components/UserQuota/UserQuotaTable.tsx`
- Test: `webui/src/components/UserQuota/UserQuotaTable.test.tsx`

- [ ] **Step 1: 写测试**

```tsx
// webui/src/components/UserQuota/UserQuotaTable.test.tsx
import { describe, expect, it, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { UserQuotaTable } from "./UserQuotaTable";
import type { AccessEntry } from "@/api/access-entries";
import "@/i18n";

const wrap = (ui: React.ReactElement) => {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
};

const entries: AccessEntry[] = [
  {
    grant_id: "g1",
    user_id: "alice",
    client_name: "edge-tokyo",
    listen_port_start: 1000,
    listen_port_end: 2000,
    protocols: ["tcp"],
    unlimited: false,
    cap: { bandwidth_in_bps: 1_000_000 },
  },
  {
    grant_id: "g2",
    user_id: "alice",
    client_name: "edge-sg",
    listen_port_start: 3000,
    listen_port_end: 4000,
    protocols: ["tcp", "udp"],
    unlimited: true,
  },
];

describe("UserQuotaTable", () => {
  it("renders one row per entry", () => {
    wrap(
      <UserQuotaTable
        userId="alice"
        entries={entries}
        clients={[
          { client_name: "edge-tokyo", connected: true },
          { client_name: "edge-sg", connected: false },
        ]}
        readOnly={false}
      />,
    );
    expect(screen.getByText("edge-tokyo")).toBeTruthy();
    expect(screen.getByText("edge-sg")).toBeTruthy();
  });

  it("shows 'Unlimited' badge on entries without cap", () => {
    wrap(
      <UserQuotaTable
        userId="alice"
        entries={entries}
        clients={[]}
        readOnly={false}
      />,
    );
    expect(screen.getAllByText(/unlimited/i).length).toBeGreaterThan(0);
  });

  it("hides + Add button in read-only mode", () => {
    wrap(
      <UserQuotaTable
        userId="alice"
        entries={entries}
        clients={[]}
        readOnly={true}
      />,
    );
    expect(screen.queryByText(/add/i)).toBeFalsy();
  });

  it("clicking + Add reveals an inline form", () => {
    wrap(
      <UserQuotaTable
        userId="alice"
        entries={entries}
        clients={[]}
        readOnly={false}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /add/i }));
    expect(screen.getAllByRole("combobox").length).toBeGreaterThan(0);
  });
});
```

- [ ] **Step 2: 运行测试验证失败**

```bash
cd webui && pnpm vitest run src/components/UserQuota/UserQuotaTable.test.tsx
```
Expected: FAIL

- [ ] **Step 3: 实现 `UserQuotaRow.tsx`**

```tsx
// webui/src/components/UserQuota/UserQuotaRow.tsx
import { AlertTriangle, ChevronDown, ChevronRight, Trash2 } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import {
  useDeleteAccessEntry,
  useUpdateAccessEntry,
  type AccessEntry,
} from "@/api/access-entries";
import { ApiError } from "@/api/client";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { TableCell, TableRow } from "@/components/ui/table";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { formatBps } from "./format";
import { UserQuotaForm, type UserQuotaFormSubmitValue } from "./UserQuotaForm";
import type { ClientLite } from "./ClientCombobox";

interface Props {
  userId: string;
  entry: AccessEntry;
  clients: ClientLite[];
  clientOnline: boolean;
  readOnly: boolean;
}

export function UserQuotaRow({ userId, entry, clients, clientOnline, readOnly }: Props) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [serverError, setServerError] = useState<string | null>(null);
  const update = useUpdateAccessEntry(userId);
  const del = useDeleteAccessEntry(userId);

  async function onSubmit(v: UserQuotaFormSubmitValue) {
    setServerError(null);
    try {
      await update.mutateAsync({
        user_id: userId,
        client_name: v.client_name,
        grant_id: entry.grant_id,
        old: {
          listen_port_start: entry.listen_port_start,
          listen_port_end: entry.listen_port_end,
          protocols: entry.protocols,
        },
        listen_port_start: v.listen_port_start,
        listen_port_end: v.listen_port_end,
        protocols: v.protocols,
        cap: v.cap,
        legacy_duplicate_ids: entry.legacy_duplicates?.map((g) => g.grant_id),
      });
      toast.success(t("userQuota.toast.updated", { client: v.client_name }));
      setExpanded(false);
    } catch (err) {
      const msg = err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message;
      setServerError(msg);
      toast.error(t("userQuota.toast.updateFailed"));
    }
  }

  async function onDelete() {
    try {
      await del.mutateAsync({
        grant_id: entry.grant_id,
        user_id: userId,
        client_name: entry.client_name,
        legacy_duplicate_ids: entry.legacy_duplicates?.map((g) => g.grant_id),
      });
      toast.success(t("userQuota.toast.deleted", { client: entry.client_name }));
      setConfirmDelete(false);
    } catch (err) {
      const msg = err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message;
      toast.error(`${t("userQuota.toast.deleteFailed")}: ${msg}`);
    }
  }

  return (
    <>
      <TableRow>
        <TableCell>
          <Button
            variant="ghost"
            size="sm"
            onClick={() => setExpanded((v) => !v)}
            aria-label={expanded ? t("userQuota.row.collapse") : t("userQuota.row.expand")}
          >
            {expanded ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
          </Button>
        </TableCell>
        <TableCell className="font-mono">{entry.client_name}</TableCell>
        <TableCell className="font-mono">
          {entry.listen_port_start}-{entry.listen_port_end}
        </TableCell>
        <TableCell>{entry.protocols.map((p) => p.toUpperCase()).join(", ")}</TableCell>
        <TableCell>
          {entry.unlimited ? (
            <Badge>{t("userQuota.unlimited")}</Badge>
          ) : entry.cap?.bandwidth_in_bps ? (
            formatBps(entry.cap.bandwidth_in_bps)
          ) : (
            "—"
          )}
        </TableCell>
        <TableCell>
          {entry.unlimited ? (
            <Badge>{t("userQuota.unlimited")}</Badge>
          ) : entry.cap?.bandwidth_out_bps ? (
            formatBps(entry.cap.bandwidth_out_bps)
          ) : (
            "—"
          )}
        </TableCell>
        <TableCell>{entry.unlimited ? "—" : entry.cap?.concurrent_connections ?? "—"}</TableCell>
        <TableCell>
          {entry.unlimited ? "—" : entry.cap?.new_connections_per_sec ?? "—"}
        </TableCell>
        <TableCell>
          {clientOnline ? (
            <Badge variant={"success" as never}>{t("userQuota.online")}</Badge>
          ) : (
            <Badge variant="secondary">{t("userQuota.offline")}</Badge>
          )}
          {entry.legacy_duplicates && (
            <span title={t("userQuota.row.duplicateTooltip")}>
              <AlertTriangle className="inline ml-2 h-4 w-4 text-amber-500" />
            </span>
          )}
        </TableCell>
        <TableCell>
          {!readOnly && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setConfirmDelete(true)}
              className="text-destructive"
            >
              <Trash2 className="h-4 w-4" />
            </Button>
          )}
        </TableCell>
      </TableRow>
      {expanded && (
        <TableRow>
          <TableCell colSpan={10} className="bg-muted/30">
            {entry.legacy_duplicates && (
              <Alert className="mb-3">
                <AlertTriangle className="h-4 w-4" />
                <AlertDescription>
                  {t("userQuota.row.duplicateBanner", {
                    count: entry.legacy_duplicates.length,
                  })}
                </AlertDescription>
              </Alert>
            )}
            {readOnly ? (
              <div className="text-sm text-muted-foreground p-2">
                {t("userQuota.row.readOnlyHint")}
              </div>
            ) : (
              <UserQuotaForm
                clients={clients}
                disabledClientNames={new Set()}
                lockClient
                defaultValues={{
                  client_name: entry.client_name,
                  listen_port_start: entry.listen_port_start,
                  listen_port_end: entry.listen_port_end,
                  protocols: entry.protocols,
                  unlimited: entry.unlimited,
                  bandwidth_in_bps: entry.cap?.bandwidth_in_bps ?? null,
                  bandwidth_out_bps: entry.cap?.bandwidth_out_bps ?? null,
                  new_connections_per_sec: entry.cap?.new_connections_per_sec ?? null,
                  concurrent_connections: entry.cap?.concurrent_connections ?? null,
                  bandwidth_in_burst: entry.cap?.bandwidth_in_burst ?? null,
                  bandwidth_out_burst: entry.cap?.bandwidth_out_burst ?? null,
                  new_connections_burst: entry.cap?.new_connections_burst ?? null,
                }}
                onSubmit={onSubmit}
                onCancel={() => setExpanded(false)}
                busy={update.isPending}
                serverError={serverError}
              />
            )}
          </TableCell>
        </TableRow>
      )}

      <ConfirmDialog
        open={confirmDelete}
        onOpenChange={setConfirmDelete}
        destructive
        title={t("userQuota.deleteTitle")}
        description={t("userQuota.deleteBody", { user: userId, client: entry.client_name })}
        busy={del.isPending}
        onConfirm={onDelete}
      />
    </>
  );
}
```

- [ ] **Step 4: 实现 `UserQuotaTable.tsx`**

```tsx
// webui/src/components/UserQuota/UserQuotaTable.tsx
import { Plus } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import {
  useCreateAccessEntry,
  type AccessEntry,
} from "@/api/access-entries";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableHead,
  TableHeader,
  TableRow,
  TableCell,
} from "@/components/ui/table";
import { UserQuotaForm, type UserQuotaFormSubmitValue } from "./UserQuotaForm";
import { UserQuotaRow } from "./UserQuotaRow";
import type { ClientLite } from "./ClientCombobox";

interface Props {
  userId: string;
  entries: AccessEntry[];
  clients: ClientLite[];
  readOnly: boolean;
}

export function UserQuotaTable({ userId, entries, clients, readOnly }: Props) {
  const { t } = useTranslation();
  const [adding, setAdding] = useState(false);
  const [serverError, setServerError] = useState<string | null>(null);
  const create = useCreateAccessEntry(userId);

  const disabledClientNames = new Set(entries.map((e) => e.client_name));

  async function onAdd(v: UserQuotaFormSubmitValue) {
    setServerError(null);
    try {
      await create.mutateAsync({
        user_id: userId,
        client_name: v.client_name,
        listen_port_start: v.listen_port_start,
        listen_port_end: v.listen_port_end,
        protocols: v.protocols,
        cap: v.cap,
      });
      toast.success(t("userQuota.toast.created", { client: v.client_name }));
      setAdding(false);
    } catch (err) {
      const msg = err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message;
      setServerError(msg);
      toast.error(t("userQuota.toast.createFailed"));
    }
  }

  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between">
        <p className="text-sm text-muted-foreground">{t("userQuota.tableHelp")}</p>
        {!readOnly && (
          <Button size="sm" onClick={() => setAdding(true)} disabled={adding}>
            <Plus className="h-4 w-4 mr-1" />
            {t("userQuota.add")}
          </Button>
        )}
      </div>

      <Table>
        <TableHeader>
          <TableRow>
            <TableHead aria-label="expand" />
            <TableHead>{t("userQuota.col.client")}</TableHead>
            <TableHead>{t("userQuota.col.portRange")}</TableHead>
            <TableHead>{t("userQuota.col.protocols")}</TableHead>
            <TableHead>{t("userQuota.col.bwIn")}</TableHead>
            <TableHead>{t("userQuota.col.bwOut")}</TableHead>
            <TableHead>{t("userQuota.col.concurrent")}</TableHead>
            <TableHead>{t("userQuota.col.newConnPerSec")}</TableHead>
            <TableHead>{t("userQuota.col.status")}</TableHead>
            <TableHead aria-label="actions" />
          </TableRow>
        </TableHeader>
        <TableBody>
          {entries.map((e) => (
            <UserQuotaRow
              key={`${e.user_id}::${e.client_name}`}
              userId={userId}
              entry={e}
              clients={clients}
              clientOnline={clients.find((c) => c.client_name === e.client_name)?.connected ?? false}
              readOnly={readOnly}
            />
          ))}
          {entries.length === 0 && !adding && (
            <TableRow>
              <TableCell colSpan={10} className="text-center text-muted-foreground py-6">
                {t("userQuota.empty")}
              </TableCell>
            </TableRow>
          )}
        </TableBody>
      </Table>

      {adding && (
        <UserQuotaForm
          clients={clients}
          disabledClientNames={disabledClientNames}
          onSubmit={onAdd}
          onCancel={() => {
            setAdding(false);
            setServerError(null);
          }}
          busy={create.isPending}
          serverError={serverError}
        />
      )}
    </div>
  );
}
```

- [ ] **Step 5: 运行测试**

```bash
cd webui && pnpm vitest run src/components/UserQuota/UserQuotaTable.test.tsx
```
Expected: PASS — 4 tests

- [ ] **Step 6: 提交**

```bash
git add webui/src/components/UserQuota/UserQuotaRow.tsx webui/src/components/UserQuota/UserQuotaTable.{tsx,test.tsx}
git commit -m "feat(webui): UserQuotaTable + UserQuotaRow with expand-to-edit"
```

---

## Task 9: i18n keys 添加

**Files:**
- Modify: `webui/src/i18n/en.json`
- Modify: `webui/src/i18n/zh-CN.json`

- [ ] **Step 1: 打开 `webui/src/i18n/en.json`，在末尾 `}` 前追加**

```json
,
  "userQuota": {
    "sectionTitle": "User quotas",
    "sectionHelp": "Each row authorises this user on a specific client with optional bandwidth/concurrency limits.",
    "tableHelp": "Per-client access and quota for this user.",
    "add": "Add quota",
    "empty": "No client access configured.",
    "unlimited": "Unlimited",
    "online": "Online",
    "offline": "Offline",
    "deleteTitle": "Remove user quota",
    "deleteBody": "Revoke {{user}}'s access to {{client}}? Any rules they pushed to this client will also be cleared.",
    "col": {
      "client": "Client",
      "portRange": "Ports",
      "protocols": "Protocols",
      "bwIn": "Bandwidth in",
      "bwOut": "Bandwidth out",
      "concurrent": "Concurrent",
      "newConnPerSec": "New/s",
      "status": "Status"
    },
    "row": {
      "expand": "Expand row",
      "collapse": "Collapse row",
      "duplicateTooltip": "Backend has additional grants for this user+client; editing will merge them.",
      "duplicateBanner": "Detected {{count}} extra legacy grant(s). Saving here will merge them into one.",
      "readOnlyHint": "Edit this entry from the user detail page."
    },
    "form": {
      "client": "Client",
      "portStart": "Port (start)",
      "portEnd": "Port (end)",
      "protocols": "Protocols",
      "unlimited": "Unlimited",
      "unlimitedHelp": "Allow access without bandwidth or concurrency limits.",
      "bandwidthIn": "Bandwidth in (bps)",
      "bandwidthOut": "Bandwidth out (bps)",
      "concurrent": "Concurrent connections",
      "newConnPerSec": "New connections / sec",
      "uncapped": "uncapped",
      "save": "Save"
    },
    "combobox": {
      "placeholder": "Select a client…",
      "search": "Search clients…",
      "empty": "No clients found.",
      "offline": "offline",
      "alreadyAssigned": "already assigned"
    },
    "toast": {
      "created": "Quota created for {{client}}",
      "updated": "Quota updated for {{client}}",
      "deleted": "Quota removed for {{client}}",
      "createFailed": "Create failed",
      "updateFailed": "Update failed",
      "deleteFailed": "Delete failed"
    },
    "redirectNotice": "Grants are now managed under each user's profile."
  }
```

(Also change `nav.grants` entry → delete it.)

- [ ] **Step 2: 同样在 `zh-CN.json` 末尾 `}` 前追加（中文）**

```json
,
  "userQuota": {
    "sectionTitle": "用户配额",
    "sectionHelp": "每一行授权此用户访问一台 client，并可选地设置带宽/并发限制。",
    "tableHelp": "此用户在各 client 上的访问与配额。",
    "add": "添加配额",
    "empty": "尚未配置任何 client 访问。",
    "unlimited": "不限制",
    "online": "在线",
    "offline": "离线",
    "deleteTitle": "移除用户配额",
    "deleteBody": "撤销 {{user}} 对 {{client}} 的访问？该用户在此 client 上推送的规则也会被一并清除。",
    "col": {
      "client": "Client",
      "portRange": "端口段",
      "protocols": "协议",
      "bwIn": "入带宽",
      "bwOut": "出带宽",
      "concurrent": "并发",
      "newConnPerSec": "新建/秒",
      "status": "状态"
    },
    "row": {
      "expand": "展开",
      "collapse": "收起",
      "duplicateTooltip": "后端存在该用户在此 client 上的额外 grant；编辑保存将合并它们。",
      "duplicateBanner": "检测到 {{count}} 条额外的历史 grant。保存编辑将合并为 1 条。",
      "readOnlyHint": "请到用户详情页编辑此条目。"
    },
    "form": {
      "client": "Client",
      "portStart": "起始端口",
      "portEnd": "结束端口",
      "protocols": "协议",
      "unlimited": "不限制",
      "unlimitedHelp": "允许访问但不设置带宽/并发上限。",
      "bandwidthIn": "入带宽 (bps)",
      "bandwidthOut": "出带宽 (bps)",
      "concurrent": "并发连接数",
      "newConnPerSec": "新建连接/秒",
      "uncapped": "不限",
      "save": "保存"
    },
    "combobox": {
      "placeholder": "选择 client…",
      "search": "搜索 client…",
      "empty": "未找到 client。",
      "offline": "离线",
      "alreadyAssigned": "已分配"
    },
    "toast": {
      "created": "已为 {{client}} 创建配额",
      "updated": "已更新 {{client}} 的配额",
      "deleted": "已移除 {{client}} 的配额",
      "createFailed": "创建失败",
      "updateFailed": "更新失败",
      "deleteFailed": "删除失败"
    },
    "redirectNotice": "Grants 管理已迁移到用户详情页。"
  }
```

- [ ] **Step 3: 跑测试套件确保 i18n 字段生效**

```bash
cd webui && pnpm vitest run
```
Expected: 全部 PASS（既有 + 新增）

- [ ] **Step 4: 提交**

```bash
git add webui/src/i18n/en.json webui/src/i18n/zh-CN.json
git commit -m "feat(webui): i18n keys for user quota"
```

---

## Task 10: UserDetail 集成

**Files:**
- Modify: `webui/src/pages/UserDetail.tsx`

- [ ] **Step 1: 替换 grants 卡片为 UserQuotaTable**

打开 `webui/src/pages/UserDetail.tsx`：

1. 顶部 import 区域追加：
   ```tsx
   import { useAccessEntries } from "@/api/access-entries";
   import { useClientsList } from "@/api/clients";
   import { UserQuotaTable } from "@/components/UserQuota/UserQuotaTable";
   ```

2. 删除既有 `useGrantsList` 与 `useRevokeGrant` 的 import 行与对应的 `const grants = ...` / `const revokeGrant = ...` 局部变量。

3. 在 `function UserDetailInner` 内部紧挨着原 grants 卡片**替换**为新 section：

   ```tsx
   const accessEntries = useAccessEntries(userId);
   const clientsQ = useClientsList();
   const clientLites = (clientsQ.data ?? []).map((c) => ({
     client_name: c.client_name,
     connected: c.connected,
   }));
   const isSuperadmin = identity?.role === "superadmin";
   ```

4. 把原 `<Card>` 标题为 `userDetail.grants` 的整段（约第 161–182 行）替换为：

   ```tsx
   <Card>
     <CardHeader>
       <CardTitle>{t("userQuota.sectionTitle")}</CardTitle>
     </CardHeader>
     <CardContent>
       {accessEntries.isLoading ? (
         <p className="text-sm text-muted-foreground">{t("confirm.busy")}</p>
       ) : (
         <UserQuotaTable
           userId={userId}
           entries={accessEntries.data ?? []}
           clients={clientLites}
           readOnly={!isSuperadmin}
         />
       )}
     </CardContent>
   </Card>
   ```

5. 同时把 `ConfirmDialog`（删除用户）的 `dependents` 数组中 `...((grants.data ?? []).map(...))` 改为：

   ```tsx
   ...((accessEntries.data ?? []).map((e) => `quota ${e.client_name}`)),
   ```

- [ ] **Step 2: 跑 typecheck + 既有测试**

```bash
cd webui && pnpm tsc -b --noEmit && pnpm vitest run
```
Expected: no errors

- [ ] **Step 3: 提交**

```bash
git add webui/src/pages/UserDetail.tsx
git commit -m "feat(webui): integrate UserQuotaTable into UserDetail page"
```

---

## Task 11: UserCreate 集成（可选初始配额）

**Files:**
- Modify: `webui/src/pages/UserCreate.tsx`

- [ ] **Step 1: 用 `Collapsible` 包一个可选区块**

打开 `webui/src/pages/UserCreate.tsx`：

1. 顶部追加 imports：
   ```tsx
   import { useState } from "react";
   import { toast } from "sonner";
   import { ApiError } from "@/api/client";
   import { useCreateAccessEntry } from "@/api/access-entries";
   import { useClientsList } from "@/api/clients";
   import { UserQuotaForm, type UserQuotaFormSubmitValue } from "@/components/UserQuota/UserQuotaForm";
   import { ChevronDown, ChevronRight } from "lucide-react";
   ```

2. 在 `UserCreate` 函数内部加局部 state：
   ```tsx
   const [showInitialQuota, setShowInitialQuota] = useState(false);
   const [pendingQuota, setPendingQuota] = useState<UserQuotaFormSubmitValue | null>(null);
   const clientsQ = useClientsList();
   const clientLites = (clientsQ.data ?? []).map((c) => ({
     client_name: c.client_name,
     connected: c.connected,
   }));
   const createEntry = useCreateAccessEntry(userId);
   ```

3. 修改 `handleSubmit` 主提交流程，在 `navigate(...)` 之前插入：

   ```tsx
   if (pendingQuota && res.user_id) {
     try {
       await createEntry.mutateAsync({
         user_id: res.user_id,
         client_name: pendingQuota.client_name,
         listen_port_start: pendingQuota.listen_port_start,
         listen_port_end: pendingQuota.listen_port_end,
         protocols: pendingQuota.protocols,
         cap: pendingQuota.cap,
       });
       toast.success(t("userQuota.toast.created", { client: pendingQuota.client_name }));
     } catch (err) {
       const msg = err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message;
       toast.warning(`${t("userQuota.toast.createFailed")}: ${msg}`);
     }
   }
   navigate(`/users/${res.user_id}`);
   ```

4. 在 form 表单底部 buttons 之前插入 collapsible 区块：

   ```tsx
   <div className="border-t pt-4">
     <button
       type="button"
       className="flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
       onClick={() => setShowInitialQuota((v) => !v)}
     >
       {showInitialQuota ? <ChevronDown className="h-4 w-4" /> : <ChevronRight className="h-4 w-4" />}
       {t("userCreate.initialQuotaToggle")}
     </button>
     {showInitialQuota && (
       <div className="mt-3">
         <UserQuotaForm
           clients={clientLites}
           disabledClientNames={new Set()}
           onSubmit={(v) => {
             setPendingQuota(v);
           }}
           onCancel={() => {
             setShowInitialQuota(false);
             setPendingQuota(null);
           }}
         />
         {pendingQuota && (
           <p className="text-xs text-muted-foreground mt-2">
             {t("userCreate.initialQuotaPending", { client: pendingQuota.client_name })}
           </p>
         )}
       </div>
     )}
   </div>
   ```

5. 在 i18n 文件 `userCreate` 命名空间下追加 keys（en + zh-CN）：

   en.json (under existing `userCreate`):
   ```
   "initialQuotaToggle": "Assign initial quota (optional)",
   "initialQuotaPending": "Will provision quota on {{client}} after user creation."
   ```
   zh-CN.json:
   ```
   "initialQuotaToggle": "分配初始配额（可选）",
   "initialQuotaPending": "用户创建后将在 {{client}} 上分配该配额。"
   ```

- [ ] **Step 2: 跑 build + 测试**

```bash
cd webui && pnpm tsc -b --noEmit && pnpm vitest run
```

- [ ] **Step 3: 提交**

```bash
git add webui/src/pages/UserCreate.tsx webui/src/i18n/
git commit -m "feat(webui): optional initial quota on UserCreate"
```

---

## Task 12: ClientDetail OwnerQuotasTab 改为只读 + 跳转

**Files:**
- Modify: `webui/src/pages/ClientDetail.tsx`

- [ ] **Step 1: 把 `OwnerQuotasTab` 的 `editingOwner` 编辑分支替换为跳转**

打开 `ClientDetail.tsx`：

1. 在顶部 import 区追加 `import { useNavigate } from "react-router-dom"`（已存在则跳过）。

2. 在 `OwnerQuotasTab` 函数体顶部加：
   ```tsx
   const navigate = useNavigate();
   ```

3. 把 `setEditingOwner` 相关逻辑替换为：
   - `actions` 列的按钮 onClick 改为 `navigate(\`/users/${encodeURIComponent(o.owner_id)}#quotas\`)`
   - 按钮文案改为 `t("ownerQuotas.openInUser")`
   - 删除 `<AddOwnerCapDialog ...>` 与 `{editingOwner && <OwnerQuotaEditor ...>}` 的整块 JSX
   - 把 `setEditingOwner`、`addDialogOpen`、`setAddDialogOpen` 等所有 state 删除
   - 删除 `+ Add owner cap` 按钮（顶部 `<Button size="sm" onClick={() => setAddDialogOpen(true)}>` 整块）
   - 在该区块顶部加一行 i18n hint：
     ```tsx
     <p className="text-sm text-muted-foreground">{t("ownerQuotas.movedHint")}</p>
     ```

4. 把同文件下方的 `AddOwnerCapDialog` 与 `OwnerQuotaEditor` 两个组件**删除**（无人使用）。

5. i18n keys 追加：

   en.json under `ownerQuotas`:
   ```
   "openInUser": "Open in user",
   "movedHint": "Owner quota editing has moved to the user detail page."
   ```
   zh-CN.json:
   ```
   "openInUser": "在用户页编辑",
   "movedHint": "Owner 配额编辑已迁移到用户详情页。"
   ```

- [ ] **Step 2: typecheck**

```bash
cd webui && pnpm tsc -b --noEmit
```

- [ ] **Step 3: 提交**

```bash
git add webui/src/pages/ClientDetail.tsx webui/src/i18n/
git commit -m "feat(webui): ClientDetail owner quotas now read-only; edits go to user page"
```

---

## Task 13: 移除 Grants 导航 + redirect

**Files:**
- Modify: `webui/src/components/Nav.tsx`
- Modify: `webui/src/App.tsx`
- Modify: `webui/src/i18n/en.json`、`zh-CN.json`

- [ ] **Step 1: Nav.tsx 删 Grants 项**

```tsx
// 在 ITEMS 数组中删除：
{ to: "/grants", i18nKey: "nav.grants", visible: () => true },
```

- [ ] **Step 2: App.tsx 把 /grants、/grants/new 替换为 redirect**

把现有 `/grants` 和 `/grants/new` 两个 `<Route>` 块替换为：

```tsx
<Route path="/grants" element={<Navigate to="/users" replace />} />
<Route path="/grants/new" element={<Navigate to="/users" replace />} />
```

并把顶部 lazy imports 中的 `GrantsList`、`GrantCreate` 两行**删除**。

- [ ] **Step 3: 删除被淘汰的页面文件**

```bash
git rm webui/src/pages/GrantsList.tsx webui/src/pages/GrantCreate.tsx
```

- [ ] **Step 4: 删 i18n 的 `nav.grants` 与 `grantCreate` / `grantsList` 命名空间**

在 `en.json` 和 `zh-CN.json` 中删除 `nav.grants`、`grants:` block、`grantCreate:` block、`grantsList:` block。

- [ ] **Step 5: 跑 build + test**

```bash
cd webui && pnpm tsc -b --noEmit && pnpm vitest run && pnpm build
```
Expected: build succeeds, size-limit passes

- [ ] **Step 6: 提交**

```bash
git add -A
git commit -m "feat(webui): retire /grants routes (now under user quotas)"
```

---

## Task 14: 端到端手测 + UI 回归

**Files:** (no code, just verification)

- [ ] **Step 1: 起 dev 环境**

```bash
make clean && make dev
```

等 banner 打印 `temporary_password=...`，登录 `_superadmin` / 该密码 @ http://localhost:5173

- [ ] **Step 2: 跑主路径**

1. 进入 Users → 选 `_superadmin` → 看到 "User quotas" 区块
2. 点 "+ Add quota" → 选 client → 设端口 / 协议 / cap → Save → 看到表格新增一行
3. 展开行 → 切 Unlimited → Save → 行带宽列变为 "Unlimited" badge
4. 删除行 → 二次确认 → 行消失

- [ ] **Step 3: 跑反向路径**

打开 DevTools Network 面板 → 通过 mock/block `PUT .../rate-limit` → 看是否 toast 红条 + 自动 DELETE grant 回滚

- [ ] **Step 4: 跑 ClientDetail 验证**

进入 `/clients/<some>` → "Owner quotas" tab → 看到只读列表 + "Open in user" 按钮跳转

- [ ] **Step 5: 跑导航回归**

访问 `/grants` → 自动 redirect 到 `/users`

- [ ] **Step 6: 跑 ui-test skill**

```
使用 ui-test skill 在 dev server 上跑一次 git-diff-aware 测试
```

预期：无 a11y / 布局 regression

- [ ] **Step 7: 跑完整测试套件**

```bash
cd webui && pnpm vitest run && pnpm build
```

- [ ] **Step 8: 收尾提交（如有 i18n 漏键修补、bundle size 调整等）**

```bash
git status
git add -A
git commit -m "chore(webui): post-QA fixups for user quotas"  # 仅在确有改动时
```

---

## Spec Coverage 自检

- §1 概念模型 → Task 3 (`AccessEntry`) + Task 2 (zod schema)
- §2 目标（User-centric 入口） → Task 10 (UserDetail) + Task 11 (UserCreate)
- §3 概念模型映射 → Task 3 join + Task 5 mutations
- §4.1 路由 → Task 13
- §4.2 组件清单 → Tasks 6–8
- §4.3 表格列设计 → Task 8 (UserQuotaTable)
- §4.4 shadcn 组件清单 → Task 1
- §4.5 表单技术栈 → Task 7 (react-hook-form + zod)
- §5 数据流 / 客户端补偿 → Task 5
- §6.1 老数据归一化 → Task 3 (`legacy_duplicates` 字段) + Task 5 (`legacy_duplicate_ids` 处理)
- §6.2 表单校验 → Task 2 zod
- §6.3 权限 → Task 8 (`readOnly` prop)
- §6.4 离线 client → Task 8 (status 列) + Task 6 (Combobox 灰显)
- §6.5 删除确认 → Task 8 (ConfirmDialog)
- §7 UserCreate 集成 → Task 11
- §8 测试策略 → Tasks 2/3/5/6/7/8 内部测试 + Task 14 端到端
- §9 风险 → 由 toast + Alert + ConfirmDialog 文案覆盖（Tasks 8、9）
- §10 PR 切片 → Tasks 1–14 大致对应
