import { useTranslation } from "react-i18next";

import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

interface DashboardIssueBlocksProps {
  unhealthyTargets: number;
  offlineClients: number;
}

export function DashboardIssueBlocks({
  unhealthyTargets,
  offlineClients,
}: DashboardIssueBlocksProps) {
  const { t } = useTranslation();

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("dashboard.operationalStatus")}</CardTitle>
      </CardHeader>
      <CardContent className="grid grid-cols-2 gap-4">
        <div className="min-w-0">
          <div className="text-xs font-medium text-muted-foreground">
            {t("dashboard.unhealthyTargets")}
          </div>
          <div className="mt-1 text-2xl font-semibold tabular-nums">{unhealthyTargets}</div>
          <p className="text-xs text-muted-foreground">
            {unhealthyTargets > 0 ? t("dashboard.down") : t("dashboard.allTargetsHealthy")}
          </p>
        </div>
        <div className="min-w-0 border-l pl-4">
          <div className="text-xs font-medium text-muted-foreground">
            {t("dashboard.offlineClients")}
          </div>
          <div className="mt-1 text-2xl font-semibold tabular-nums">{offlineClients}</div>
          <p className="text-xs text-muted-foreground">
            {offlineClients > 0 ? t("dashboard.offline") : t("dashboard.allClientsOnline")}
          </p>
        </div>
      </CardContent>
    </Card>
  );
}
