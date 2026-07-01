import { Controller, type FieldValues } from "react-hook-form";

import type { BaseFieldProps } from "@/components/form/field-types";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldError,
  FieldLabel,
} from "@/components/ui/field";

export function FormCheckboxField<T extends FieldValues>({
  control,
  name,
  label,
  description,
  disabled,
}: BaseFieldProps<T>) {
  return (
    <Controller
      control={control}
      name={name}
      render={({ field, fieldState }) => (
        <Field orientation="horizontal" data-invalid={fieldState.invalid || undefined}>
          <Checkbox
            id={field.name}
            checked={field.value}
            onCheckedChange={(checked) => field.onChange(checked === true)}
            disabled={disabled}
            aria-invalid={fieldState.invalid || undefined}
          />
          <FieldContent>
            <FieldLabel htmlFor={field.name} className="font-normal">
              {label}
            </FieldLabel>
            {description && <FieldDescription>{description}</FieldDescription>}
            {fieldState.error && <FieldError errors={[fieldState.error]} />}
          </FieldContent>
        </Field>
      )}
    />
  );
}
