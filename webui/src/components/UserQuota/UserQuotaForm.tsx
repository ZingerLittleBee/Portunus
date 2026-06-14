// webui/src/components/UserQuota/UserQuotaForm.tsx
import { useForm, Controller } from "react-hook-form";
import { useTranslation } from "react-i18next";
import type { z } from "zod";

import type { RateLimit } from "@/api/types";
import { zResolver } from "@/lib/zod-resolver";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSet,
} from "@/components/ui/field";
import { cn } from "@/lib/cn";
import { ClientCombobox, type ClientLite } from "./ClientCombobox";
import { accessEntrySchema } from "./format";

export type FormValues = z.infer<typeof accessEntrySchema>;

export interface UserQuotaFormSubmitValue {
  /// 015-client-stable-id (US3): stable id for URL addressing.
  client_id: string;
  /// Display name (resolved from the clients list) for grant body + toasts.
  client_name: string;
  listen_port_start: number;
  listen_port_end: number;
  protocols: ("tcp" | "udp")[];
  cap: RateLimit | undefined;
}

interface Props {
  clients: ClientLite[];
  disabledClientIds: Set<string>;
  /// Lock the client picker (used when editing an existing entry).
  lockClient?: boolean;
  // allow explicit undefined for exactOptionalPropertyTypes
  defaultValues?: Partial<FormValues> | undefined;
  onSubmit: (v: UserQuotaFormSubmitValue) => void | Promise<void>;
  onCancel: () => void;
  busy?: boolean;
  framed?: boolean;
  /// Render as a plain `<div>` (not a `<form>`) so this can be embedded
  /// inside another `<form>` without nesting form elements — nested forms
  /// are invalid HTML and cause the inner Save to trigger the outer form's
  /// native submit. When set, Save validates + reports via `onSubmit`
  /// without a form submission.
  nested?: boolean;
  popoverContainer?: HTMLElement | null | undefined;
  serverError?: string | null;
}

const DEFAULTS: FormValues = {
  client_id: "",
  listen_port_start: 10_000,
  listen_port_end: 19_999,
  protocols: ["tcp", "udp"],
  unlimited: true,
  bandwidth_in_bps: null,
  bandwidth_out_bps: null,
  new_connections_per_sec: null,
  concurrent_connections: null,
  bandwidth_in_burst: null,
  bandwidth_out_burst: null,
  new_connections_burst: null,
};

function nullableInt(v: unknown): number | null {
  if (v === "" || v === null || v === undefined) return null;
  const n = Number(v);
  return Number.isNaN(n) ? null : n;
}

export function UserQuotaForm({
  clients,
  disabledClientIds,
  lockClient,
  defaultValues,
  onSubmit,
  onCancel,
  busy,
  framed = true,
  nested = false,
  popoverContainer,
  serverError,
}: Props) {
  const { t } = useTranslation();
  const form = useForm<FormValues>({
    resolver: zResolver<FormValues>(accessEntrySchema),
    defaultValues: { ...DEFAULTS, ...defaultValues },
  });
  const { register, handleSubmit, watch, control, formState } = form;
  const unlimited = watch("unlimited");

  async function submit(v: FormValues) {
    let cap: RateLimit | undefined;
    if (!v.unlimited) {
      const c: RateLimit = {};
      if (v.bandwidth_in_bps != null) c.bandwidth_in_bps = v.bandwidth_in_bps;
      if (v.bandwidth_out_bps != null) c.bandwidth_out_bps = v.bandwidth_out_bps;
      if (v.new_connections_per_sec != null) c.new_connections_per_sec = v.new_connections_per_sec;
      if (v.concurrent_connections != null) c.concurrent_connections = v.concurrent_connections;
      if (v.bandwidth_in_burst != null) c.bandwidth_in_burst = v.bandwidth_in_burst;
      if (v.bandwidth_out_burst != null) c.bandwidth_out_burst = v.bandwidth_out_burst;
      if (v.new_connections_burst != null) c.new_connections_burst = v.new_connections_burst;
      cap = c;
    }
    const clientName =
      clients.find((c) => c.client_id === v.client_id)?.client_name ?? "";
    await onSubmit({
      client_id: v.client_id,
      client_name: clientName,
      listen_port_start: v.listen_port_start,
      listen_port_end: v.listen_port_end,
      protocols: v.protocols,
      cap,
    });
  }

  const rootClassName = cn("flex flex-col", framed && "rounded-md border bg-card p-4");
  const body = (
      <FieldGroup>
        <Field data-invalid={formState.errors.client_id ? true : undefined}>
          <FieldLabel htmlFor="quota-client">{t("userQuota.form.client")}</FieldLabel>
          <Controller
            name="client_id"
            control={control}
            render={({ field }) => (
              <ClientCombobox
                clients={clients}
                value={field.value}
                onChange={field.onChange}
                disabledClientIds={disabledClientIds}
                disabled={lockClient ?? false}
                popoverContainer={popoverContainer}
              />
            )}
          />
          {formState.errors.client_id && (
            <FieldError errors={[formState.errors.client_id]} />
          )}
        </Field>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Field data-invalid={formState.errors.listen_port_start ? true : undefined}>
            <FieldLabel htmlFor="port-start">{t("userQuota.form.portStart")}</FieldLabel>
            <Input
              id="port-start"
              type="number"
              min={1}
              max={65535}
              aria-invalid={formState.errors.listen_port_start ? true : undefined}
              {...register("listen_port_start", { valueAsNumber: true })}
            />
          </Field>
          <Field data-invalid={formState.errors.listen_port_end ? true : undefined}>
            <FieldLabel htmlFor="port-end">{t("userQuota.form.portEnd")}</FieldLabel>
            <Input
              id="port-end"
              type="number"
              min={1}
              max={65535}
              aria-invalid={formState.errors.listen_port_end ? true : undefined}
              {...register("listen_port_end", { valueAsNumber: true })}
            />
          </Field>
        </div>
        {formState.errors.listen_port_end && (
          <FieldError errors={[formState.errors.listen_port_end]} />
        )}

        <Controller
          name="protocols"
          control={control}
          render={({ field }) => (
            <FieldSet>
              <FieldLegend variant="label">{t("userQuota.form.protocols")}</FieldLegend>
              <div className="flex gap-4">
                {(["tcp", "udp"] as const).map((p) => (
                  <Field orientation="horizontal" key={p} className="w-auto">
                    <Checkbox
                      id={`protocol-${p}`}
                      checked={field.value.includes(p)}
                      onCheckedChange={(checked) => {
                        const next = checked
                          ? Array.from(new Set([...field.value, p]))
                          : field.value.filter((x) => x !== p);
                        field.onChange(next);
                      }}
                    />
                    <FieldLabel htmlFor={`protocol-${p}`} className="font-normal">
                      {p.toUpperCase()}
                    </FieldLabel>
                  </Field>
                ))}
              </div>
              {formState.errors.protocols && (
                <FieldError errors={[formState.errors.protocols]} />
              )}
            </FieldSet>
          )}
        />

        <Field orientation="horizontal" className="border-t pt-4">
          <FieldContent>
            <FieldLabel htmlFor="unlimited">{t("userQuota.form.unlimited")}</FieldLabel>
            <FieldDescription>{t("userQuota.form.unlimitedHelp")}</FieldDescription>
          </FieldContent>
          <Controller
            name="unlimited"
            control={control}
            render={({ field }) => (
              <Switch id="unlimited" checked={field.value} onCheckedChange={field.onChange} />
            )}
          />
        </Field>

        {!unlimited && (
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
            <Field data-invalid={formState.errors.bandwidth_in_bps ? true : undefined}>
              <FieldLabel htmlFor="bw-in">{t("userQuota.form.bandwidthIn")}</FieldLabel>
              <Input
                id="bw-in"
                type="number"
                min={1}
                placeholder={t("userQuota.form.uncapped")}
                aria-invalid={formState.errors.bandwidth_in_bps ? true : undefined}
                {...register("bandwidth_in_bps", { setValueAs: nullableInt })}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="bw-out">{t("userQuota.form.bandwidthOut")}</FieldLabel>
              <Input
                id="bw-out"
                type="number"
                min={1}
                placeholder={t("userQuota.form.uncapped")}
                {...register("bandwidth_out_bps", { setValueAs: nullableInt })}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="conc">{t("userQuota.form.concurrent")}</FieldLabel>
              <Input
                id="conc"
                type="number"
                min={1}
                placeholder={t("userQuota.form.uncapped")}
                {...register("concurrent_connections", { setValueAs: nullableInt })}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor="ncps">{t("userQuota.form.newConnPerSec")}</FieldLabel>
              <Input
                id="ncps"
                type="number"
                min={1}
                placeholder={t("userQuota.form.uncapped")}
                {...register("new_connections_per_sec", { setValueAs: nullableInt })}
              />
            </Field>
            {formState.errors.bandwidth_in_bps && (
              <FieldError className="sm:col-span-2" errors={[formState.errors.bandwidth_in_bps]} />
            )}
          </div>
        )}

        {serverError && (
          <FieldError>{serverError}</FieldError>
        )}

        <div className="flex flex-col gap-2 sm:flex-row sm:justify-end">
          <Button
            type={nested ? "button" : "submit"}
            onClick={nested ? handleSubmit(submit) : undefined}
            disabled={busy}
          >
            {busy ? t("confirm.busy") : t("userQuota.form.save")}
          </Button>
          <Button type="button" variant="outline" onClick={onCancel}>
            {t("confirm.cancel")}
          </Button>
        </div>
      </FieldGroup>
  );
  if (nested) {
    return <div className={rootClassName}>{body}</div>;
  }
  return (
    <form onSubmit={handleSubmit(submit)} className={rootClassName}>
      {body}
    </form>
  );
}
