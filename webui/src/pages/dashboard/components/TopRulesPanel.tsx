import { useTranslation } from "react-i18next";

import type { TopRule } from "@/api/metrics";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { formatBytes } from "@/lib/format";

export interface TopRulesPanelProps {
  rules: TopRule[];
}

export function TopRulesPanel({ rules }: TopRulesPanelProps) {
  const { t } = useTranslation();
  const max = rules[0]?.total ?? 0;

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("dashboard.topRules")}</CardTitle>
      </CardHeader>
      <CardContent>
        {rules.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t("dashboard.noRulesYet")}</p>
        ) : (
          <ul className="space-y-2 text-xs">
            {rules.map((r) => (
              <li key={r.rule}>
                <div className="flex justify-between">
                  <span className="truncate font-medium">#{r.rule}</span>
                  <span className="tabular-nums text-muted-foreground">{formatBytes(r.total)}</span>
                </div>
                <div className="mt-1 h-1 overflow-hidden rounded bg-muted">
                  <div
                    className="h-full bg-blue-500"
                    style={{ width: `${max > 0 ? (r.total / max) * 100 : 0}%` }}
                  />
                </div>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
