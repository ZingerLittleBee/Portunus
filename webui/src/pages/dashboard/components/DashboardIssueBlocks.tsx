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
    <div className="grid h-fit grid-cols-2 gap-3 md:max-w-md">
      <Card>
        <CardHeader className="p-3 pb-1">
          <CardTitle className="text-xs font-medium text-muted-foreground">
            {t("dashboard.unhealthyTargets")}
          </CardTitle>
        </CardHeader>
        <CardContent className="p-3 pt-0">
          <div className="text-2xl font-semibold tabular-nums">{unhealthyTargets}</div>
          <p className="text-xs text-muted-foreground">
            {unhealthyTargets > 0 ? t("dashboard.down") : t("dashboard.allTargetsHealthy")}
          </p>
        </CardContent>
      </Card>
      <Card>
        <CardHeader className="p-3 pb-1">
          <CardTitle className="text-xs font-medium text-muted-foreground">
            {t("dashboard.offlineClients")}
          </CardTitle>
        </CardHeader>
        <CardContent className="p-3 pt-0">
          <div className="text-2xl font-semibold tabular-nums">{offlineClients}</div>
          <p className="text-xs text-muted-foreground">
            {offlineClients > 0 ? t("dashboard.offline") : t("dashboard.allClientsOnline")}
          </p>
        </CardContent>
      </Card>
    </div>
  );
}
