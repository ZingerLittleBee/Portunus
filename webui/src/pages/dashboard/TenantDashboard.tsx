import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";

import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { useClientsList } from "@/api/clients";
import { useRulesList } from "@/api/rules";
import { useUserQuotas } from "@/api/quotas";
import { useUserTraffic, userTrafficKey } from "@/api/traffic";
import { formatBytes } from "@/lib/format";

import { AlertBanner } from "./components/AlertBanner";
import { KpiCard } from "./components/KpiCard";
import { OfflineClientsPanel } from "./components/OfflineClientsPanel";
import { RecentAuditPanel } from "./components/RecentAuditPanel";
import { ThroughputChart } from "./components/ThroughputChart";
import { UnhealthyTargetsPanel } from "./components/UnhealthyTargetsPanel";
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

  const connectedCount = (clients.data ?? []).filter((c) => c.connected).length;
  const totalClients = clients.data?.length ?? 0;
  const ruleCount = rules.data?.length ?? 0;
  const offlineClientCount = (clients.data ?? [])
    .filter((c) => !c.connected && !c.revoked_at).length;

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

  return (
    <div className="space-y-4">
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

      <div className="grid grid-cols-1 gap-3 md:grid-cols-3">
        <UnhealthyTargetsPanel />
        <OfflineClientsPanel />
        <RecentAuditPanel />
      </div>

      <ThroughputChart
        samples={traffic.data?.samples}
        isLoading={traffic.isLoading}
        error={traffic.error}
        rangeId={rangeId}
        onRangeChange={setRange}
        onRetry={() => qc.invalidateQueries({ queryKey: userTrafficKey(userId, range) })}
      />
    </div>
  );
}
