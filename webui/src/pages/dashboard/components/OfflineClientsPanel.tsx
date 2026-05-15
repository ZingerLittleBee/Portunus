// webui/src/pages/dashboard/components/OfflineClientsPanel.tsx
import { useTranslation } from "react-i18next";

import { useClientsList } from "@/api/clients";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

function parseTs(iso: string | null): number {
  if (!iso) return 0;
  const v = Date.parse(iso);
  return Number.isFinite(v) ? v : 0;
}

function relativeIso(iso: string | null): string {
  const ms = parseTs(iso);
  if (ms === 0) return "—";
  const diffSec = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (diffSec < 60) return `${diffSec}s`;
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h`;
  return `${Math.floor(diffSec / 86400)}d`;
}

export function OfflineClientsPanel() {
  const { t } = useTranslation();
  const clients = useClientsList();

  // Sort offline clients by `connected_at` descending (most recently
  // online first). Never-connected clients sort to the bottom via the
  // 0 sentinel in `parseTs`.
  const offline = (clients.data ?? [])
    .filter((c) => !c.connected && !c.revoked_at)
    .sort((a, b) => parseTs(b.connected_at) - parseTs(a.connected_at))
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
