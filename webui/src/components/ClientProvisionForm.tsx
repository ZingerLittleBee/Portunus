import { useState } from "react";
import { useTranslation } from "react-i18next";

import { useCreateClientEnrollment } from "@/api/clients";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { EnrollmentInstallGuide } from "@/components/EnrollmentInstallGuide";
import type { ClientEnrollmentResponse } from "@/api/types";

interface ClientProvisionFormProps {
  /** Dismiss the form: cancel before enrollment, or "back to list" afterwards. */
  onDone: () => void;
}

export function ClientProvisionForm({ onDone }: ClientProvisionFormProps) {
  const { t } = useTranslation();
  const enrollmentMutation = useCreateClientEnrollment();
  const [name, setName] = useState("");
  const [address, setAddress] = useState("");
  const [enrollment, setEnrollment] = useState<ClientEnrollmentResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    try {
      const res = await enrollmentMutation.mutateAsync({ name, address });
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
        <EnrollmentInstallGuide enrollment={enrollment} mode="provision" />
        <Button variant="link" className="px-0" onClick={onDone}>
          {t("clientProvision.backToList")}
        </Button>
      </div>
    );
  }

  return (
    <form onSubmit={handleSubmit} className="space-y-4">
      <div className="space-y-2">
        <Label htmlFor="name">{t("clients.name")}</Label>
        <Input
          id="name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="client-a"
          required
        />
      </div>
      <div className="space-y-2">
        <Label htmlFor="address">{t("clientProvision.address")}</Label>
        <Input
          id="address"
          value={address}
          onChange={(e) => setAddress(e.target.value)}
          placeholder="68.77.201.69 or edge.example.com"
          required
        />
        <p className="text-xs text-muted-foreground">{t("clientProvision.addressHint")}</p>
      </div>
      {error && <p className="text-sm text-destructive">{error}</p>}
      <div className="flex gap-2">
        <Button type="submit" disabled={enrollmentMutation.isPending}>
          {enrollmentMutation.isPending ? t("confirm.busy") : t("clientProvision.submit")}
        </Button>
        <Button type="button" variant="outline" onClick={onDone}>
          {t("confirm.cancel")}
        </Button>
      </div>
    </form>
  );
}
