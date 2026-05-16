// webui/src/components/UserQuota/UserQuotaTable.tsx
import { Plus } from "lucide-react";
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";

import { useCreateAccessEntry, type AccessEntry } from "@/api/access-entries";
import { ApiError } from "@/api/client";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { UserQuotaForm, type UserQuotaFormSubmitValue } from "./UserQuotaForm";
import { UserQuotaRow } from "./UserQuotaRow";
import type { ClientLite } from "./ClientCombobox";

interface Props {
  userId: string;
  entries: AccessEntry[];
  clients: ClientLite[];
  readOnly: boolean;
}

export function UserQuotaTable({ userId, entries, clients, readOnly }: Props) {
  const { t } = useTranslation();
  const [adding, setAdding] = useState(false);
  const [serverError, setServerError] = useState<string | null>(null);
  const create = useCreateAccessEntry(userId);

  const disabledClientNames = new Set(entries.map((e) => e.client_name));

  async function onAdd(v: UserQuotaFormSubmitValue) {
    setServerError(null);
    try {
      await create.mutateAsync({
        user_id: userId,
        client_name: v.client_name,
        listen_port_start: v.listen_port_start,
        listen_port_end: v.listen_port_end,
        protocols: v.protocols,
        ...(v.cap !== undefined ? { cap: v.cap } : {}),
      });
      toast.success(t("userQuota.toast.created", { client: v.client_name }));
      setAdding(false);
    } catch (err) {
      const msg =
        err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message;
      setServerError(msg);
      toast.error(t("userQuota.toast.createFailed"));
    }
  }

  return (
    <div className="space-y-3">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <p className="text-sm text-muted-foreground">{t("userQuota.tableHelp")}</p>
        {!readOnly && (
          <Button size="sm" onClick={() => setAdding(true)} disabled={adding} className="w-full sm:w-auto">
            <Plus className="h-4 w-4 mr-1" />
            {t("userQuota.add")}
          </Button>
        )}
      </div>

      <div className="overflow-x-auto">
        <Table className="min-w-[980px]">
          <TableHeader>
            <TableRow>
              <TableHead aria-label="expand" />
              <TableHead>{t("userQuota.col.client")}</TableHead>
              <TableHead>{t("userQuota.col.portRange")}</TableHead>
              <TableHead>{t("userQuota.col.protocols")}</TableHead>
              <TableHead>{t("userQuota.col.bwIn")}</TableHead>
              <TableHead>{t("userQuota.col.bwOut")}</TableHead>
              <TableHead>{t("userQuota.col.concurrent")}</TableHead>
              <TableHead>{t("userQuota.col.newConnPerSec")}</TableHead>
              <TableHead>{t("userQuota.monthlyQuota")}</TableHead>
              <TableHead>{t("userQuota.thisPeriod")}</TableHead>
              <TableHead>{t("userQuota.col.status")}</TableHead>
              <TableHead aria-label="actions" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {entries.map((e) => (
              <UserQuotaRow
                key={`${e.user_id}::${e.client_name}`}
                userId={userId}
                entry={e}
                clients={clients}
                clientOnline={clients.find((c) => c.client_name === e.client_name)?.connected ?? false}
                readOnly={readOnly}
              />
            ))}
            {entries.length === 0 && !adding && (
              <TableRow>
                <TableCell colSpan={12} className="text-center text-muted-foreground py-6">
                  {t("userQuota.empty")}
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
      </div>

      {adding && (
        <UserQuotaForm
          clients={clients}
          disabledClientNames={disabledClientNames}
          onSubmit={onAdd}
          onCancel={() => {
            setAdding(false);
            setServerError(null);
          }}
          busy={create.isPending}
          serverError={serverError}
        />
      )}
    </div>
  );
}
