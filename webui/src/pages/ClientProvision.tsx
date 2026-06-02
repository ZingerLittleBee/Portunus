import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";

import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { ClientProvisionForm } from "@/components/ClientProvisionForm";

export function ClientProvision() {
  const { t } = useTranslation();
  const navigate = useNavigate();

  return (
    <Card className="max-w-3xl">
      <CardHeader>
        <CardTitle>{t("clientProvision.title")}</CardTitle>
      </CardHeader>
      <CardContent>
        <ClientProvisionForm onDone={() => navigate("/clients")} />
      </CardContent>
    </Card>
  );
}
