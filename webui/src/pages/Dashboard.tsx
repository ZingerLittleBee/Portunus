import { useTranslation } from "react-i18next";
import { useQuery } from "@tanstack/react-query";

import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { useClientsList } from "@/api/clients";
import { useRulesList } from "@/api/rules";
import { useDashboardGauges } from "@/api/metrics";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";

export function Dashboard() {
  const { t } = useTranslation();
  const { data: identity } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  const clients = useClientsList();
  const rules = useRulesList();
  const gauges = useDashboardGauges();

  // Prefer the Prometheus gauge when available (it's the authoritative
  // count); fall back to the live list shape so tenants — who can't
  // read /metrics — still see something useful.
  const connectedCount =
    gauges.clientsConnected ?? (clients.data ?? []).filter((c) => c.connected).length;
  const ruleCount = gauges.rulesActive ?? (rules.data ?? []).length;

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold">
          {t("dashboard.greeting")}, {identity?.display_name ?? identity?.user_id ?? "—"}
        </h1>
        {identity && (
          <Badge className="mt-2" variant={identity.role === "superadmin" ? "default" : "secondary"}>
            {identity.role}
          </Badge>
        )}
      </div>
      <div className="grid gap-4 md:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>{t("dashboard.connectedClients")}</CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-3xl font-semibold">
              {clients.isLoading ? "…" : connectedCount}
            </p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>{t("dashboard.activeRules")}</CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-3xl font-semibold">{rules.isLoading ? "…" : ruleCount}</p>
          </CardContent>
        </Card>
      </div>
    </div>
  );
}
