import { useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";

import { useClientsList } from "@/api/clients";
import { useDashboardGauges } from "@/api/metrics";
import { useRulesList } from "@/api/rules";
import { globalTrafficKey, useGlobalTraffic } from "@/api/traffic";
import { useThroughputRate } from "@/api/use-throughput-rate";
import { formatBytes } from "@/lib/format";

import { AlertBanner } from "./components/AlertBanner";
import { DashboardIssueBlocks } from "./components/DashboardIssueBlocks";
import { KpiCard } from "./components/KpiCard";
import { RecentAuditPanel } from "./components/RecentAuditPanel";
import { ThroughputChart } from "./components/ThroughputChart";
import { TopRulesPanel } from "./components/TopRulesPanel";
import { TrafficComparisonChart } from "./components/TrafficComparisonChart";
import { TrafficDirectionChart } from "./components/TrafficDirectionChart";
import { useDashboardTrafficBreakdown } from "./trafficBreakdown";
import { useDashboardRange } from "./useDashboardRange";

export function SuperadminDashboard() {
  const { t } = useTranslation();
  const gauges = useDashboardGauges();
  const clients = useClientsList();
  const rules = useRulesList();
  const throughput = useThroughputRate();
  const { rangeId, range, setRange } = useDashboardRange("24h");
  const global = useGlobalTraffic(range);
  const breakdown = useDashboardTrafficBreakdown(range);
  const qc = useQueryClient();

  const connectedCount = (clients.data ?? []).filter((c) => c.connected).length;
  const totalClients = clients.data?.length ?? 0;
  const ruleCount = gauges.rulesActive ?? rules.data?.length ?? 0;

  const allTargets = (rules.data ?? []).flatMap((r) => r.targets ?? []);
  const totalTargets = allTargets.length;
  const unhealthyCount = allTargets.filter((tt) => tt.health?.healthy === false).length;
  const healthyTargets = totalTargets - unhealthyCount;
  const offlineClientCount = totalClients - connectedCount;

  const cumulativeBytes = gauges.topRules.reduce(
    (acc, r) => acc + r.bytesIn + r.bytesOut,
    0,
  );

  const issues: string[] = [];
  if (unhealthyCount > 0) issues.push(t("dashboard.alertUnhealthy", { n: unhealthyCount }));
  if (offlineClientCount > 0) issues.push(t("dashboard.alertOffline", { n: offlineClientCount }));

  const breakdownLoading = breakdown.isLoading && !breakdown.data;

  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-semibold">{t("dashboard.title")}</h1>

      <AlertBanner issues={issues} />

      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-6">
        <KpiCard
          label={t("dashboard.connectedClients")}
          value={`${connectedCount} / ${totalClients}`}
        />
        <KpiCard label={t("dashboard.activeRules")} value={ruleCount} />
        <KpiCard
          label={t("dashboard.targetsOk")}
          value={`${healthyTargets} / ${totalTargets}`}
          tone={unhealthyCount > 0 ? "bad" : "muted"}
          delta={unhealthyCount > 0 ? t("dashboard.targetsDown", { n: unhealthyCount }) : undefined}
        />
        <KpiCard
          label={t("dashboard.throughputNow")}
          value={throughput === null ? t("dashboard.calculating") : `${formatBytes(throughput)}/s`}
        />
        <KpiCard
          label={t("dashboard.totalTransferred")}
          value={formatBytes(cumulativeBytes)}
          delta={t("dashboard.sinceProcessStart")}
          tone="muted"
        />
        <KpiCard
          label={t("dashboard.activeConnections")}
          value={gauges.activeConnections ?? "—"}
        />
      </div>

      <div className="grid grid-cols-1 items-start gap-3 lg:grid-cols-[minmax(16rem,24rem)_minmax(0,1fr)]">
        <DashboardIssueBlocks
          unhealthyTargets={unhealthyCount}
          offlineClients={offlineClientCount}
        />
        <RecentAuditPanel />
      </div>

      <div className="grid grid-cols-1 gap-3 lg:grid-cols-[2fr_1fr]">
        <ThroughputChart
          samples={global.data?.samples}
          isLoading={global.isLoading}
          error={global.error}
          rangeId={rangeId}
          onRangeChange={setRange}
          onRetry={() => qc.invalidateQueries({ queryKey: globalTrafficKey(range) })}
        />
        <TopRulesPanel rules={gauges.topRules} />
      </div>

      <div className="grid grid-cols-1 gap-3 xl:grid-cols-3">
        <TrafficComparisonChart
          title={t("dashboard.trafficByClient")}
          items={breakdown.data?.clients ?? []}
          isLoading={breakdownLoading}
          error={breakdown.error}
        />
        <TrafficComparisonChart
          title={t("dashboard.trafficByUser")}
          items={breakdown.data?.users ?? []}
          isLoading={breakdownLoading}
          error={breakdown.error}
        />
        <TrafficDirectionChart
          rows={breakdown.data?.directions ?? []}
          isLoading={breakdownLoading}
          error={breakdown.error}
        />
      </div>
    </div>
  );
}
