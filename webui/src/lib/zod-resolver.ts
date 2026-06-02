import { zodResolver } from "@hookform/resolvers/zod";
import type { FieldValues, Resolver } from "react-hook-form";
import type { ZodType } from "zod";

/// `@hookform/resolvers` 5.2.2 was typed for zod 4.0.x (it asserts
/// `_zod.version.minor === 0`) but this project runs zod 4.4.x. The runtime
/// behaviour is correct; this wrapper papers over the type-only version
/// mismatch so call sites stay free of inline `as any` casts. Mirrors the
/// pre-existing workaround in `UserQuotaForm.tsx`.
export function zResolver<T extends FieldValues>(schema: ZodType): Resolver<T> {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return zodResolver(schema as any) as Resolver<T>;
}
