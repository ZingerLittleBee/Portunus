import { useTranslation } from "react-i18next";
import { Link, useParams } from "react-router-dom";
import { Activity, Wifi, WifiOff } from "lucide-react";

import { useRule } from "@/api/rules";
import { useRuleStatsStream } from "@/api/stats-stream";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { formatBytes, formatTimestamp } from "@/lib/format";
import { parseRuleState, type PerTargetStats, type TargetWithHealth } from "@/api/types";

export function RuleDetail() {
  const { t } = useTranslation();
  const { ruleId: ruleIdRaw } = useParams<{ ruleId: string }>();
  const ruleId = ruleIdRaw ? Number(ruleIdRaw) : undefined;

  const rule = useRule(ruleId);
  // 007-multi-target-failover T047: opt into per-target stats. Rules
  // with `targets[]` need the per-target byte counters surfaced; the
  // stream still works for legacy single-target rules (the per_target
  // body is just empty).
  const live = useRuleStatsStream(ruleId, { perTarget: true });

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
  const isMultiTarget = (r.targets?.length ?? 0) > 1;

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
          {isMultiTarget && (
            <Badge variant="outline" title={t("ruleDetail.multiTargetTooltip")}>
              {t("ruleDetail.multiTargetBadge", { count: r.targets?.length ?? 0 })}
            </Badge>
          )}
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
              {isMultiTarget && (
                <Stat
                  label={t("ruleDetail.targetFailovers")}
                  value={String(snap.target_failovers_total ?? 0)}
                />
              )}
              <Stat label={t("ruleDetail.updatedAt")} value={formatTimestamp(snap.updated_at)} />
            </div>
          ) : (
            <p className="text-sm text-muted-foreground">{t("ruleDetail.waiting")}</p>
          )}
        </CardContent>
      </Card>

      {(r.targets?.length ?? 0) > 0 && (
        <Card>
          <CardHeader>
            <CardTitle>{t("ruleDetail.targetsTitle")}</CardTitle>
          </CardHeader>
          <CardContent>
            <TargetsTable
              targets={r.targets ?? []}
              perTarget={snap?.per_target ?? []}
            />
          </CardContent>
        </Card>
      )}
    </div>
  );
}

function TargetsTable({
  targets,
  perTarget,
}: {
  targets: TargetWithHealth[];
  perTarget: PerTargetStats[];
}) {
  const { t } = useTranslation();
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm">
        <thead>
          <tr className="border-b text-left text-xs uppercase text-muted-foreground">
            <th className="px-2 py-2">#</th>
            <th className="px-2 py-2">{t("ruleDetail.targetCol.host")}</th>
            <th className="px-2 py-2">{t("ruleDetail.targetCol.port")}</th>
            <th className="px-2 py-2">{t("ruleDetail.targetCol.priority")}</th>
            <th className="px-2 py-2">{t("ruleDetail.targetCol.health")}</th>
            <th className="px-2 py-2">{t("ruleDetail.targetCol.consecutiveFailures")}</th>
            <th className="px-2 py-2 text-right">{t("ruleDetail.targetCol.bytesIn")}</th>
            <th className="px-2 py-2 text-right">{t("ruleDetail.targetCol.bytesOut")}</th>
            <th className="px-2 py-2 text-right">{t("ruleDetail.targetCol.connections")}</th>
            <th className="px-2 py-2">{t("ruleDetail.targetCol.lastFailure")}</th>
          </tr>
        </thead>
        <tbody>
          {targets.map((target, idx) => {
            const stats = perTarget.find((p) => p.index === idx);
            const healthBadge = renderHealthBadge(target, stats, t);
            const lastFailure =
              stats?.last_failure_at_unix_ms && stats.last_failure_at_unix_ms > 0
                ? new Date(stats.last_failure_at_unix_ms).toLocaleString()
                : "—";
            return (
              <tr key={`${target.host}:${target.port}-${idx}`} className="border-b">
                <td className="px-2 py-2 font-mono">{idx}</td>
                <td className="px-2 py-2 font-mono">{target.host}</td>
                <td className="px-2 py-2 font-mono">{target.port}</td>
                <td className="px-2 py-2 font-mono">{target.priority}</td>
                <td className="px-2 py-2">{healthBadge}</td>
                <td className="px-2 py-2 font-mono">
                  {stats?.consecutive_failures ?? target.health?.consecutive_failures ?? 0}
                </td>
                <td className="px-2 py-2 text-right font-mono">
                  {stats ? formatBytes(stats.bytes_in) : "—"}
                </td>
                <td className="px-2 py-2 text-right font-mono">
                  {stats ? formatBytes(stats.bytes_out) : "—"}
                </td>
                <td className="px-2 py-2 text-right font-mono">
                  {stats?.connections_accepted ?? "—"}
                </td>
                <td className="px-2 py-2 text-xs text-muted-foreground">{lastFailure}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

function renderHealthBadge(
  target: TargetWithHealth,
  stats: PerTargetStats | undefined,
  t: (key: string) => string,
) {
  // Per-target stats (when available) win — they're populated from the
  // live HealthState. Fall back to the rule-listing snapshot, then to
  // "unknown" before any observation has landed.
  let healthy: boolean | undefined;
  if (stats) {
    healthy = stats.health === 0;
  } else if (target.health) {
    healthy = target.health.healthy;
  }
  if (healthy === undefined) {
    return (
      <Badge variant="secondary">{t("ruleDetail.health.unknown")}</Badge>
    );
  }
  if (healthy) {
    return (
      <Badge variant={"success" as never}>{t("ruleDetail.health.healthy")}</Badge>
    );
  }
  return (
    <Badge variant="destructive">{t("ruleDetail.health.failed")}</Badge>
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
