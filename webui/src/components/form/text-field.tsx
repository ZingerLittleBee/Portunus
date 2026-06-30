import { Controller, type FieldValues } from "react-hook-form";

import type { BaseFieldProps } from "@/components/form/field-types";
import {
  Field,
  FieldDescription,
  FieldError,
  FieldLabel,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";

export function FormTextField<T extends FieldValues>({
  control,
  name,
  label,
  description,
  disabled,
  type = "text",
  placeholder,
  autoComplete,
  autoFocus,
  spellCheck,
  min,
  max,
}: BaseFieldProps<T> & {
  type?: React.HTMLInputTypeAttribute;
  placeholder?: string;
  autoComplete?: string;
  autoFocus?: boolean;
  spellCheck?: boolean;
  min?: number;
  max?: number;
}) {
  return (
    <Controller
      control={control}
      name={name}
      render={({ field, fieldState }) => (
        <Field data-invalid={fieldState.invalid || undefined}>
          <FieldLabel htmlFor={field.name}>{label}</FieldLabel>
          <Input
            {...field}
            value={field.value ?? ""}
            id={field.name}
            type={type}
            placeholder={placeholder}
            autoComplete={autoComplete}
            autoFocus={autoFocus}
            spellCheck={spellCheck}
            min={min}
            max={max}
            disabled={disabled}
            aria-invalid={fieldState.invalid || undefined}
          />
          {description && <FieldDescription>{description}</FieldDescription>}
          {fieldState.error && <FieldError errors={[fieldState.error]} />}
        </Field>
      )}
    />
  );
}
