import { useTranslation } from "react-i18next";

import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ThemeToggle } from "@/components/ThemeToggle";
import { LanguageToggle } from "@/components/LanguageToggle";

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
    </div>
  );
}
