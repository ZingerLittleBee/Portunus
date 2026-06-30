/// 011-rate-limiting-qos T040: client detail page with two tabs —
/// "Overview" (connection state, provisioned-at, client address) and
/// "Owner quotas" (per-owner cap read-only table with link to user page).

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { ArrowLeft, RefreshCw } from "lucide-react";

import { formatApiError } from "@/api/client";
import {
  useClientOwnersList,
  useClientsList,
  useCreateClientReEnrollment,
} from "@/api/clients";
import { useClientQuotas } from "@/api/quotas";
import { ExhaustedBanner } from "@/components/Traffic/ExhaustedBanner";
import { TrafficPanel } from "@/components/Traffic/TrafficPanel";
import { ME_QUERY_KEY, fetchIdentity } from "@/auth/identity";
import { canProvisionClient, isSuperadmin } from "@/lib/permissions";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { EnrollmentInstallGuide } from "@/components/EnrollmentInstallGuide";
import { DataTable, type Column } from "@/components/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { formatTimestamp } from "@/lib/format";
import type { ClientEnrollmentResponse, OwnerListEntry } from "@/api/types";

export function ClientDetail() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  // 015-client-stable-id (US3): clients are addressed by their stable
  // opaque id; the display name is derived for the still-name-keyed
  // sub-resource APIs (owners / quotas / traffic / re-enroll).
  const { clientId = "" } = useParams<{ clientId: string }>();
  const clients = useClientsList();
  const client = clients.data?.find((c) => c.client_id === clientId);
  const clientName = client?.client_name ?? "";
  const { data: identity } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  const canReEnroll = canProvisionClient(identity);
  // The owner-quotas tab lists owners/caps across tenants for this
  // client; the backend gates GET /v1/clients/{id}/owners superadmin-only,
  // so hide the tab from non-superadmins instead of rendering one that
  // polls a guaranteed 403.
  const showOwnerQuotas = isSuperadmin(identity);
  const reenroll = useCreateClientReEnrollment();

  const [confirmOpen, setConfirmOpen] = useState(false);
  const [reenrollment, setReenrollment] = useState<ClientEnrollmentResponse | null>(null);
  const [reenrollError, setReenrollError] = useState<string | null>(null);

  async function doReenroll() {
    setReenrollError(null);
    try {
      const enrollment = await reenroll.mutateAsync({ clientId });
      setReenrollment(enrollment);
      setConfirmOpen(false);
    } catch (err) {
      setReenrollError(formatApiError(err));
    }
  }

  // US3: an unknown id (e.g. a deleted client or a bad link) is a clear
  // not-found rather than a blank shell. Wait for the list to load first.
  if (clients.isSuccess && !client) {
    return (
      <div className="space-y-4">
        <Button variant="ghost" size="sm" onClick={() => navigate("/clients")}>
          <ArrowLeft className="h-4 w-4 mr-1" />
          {t("clientDetail.back")}
        </Button>
        <EmptyState
          title={t("clientDetail.notFoundTitle")}
          description={t("clientDetail.notFoundBody")}
        />
      </div>
    );
  }

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:flex-wrap sm:items-center">
        <Button variant="ghost" size="sm" onClick={() => navigate("/clients")}>
          <ArrowLeft className="h-4 w-4 mr-1" />
          {t("clientDetail.back")}
        </Button>
        <h1 className="break-all text-2xl font-semibold">{clientName}</h1>
        <code className="text-xs text-muted-foreground">{clientId}</code>
        {client?.connected && (
          <Badge variant={"success" as never}>{t("clients.connected")}</Badge>
        )}
        {client && !client.connected && !client.revoked_at && (
          <Badge variant="secondary">{t("clients.disconnected")}</Badge>
        )}
        {client?.revoked_at && (
          <Badge variant="destructive">{t("clients.revoked")}</Badge>
        )}
        {canReEnroll && client && (
          <Button
            variant="outline"
            size="sm"
            className="sm:ml-auto"
            onClick={() => {
              setReenrollError(null);
              setConfirmOpen(true);
            }}
            disabled={reenroll.isPending}
          >
            <RefreshCw className="mr-1 h-4 w-4" />
            {t("clientDetail.reenroll")}
          </Button>
        )}
      </div>

      <ClientExhaustedBanner clientId={clientId} />

      <Tabs defaultValue="overview">
        <TabsList className="w-full justify-start overflow-x-auto">
          <TabsTrigger value="overview">{t("clientDetail.tabOverview")}</TabsTrigger>
          {showOwnerQuotas && (
            <TabsTrigger value="owners">{t("clientDetail.tabOwnerQuotas")}</TabsTrigger>
          )}
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
          {reenrollment && (
            <EnrollmentInstallGuide enrollment={reenrollment} mode="reenroll" />
          )}
        </TabsContent>
        {showOwnerQuotas && (
          <TabsContent value="owners">
            <OwnerQuotasTab clientId={clientId} />
          </TabsContent>
        )}
        <TabsContent value="traffic">
          <TrafficPanel clientId={clientId} />
        </TabsContent>
      </Tabs>

      <ConfirmDialog
        open={confirmOpen}
        onOpenChange={(open) => {
          if (!open) {
            setConfirmOpen(false);
            setReenrollError(null);
          }
        }}
        title={t("clientDetail.reenrollConfirmTitle", { name: clientName })}
        description={t("clientDetail.reenrollConfirmBody")}
        confirmLabel={t("clientDetail.reenrollConfirmAction")}
        destructive
        busy={reenroll.isPending}
        onConfirm={doReenroll}
      >
        {reenrollError && <p className="text-sm text-destructive">{reenrollError}</p>}
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

function ClientExhaustedBanner({ clientId }: { clientId: string }) {
  const quotas = useClientQuotas(clientId);
  const exhausted = (quotas.data ?? []).filter((q) => q.exhausted);
  return <ExhaustedBanner exhausted={exhausted} />;
}

interface OwnerQuotasTabProps {
  clientId: string;
}

export function OwnerQuotasTab({ clientId }: OwnerQuotasTabProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const owners = useClientOwnersList(clientId);

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
