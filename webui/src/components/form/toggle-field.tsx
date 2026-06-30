import type { ReactNode } from "react";
import { Controller, type FieldValues } from "react-hook-form";

import type { BaseFieldProps } from "@/components/form/field-types";
import {
  Field,
  FieldDescription,
  FieldError,
  FieldLabel,
} from "@/components/ui/field";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";

export function FormToggleField<T extends FieldValues>({
  control,
  name,
  label,
  description,
  disabled,
  options,
}: BaseFieldProps<T> & {
  options: { value: string; label: ReactNode }[];
}) {
  return (
    <Controller
      control={control}
      name={name}
      render={({ field, fieldState }) => (
        <Field data-invalid={fieldState.invalid || undefined}>
          <FieldLabel htmlFor={field.name}>{label}</FieldLabel>
          <ToggleGroup
            type="single"
            value={field.value}
            onValueChange={(value) => {
              // Radix emits "" when the active item is re-clicked; keep the current choice.
              if (value) field.onChange(value);
            }}
            variant="outline"
            disabled={disabled ?? false}
            className="justify-start"
          >
            {options.map((option) => (
              <ToggleGroupItem key={option.value} value={option.value}>
                {option.label}
              </ToggleGroupItem>
            ))}
          </ToggleGroup>
          {description && <FieldDescription>{description}</FieldDescription>}
          {fieldState.error && <FieldError errors={[fieldState.error]} />}
        </Field>
      )}
    />
  );
}
