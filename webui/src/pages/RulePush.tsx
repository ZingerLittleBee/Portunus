import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";

import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { RuleForm } from "@/components/RuleForm";

export function RulePush() {
  const { t } = useTranslation();
  const navigate = useNavigate();

  return (
    <Card className="w-full max-w-2xl">
      <CardHeader>
        <CardTitle>{t("rulePush.title")}</CardTitle>
      </CardHeader>
      <CardContent>
        <RuleForm
          onSuccess={(ruleId) => navigate(`/rules/${ruleId}`)}
          onCancel={() => navigate(-1)}
        />
      </CardContent>
    </Card>
  );
}
