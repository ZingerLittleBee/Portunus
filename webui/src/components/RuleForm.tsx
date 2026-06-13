import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useForm, useFieldArray, Controller } from "react-hook-form";
import { z } from "zod";
import { Plus, Trash2 } from "lucide-react";

import { usePushRule } from "@/api/rules";
import { useClientsList } from "@/api/clients";
import { formatApiError } from "@/api/client";
import { zResolver } from "@/lib/zod-resolver";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Alert, AlertDescription } from "@/components/ui/alert";
import {
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
} from "@/components/ui/field";
import { ClientCombobox } from "@/components/UserQuota/ClientCombobox";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  FormTextField,
  FormToggleField,
} from "@/components/form/fields";
import {
  EMPTY_RATE_LIMIT_FORM,
  RateLimitForm,
  formStateToRateLimit,
} from "@/components/RateLimitForm";

const PROXY_PROTOCOL_NONE = "__none";

// Stable reference so the combobox doesn't see a new Set every render.
const EMPTY_DISABLED_CLIENTS = new Set<string>();

const isPort = (s: string) => /^\d{1,5}$/.test(s) && Number(s) >= 1 && Number(s) <= 65535;

interface RuleFormProps {
  /** Called with the new rule id after a successful push. */
  onSuccess: (ruleId: number) => void;
  /** Called when the operator dismisses the form. */
  onCancel: () => void;
}

export function RuleForm({ onSuccess, onCancel }: RuleFormProps) {
  const { t } = useTranslation();
  const push = usePushRule();
  const [error, setError] = useState<string | null>(null);
  // C1: the client is chosen from the connected/granted clients list
  // (addressed by stable id) rather than typed free-form. The wire still
  // carries the display name, which the server resolves to a handle.
  const clientsQ = useClientsList();
  const clientLites = (clientsQ.data ?? []).map((c) => ({
    client_id: c.client_id,
    client_name: c.client_name,
    connected: c.connected,
  }));

  const rateLimitSchema = z.object({
    bandwidth_in_bps: z.string(),
    bandwidth_out_bps: z.string(),
    new_connections_per_sec: z.string(),
    concurrent_connections: z.string(),
    bandwidth_in_burst: z.string(),
    bandwidth_out_burst: z.string(),
    new_connections_burst: z.string(),
  });

  const schema = z
    .object({
      client: z.string().trim().min(1, t("rulePush.requiredField")),
      listenStart: z.string().refine(isPort, t("rulePush.invalidPort")),
      listenEnd: z.string().refine((s) => s === "" || isPort(s), t("rulePush.invalidPort")),
      mode: z.enum(["single", "multi"]),
      target: z.string(),
      targetStart: z.string(),
      targetEnd: z.string(),
      targets: z.array(
        z.object({
          host: z.string(),
          port: z.string(),
          proxyProtocol: z.enum(["", "v1", "v2"]),
        }),
      ),
      healthCheckInterval: z.string(),
      protocol: z.enum(["tcp", "udp"]),
      sniPattern: z.string(),
      rateLimit: rateLimitSchema,
    })
    .superRefine((v, ctx) => {
      if (v.mode === "single") {
        if (!v.target.trim()) {
          ctx.addIssue({ code: "custom", path: ["target"], message: t("rulePush.requiredField") });
        }
        if (!isPort(v.targetStart)) {
          ctx.addIssue({ code: "custom", path: ["targetStart"], message: t("rulePush.invalidPort") });
        }
        if (v.targetEnd !== "" && !isPort(v.targetEnd)) {
          ctx.addIssue({ code: "custom", path: ["targetEnd"], message: t("rulePush.invalidPort") });
        }
        // 011-rate-limiting-qos: caps are only accepted on the targets[]
        // shape, which cannot express a target *port range*. Block the
        // unsupported combination up front instead of sending a request the
        // server is bound to reject.
        if (v.targetEnd !== "" && isPort(v.targetEnd) && formStateToRateLimit(v.rateLimit)) {
          ctx.addIssue({
            code: "custom",
            path: ["targetEnd"],
            message: t("rulePush.rateLimitRangeConflict"),
          });
        }
      } else {
        v.targets.forEach((row, i) => {
          if (!row.host.trim()) {
            ctx.addIssue({
              code: "custom",
              path: ["targets", i, "host"],
              message: t("rulePush.requiredField"),
            });
          }
          if (!isPort(row.port)) {
            ctx.addIssue({
              code: "custom",
              path: ["targets", i, "port"],
              message: t("rulePush.invalidPort"),
            });
          }
        });
        if (v.healthCheckInterval !== "") {
          const n = Number(v.healthCheckInterval);
          if (!Number.isInteger(n) || n < 1 || n > 3600) {
            ctx.addIssue({
              code: "custom",
              path: ["healthCheckInterval"],
              message: t("rulePush.invalidHealthCheckInterval"),
            });
          }
        }
      }
    });

  type RuleFormValues = z.infer<typeof schema>;

  const form = useForm<RuleFormValues>({
    resolver: zResolver<RuleFormValues>(schema),
    defaultValues: {
      client: "",
      listenStart: "30000",
      listenEnd: "",
      mode: "single",
      target: "127.0.0.1",
      targetStart: "9000",
      targetEnd: "",
      // 007-multi-target-failover T046: the legacy single-target shape stays
      // the default to keep operator muscle memory intact.
      targets: [
        { host: "127.0.0.1", port: "9000", proxyProtocol: "" },
        { host: "127.0.0.1", port: "9001", proxyProtocol: "" },
      ],
      healthCheckInterval: "",
      protocol: "tcp",
      sniPattern: "",
      // 011-rate-limiting-qos T039: empty form = no rate_limit field on the
      // wire (preserves SC-004 byte-stability for opt-out rules).
      rateLimit: { ...EMPTY_RATE_LIMIT_FORM },
    },
  });
  const { control, watch, formState, handleSubmit } = form;
  const { fields, append, remove } = useFieldArray({ control, name: "targets" });

  const mode = watch("mode");
  const protocol = watch("protocol");
  const listenEnd = watch("listenEnd");
  // 009-tls-sni-routing T084: SNI is only valid on TCP single-port rules;
  // UDP and port-range rules would be rejected at the API layer.
  const sniEligible = protocol === "tcp" && !listenEnd;

  async function onSubmit(values: RuleFormValues) {
    setError(null);
    try {
      const trimmedSni = values.sniPattern.trim();
      const rl = formStateToRateLimit(values.rateLimit);
      // `values.client` holds the selected client_id; the push API resolves
      // by display name, so map it back before building the body.
      const clientName =
        clientLites.find((c) => c.client_id === values.client)?.client_name ?? values.client;
      const baseBody = {
        client: clientName,
        listen_port: Number(values.listenStart),
        ...(values.listenEnd ? { listen_port_end: Number(values.listenEnd) } : {}),
        protocol: values.protocol,
        // 009-tls-sni-routing T084: empty / non-eligible inputs are omitted so
        // the server applies its grammar-validated default (legacy shape).
        ...(sniEligible && trimmedSni ? { sni_pattern: trimmedSni } : {}),
        // 011-rate-limiting-qos T039: omit rate_limit entirely when every cap
        // is blank. Preserves SC-004 wire byte-stability for v0.10 rule pushes.
        ...(rl ? { rate_limit: rl } : {}),
      };
      let body;
      if (values.mode === "multi") {
        body = {
          ...baseBody,
          targets: values.targets.map((row, idx) => ({
            host: row.host,
            port: Number(row.port),
            priority: idx,
            ...(values.protocol === "tcp" && row.proxyProtocol
              ? { proxy_protocol: row.proxyProtocol }
              : {}),
          })),
          ...(values.healthCheckInterval
            ? { health_check_interval_secs: Number(values.healthCheckInterval) }
            : {}),
        };
      } else if (rl) {
        // R2a: the server only accepts rate_limit on the targets[] shape, so
        // a single target with caps is transparently sent as a one-element
        // targets[]. (A target port range is incompatible with caps and is
        // blocked by the form's superRefine guard above.)
        body = {
          ...baseBody,
          targets: [
            { host: values.target, port: Number(values.targetStart), priority: 0 },
          ],
        };
      } else {
        body = {
          ...baseBody,
          target_host: values.target,
          target_port: Number(values.targetStart),
          ...(values.targetEnd ? { target_port_end: Number(values.targetEnd) } : {}),
        };
      }
      const res = await push.mutateAsync(body);
      onSuccess(res.rule_id);
    } catch (err) {
      setError(formatApiError(err));
    }
  }

  return (
    <form onSubmit={handleSubmit(onSubmit)}>
      <FieldGroup>
        <Field data-invalid={formState.errors.client ? true : undefined}>
          <FieldLabel htmlFor="rule-client">{t("rules.client")}</FieldLabel>
          <Controller
            control={control}
            name="client"
            render={({ field }) => (
              <ClientCombobox
                clients={clientLites}
                value={field.value}
                onChange={field.onChange}
                disabledClientIds={EMPTY_DISABLED_CLIENTS}
              />
            )}
          />
          {formState.errors.client && <FieldError errors={[formState.errors.client]} />}
        </Field>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <FormTextField
            control={control}
            name="listenStart"
            type="number"
            label={t("rulePush.listenStart")}
          />
          <FormTextField
            control={control}
            name="listenEnd"
            type="number"
            label={t("rulePush.listenEnd")}
            placeholder={t("rulePush.optional")}
          />
        </div>

        <FormToggleField
          control={control}
          name="mode"
          label={t("rulePush.targetMode")}
          options={[
            { value: "single", label: t("rulePush.targetMode_single") },
            { value: "multi", label: t("rulePush.targetMode_multi") },
          ]}
        />

        {mode === "single" ? (
          <>
            <FormTextField control={control} name="target" label={t("rulePush.targetHost")} />
            <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
              <FormTextField
                control={control}
                name="targetStart"
                type="number"
                label={t("rulePush.targetStart")}
              />
              <FormTextField
                control={control}
                name="targetEnd"
                type="number"
                label={t("rulePush.targetEnd")}
                placeholder={t("rulePush.optional")}
              />
            </div>
          </>
        ) : (
          <div className="flex flex-col gap-3">
            <FieldLabel>{t("rulePush.targets")}</FieldLabel>
            <div className="flex flex-col gap-2">
              {fields.map((row, idx) => {
                const rowErr = formState.errors.targets?.[idx];
                return (
                  <div
                    key={row.id}
                    className="grid grid-cols-1 gap-2 rounded-md border p-3 sm:grid-cols-[1fr_120px_120px_72px_auto] sm:items-center sm:border-0 sm:p-0"
                  >
                    <Input
                      placeholder={t("rulePush.targetHost")}
                      aria-label={t("rulePush.targetHost")}
                      aria-invalid={rowErr?.host ? true : undefined}
                      {...form.register(`targets.${idx}.host`)}
                    />
                    <Input
                      placeholder={t("rulePush.targetPort")}
                      aria-label={t("rulePush.targetPort")}
                      type="number"
                      aria-invalid={rowErr?.port ? true : undefined}
                      {...form.register(`targets.${idx}.port`)}
                    />
                    <Controller
                      control={control}
                      name={`targets.${idx}.proxyProtocol`}
                      render={({ field }) => (
                        <Select
                          value={field.value || PROXY_PROTOCOL_NONE}
                          onValueChange={(value) =>
                            field.onChange(value === PROXY_PROTOCOL_NONE ? "" : value)
                          }
                          disabled={protocol !== "tcp"}
                        >
                          <SelectTrigger aria-label={t("rulePush.proxyProtocolDisabled")}>
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            <SelectGroup>
                              <SelectItem value={PROXY_PROTOCOL_NONE}>
                                {t("rulePush.proxyProtocolDisabled")}
                              </SelectItem>
                              <SelectItem value="v1">{t("rulePush.proxyProtocolV1")}</SelectItem>
                              <SelectItem value="v2">{t("rulePush.proxyProtocolV2")}</SelectItem>
                            </SelectGroup>
                          </SelectContent>
                        </Select>
                      )}
                    />
                    <span className="text-sm text-muted-foreground sm:text-center">
                      {t("rulePush.priority")} {idx}
                    </span>
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      onClick={() => remove(idx)}
                      disabled={fields.length <= 1}
                      aria-label={t("rulePush.removeTarget")}
                    >
                      <Trash2 className="h-4 w-4" />
                    </Button>
                  </div>
                );
              })}
            </div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() => append({ host: "", port: "", proxyProtocol: "" })}
              className="w-full sm:w-auto"
            >
              <Plus className="h-4 w-4 mr-1" />
              {t("rulePush.addTarget")}
            </Button>
            <FormTextField
              control={control}
              name="healthCheckInterval"
              type="number"
              min={1}
              max={3600}
              label={t("rulePush.healthCheckInterval")}
              placeholder={t("rulePush.healthCheckIntervalPlaceholder")}
              description={t("rulePush.healthCheckIntervalHelp")}
            />
          </div>
        )}

        <FormToggleField
          control={control}
          name="protocol"
          label={t("rules.protocol")}
          options={[
            { value: "tcp", label: "TCP" },
            { value: "udp", label: "UDP" },
          ]}
        />

        {/* 009-tls-sni-routing T084: SNI selector input. Only rendered when
            the rule is TCP single-port; UDP and port-range rules would be
            rejected at the API layer. */}
        {sniEligible && (
          <FormTextField
            control={control}
            name="sniPattern"
            label={t("rulePush.sniPattern")}
            placeholder={t("rulePush.sniPatternPlaceholder")}
            description={t("rulePush.sniPatternHelp")}
          />
        )}

        <Field>
          <FieldLabel>{t("rulePush.rateLimitTitle")}</FieldLabel>
          <FieldDescription>{t("rulePush.rateLimitHelp")}</FieldDescription>
          <Controller
            control={control}
            name="rateLimit"
            render={({ field }) => (
              <RateLimitForm state={field.value} onChange={field.onChange} />
            )}
          />
        </Field>

        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
        <div className="flex flex-col gap-2 sm:flex-row">
          <Button type="submit" disabled={push.isPending}>
            {push.isPending ? t("confirm.busy") : t("rulePush.submit")}
          </Button>
          <Button type="button" variant="outline" onClick={onCancel}>
            {t("confirm.cancel")}
          </Button>
        </div>
      </FieldGroup>
    </form>
  );
}
