import { useMemo, useReducer, type Dispatch, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Ban, Circle, CircleDot, MoreHorizontal, Pencil, Plus, Trash2 } from "lucide-react";

import { formatApiError } from "@/api/client";
import {
  useClientsList,
  useDeleteClient,
  useRenameClient,
  useRevokeClient,
  useUpdateClient,
} from "@/api/clients";
import { ME_QUERY_KEY, fetchIdentity } from "@/auth/identity";
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
import { formatTimestamp, shortId } from "@/lib/format";
import type { ClientView } from "@/api/types";

interface ClientsListState {
  showRevoked: boolean;
  provisionOpen: boolean;
  pendingEdit: ClientView | null;
  editName: string;
  editAddress: string;
  editError: string | null;
  pendingRevoke: ClientView | null;
  pendingDelete: ClientView | null;
  revokeError: string | null;
  deleteError: string | null;
}

type ClientsListAction =
  | { type: "toggle-revoked" }
  | { type: "provision-open"; open: boolean }
  | { type: "edit-open"; client: ClientView }
  | { type: "edit-close" }
  | { type: "edit-name"; value: string }
  | { type: "edit-address"; value: string }
  | { type: "edit-error"; message: string | null }
  | { type: "revoke-open"; client: ClientView }
  | { type: "revoke-close" }
  | { type: "revoke-error"; message: string | null }
  | { type: "delete-open"; client: ClientView }
  | { type: "delete-close" }
  | { type: "delete-error"; message: string | null };

const initialClientsListState: ClientsListState = {
  showRevoked: false,
  provisionOpen: false,
  pendingEdit: null,
  editName: "",
  editAddress: "",
  editError: null,
  pendingRevoke: null,
  pendingDelete: null,
  revokeError: null,
  deleteError: null,
};

function clientsListReducer(
  state: ClientsListState,
  action: ClientsListAction,
): ClientsListState {
  switch (action.type) {
    case "toggle-revoked":
      return { ...state, showRevoked: !state.showRevoked };
    case "provision-open":
      return { ...state, provisionOpen: action.open };
    case "edit-open":
      return {
        ...state,
        pendingEdit: action.client,
        editName: action.client.client_name,
        editAddress: action.client.client_address ?? "",
        editError: null,
      };
    case "edit-close":
      return {
        ...state,
        pendingEdit: null,
        editName: "",
        editAddress: "",
        editError: null,
      };
    case "edit-name":
      return { ...state, editName: action.value };
    case "edit-address":
      return { ...state, editAddress: action.value };
    case "edit-error":
      return { ...state, editError: action.message };
    case "revoke-open":
      return { ...state, pendingRevoke: action.client, revokeError: null };
    case "revoke-close":
      return { ...state, pendingRevoke: null, revokeError: null };
    case "revoke-error":
      return { ...state, revokeError: action.message };
    case "delete-open":
      return { ...state, pendingDelete: action.client, deleteError: null };
    case "delete-close":
      return { ...state, pendingDelete: null, deleteError: null };
    case "delete-error":
      return { ...state, deleteError: action.message };
  }
}

interface BuildClientColumnsArgs {
  canProvision: boolean;
  dispatch: Dispatch<ClientsListAction>;
  t: ReturnType<typeof useTranslation>["t"];
}

function buildClientColumns({ canProvision, dispatch, t }: BuildClientColumnsArgs): Column<ClientView>[] {
  return [
    {
      key: "connected",
      header: "",
      width: "40px",
      render: (client) =>
        client.connected ? (
          <CircleDot className="size-4 text-emerald-500" aria-label={t("clients.connected")} />
        ) : (
          <Circle className="size-4 text-muted-foreground" aria-label={t("clients.disconnected")} />
        ),
    },
    {
      key: "name",
      header: t("clients.name"),
      render: (client) => (
        <div className="flex flex-col">
          <Link
            to={`/clients/${encodeURIComponent(client.client_id)}`}
            className="text-primary hover:underline"
          >
            {client.client_name}
          </Link>
          {/* 015-client-stable-id (US3 / FR-013): a short id disambiguates
              duplicate display names, which are now allowed. */}
          <code className="text-xs text-muted-foreground">{shortId(client.client_id)}</code>
        </div>
      ),
      sortable: true,
      sortValue: (client) => client.client_name,
    },
    {
      key: "address",
      header: t("clients.address"),
      render: (client) => client.client_address ?? "—",
    },
    {
      key: "since",
      header: t("clients.since"),
      render: (client) => (client.connected_at ? formatTimestamp(client.connected_at) : "—"),
    },
    {
      key: "status",
      header: t("clients.status"),
      width: "120px",
      render: (client) =>
        client.revoked_at ? (
          <Badge variant="destructive">{t("clients.revoked")}</Badge>
        ) : client.connected ? (
          <Badge variant="success">{t("clients.connected")}</Badge>
        ) : (
          <Badge variant="secondary">{t("clients.disconnected")}</Badge>
        ),
    },
    {
      key: "actions",
      header: <span className="sr-only">{t("clients.actions")}</span>,
      width: "64px",
      render: (client) => {
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
                <DropdownMenuItem onSelect={() => dispatch({ type: "edit-open", client })}>
                  <Pencil className="size-4" />
                  {t("clients.edit")}
                </DropdownMenuItem>
                <DropdownMenuSeparator />
                {client.revoked_at ? (
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onSelect={() => dispatch({ type: "delete-open", client })}
                  >
                    <Trash2 className="size-4" />
                    {t("clients.delete")}
                  </DropdownMenuItem>
                ) : (
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onSelect={() => dispatch({ type: "revoke-open", client })}
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
}

export function ClientsList() {
  const { t } = useTranslation();
  const { data: identity } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  const clients = useClientsList();
  const update = useUpdateClient();
  const rename = useRenameClient();
  const revoke = useRevokeClient();
  const remove = useDeleteClient();
  const canProvision = canProvisionClient(identity);

  const [state, dispatch] = useReducer(clientsListReducer, initialClientsListState);

  const { rows, revokedCount } = useMemo(() => {
    const all = clients.data ?? [];
    const revokedCount = all.filter((c) => c.revoked_at).length;
    const rows = state.showRevoked ? all : all.filter((c) => !c.revoked_at);
    return { rows, revokedCount };
  }, [clients.data, state.showRevoked]);

  async function confirmEdit(e: FormEvent<HTMLFormElement>) {
    e.preventDefault();
    if (!state.pendingEdit) return;
    dispatch({ type: "edit-error", message: null });
    try {
      // Address update is still addressed by the current name; run it
      // BEFORE the rename so its key is valid.
      if (state.editAddress !== (state.pendingEdit.client_address ?? "")) {
        await update.mutateAsync({
          clientId: state.pendingEdit.client_id,
          body: { address: state.editAddress },
        });
      }
      // 015-client-stable-id (US2): identity-safe rename addressed by
      // the stable client_id. Skipped when the name is unchanged.
      if (state.editName !== state.pendingEdit.client_name) {
        await rename.mutateAsync({
          clientId: state.pendingEdit.client_id,
          clientName: state.editName,
        });
      }
      dispatch({ type: "edit-close" });
    } catch (err) {
      dispatch({ type: "edit-error", message: formatApiError(err) });
    }
  }

  async function confirmRevoke() {
    if (!state.pendingRevoke) return;
    dispatch({ type: "revoke-error", message: null });
    try {
      await revoke.mutateAsync(state.pendingRevoke.client_id);
      dispatch({ type: "revoke-close" });
    } catch (err) {
      dispatch({ type: "revoke-error", message: formatApiError(err) });
    }
  }

  async function confirmDelete() {
    if (!state.pendingDelete) return;
    dispatch({ type: "delete-error", message: null });
    try {
      await remove.mutateAsync(state.pendingDelete.client_id);
      dispatch({ type: "delete-close" });
    } catch (err) {
      dispatch({ type: "delete-error", message: formatApiError(err) });
    }
  }

  const columns = useMemo(
    () => buildClientColumns({ canProvision, dispatch, t }),
    [canProvision, t],
  );

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <h1 className="text-2xl font-semibold">{t("clients.title")}</h1>
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
          {revokedCount > 0 && (
            <Button
              variant={state.showRevoked ? "secondary" : "outline"}
              size="sm"
              onClick={() => dispatch({ type: "toggle-revoked" })}
            >
              {t("clients.showRevoked")} · {t("clients.revokedCount", { count: revokedCount })}
            </Button>
          )}
          {canProvision && (
            <Dialog
              open={state.provisionOpen}
              onOpenChange={(open) => dispatch({ type: "provision-open", open })}
            >
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
                <ClientProvisionForm onDone={() => dispatch({ type: "provision-open", open: false })} />
              </DialogContent>
            </Dialog>
          )}
        </div>
      </div>
      <DataTable
        rows={rows}
        columns={columns}
        rowKey={(c) => c.client_id}
        emptyState={<EmptyState title={t("clients.emptyTitle")} description={t("clients.emptyBody")} />}
        ariaLabel={t("clients.title")}
      />
      <Dialog
        open={!!state.pendingEdit}
        onOpenChange={(open) => {
          if (!open) {
            dispatch({ type: "edit-close" });
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
                value={state.editName}
                onChange={(event) => dispatch({ type: "edit-name", value: event.target.value })}
                required
              />
              <p className="text-xs text-muted-foreground">{t("clients.renameHint")}</p>
            </div>
            <div className="flex flex-col gap-2">
              <Label htmlFor="client-edit-address">{t("clientProvision.address")}</Label>
              <Input
                id="client-edit-address"
                value={state.editAddress}
                onChange={(event) => dispatch({ type: "edit-address", value: event.target.value })}
                placeholder="68.77.201.69 or edge.example.com"
                required
              />
              <p className="text-xs text-muted-foreground">{t("clientProvision.addressHint")}</p>
            </div>
            {state.editError && <p className="text-sm text-destructive">{state.editError}</p>}
            <div className="flex flex-col gap-2 sm:flex-row sm:justify-end">
              <Button type="submit" disabled={update.isPending || rename.isPending}>
                {update.isPending || rename.isPending ? t("confirm.busy") : t("clients.editSave")}
              </Button>
              <Button type="button" variant="outline" onClick={() => dispatch({ type: "edit-close" })}>
                {t("confirm.cancel")}
              </Button>
            </div>
          </form>
        </DialogContent>
      </Dialog>
      <ConfirmDialog
        open={!!state.pendingRevoke}
        onOpenChange={(open) => {
          if (!open) {
            dispatch({ type: "revoke-close" });
          }
        }}
        title={t("clients.revokeConfirmTitle", { name: state.pendingRevoke?.client_name ?? "" })}
        description={t("clients.revokeConfirmBody")}
        confirmLabel={t("clients.revokeConfirmAction")}
        destructive
        busy={revoke.isPending}
        onConfirm={confirmRevoke}
      >
        {state.revokeError && <p className="text-sm text-destructive">{state.revokeError}</p>}
      </ConfirmDialog>
      <ConfirmDialog
        open={!!state.pendingDelete}
        onOpenChange={(open) => {
          if (!open) {
            dispatch({ type: "delete-close" });
          }
        }}
        title={t("clients.deleteConfirmTitle", { name: state.pendingDelete?.client_name ?? "" })}
        description={t("clients.deleteConfirmBody")}
        confirmLabel={t("clients.deleteConfirmAction")}
        destructive
        busy={remove.isPending}
        onConfirm={confirmDelete}
      >
        {state.deleteError && <p className="text-sm text-destructive">{state.deleteError}</p>}
      </ConfirmDialog>
    </div>
  );
}
