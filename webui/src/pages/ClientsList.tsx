import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Plus, CircleDot, Circle } from "lucide-react";

import { useClientsList, useRevokeClient } from "@/api/clients";
import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { canProvisionClient } from "@/lib/permissions";
import { DataTable, type Column } from "@/components/DataTable";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/EmptyState";
import { formatTimestamp } from "@/lib/format";
import type { ClientView } from "@/api/types";

export function ClientsList() {
  const { t } = useTranslation();
  const { data: identity } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  const clients = useClientsList();
  const revoke = useRevokeClient();
  const canProvision = canProvisionClient(identity);

  const columns: Column<ClientView>[] = [
    {
      key: "connected",
      header: "",
      width: "40px",
      render: (c) =>
        c.connected ? (
          <CircleDot className="h-4 w-4 text-emerald-500" aria-label={t("clients.connected")} />
        ) : (
          <Circle className="h-4 w-4 text-muted-foreground" aria-label={t("clients.disconnected")} />
        ),
    },
    {
      key: "name",
      header: t("clients.name"),
      render: (c) => <span className="font-mono">{c.client_name}</span>,
      sortable: true,
      sortValue: (c) => c.client_name,
    },
    {
      key: "remote",
      header: t("clients.remote"),
      render: (c) => c.remote_addr ?? "—",
    },
    {
      key: "since",
      header: t("clients.since"),
      render: (c) => (c.connected_at ? formatTimestamp(c.connected_at) : "—"),
    },
    {
      key: "status",
      header: t("clients.status"),
      width: "120px",
      render: (c) =>
        c.revoked_at ? (
          <Badge variant="destructive">{t("clients.revoked")}</Badge>
        ) : c.connected ? (
          <Badge variant={"success" as never}>{t("clients.connected")}</Badge>
        ) : (
          <Badge variant="secondary">{t("clients.disconnected")}</Badge>
        ),
    },
    {
      key: "actions",
      header: "",
      width: "100px",
      render: (c) =>
        canProvision && !c.revoked_at ? (
          <Button variant="ghost" size="sm" onClick={() => revoke.mutate(c.client_name)}>
            {t("clients.revoke")}
          </Button>
        ) : null,
    },
  ];

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold">{t("clients.title")}</h1>
        {canProvision && (
          <Button asChild>
            <Link to="/clients/new">
              <Plus className="mr-1 h-4 w-4" />
              {t("clients.provision")}
            </Link>
          </Button>
        )}
      </div>
      <DataTable
        rows={clients.data ?? []}
        columns={columns}
        rowKey={(c) => c.client_name}
        emptyState={<EmptyState title={t("clients.emptyTitle")} description={t("clients.emptyBody")} />}
        ariaLabel={t("clients.title")}
      />
    </div>
  );
}
