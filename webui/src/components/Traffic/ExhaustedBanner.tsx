// 013-traffic-quotas G4: destructive banner shown above the user / client
// detail tabs when one or more quotas are exhausted.

import { AlertTriangle } from "lucide-react";
import { useTranslation } from "react-i18next";

import type { MonthlyQuotaView } from "@/api/types";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";

interface Props {
  exhausted: MonthlyQuotaView[];
  onClearUsage?: ((q: MonthlyQuotaView) => void) | undefined;
  onIncreaseLimit?: ((q: MonthlyQuotaView) => void) | undefined;
}

export function ExhaustedBanner({
  exhausted,
  onClearUsage,
  onIncreaseLimit,
}: Props) {
  const { t } = useTranslation();
  if (exhausted.length === 0) return null;
  return (
    <Alert variant="destructive">
      <AlertTriangle className="h-4 w-4" />
      <AlertTitle>{t("traffic.banner.title")}</AlertTitle>
      <AlertDescription>
        {exhausted.map((q) => (
          <div
            key={`${q.user_id}|${q.client_name}`}
            className="flex flex-wrap items-center gap-2 my-1"
          >
            <span>{t("traffic.banner.row", { client: q.client_name })}</span>
            {onClearUsage ? (
              <Button
                size="sm"
                variant="outline"
                onClick={() => onClearUsage(q)}
              >
                {t("userQuota.clearUsage")}
              </Button>
            ) : null}
            {onIncreaseLimit ? (
              <Button
                size="sm"
                variant="outline"
                onClick={() => onIncreaseLimit(q)}
              >
                {t("traffic.banner.increase")}
              </Button>
            ) : null}
          </div>
        ))}
      </AlertDescription>
    </Alert>
  );
}
