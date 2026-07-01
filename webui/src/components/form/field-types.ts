import type { ReactNode } from "react";
import type { Control, FieldPath, FieldValues } from "react-hook-form";

export interface BaseFieldProps<T extends FieldValues> {
  control: Control<T>;
  name: FieldPath<T>;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
}
