import { useTranslation } from "react-i18next";

import { useRulesList } from "@/api/rules";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

interface UnhealthyEntry {
  ruleId: number;
  endpoint: string;
  lastFailedAtMs: number | null;
}

function relativeMs(ms: number): string {
  const diffSec = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (diffSec < 60) return `${diffSec}s ago`;
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86400)}d ago`;
}

export function UnhealthyTargetsPanel() {
  const { t } = useTranslation();
  const rules = useRulesList();
  const entries: UnhealthyEntry[] = (rules.data ?? []).flatMap((rule) =>
    (rule.targets ?? [])
      .filter((target) => target.health?.healthy === false)
      .map((target) => ({
        ruleId: rule.id,
        endpoint: `${target.host}:${target.port}`,
        lastFailedAtMs: target.health?.last_failure_at_unix_ms ?? null,
      })),
  );

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("dashboard.unhealthyTargets")}</CardTitle>
      </CardHeader>
      <CardContent>
        {entries.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t("dashboard.allTargetsHealthy")}</p>
        ) : (
          <ul className="space-y-1 text-sm">
            {entries.slice(0, 8).map((e, i) => (
              <li key={`${e.ruleId}-${e.endpoint}-${i}`} className="flex justify-between gap-2">
                <span className="truncate">#{e.ruleId} → {e.endpoint}</span>
                <span className="shrink-0 text-xs text-red-600 dark:text-red-400">
                  {e.lastFailedAtMs ? relativeMs(e.lastFailedAtMs) : t("dashboard.down")}
                </span>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
