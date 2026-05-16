import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";

import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { useClientsList } from "@/api/clients";
import { useRulesList } from "@/api/rules";
import { useUserQuotas } from "@/api/quotas";
import { useUserTraffic, userTrafficKey } from "@/api/traffic";
import { formatBytes } from "@/lib/format";

import { AlertBanner } from "./components/AlertBanner";
import { DashboardIssueBlocks } from "./components/DashboardIssueBlocks";
import { KpiCard } from "./components/KpiCard";
import { ThroughputChart } from "./components/ThroughputChart";
import { TrafficDirectionChart } from "./components/TrafficDirectionChart";
import { trafficDirectionRows } from "./trafficBreakdown";
import { useDashboardRange } from "./useDashboardRange";

export function TenantDashboard() {
  const { t } = useTranslation();
  const { data: identity } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  const userId = identity?.user_id ?? "";

  const clients = useClientsList();
  const rules = useRulesList();
  const quotas = useUserQuotas(userId);
  const { rangeId, range, setRange } = useDashboardRange("24h");
  const traffic = useUserTraffic(userId, range);
  const qc = useQueryClient();

  // KPI · 24h transferred (always last 24h regardless of range toggle).
  const nowSec = Math.floor(Date.now() / 1000);
  const last24h = { from: nowSec - 86_400, to: nowSec, bucket: "1m" as const };
  const traffic24h = useUserTraffic(userId, last24h);
  const transferred24h =
    (traffic24h.data?.total_bytes_in ?? 0) + (traffic24h.data?.total_bytes_out ?? 0);

  // Only count clients this tenant actually owns (= clients referenced by
  // their server-filtered rules list). /v1/clients is not tenant-scoped
  // server-side, so we filter here.
  const myClientNames = new Set((rules.data ?? []).map((r) => r.client_name));
  const myClients = (clients.data ?? []).filter((c) => myClientNames.has(c.client_name));
  const connectedCount = myClients.filter((c) => c.connected).length;
  const totalClients = myClients.length;
  const ruleCount = rules.data?.length ?? 0;
  const offlineClientCount = myClients.filter((c) => !c.connected && !c.revoked_at).length;

  // Aggregate quota usage across this user's per-client rows.
  const quotaUsed = (quotas.data ?? []).reduce(
    (acc, q) => acc + q.current_period_bytes_used,
    0,
  );
  const quotaLimit = (quotas.data ?? []).reduce(
    (acc, q) => acc + q.monthly_bytes,
    0,
  );
  const quotaPct = quotaLimit > 0 ? Math.min(100, (quotaUsed / quotaLimit) * 100) : null;

  const unhealthyCount = (rules.data ?? []).reduce(
    (acc, r) => acc + (r.targets ?? []).filter((tt) => tt.health?.healthy === false).length,
    0,
  );

  const issues: string[] = [];
  if (unhealthyCount > 0) issues.push(t("dashboard.alertUnhealthy", { n: unhealthyCount }));
  if (offlineClientCount > 0) issues.push(t("dashboard.alertOffline", { n: offlineClientCount }));
  if (quotaPct !== null && quotaPct >= 80) {
    issues.push(t("dashboard.alertQuotaNear", { pct: Math.round(quotaPct) }));
  }

  const directionRows = trafficDirectionRows({
    total_bytes_in: traffic.data?.total_bytes_in ?? 0,
    total_bytes_out: traffic.data?.total_bytes_out ?? 0,
  });

  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-semibold">
        {t("dashboard.greeting")}, {identity?.display_name ?? identity?.user_id ?? "—"}
      </h1>

      <AlertBanner issues={issues} />

      <div className="grid grid-cols-2 gap-3 md:grid-cols-3 lg:grid-cols-5">
        <KpiCard
          label={t("dashboard.myClients")}
          value={`${connectedCount} / ${totalClients}`}
        />
        <KpiCard label={t("dashboard.myRules")} value={ruleCount} />
        <KpiCard
          label={t("dashboard.my24hTransferred")}
          value={formatBytes(transferred24h)}
        />
        <KpiCard
          label={t("dashboard.myQuotaUsed")}
          value={quotaPct === null ? "—" : `${quotaPct.toFixed(0)}%`}
          delta={
            quotaLimit > 0
              ? `${formatBytes(quotaUsed)} / ${formatBytes(quotaLimit)}`
              : t("dashboard.noQuotaSet")
          }
          tone={quotaPct !== null && quotaPct >= 80 ? "warn" : "muted"}
        />
        <KpiCard
          label={t("dashboard.myActiveConns")}
          value="—"
          delta={t("dashboard.openComing")}
          tone="muted"
        />
      </div>

      <DashboardIssueBlocks
        unhealthyTargets={unhealthyCount}
        offlineClients={offlineClientCount}
      />

      <div className="grid grid-cols-1 gap-3 lg:grid-cols-[2fr_1fr]">
        <ThroughputChart
          samples={traffic.data?.samples}
          isLoading={traffic.isLoading}
          error={traffic.error}
          rangeId={rangeId}
          onRangeChange={setRange}
          onRetry={() => qc.invalidateQueries({ queryKey: userTrafficKey(userId, range) })}
        />
        <TrafficDirectionChart
          rows={directionRows}
          isLoading={traffic.isLoading}
          error={traffic.error}
        />
      </div>
    </div>
  );
}
