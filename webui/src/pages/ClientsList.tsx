import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Plus, CircleDot, Circle, Trash2 } from "lucide-react";

import { ApiError } from "@/api/client";
import { useClientsList, useDeleteClient, useRevokeClient } from "@/api/clients";
import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { canProvisionClient } from "@/lib/permissions";
import { DataTable, type Column } from "@/components/DataTable";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { ConfirmDialog } from "@/components/ConfirmDialog";
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
  const remove = useDeleteClient();
  const canProvision = canProvisionClient(identity);

  const [showRevoked, setShowRevoked] = useState(false);
  const [pendingDelete, setPendingDelete] = useState<ClientView | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const { rows, revokedCount } = useMemo(() => {
    const all = clients.data ?? [];
    const revokedCount = all.filter((c) => c.revoked_at).length;
    const rows = showRevoked ? all : all.filter((c) => !c.revoked_at);
    return { rows, revokedCount };
  }, [clients.data, showRevoked]);

  async function confirmDelete() {
    if (!pendingDelete) return;
    setDeleteError(null);
    try {
      await remove.mutateAsync(pendingDelete.client_name);
      setPendingDelete(null);
    } catch (err) {
      setDeleteError(err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message);
    }
  }

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
      render: (c) => (
        <Link
          to={`/clients/${encodeURIComponent(c.client_name)}`}
          className="font-mono text-primary hover:underline"
        >
          {c.client_name}
        </Link>
      ),
      sortable: true,
      sortValue: (c) => c.client_name,
    },
    {
      key: "address",
      header: t("clients.address"),
      render: (c) => c.client_address ?? "—",
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
      width: "180px",
      render: (c) => {
        if (!canProvision) return null;
        if (c.revoked_at) {
          return (
            <Button
              variant="ghost"
              size="sm"
              className="text-destructive"
              onClick={() => {
                setDeleteError(null);
                setPendingDelete(c);
              }}
            >
              <Trash2 className="mr-1 h-4 w-4" />
              {t("clients.delete")}
            </Button>
          );
        }
        return (
          <Button variant="ghost" size="sm" onClick={() => revoke.mutate(c.client_name)}>
            {t("clients.revoke")}
          </Button>
        );
      },
    },
  ];

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <h1 className="text-2xl font-semibold">{t("clients.title")}</h1>
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
          {revokedCount > 0 && (
            <Button
              variant={showRevoked ? "secondary" : "outline"}
              size="sm"
              onClick={() => setShowRevoked((s) => !s)}
            >
              {t("clients.showRevoked")} · {t("clients.revokedCount", { count: revokedCount })}
            </Button>
          )}
          {canProvision && (
            <Button asChild className="w-full sm:w-auto">
              <Link to="/clients/new">
                <Plus className="mr-1 h-4 w-4" />
                {t("clients.provision")}
              </Link>
            </Button>
          )}
        </div>
      </div>
      <DataTable
        rows={rows}
        columns={columns}
        rowKey={(c) => c.client_name}
        emptyState={<EmptyState title={t("clients.emptyTitle")} description={t("clients.emptyBody")} />}
        ariaLabel={t("clients.title")}
      />
      <ConfirmDialog
        open={!!pendingDelete}
        onOpenChange={(open) => {
          if (!open) {
            setPendingDelete(null);
            setDeleteError(null);
          }
        }}
        title={t("clients.deleteConfirmTitle", { name: pendingDelete?.client_name ?? "" })}
        description={t("clients.deleteConfirmBody")}
        confirmLabel={t("clients.deleteConfirmAction")}
        destructive
        busy={remove.isPending}
        onConfirm={confirmDelete}
      >
        {deleteError && <p className="text-sm text-destructive">{deleteError}</p>}
      </ConfirmDialog>
    </div>
  );
}
