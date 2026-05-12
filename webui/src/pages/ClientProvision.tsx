import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";

import { useProvisionClient } from "@/api/clients";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ClientInstallSteps } from "@/components/ClientInstallSteps";
import { CredentialBundleCard } from "@/components/CredentialBundleCard";
import type { CredentialBundle } from "@/api/types";

export function ClientProvision() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const provision = useProvisionClient();
  const [name, setName] = useState("");
  const [bundle, setBundle] = useState<CredentialBundle | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function handleSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    try {
      const res = await provision.mutateAsync({ name });
      setBundle(res);
    } catch (err) {
      setError(err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message);
    }
  }

  return (
    <div className="max-w-3xl space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>{t("clientProvision.title")}</CardTitle>
        </CardHeader>
        <CardContent>
          <form onSubmit={handleSubmit} className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="name">{t("clients.name")}</Label>
              <Input
                id="name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="client-a"
                required
                disabled={!!bundle}
              />
            </div>
            {error && <p className="text-sm text-destructive">{error}</p>}
            {!bundle && (
              <div className="flex gap-2">
                <Button type="submit" disabled={provision.isPending}>
                  {provision.isPending ? t("confirm.busy") : t("clientProvision.submit")}
                </Button>
                <Button type="button" variant="outline" onClick={() => navigate(-1)}>
                  {t("confirm.cancel")}
                </Button>
              </div>
            )}
          </form>
        </CardContent>
      </Card>

      {bundle && (
        <>
          <CredentialBundleCard bundle={bundle} intent="provision" />
          <ClientInstallSteps bundle={bundle} />
          <Button variant="link" onClick={() => navigate("/clients")}>
            {t("clientProvision.backToList")}
          </Button>
        </>
      )}
    </div>
  );
}
