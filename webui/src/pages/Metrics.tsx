import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Copy, Check } from "lucide-react";

import { useDashboardGauges, useMetricsText } from "@/api/metrics";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";

export function Metrics() {
  const { t } = useTranslation();
  const metrics = useMetricsText();
  const gauges = useDashboardGauges();
  const [copied, setCopied] = useState(false);

  async function copyAll() {
    if (!metrics.data) return;
    try {
      await navigator.clipboard.writeText(metrics.data);
      setCopied(true);
      setTimeout(() => setCopied(false), 2_000);
    } catch {
      /* ignore */
    }
  }

  return (
    <div className="space-y-6">
      <h1 className="text-2xl font-semibold">{t("metrics.title")}</h1>

      <div className="grid gap-4 md:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>{t("dashboard.connectedClients")}</CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-3xl font-semibold">{gauges.clientsConnected ?? "—"}</p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>{t("dashboard.activeRules")}</CardTitle>
          </CardHeader>
          <CardContent>
            <p className="text-3xl font-semibold">{gauges.rulesActive ?? "—"}</p>
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader className="flex-row items-center justify-between">
          <CardTitle>{t("metrics.rawHeading")}</CardTitle>
          <Button variant="outline" size="sm" onClick={copyAll} disabled={!metrics.data}>
            {copied ? <Check className="mr-1 h-4 w-4" /> : <Copy className="mr-1 h-4 w-4" />}
            {copied ? t("tokenReveal.copied") : t("metrics.copyAll")}
          </Button>
        </CardHeader>
        <CardContent>
          {metrics.isLoading || !metrics.data ? (
            <Skeleton className="h-64 w-full" />
          ) : (
            <pre className="max-h-[500px] overflow-auto rounded-md bg-muted p-3 text-xs leading-snug">
              {metrics.data}
            </pre>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
