import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Controller, useFieldArray, useForm } from "react-hook-form";

import { useClientsList } from "@/api/clients";
import { formatApiError } from "@/api/client";
import { usePushRule } from "@/api/rules";
import { zResolver } from "@/lib/zod-resolver";
import { Button } from "@/components/ui/button";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Field, FieldDescription, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { ClientCombobox } from "@/components/UserQuota/ClientCombobox";
import { FormTextField, FormToggleField } from "@/components/form/fields";
import { RateLimitForm } from "@/components/RateLimitForm";
import {
  buildPushRuleBody,
  createRuleFormDefaultValues,
  createRuleFormSchema,
  EMPTY_DISABLED_CLIENTS,
  type RuleFormClientLite,
  type RuleFormValues,
} from "@/components/RuleForm.model";
import { RuleTargetsFields } from "@/components/RuleFormTargets";

interface RuleFormProps {
  /** Called with the new rule id after a successful push. */
  onSuccess: (ruleId: number) => void;
  /** Called when the operator dismisses the form. */
  onCancel: () => void;
}

export function RuleForm({ onSuccess, onCancel }: RuleFormProps) {
  const { t } = useTranslation();
  const push = usePushRule();
  const clientsQ = useClientsList();
  const [error, setError] = useState<string | null>(null);

  const schema = useMemo(() => createRuleFormSchema(t), [t]);
  const defaultValues = useMemo(() => createRuleFormDefaultValues(), []);
  const clientLites = useMemo<RuleFormClientLite[]>(
    () =>
      (clientsQ.data ?? []).map((client) => ({
        client_id: client.client_id,
        client_name: client.client_name,
        connected: client.connected,
      })),
    [clientsQ.data],
  );

  const form = useForm<RuleFormValues>({
    resolver: zResolver<RuleFormValues>(schema),
    defaultValues,
  });
  const { control, watch, formState, handleSubmit, register } = form;
  const { fields, append, remove } = useFieldArray({ control, name: "targets" });

  const mode = watch("mode");
  const protocol = watch("protocol");
  const listenEnd = watch("listenEnd");
  const sniEligible = protocol === "tcp" && !listenEnd;

  async function onSubmit(values: RuleFormValues) {
    setError(null);
    try {
      const body = buildPushRuleBody(values, clientLites, sniEligible);
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
          <RuleTargetsFields
            append={append}
            control={control}
            errors={formState.errors}
            fields={fields}
            protocol={protocol}
            register={register}
            remove={remove}
          />
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
            render={({ field }) => <RateLimitForm state={field.value} onChange={field.onChange} />}
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
