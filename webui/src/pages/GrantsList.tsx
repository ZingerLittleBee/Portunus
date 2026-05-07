import { useTranslation } from "react-i18next";
import { Link, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";
import { Plus } from "lucide-react";

import { useGrantsList, useRevokeGrant } from "@/api/grants";
import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { canManageGrants } from "@/lib/permissions";
import { DataTable, type Column } from "@/components/DataTable";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { EmptyState } from "@/components/EmptyState";
import type { GrantView } from "@/api/types";

export function GrantsList() {
  const { t } = useTranslation();
  const [params] = useSearchParams();
  const userIdFilter = params.get("user_id") ?? undefined;

  const { data: identity } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  const grants = useGrantsList(userIdFilter);
  const revoke = useRevokeGrant();
  const isAdmin = canManageGrants(identity);

  const columns: Column<GrantView>[] = [
    {
      key: "user_id",
      header: t("grants.user"),
      render: (g) => (
        <Link to={`/users/${g.user_id}`} className="font-mono text-primary hover:underline">
          {g.user_id}
        </Link>
      ),
      sortable: true,
      sortValue: (g) => g.user_id,
    },
    { key: "client", header: t("grants.client"), render: (g) => g.client },
    {
      key: "ports",
      header: t("grants.ports"),
      render: (g) =>
        g.listen_port_start === g.listen_port_end
          ? `${g.listen_port_start}`
          : `${g.listen_port_start}–${g.listen_port_end}`,
    },
    {
      key: "protocols",
      header: t("grants.protocols"),
      render: (g) => (
        <span className="flex gap-1">
          {g.protocols.map((p) => (
            <Badge key={p} variant="secondary">
              {p}
            </Badge>
          ))}
        </span>
      ),
    },
    {
      key: "actions",
      header: "",
      width: "120px",
      render: (g) =>
        isAdmin ? (
          <Button variant="ghost" size="sm" onClick={() => revoke.mutate(g.grant_id)}>
            {t("grants.revoke")}
          </Button>
        ) : null,
    },
  ];

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold">{t("grants.title")}</h1>
        {isAdmin && (
          <Button asChild>
            <Link to="/grants/new">
              <Plus className="mr-1 h-4 w-4" />
              {t("grants.newGrant")}
            </Link>
          </Button>
        )}
      </div>
      <DataTable
        rows={grants.data ?? []}
        columns={columns}
        rowKey={(g) => g.grant_id}
        emptyState={
          <EmptyState title={t("grants.emptyTitle")} description={t("grants.emptyBody")} />
        }
        ariaLabel={t("grants.title")}
      />
    </div>
  );
}
