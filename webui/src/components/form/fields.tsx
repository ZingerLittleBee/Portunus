/// Thin controlled-field helpers that collapse the shadcn-recommended
/// `Controller` + `Field` + `FieldLabel` + control + `FieldError` boilerplate
/// into a single call. Every helper drives the control via react-hook-form's
/// `Controller` (never `register`) so non-native controls (Select, Checkbox,
/// Switch, ToggleGroup) compose uniformly. `data-invalid`/`aria-invalid` and
/// `FieldError` are wired from `fieldState` for accessibility.

import type { ReactNode } from "react";
import {
  Controller,
  type Control,
  type FieldPath,
  type FieldValues,
} from "react-hook-form";

import {
  Field,
  FieldContent,
  FieldDescription,
  FieldError,
  FieldLabel,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Checkbox } from "@/components/ui/checkbox";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";

interface BaseProps<T extends FieldValues> {
  control: Control<T>;
  name: FieldPath<T>;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
}

/// Text / password / number input. Number fields hold strings in form
/// state; pair with `z.coerce.number()` in the schema so the submit
/// handler receives a number.
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
}: BaseProps<T> & {
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

/// Single-choice dropdown.
export function FormSelectField<T extends FieldValues>({
  control,
  name,
  label,
  description,
  disabled,
  options,
  placeholder,
  "aria-label": ariaLabel,
}: BaseProps<T> & {
  options: { value: string; label: ReactNode }[];
  placeholder?: string;
  "aria-label"?: string;
}) {
  return (
    <Controller
      control={control}
      name={name}
      render={({ field, fieldState }) => (
        <Field data-invalid={fieldState.invalid || undefined}>
          <FieldLabel htmlFor={field.name}>{label}</FieldLabel>
          <Select
            value={field.value}
            onValueChange={field.onChange}
            disabled={disabled ?? false}
          >
            <SelectTrigger
              id={field.name}
              aria-label={ariaLabel}
              aria-invalid={fieldState.invalid || undefined}
            >
              <SelectValue placeholder={placeholder} />
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                {options.map((o) => (
                  <SelectItem key={o.value} value={o.value}>
                    {o.label}
                  </SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
          {description && <FieldDescription>{description}</FieldDescription>}
          {fieldState.error && <FieldError errors={[fieldState.error]} />}
        </Field>
      )}
    />
  );
}

/// Mutually-exclusive 2–7 option set rendered as a segmented control.
export function FormToggleField<T extends FieldValues>({
  control,
  name,
  label,
  description,
  disabled,
  options,
}: BaseProps<T> & {
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
            // Radix toggle-group emits "" when the active item is
            // re-clicked; ignore that so the choice stays sticky.
            onValueChange={(value) => {
              if (value) field.onChange(value);
            }}
            variant="outline"
            disabled={disabled ?? false}
            className="justify-start"
          >
            {options.map((o) => (
              <ToggleGroupItem key={o.value} value={o.value}>
                {o.label}
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

/// Boolean checkbox laid out horizontally (label beside the control).
export function FormCheckboxField<T extends FieldValues>({
  control,
  name,
  label,
  description,
  disabled,
}: BaseProps<T>) {
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

/// Boolean switch laid out horizontally (label, then control on the right).
export function FormSwitchField<T extends FieldValues>({
  control,
  name,
  label,
  description,
  disabled,
}: BaseProps<T>) {
  return (
    <Controller
      control={control}
      name={name}
      render={({ field, fieldState }) => (
        <Field orientation="horizontal" data-invalid={fieldState.invalid || undefined}>
          <FieldContent>
            <FieldLabel htmlFor={field.name}>{label}</FieldLabel>
            {description && <FieldDescription>{description}</FieldDescription>}
          </FieldContent>
          <Switch
            id={field.name}
            checked={field.value}
            onCheckedChange={field.onChange}
            disabled={disabled}
            aria-invalid={fieldState.invalid || undefined}
          />
        </Field>
      )}
    />
  );
}
