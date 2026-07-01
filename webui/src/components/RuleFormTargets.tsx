import { useTranslation } from "react-i18next";
import { Controller } from "react-hook-form";
import type {
  Control,
  FieldArrayWithId,
  FieldErrors,
  UseFieldArrayAppend,
  UseFieldArrayRemove,
  UseFormRegister,
} from "react-hook-form";
import { Plus, Trash2 } from "lucide-react";

import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { FieldLabel } from "@/components/ui/field";
import { FormTextField } from "@/components/form/fields";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { PROXY_PROTOCOL_NONE, type RuleFormValues } from "@/components/RuleForm.model";

interface RuleTargetsFieldsProps {
  append: UseFieldArrayAppend<RuleFormValues, "targets">;
  control: Control<RuleFormValues>;
  errors: FieldErrors<RuleFormValues>;
  fields: FieldArrayWithId<RuleFormValues, "targets", "id">[];
  protocol: RuleFormValues["protocol"];
  register: UseFormRegister<RuleFormValues>;
  remove: UseFieldArrayRemove;
}

export function RuleTargetsFields({
  append,
  control,
  errors,
  fields,
  protocol,
  register,
  remove,
}: RuleTargetsFieldsProps) {
  const { t } = useTranslation();

  return (
    <div className="flex flex-col gap-3">
      <FieldLabel>{t("rulePush.targets")}</FieldLabel>
      <div className="flex flex-col gap-2">
        {fields.map((row, index) => {
          const rowError = errors.targets?.[index];
          return (
            <div
              key={row.id}
              className="grid grid-cols-1 gap-2 rounded-md border p-3 sm:grid-cols-[1fr_120px_120px_72px_auto] sm:items-center sm:border-0 sm:p-0"
            >
              <Input
                placeholder={t("rulePush.targetHost")}
                aria-label={t("rulePush.targetHost")}
                aria-invalid={rowError?.host ? true : undefined}
                {...register(`targets.${index}.host`)}
              />
              <Input
                placeholder={t("rulePush.targetPort")}
                aria-label={t("rulePush.targetPort")}
                type="number"
                aria-invalid={rowError?.port ? true : undefined}
                {...register(`targets.${index}.port`)}
              />
              <Controller
                control={control}
                name={`targets.${index}.proxyProtocol`}
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
                {t("rulePush.priority")} {index}
              </span>
              <Button
                type="button"
                variant="ghost"
                size="icon"
                onClick={() => remove(index)}
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
  );
}
