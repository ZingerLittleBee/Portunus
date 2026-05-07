import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router-dom";
import { Activity, Wifi, WifiOff } from "lucide-react";

import { useRule } from "@/api/rules";
import { useRuleStatsStream } from "@/api/stats-stream";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { formatBytes, formatTimestamp } from "@/lib/format";
import { parseRuleState } from "@/api/types";

export function RuleDetail() {
  const { t } = useTranslation();
  const { ruleId: ruleIdRaw } = useParams<{ ruleId: string }>();
  const ruleId = ruleIdRaw ? Number(ruleIdRaw) : undefined;

  const rule = useRule(ruleId);
  const live = useRuleStatsStream(ruleId);

  if (rule.isLoading) {
    return <p className="text-muted-foreground">{t("table.loading")}</p>;
  }
  if (!rule.data) {
    return (
      <div className="space-y-3">
        <p className="text-muted-foreground">{t("ruleDetail.notFound")}</p>
        <Button asChild variant="outline">
          <Link to="/rules">{t("ruleDetail.backToRules")}</Link>
        </Button>
      </div>
    );
  }

  const r = rule.data;
  const state = parseRuleState(r.state);
  const snap = live.snapshot;

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold">
          {t("ruleDetail.title")} <span className="font-mono text-base text-muted-foreground">#{r.id}</span>
        </h1>
        <div className="mt-2 flex flex-wrap items-center gap-2 text-sm">
          <Badge variant="outline">{r.protocol}</Badge>
          <Badge variant={state.kind === "Active" ? ("success" as never) : "secondary"}>
            {state.kind}
            {state.kind === "Failed" && `: ${state.reason}`}
          </Badge>
          <span className="text-muted-foreground">
            {r.client_name} · {r.listen_port}
            {r.listen_port_end && r.listen_port_end !== r.listen_port ? `–${r.listen_port_end}` : ""} →{" "}
            {r.target_host}:{r.target_port}
            {r.target_port_end && r.target_port_end !== r.target_port ? `–${r.target_port_end}` : ""}
          </span>
        </div>
      </div>

      <Card>
        <CardHeader className="flex-row items-center justify-between">
          <CardTitle className="flex items-center gap-2">
            <Activity className="h-4 w-4" /> {t("ruleDetail.liveStats")}
          </CardTitle>
          <div className="flex items-center gap-2">
            <Badge
              variant={live.source === "sse" ? ("success" as never) : "warning"}
              title={t("ruleDetail.reconnects", { count: live.reconnectAttempts })}
            >
              {live.source === "sse" ? (
                <Wifi className="mr-1 h-3 w-3" />
              ) : (
                <WifiOff className="mr-1 h-3 w-3" />
              )}
              {live.source === "sse" ? t("ruleDetail.streaming") : t("ruleDetail.polling")}
            </Badge>
          </div>
        </CardHeader>
        <CardContent>
          {snap ? (
            <div className="grid grid-cols-2 gap-4 text-sm md:grid-cols-3">
              <Stat label={t("ruleDetail.bytesIn")} value={formatBytes(snap.bytes_in)} />
              <Stat label={t("ruleDetail.bytesOut")} value={formatBytes(snap.bytes_out)} />
              <Stat label={t("ruleDetail.activeConns")} value={String(snap.active_connections)} />
              <Stat label={t("ruleDetail.dnsFailures")} value={String(snap.dns_failures)} />
              <Stat label={t("ruleDetail.activeFlows")} value={String(snap.active_flows)} />
              <Stat label={t("ruleDetail.flowsDropped")} value={String(snap.flows_dropped_overflow)} />
              <Stat label={t("ruleDetail.datagramsIn")} value={String(snap.datagrams_in)} />
              <Stat label={t("ruleDetail.datagramsOut")} value={String(snap.datagrams_out)} />
              <Stat label={t("ruleDetail.updatedAt")} value={formatTimestamp(snap.updated_at)} />
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">{t("ruleDetail.waiting")}</p>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border bg-muted/30 p-3">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="font-mono text-base">{value}</div>
    </div>
  );
}
