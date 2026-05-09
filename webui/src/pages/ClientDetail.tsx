/// 011-rate-limiting-qos T040: client detail page with two tabs —
/// "Overview" (connection state, provisioned-at, remote address) and
/// "Owner quotas" (per-owner cap CRUD, capability-gate aware).
///
/// The owner-list response carries `rule_count` and `has_rate_limit`
/// so operators can spot owners who push rules to this client even
/// before any cap is set.

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "react-router-dom";
import { ArrowLeft, Trash2 } from "lucide-react";

import { ApiError } from "@/api/client";
import {
  useClientOwnersList,
  useClientsList,
  useDeleteOwnerRateLimit,
  useOwnerRateLimit,
  usePutOwnerRateLimit,
} from "@/api/clients";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  EMPTY_RATE_LIMIT_FORM,
  RateLimitForm,
  formStateToRateLimit,
  rateLimitToFormState,
  summarizeRateLimit,
} from "@/components/RateLimitForm";
import { DataTable, type Column } from "@/components/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { formatTimestamp } from "@/lib/format";
import type { OwnerListEntry } from "@/api/types";

export function ClientDetail() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { clientName = "" } = useParams<{ clientName: string }>();
  const clients = useClientsList();
  const client = clients.data?.find((c) => c.client_name === clientName);

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-2">
        <Button variant="ghost" size="sm" onClick={() => navigate("/clients")}>
          <ArrowLeft className="h-4 w-4 mr-1" />
          {t("clientDetail.back")}
        </Button>
        <h1 className="text-2xl font-semibold font-mono">{clientName}</h1>
        {client?.connected && (
          <Badge variant={"success" as never}>{t("clients.connected")}</Badge>
        )}
        {client && !client.connected && !client.revoked_at && (
          <Badge variant="secondary">{t("clients.disconnected")}</Badge>
        )}
        {client?.revoked_at && (
          <Badge variant="destructive">{t("clients.revoked")}</Badge>
        )}
      </div>

      <Tabs defaultValue="overview">
        <TabsList>
          <TabsTrigger value="overview">{t("clientDetail.tabOverview")}</TabsTrigger>
          <TabsTrigger value="owners">{t("clientDetail.tabOwnerQuotas")}</TabsTrigger>
        </TabsList>
        <TabsContent value="overview">
          <Card>
            <CardHeader>
              <CardTitle>{t("clientDetail.overviewTitle")}</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2 text-sm">
              <Row label={t("clientDetail.remote")} value={client?.remote_addr ?? "—"} />
              <Row
                label={t("clientDetail.connectedAt")}
                value={
                  client?.connected_at ? formatTimestamp(client.connected_at) : "—"
                }
              />
              <Row
                label={t("clientDetail.provisionedAt")}
                value={
                  client ? formatTimestamp(client.provisioned_at) : "—"
                }
              />
              {client?.revoked_at && (
                <Row
                  label={t("clientDetail.revokedAt")}
                  value={formatTimestamp(client.revoked_at)}
                />
              )}
            </CardContent>
          </Card>
        </TabsContent>
        <TabsContent value="owners">
          <OwnerQuotasTab clientName={clientName} />
        </TabsContent>
      </Tabs>
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex gap-4">
      <span className="text-muted-foreground w-32">{label}</span>
      <span className="font-mono">{value}</span>
    </div>
  );
}

interface OwnerQuotasTabProps {
  clientName: string;
}

function OwnerQuotasTab({ clientName }: OwnerQuotasTabProps) {
  const { t } = useTranslation();
  const owners = useClientOwnersList(clientName);
  const [editingOwner, setEditingOwner] = useState<string | null>(null);

  const columns: Column<OwnerListEntry>[] = [
    {
      key: "owner",
      header: t("ownerQuotas.owner"),
      render: (o) => (
        <Badge variant={o.owner_id === "_superadmin" ? "default" : "secondary"}>
          {o.owner_id}
        </Badge>
      ),
      sortable: true,
      sortValue: (o) => o.owner_id,
    },
    {
      key: "rules",
      header: t("ownerQuotas.ruleCount"),
      render: (o) => o.rule_count,
      sortable: true,
      sortValue: (o) => o.rule_count,
    },
    {
      key: "capStatus",
      header: t("ownerQuotas.capStatus"),
      render: (o) =>
        o.has_rate_limit ? (
          <Badge variant={"success" as never}>{t("ownerQuotas.capped")}</Badge>
        ) : (
          <span className="text-muted-foreground">{t("ownerQuotas.uncapped")}</span>
        ),
    },
    {
      key: "actions",
      header: "",
      width: "120px",
      render: (o) => (
        <Button size="sm" variant="outline" onClick={() => setEditingOwner(o.owner_id)}>
          {o.has_rate_limit ? t("ownerQuotas.edit") : t("ownerQuotas.setCap")}
        </Button>
      ),
    },
  ];

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">{t("ownerQuotas.help")}</p>
      <DataTable
        rows={owners.data ?? []}
        columns={columns}
        rowKey={(o) => o.owner_id}
        emptyState={
          <EmptyState
            title={t("ownerQuotas.emptyTitle")}
            description={t("ownerQuotas.emptyBody")}
          />
        }
        ariaLabel={t("ownerQuotas.tableAriaLabel")}
      />
      {editingOwner && (
        <OwnerQuotaEditor
          clientName={clientName}
          ownerId={editingOwner}
          onClose={() => setEditingOwner(null)}
        />
      )}
    </div>
  );
}

interface EditorProps {
  clientName: string;
  ownerId: string;
  onClose: () => void;
}

function OwnerQuotaEditor({ clientName, ownerId, onClose }: EditorProps) {
  const { t } = useTranslation();
  const view = useOwnerRateLimit(clientName, ownerId);
  const put = usePutOwnerRateLimit(clientName);
  const del = useDeleteOwnerRateLimit(clientName);
  const [form, setForm] = useState(EMPTY_RATE_LIMIT_FORM);
  const [error, setError] = useState<string | null>(null);
  const [hydrated, setHydrated] = useState(false);

  // Hydrate the form once the GET resolves (200 OK or 404→null).
  // Re-runs only when the cached envelope changes — manual edits to
  // `form` are not clobbered.
  if (!hydrated && !view.isLoading) {
    setForm(rateLimitToFormState(view.data?.rate_limit));
    setHydrated(true);
  }

  async function onSubmit(e: React.FormEvent<HTMLFormElement>) {
    e.preventDefault();
    setError(null);
    const body = formStateToRateLimit(form);
    if (!body) {
      setError(t("ownerQuotas.errorEmpty"));
      return;
    }
    try {
      await put.mutateAsync({ ownerId, body });
      onClose();
    } catch (err) {
      setError(formatApiError(err));
    }
  }

  async function onDelete() {
    setError(null);
    try {
      await del.mutateAsync(ownerId);
      onClose();
    } catch (err) {
      setError(formatApiError(err));
    }
  }

  const summary = summarizeRateLimit(view.data?.rate_limit);

  return (
    <Card>
      <CardHeader>
        <CardTitle>
          {t("ownerQuotas.editorTitle")} — <span className="font-mono">{ownerId}</span>
        </CardTitle>
      </CardHeader>
      <CardContent>
        {summary && (
          <p className="text-xs text-muted-foreground mb-3">
            {t("ownerQuotas.currentCap")} <span className="font-mono">{summary}</span>
          </p>
        )}
        <form onSubmit={onSubmit} className="space-y-4">
          <RateLimitForm state={form} onChange={setForm} />
          {error && <p className="text-sm text-destructive">{error}</p>}
          <div className="flex gap-2">
            <Button type="submit" disabled={put.isPending}>
              {put.isPending ? t("confirm.busy") : t("ownerQuotas.save")}
            </Button>
            <Button type="button" variant="outline" onClick={onClose}>
              {t("confirm.cancel")}
            </Button>
            {view.data && (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                onClick={onDelete}
                disabled={del.isPending}
                className="text-destructive ml-auto"
              >
                <Trash2 className="h-4 w-4 mr-1" />
                {t("ownerQuotas.delete")}
              </Button>
            )}
          </div>
        </form>
      </CardContent>
    </Card>
  );
}

function formatApiError(err: unknown): string {
  if (err instanceof ApiError) return `${err.code}: ${err.message}`;
  return (err as Error).message;
}
