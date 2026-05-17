import { useState } from "react";
import { useTranslation } from "react-i18next";

import { useAdvertisedEndpoint, useSetAdvertisedEndpoint } from "@/api/settings";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ThemeToggle } from "@/components/ThemeToggle";
import { LanguageToggle } from "@/components/LanguageToggle";

function AdvertisedEndpointCard() {
  const { t } = useTranslation();
  const { data } = useAdvertisedEndpoint();
  const save = useSetAdvertisedEndpoint();
  const [value, setValue] = useState<string | null>(null);
  const current = value ?? data?.override ?? "";
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t("settings.advertisedHeading")}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <p className="text-sm text-muted-foreground">{t("settings.advertisedDescription")}</p>
        <Label htmlFor="advertised-endpoint">{t("settings.advertisedHeading")}</Label>
        <Input
          id="advertised-endpoint"
          value={current}
          placeholder="proxy.example.com:34567"
          onChange={(e) => setValue(e.target.value)}
        />
        <div className="flex gap-2">
          <Button
            onClick={() => {
              const trimmed = current.trim();
              save.mutate(trimmed === "" ? null : trimmed, {
                onSuccess: () => setValue(null),
              });
            }}
            disabled={save.isPending}
          >
            {t("settings.advertisedSave")}
          </Button>
          <Button
            variant="outline"
            onClick={() => save.mutate(null, { onSuccess: () => setValue(null) })}
            disabled={save.isPending}
          >
            {t("settings.advertisedClear")}
          </Button>
        </div>
        {data?.effective && (
          <p className="text-sm">{t("settings.advertisedEffective", { effective: data.effective, source: data.source })}</p>
        )}
        {data?.diagnostic && (
          <p className="text-sm text-destructive">{t("settings.advertisedDiagnostic", { diagnostic: data.diagnostic })}</p>
        )}
        {save.isError && (
          <p className="text-sm text-destructive">
            {save.error instanceof ApiError ? save.error.message : String(save.error)}
          </p>
        )}
      </CardContent>
    </Card>
  );
}

export function Settings() {
  const { t } = useTranslation();
  return (
    <div className="max-w-2xl space-y-4">
      <h1 className="text-2xl font-semibold">{t("settings.title")}</h1>
      <Card>
        <CardHeader>
          <CardTitle>{t("settings.themeHeading")}</CardTitle>
        </CardHeader>
        <CardContent>
          <ThemeToggle />
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>{t("settings.languageHeading")}</CardTitle>
        </CardHeader>
        <CardContent>
          <LanguageToggle />
        </CardContent>
      </Card>
      <AdvertisedEndpointCard />
    </div>
  );
}
