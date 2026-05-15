import { useTranslation } from "react-i18next";

import { useAuditLog } from "@/api/audit";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

export function RecentAuditPanel() {
  const { t } = useTranslation();
  const audit = useAuditLog({ limit: 10 });

  // Tenants who lack audit-read permission get a 403; hide the panel
  // rather than render an error state inside it.
  if (audit.error) return null;

  const entries = audit.data ?? [];

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("dashboard.recentAudit")}</CardTitle>
      </CardHeader>
      <CardContent>
        {entries.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t("dashboard.noRecentActivity")}</p>
        ) : (
          <ul className="space-y-1 text-sm">
            {entries.slice(0, 8).map((e, i) => (
              <li key={`${e.timestamp}-${i}`} className="flex justify-between gap-2">
                <span className="truncate">
                  <span className="font-mono text-xs">{e.method}</span> {e.path}
                  {e.actor && <span className="text-muted-foreground"> · {e.actor}</span>}
                </span>
                <span className="shrink-0 text-xs text-muted-foreground">
                  {new Date(e.timestamp).toLocaleTimeString()}
                </span>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
