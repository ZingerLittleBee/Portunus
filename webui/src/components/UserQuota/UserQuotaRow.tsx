// webui/src/components/UserQuota/UserQuotaRow.tsx
import {
  AlertTriangle,
  ChevronDown,
  ChevronRight,
  MoreHorizontal,
  Pencil,
  Trash2,
} from "lucide-react";
import { useReducer } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import {
  useDeleteAccessEntry,
  useUpdateAccessEntry,
  type AccessEntry,
} from "@/api/access-entries";
import { formatApiError } from "@/api/client";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { TableCell, TableRow } from "@/components/ui/table";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { formatBps } from "./format";
import { QuotaCellMonthly } from "./QuotaCellMonthly";
import { QuotaCellPeriodProgress } from "./QuotaCellPeriodProgress";
import { UserQuotaForm, type UserQuotaFormSubmitValue } from "./UserQuotaForm";
import type { ClientLite } from "./ClientCombobox";

interface Props {
  userId: string;
  entry: AccessEntry;
  clients: ClientLite[];
  clientOnline: boolean;
  readOnly: boolean;
}

interface RowState {
  expanded: boolean;
  editOpen: boolean;
  editDialogContainer: HTMLDivElement | null;
  confirmDelete: boolean;
  serverError: string | null;
  staleFailure: string | null;
}

type RowAction =
  | { type: "toggle-expanded" }
  | { type: "edit-open"; open: boolean }
  | { type: "edit-container"; container: HTMLDivElement | null }
  | { type: "confirm-delete"; open: boolean }
  | { type: "server-error"; message: string | null }
  | { type: "stale-failure"; message: string };

const initialRowState: RowState = {
  expanded: false,
  editOpen: false,
  editDialogContainer: null,
  confirmDelete: false,
  serverError: null,
  staleFailure: null,
};

function rowReducer(state: RowState, action: RowAction): RowState {
  switch (action.type) {
    case "toggle-expanded":
      return { ...state, expanded: !state.expanded };
    case "edit-open":
      return {
        ...state,
        editOpen: action.open,
        serverError: action.open ? state.serverError : null,
      };
    case "edit-container":
      return { ...state, editDialogContainer: action.container };
    case "confirm-delete":
      return { ...state, confirmDelete: action.open };
    case "server-error":
      return { ...state, serverError: action.message };
    case "stale-failure":
      return { ...state, staleFailure: action.message };
  }
}

function hasStage(err: unknown): err is { stage?: unknown } {
  return typeof err === "object" && err !== null && "stage" in err;
}

export function UserQuotaRow({ userId, entry, clients, clientOnline, readOnly }: Props) {
  const { t } = useTranslation();
  const [state, dispatch] = useReducer(rowReducer, initialRowState);
  const update = useUpdateAccessEntry(userId);
  const del = useDeleteAccessEntry(userId);
  const hasDetail = readOnly || state.staleFailure !== null || entry.legacy_duplicates !== undefined;

  async function onSubmit(v: UserQuotaFormSubmitValue) {
    dispatch({ type: "server-error", message: null });
    try {
      await update.mutateAsync({
        user_id: userId,
        client_id: entry.client_id,
        client_name: v.client_name,
        grant_id: entry.grant_id,
        old: {
          listen_port_start: entry.listen_port_start,
          listen_port_end: entry.listen_port_end,
          protocols: entry.protocols,
        },
        listen_port_start: v.listen_port_start,
        listen_port_end: v.listen_port_end,
        protocols: v.protocols,
        ...(v.cap !== undefined ? { cap: v.cap } : {}),
        ...(entry.legacy_duplicates
          ? { legacy_duplicate_ids: entry.legacy_duplicates.map((g) => g.grant_id) }
          : {}),
      });
      toast.success(t("userQuota.toast.updated", { client: v.client_name }));
      dispatch({ type: "edit-open", open: false });
    } catch (err) {
      dispatch({ type: "server-error", message: formatApiError(err) });
      toast.error(t("userQuota.toast.updateFailed"));

      // If the recreate-after-delete leg failed, the user's access is now
      // gone with no automatic recovery. Persist a row-level banner so
      // the operator sees the inconsistency even after the row re-renders.
      const stage = hasStage(err) ? err.stage : undefined;
      if (stage === "grant_create") {
        dispatch({ type: "stale-failure", message: t("userQuota.row.staleAfterDeleteHint") });
      }
    }
  }

  async function onDelete() {
    try {
      await del.mutateAsync({
        grant_id: entry.grant_id,
        user_id: userId,
        client_id: entry.client_id,
        ...(entry.legacy_duplicates
          ? { legacy_duplicate_ids: entry.legacy_duplicates.map((g) => g.grant_id) }
          : {}),
      });
      toast.success(t("userQuota.toast.deleted", { client: entry.client_name }));
      dispatch({ type: "confirm-delete", open: false });
    } catch (err) {
      toast.error(`${t("userQuota.toast.deleteFailed")}: ${formatApiError(err)}`);
    }
  }

  return (
    <>
      <TableRow>
        <TableCell>
          {hasDetail && (
            <Button
              variant="ghost"
              size="sm"
              onClick={() => dispatch({ type: "toggle-expanded" })}
              aria-label={state.expanded ? t("userQuota.row.collapse") : t("userQuota.row.expand")}
            >
              {state.expanded ? <ChevronDown className="size-4" /> : <ChevronRight className="size-4" />}
            </Button>
          )}
        </TableCell>
        <TableCell className="font-mono">{entry.client_name}</TableCell>
        <TableCell className="font-mono">
          {entry.listen_port_start}-{entry.listen_port_end}
        </TableCell>
        <TableCell>{entry.protocols.map((p) => p.toUpperCase()).join(", ")}</TableCell>
        <TableCell>
          {entry.unlimited ? (
            <Badge>{t("userQuota.unlimited")}</Badge>
          ) : entry.cap?.bandwidth_in_bps ? (
            formatBps(entry.cap.bandwidth_in_bps)
          ) : (
            "—"
          )}
        </TableCell>
        <TableCell>
          {entry.unlimited ? (
            <Badge>{t("userQuota.unlimited")}</Badge>
          ) : entry.cap?.bandwidth_out_bps ? (
            formatBps(entry.cap.bandwidth_out_bps)
          ) : (
            "—"
          )}
        </TableCell>
        <TableCell>{entry.unlimited ? "—" : (entry.cap?.concurrent_connections ?? "—")}</TableCell>
        <TableCell>
          {entry.unlimited ? "—" : (entry.cap?.new_connections_per_sec ?? "—")}
        </TableCell>
        <TableCell>
          <QuotaCellMonthly quota={entry.quota} />
        </TableCell>
        <TableCell>
          <QuotaCellPeriodProgress quota={entry.quota} />
        </TableCell>
        <TableCell>
          {clientOnline ? (
            <Badge variant="success">{t("userQuota.online")}</Badge>
          ) : (
            <Badge variant="secondary">{t("userQuota.offline")}</Badge>
          )}
          {entry.legacy_duplicates && (
            <span title={t("userQuota.row.duplicateTooltip")}>
              <AlertTriangle className="ml-2 inline size-4 text-amber-500" />
            </span>
          )}
          {state.staleFailure && (
            <span title={state.staleFailure} className="ml-2 inline-flex">
              <AlertTriangle className="size-4 text-destructive" />
            </span>
          )}
        </TableCell>
        <TableCell>
          {!readOnly && (
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className="size-8"
                  aria-label={t("userQuota.row.actions")}
                >
                  <MoreHorizontal className="size-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuGroup>
                  <DropdownMenuItem onSelect={() => dispatch({ type: "edit-open", open: true })}>
                    <Pencil className="size-4" />
                    {t("userQuota.row.edit")}
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    className="text-destructive focus:text-destructive"
                    onSelect={() => dispatch({ type: "confirm-delete", open: true })}
                  >
                    <Trash2 className="size-4" />
                    {t("userQuota.row.delete")}
                  </DropdownMenuItem>
                </DropdownMenuGroup>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
        </TableCell>
      </TableRow>
      {state.expanded && hasDetail && (
        <TableRow>
          <TableCell colSpan={12} className="bg-muted/30">
            {state.staleFailure && (
              <Alert variant="destructive" className="mb-3">
                <AlertTriangle className="size-4" />
                <AlertDescription>{state.staleFailure}</AlertDescription>
              </Alert>
            )}
            {entry.legacy_duplicates && (
              <Alert className="mb-3">
                <AlertTriangle className="size-4" />
                <AlertDescription>
                  {t("userQuota.row.duplicateBanner", {
                    count: entry.legacy_duplicates.length,
                  })}
                </AlertDescription>
              </Alert>
            )}
            {readOnly && (
              <div className="text-sm text-muted-foreground p-2">
                {t("userQuota.row.readOnlyHint")}
              </div>
            )}
          </TableCell>
        </TableRow>
      )}

      <Dialog
        open={state.editOpen}
        onOpenChange={(open) => {
          dispatch({ type: "edit-open", open });
        }}
      >
        <DialogContent
          ref={(container) => dispatch({ type: "edit-container", container })}
          className="max-h-[calc(100vh-4rem)] max-w-3xl overflow-y-auto"
        >
          <DialogHeader>
            <DialogTitle>{t("userQuota.editDialogTitle")}</DialogTitle>
            <DialogDescription>
              {t("userQuota.editDialogBody", { client: entry.client_name })}
            </DialogDescription>
          </DialogHeader>
          <UserQuotaForm
            clients={clients}
            disabledClientIds={new Set()}
            lockClient
            defaultValues={{
              client_id: entry.client_id,
              listen_port_start: entry.listen_port_start,
              listen_port_end: entry.listen_port_end,
              protocols: entry.protocols,
              unlimited: entry.unlimited,
              bandwidth_in_bps: entry.cap?.bandwidth_in_bps ?? null,
              bandwidth_out_bps: entry.cap?.bandwidth_out_bps ?? null,
              new_connections_per_sec: entry.cap?.new_connections_per_sec ?? null,
              concurrent_connections: entry.cap?.concurrent_connections ?? null,
              bandwidth_in_burst: entry.cap?.bandwidth_in_burst ?? null,
              bandwidth_out_burst: entry.cap?.bandwidth_out_burst ?? null,
              new_connections_burst: entry.cap?.new_connections_burst ?? null,
            }}
            onSubmit={onSubmit}
            onCancel={() => dispatch({ type: "edit-open", open: false })}
            busy={update.isPending}
            framed={false}
            popoverContainer={state.editDialogContainer}
            serverError={state.serverError}
          />
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={state.confirmDelete}
        onOpenChange={(open) => dispatch({ type: "confirm-delete", open })}
        destructive
        title={t("userQuota.deleteTitle")}
        description={t("userQuota.deleteBody", { user: userId, client: entry.client_name })}
        busy={del.isPending}
        onConfirm={onDelete}
      />
    </>
  );
}
