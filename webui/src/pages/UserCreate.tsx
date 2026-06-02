import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";

import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { UserCreateForm } from "@/components/UserCreateForm";

export function UserCreate() {
  const { t } = useTranslation();
  const navigate = useNavigate();

  return (
    <Card className="max-w-xl">
      <CardHeader>
        <CardTitle>{t("userCreate.title")}</CardTitle>
      </CardHeader>
      <CardContent>
        <UserCreateForm
          onSuccess={(userId) => navigate(`/users/${userId}`)}
          onCancel={() => navigate(-1)}
        />
      </CardContent>
    </Card>
  );
}
