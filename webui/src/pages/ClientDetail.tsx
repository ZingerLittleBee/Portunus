/// 011-rate-limiting-qos T040: client detail page with two tabs —
/// "Overview" (connection state, provisioned-at, client address) and
/// "Owner quotas" (per-owner cap read-only table with link to user page).

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { ArrowLeft, RefreshCw } from "lucide-react";

import { ApiError } from "@/api/client";
import {
  useClientOwnersList,
  useClientsList,
  useReissueClient,
} from "@/api/clients";
import { useClientQuotas } from "@/api/quotas";
import { ExhaustedBanner } from "@/components/Traffic/ExhaustedBanner";
import { TrafficPanel } from "@/components/Traffic/TrafficPanel";
import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { canProvisionClient } from "@/lib/permissions";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ClientInstallSteps } from "@/components/ClientInstallSteps";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { CredentialBundleCard } from "@/components/CredentialBundleCard";
import type { CredentialBundle } from "@/api/types";
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
  const { data: identity } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  const canReissue = canProvisionClient(identity);
  const reissue = useReissueClient();

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [reissuedBundle, setReissuedBundle] = useState<CredentialBundle | null>(null);
  const [reissueError, setReissueError] = useState<string | null>(null);

  async function doReissue() {
    setReissueError(null);
    try {
      const bundle = await reissue.mutateAsync(clientName);
      setReissuedBundle(bundle);
      setConfirmOpen(false);
    } catch (err) {
      setReissueError(err instanceof ApiError ? `${err.code}: ${err.message}` : (err as Error).message);
    }
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:flex-wrap sm:items-center">
        <Button variant="ghost" size="sm" onClick={() => navigate("/clients")}>
          <ArrowLeft className="h-4 w-4 mr-1" />
          {t("clientDetail.back")}
        </Button>
        <h1 className="break-all font-mono text-2xl font-semibold">{clientName}</h1>
        {client?.connected && (
          <Badge variant={"success" as never}>{t("clients.connected")}</Badge>
        )}
        {client && !client.connected && !client.revoked_at && (
          <Badge variant="secondary">{t("clients.disconnected")}</Badge>
        )}
        {client?.revoked_at && (
          <Badge variant="destructive">{t("clients.revoked")}</Badge>
        )}
        {canReissue && client && (
          <Button
            variant="outline"
            size="sm"
            className="sm:ml-auto"
            onClick={() => {
              setReissueError(null);
              setConfirmOpen(true);
            }}
            disabled={reissue.isPending}
          >
            <RefreshCw className="mr-1 h-4 w-4" />
            {t("clientDetail.reissue")}
          </Button>
        )}
      </div>

      <ClientExhaustedBanner clientName={clientName} />

      <Tabs defaultValue="overview">
        <TabsList className="w-full justify-start overflow-x-auto">
          <TabsTrigger value="overview">{t("clientDetail.tabOverview")}</TabsTrigger>
          <TabsTrigger value="owners">{t("clientDetail.tabOwnerQuotas")}</TabsTrigger>
          <TabsTrigger value="traffic">{t("traffic.tab")}</TabsTrigger>
        </TabsList>
        <TabsContent value="overview" className="space-y-4">
          <Card>
            <CardHeader>
              <CardTitle>{t("clientDetail.overviewTitle")}</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2 text-sm">
              <Row label={t("clientDetail.address")} value={client?.client_address ?? "—"} />
              <Row label={t("clientDetail.observedPeer")} value={client?.remote_addr ?? "—"} />
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
          {reissuedBundle && (
            <>
              <CredentialBundleCard bundle={reissuedBundle} intent="reissue" />
              <ClientInstallSteps bundle={reissuedBundle} />
            </>
          )}
        </TabsContent>
        <TabsContent value="owners">
          <OwnerQuotasTab clientName={clientName} />
        </TabsContent>
        <TabsContent value="traffic">
          <TrafficPanel clientName={clientName} />
        </TabsContent>
      </Tabs>

      <ConfirmDialog
        open={confirmOpen}
        onOpenChange={(open) => {
          if (!open) {
            setConfirmOpen(false);
            setReissueError(null);
          }
        }}
        title={t("clientDetail.reissueConfirmTitle", { name: clientName })}
        description={t("clientDetail.reissueConfirmBody")}
        confirmLabel={t("clientDetail.reissueConfirmAction")}
        destructive
        busy={reissue.isPending}
        onConfirm={doReissue}
      >
        {reissueError && <p className="text-sm text-destructive">{reissueError}</p>}
      </ConfirmDialog>
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="grid gap-1 sm:grid-cols-[8rem_1fr] sm:gap-4">
      <span className="text-muted-foreground">{label}</span>
      <span className="break-all font-mono">{value}</span>
    </div>
  );
}

function ClientExhaustedBanner({ clientName }: { clientName: string }) {
  const quotas = useClientQuotas(clientName);
  const exhausted = (quotas.data ?? []).filter((q) => q.exhausted);
  return <ExhaustedBanner exhausted={exhausted} />;
}

interface OwnerQuotasTabProps {
  clientName: string;
}

export function OwnerQuotasTab({ clientName }: OwnerQuotasTabProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const owners = useClientOwnersList(clientName);

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
        <Button
          size="sm"
          variant="outline"
          onClick={() => navigate(`/users/${encodeURIComponent(o.owner_id)}`)}
        >
          {t("ownerQuotas.openInUser")}
        </Button>
      ),
    },
  ];

  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">{t("ownerQuotas.movedHint")}</p>
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
    </div>
  );
}
