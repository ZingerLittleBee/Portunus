# 表单统一迁移到 shadcn Field 体系 — 设计

**日期**：2026-06-02
**范围**：`webui/` 前端全部表单
**状态**：已批准，进入实现

## 目标

把全项目 8 个表单统一到 shadcn 官方推荐的表单栈，获得一致的**布局**、**无障碍**和**校验**：

- 布局：`div + space-y-*` → `FieldGroup` / `Field`
- 无障碍：临时 `<p>` 错误 → `FieldError` + `data-invalid`/`aria-invalid` 关联
- 校验：7 个裸 `useState` 表单统一迁到 `react-hook-form` + `zod`（`UserQuotaForm` 已是该栈）

## 技术栈（shadcn 官方推荐）

- `react-hook-form` 7.75 + `zod` 4.4 + `@hookform/resolvers` 5.2（项目已有，无需新增依赖）
- shadcn `Field` 组件族（需 `pnpm dlx shadcn add field`）
- 集成模式（与官方 React Hook Form 指南逐行对应）：
  ```tsx
  <Controller name={name} control={control} render={({ field, fieldState }) => (
    <Field data-invalid={fieldState.invalid}>
      <FieldLabel htmlFor={field.name}>{label}</FieldLabel>
      <Input {...field} id={field.name} aria-invalid={fieldState.invalid} />
      {description && <FieldDescription>{description}</FieldDescription>}
      {fieldState.invalid && <FieldError errors={[fieldState.error]} />}
    </Field>
  )} />
  ```
- **删除** `src/components/ui/form.tsx`（旧版 RHF 绑定 `Form/FormField/...`，全项目无业务引用，已被 Field+Controller 取代）。

## 共享 helper（唯一自有抽象）

新增 `src/components/form/`，把官方样板收敛成一行调用。一律走 `Controller`（不用 `register`）：

| helper | 包装控件 | 用途 |
|--------|---------|------|
| `FormTextField` | `Input`（text/password/number）| 高频文本/数字字段 |
| `FormSelectField` | `Select` | RuleForm proxy protocol |
| `FormToggleField` | `ToggleGroup`（single）| RuleForm 单/多目标、TCP/UDP |
| `FormCheckboxField` | `Checkbox`（horizontal Field）| UserCreateForm 强制改密 |
| `FormSwitchField` | `Switch`（horizontal Field）| UserQuotaForm unlimited |

**不强行抽象**：动态数组行（RuleForm multi-target）、`ClientCombobox`、`protocols` 复选组 —— 直接内联 `Controller` + `Field`/`FieldSet`。

## 服务端错误（ApiError）展示

统一改为**表单级 `Alert`**（`variant="destructive"`），放在表单内提交按钮上方；字段级校验走 `FieldError`，两者职责分明。

## 校验 UX

`useForm({ mode: "onTouched" })`：失焦后校验、改动后实时复验，避免一上来就报错。

## 逐表单迁移清单（渐进，每个独立可测）

按从简到难顺序：

1. **LoginPage**：两段流程（登录 / 强制改密）各一个 `useForm` + zod；改密含两次密码一致性 refine；失败用 Alert。
2. **OnboardingPage**：单 `useForm` + zod；密码一致性 refine。
3. **ClientProvisionForm**：单 `useForm` + zod（name/address 必填）。
4. **UserCreateForm**：`useForm` + zod（user_id 正则 `^[a-z][a-z0-9-_]*$`）；强制改密用 `FormCheckboxField`；内嵌的初始配额仍走 `UserQuotaForm` 子组件，不并入。
5. **UserDetail 重置密码弹窗**（**范围例外**）：位于 `ConfirmDialog` body，由 dialog 持有提交、无校验规则。仅套 `Field`/`FieldGroup` 布局 + 已迁好的 shadcn `Checkbox`，**保留 useState**，不引入 RHF。文档显式记录此例外。
6. **UserQuotaForm**：已是 RHF+zod，仅把布局换成 `Field`/`FieldSet`/`FieldError`。**必须保留测试契约**：`switch name=/unlimited/i`、`checkbox name="TCP"/"UDP"`、`labelText=/bandwidth in/i`、端口倒置阻止提交、源码含 `sm:justify-end`。
7. **RuleForm**：单 `useForm` + zod 管全部字段（含 rate-limit 子对象与 multi-target 数组，用 `useFieldArray`）；条件 SNI、mode toggle、protocol toggle 走 helper / 内联 Controller。
8. **RateLimitForm**：编辑器拆成绑定父 `control` 的 Field 字段组（7 个 rate-limit 字段并入 RuleForm 的 zod schema）；纯函数 `summarizeRateLimit` 原样保留（`RulesList` 仍用）。

## 数据流不变量（必须保持）

- RuleForm 提交体：空 listen_port_end / target_port_end / sni / rate-limit 字段照旧**省略**（保 SC-004 wire 字节稳定）。
- rate-limit 全空 → 不发 `rate_limit` 字段。
- SNI 仅 TCP 单端口可填。
- multi-target 的 `proxy_protocol` 仅 TCP 生效。

## 验证

- `pnpm exec tsc -b`、`pnpm lint`、`pnpm test`（vitest）全绿，尤其 `UserQuotaForm.test.tsx` 不回归。
- 手动：`pnpm dev` 起服务，用 Claude in Chrome 打开 http://localhost:5173，以 `_superadmin` 登录，逐表单走查（登录/改密、建用户、建规则含 rate-limit、配额）。

## 不做（YAGNI）

- 不引入 `InputGroup`/`Textarea`/`InputOTP`（项目无对应场景）。
- 不重构 `ConfirmDialog`、`ClientCombobox` 内部。
- 不动后端 / wire 协议。
