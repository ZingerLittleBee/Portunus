import { useTranslation } from "react-i18next";

import { useClientsList } from "@/api/clients";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

function relativeIso(iso: string | null): string {
  if (!iso) return "—";
  const diffSec = Math.max(0, Math.floor((Date.now() - Date.parse(iso)) / 1000));
  if (diffSec < 60) return `${diffSec}s`;
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h`;
  return `${Math.floor(diffSec / 86400)}d`;
}

export function OfflineClientsPanel() {
  const { t } = useTranslation();
  const clients = useClientsList();

  // Sort offline clients by `connected_at` descending — most recently
  // disconnected first (since they were online most recently).
  const offline = (clients.data ?? [])
    .filter((c) => !c.connected && !c.revoked_at)
    .sort((a, b) => Date.parse(b.connected_at ?? "") - Date.parse(a.connected_at ?? ""))
    .slice(0, 8);

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("dashboard.offlineClients")}</CardTitle>
      </CardHeader>
      <CardContent>
        {offline.length === 0 ? (
          <p className="text-xs text-muted-foreground">{t("dashboard.allClientsOnline")}</p>
        ) : (
          <ul className="space-y-1 text-sm">
            {offline.map((c) => (
              <li key={c.client_name} className="flex justify-between gap-2">
                <span className="truncate">{c.client_name}</span>
                <span className="shrink-0 text-xs text-amber-600 dark:text-amber-400">
                  {relativeIso(c.connected_at)}
                </span>
              </li>
            ))}
          </ul>
        )}
      </CardContent>
    </Card>
  );
}
