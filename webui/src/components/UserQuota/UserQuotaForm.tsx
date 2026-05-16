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
import { cn } from "@/lib/cn";
import { ClientCombobox, type ClientLite } from "./ClientCombobox";
import { accessEntrySchema } from "./format";

export type FormValues = z.infer<typeof accessEntrySchema>;

export interface UserQuotaFormSubmitValue {
  client_name: string;
  listen_port_start: number;
  listen_port_end: number;
  protocols: ("tcp" | "udp")[];
  cap: RateLimit | undefined;
}

interface Props {
  clients: ClientLite[];
  disabledClientNames: Set<string>;
  /// Lock the client picker (used when editing an existing entry).
  lockClient?: boolean;
  // allow explicit undefined for exactOptionalPropertyTypes
  defaultValues?: Partial<FormValues> | undefined;
  onSubmit: (v: UserQuotaFormSubmitValue) => void | Promise<void>;
  onCancel: () => void;
  busy?: boolean;
  framed?: boolean;
  popoverContainer?: HTMLElement | null | undefined;
  serverError?: string | null;
}

const DEFAULTS: FormValues = {
  client_name: "",
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

// Cast needed: @hookform/resolvers 5.2.2 types were written for zod 4.0.x
// (expects _zod.version.minor === 0) but zod 4.4.x has minor === 4.
// The runtime works correctly; this cast papers over the type mismatch.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const schemaForResolver = accessEntrySchema as any;

export function UserQuotaForm({
  clients,
  disabledClientNames,
  lockClient,
  defaultValues,
  onSubmit,
  onCancel,
  busy,
  framed = true,
  popoverContainer,
  serverError,
}: Props) {
  const { t } = useTranslation();
  const form = useForm<FormValues>({
    resolver: zodResolver(schemaForResolver),
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
    await onSubmit({
      client_name: v.client_name,
      listen_port_start: v.listen_port_start,
      listen_port_end: v.listen_port_end,
      protocols: v.protocols,
      cap,
    });
  }

  return (
    <form
      onSubmit={handleSubmit(submit)}
      className={cn("flex flex-col gap-4", framed && "rounded-md border bg-card p-4")}
    >
      <div className="flex flex-col gap-2">
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
              disabled={lockClient ?? false}
              popoverContainer={popoverContainer}
            />
          )}
        />
        {formState.errors.client_name && (
          <p className="text-sm text-destructive">{formState.errors.client_name.message}</p>
        )}
      </div>

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <div className="flex flex-col gap-1">
          <Label htmlFor="port-start">{t("userQuota.form.portStart")}</Label>
          <Input
            id="port-start"
            type="number"
            min={1}
            max={65535}
            {...register("listen_port_start", { valueAsNumber: true })}
          />
        </div>
        <div className="flex flex-col gap-1">
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

      <div className="flex flex-col gap-2">
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

      <div className="flex flex-col gap-3 border-t pt-4 sm:flex-row sm:items-center sm:justify-between">
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
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <div className="flex flex-col gap-1">
            <Label htmlFor="bw-in">{t("userQuota.form.bandwidthIn")}</Label>
            <Input
              id="bw-in"
              type="number"
              min={1}
              placeholder={t("userQuota.form.uncapped")}
              {...register("bandwidth_in_bps", { setValueAs: nullableInt })}
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="bw-out">{t("userQuota.form.bandwidthOut")}</Label>
            <Input
              id="bw-out"
              type="number"
              min={1}
              placeholder={t("userQuota.form.uncapped")}
              {...register("bandwidth_out_bps", { setValueAs: nullableInt })}
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="conc">{t("userQuota.form.concurrent")}</Label>
            <Input
              id="conc"
              type="number"
              min={1}
              placeholder={t("userQuota.form.uncapped")}
              {...register("concurrent_connections", { setValueAs: nullableInt })}
            />
          </div>
          <div className="flex flex-col gap-1">
            <Label htmlFor="ncps">{t("userQuota.form.newConnPerSec")}</Label>
            <Input
              id="ncps"
              type="number"
              min={1}
              placeholder={t("userQuota.form.uncapped")}
              {...register("new_connections_per_sec", { setValueAs: nullableInt })}
            />
          </div>
          {formState.errors.bandwidth_in_bps && (
            <p className="text-sm text-destructive sm:col-span-2">
              {formState.errors.bandwidth_in_bps.message}
            </p>
          )}
        </div>
      )}

      {serverError && <p className="text-sm text-destructive">{serverError}</p>}

      <div className="flex flex-col gap-2 sm:flex-row sm:justify-end">
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
