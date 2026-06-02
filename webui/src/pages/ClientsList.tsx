import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Ban, Circle, CircleDot, MoreHorizontal, Pencil, Plus, Trash2 } from "lucide-react";

import { ApiError } from "@/api/client";
import {
  useClientsList,
  useDeleteClient,
  useRevokeClient,
  useUpdateClient,
} from "@/api/clients";
import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { canProvisionClient } from "@/lib/permissions";
import { DataTable, type Column } from "@/components/DataTable";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ClientProvisionForm } from "@/components/ClientProvisionForm";
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
  const update = useUpdateClient();
  const revoke = useRevokeClient();
  const remove = useDeleteClient();
  const canProvision = canProvisionClient(identity);

  const [showRevoked, setShowRevoked] = useState(false);
  const [provisionOpen, setProvisionOpen] = useState(false);
  const [pendingEdit, setPendingEdit] = useState<ClientView | null>(null);
  const [editAddress, setEditAddress] = useState("");
  const [editError, setEditError] = useState<string | null>(null);
  const [pendingRevoke, setPendingRevoke] = useState<ClientView | null>(null);
  const [pendingDelete, setPendingDelete] = useState<ClientView | null>(null);
  const [revokeError, setRevokeError] = useState<string | null>(null);
  const [deleteError, setDeleteError] = useState<string | null>(null);

  const { rows, revokedCount } = useMemo(() => {
    const all = clients.data ?? [];
    const revokedCount = all.filter((c) => c.revoked_at).length;
    const rows = showRevoked ? all : all.filter((c) => !c.revoked_at);
    return { rows, revokedCount };
  }, [clients.data, showRevoked]);

  function openEdit(client: ClientView) {
    setPendingEdit(client);
    setEditAddress(client.client_address ?? "");
    setEditError(null);
  }

  async function confirmEdit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (!pendingEdit) return;
    setEditError(null);
    try {
      await update.mutateAsync({
        name: pendingEdit.client_name,
        body: { address: editAddress },
      });
      setPendingEdit(null);
    } catch (err) {
      setEditError(err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message);
    }
  }

  async function confirmRevoke() {
    if (!pendingRevoke) return;
    setRevokeError(null);
    try {
      await revoke.mutateAsync(pendingRevoke.client_name);
      setPendingRevoke(null);
    } catch (err) {
      setRevokeError(err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message);
    }
  }

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
          <CircleDot className="size-4 text-emerald-500" aria-label={t("clients.connected")} />
        ) : (
          <Circle className="size-4 text-muted-foreground" aria-label={t("clients.disconnected")} />
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
          <Badge variant="success">{t("clients.connected")}</Badge>
        ) : (
          <Badge variant="secondary">{t("clients.disconnected")}</Badge>
        ),
    },
    {
      key: "actions",
      header: <span className="sr-only">{t("clients.actions")}</span>,
      width: "64px",
      render: (c) => {
        if (!canProvision) return null;
        return (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className="size-8"
                aria-label={t("clients.actions")}
              >
                <MoreHorizontal className="size-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuGroup>
                <DropdownMenuItem onSelect={() => openEdit(c)}>
                  <Pencil className="size-4" />
                  {t("clients.edit")}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                {c.revoked_at ? (
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onSelect={() => {
                      setDeleteError(null);
                      setPendingDelete(c);
                    }}
                  >
                    <Trash2 className="size-4" />
                    {t("clients.delete")}
                  </DropdownMenuItem>
                ) : (
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onSelect={() => {
                      setRevokeError(null);
                      setPendingRevoke(c);
                    }}
                  >
                    <Ban className="size-4" />
                    {t("clients.revoke")}
                  </DropdownMenuItem>
                )}
              </DropdownMenuGroup>
            </DropdownMenuContent>
          </DropdownMenu>
        );
      },
    },
  ];

  return (
    <div className="flex flex-col gap-4">
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
            <Dialog open={provisionOpen} onOpenChange={setProvisionOpen}>
              <DialogTrigger asChild>
                <Button className="w-full sm:w-auto">
                  <Plus className="mr-1 size-4" />
                  {t("clients.provision")}
                </Button>
              </DialogTrigger>
              <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
                <DialogHeader>
                  <DialogTitle>{t("clientProvision.title")}</DialogTitle>
                </DialogHeader>
                <ClientProvisionForm onDone={() => setProvisionOpen(false)} />
              </DialogContent>
            </Dialog>
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
      <Dialog
        open={!!pendingEdit}
        onOpenChange={(open) => {
          if (!open) {
            setPendingEdit(null);
            setEditAddress("");
            setEditError(null);
          }
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t("clients.editDialogTitle")}</DialogTitle>
            <DialogDescription>{t("clients.editDialogBody")}</DialogDescription>
          </DialogHeader>
          <form onSubmit={confirmEdit} className="flex flex-col gap-4">
            <div className="flex flex-col gap-2">
              <Label htmlFor="client-edit-name">{t("clients.name")}</Label>
              <Input
                id="client-edit-name"
                value={pendingEdit?.client_name ?? ""}
                disabled
                className="font-mono"
              />
            </div>
            <div className="flex flex-col gap-2">
              <Label htmlFor="client-edit-address">{t("clientProvision.address")}</Label>
              <Input
                id="client-edit-address"
                value={editAddress}
                onChange={(event) => setEditAddress(event.target.value)}
                placeholder="68.77.201.69 or edge.example.com"
                required
              />
              <p className="text-xs text-muted-foreground">{t("clientProvision.addressHint")}</p>
            </div>
            {editError && <p className="text-sm text-destructive">{editError}</p>}
            <div className="flex flex-col gap-2 sm:flex-row sm:justify-end">
              <Button type="submit" disabled={update.isPending}>
                {update.isPending ? t("confirm.busy") : t("clients.editSave")}
              </Button>
              <Button type="button" variant="outline" onClick={() => setPendingEdit(null)}>
                {t("confirm.cancel")}
              </Button>
            </div>
          </form>
        </DialogContent>
      </Dialog>
      <ConfirmDialog
        open={!!pendingRevoke}
        onOpenChange={(open) => {
          if (!open) {
            setPendingRevoke(null);
            setRevokeError(null);
          }
        }}
        title={t("clients.revokeConfirmTitle", { name: pendingRevoke?.client_name ?? "" })}
        description={t("clients.revokeConfirmBody")}
        confirmLabel={t("clients.revokeConfirmAction")}
        destructive
        busy={revoke.isPending}
        onConfirm={confirmRevoke}
      >
        {revokeError && <p className="text-sm text-destructive">{revokeError}</p>}
      </ConfirmDialog>
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
