import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useForm } from "react-hook-form";
import { z } from "zod";

import { useCreateClientEnrollment } from "@/api/clients";
import { ApiError } from "@/api/client";
import { zResolver } from "@/lib/zod-resolver";
import { Button } from "@/components/ui/button";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { FieldGroup } from "@/components/ui/field";
import { FormTextField } from "@/components/form/fields";
import { EnrollmentInstallGuide } from "@/components/EnrollmentInstallGuide";
import type { ClientEnrollmentResponse } from "@/api/types";

interface ClientProvisionFormProps {
  /** Dismiss the form: cancel before enrollment, or "back to list" afterwards. */
  onDone: () => void;
}

export function ClientProvisionForm({ onDone }: ClientProvisionFormProps) {
  const { t } = useTranslation();
  const enrollmentMutation = useCreateClientEnrollment();
  const [enrollment, setEnrollment] = useState<ClientEnrollmentResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  const schema = z.object({
    name: z.string().trim().min(1),
    address: z.string().trim().min(1),
  });
  const form = useForm<z.infer<typeof schema>>({
    resolver: zResolver<z.infer<typeof schema>>(schema),
    defaultValues: { name: "", address: "" },
  });

  async function onSubmit(values: z.infer<typeof schema>) {
    setError(null);
    try {
      const res = await enrollmentMutation.mutateAsync({
        name: values.name,
        address: values.address,
      });
      setEnrollment(res);
    } catch (err) {
      if (err instanceof ApiError) {
        setError(`${err.code}: ${err.message}`);
      } else if (err instanceof Error) {
        setError(err.message);
      } else {
        setError(String(err));
      }
    }
  }

  if (enrollment) {
    return (
      <div className="space-y-6">
        <EnrollmentInstallGuide enrollment={enrollment} mode="provision" framed={false} />
        <Button variant="link" className="px-0" onClick={onDone}>
          {t("clientProvision.backToList")}
        </Button>
      </div>
    );
  }

  const busy = enrollmentMutation.isPending || form.formState.isSubmitting;

  return (
    <form onSubmit={form.handleSubmit(onSubmit)}>
      <FieldGroup>
        <FormTextField
          control={form.control}
          name="name"
          label={t("clients.name")}
          placeholder="client-a"
          disabled={busy}
        />
        <FormTextField
          control={form.control}
          name="address"
          label={t("clientProvision.address")}
          placeholder="68.77.201.69 or edge.example.com"
          description={t("clientProvision.addressHint")}
          disabled={busy}
        />
        {error && (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        )}
        <div className="flex gap-2">
          <Button type="submit" disabled={busy}>
            {busy ? t("confirm.busy") : t("clientProvision.submit")}
          </Button>
          <Button type="button" variant="outline" onClick={onDone}>
            {t("confirm.cancel")}
          </Button>
        </div>
      </FieldGroup>
    </form>
  );
}
