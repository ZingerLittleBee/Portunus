import { useTranslation } from "react-i18next";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useQuery } from "@tanstack/react-query";

import { useRulesList, useRemoveRule } from "@/api/rules";
import { useUsersList } from "@/api/users";
import { ME_QUERY_KEY, fetchIdentity } from "@/auth/AuthGate";
import { isSuperadmin } from "@/lib/permissions";
import { DataTable, type Column } from "@/components/DataTable";
import { Button } from "@/components/ui/button";
import { RulePushDialog } from "@/components/RulePushDialog";
import { Badge } from "@/components/ui/badge";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { EmptyState } from "@/components/EmptyState";
import { parseRuleState, type Rule } from "@/api/types";
import { summarizeRateLimit } from "@/components/RateLimitForm";

const OWNER_FILTER_ALL = "__all";

export function RulesList() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [params, setParams] = useSearchParams();
  const clientFilter = params.get("client") ?? undefined;
  const ownerFilter = params.get("owner") ?? undefined;

  const { data: identity } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  const isAdmin = isSuperadmin(identity);
  const rules = useRulesList({ ...(clientFilter ? { client: clientFilter } : {}), ...(ownerFilter ? { owner: ownerFilter } : {}) });
  const users = useUsersList();
  const remove = useRemoveRule();

  const columns: Column<Rule>[] = [
    {
      key: "id",
      header: t("rules.id"),
      width: "100px",
      render: (r) => (
        <Link to={`/rules/${r.id}`} className="font-mono text-primary hover:underline">
          #{r.id}
        </Link>
      ),
      sortable: true,
      sortValue: (r) => r.id,
    },
    {
      key: "owner",
      header: t("rules.owner"),
      render: (r) => (
        <Badge variant={r.owner_user_id === "_superadmin" ? "default" : "secondary"}>
          {r.owner_user_id}
        </Badge>
      ),
    },
    { key: "client", header: t("rules.client"), render: (r) => r.client_name },
    {
      key: "listen",
      header: t("rules.listenPort"),
      render: (r) =>
        r.listen_port_end && r.listen_port_end !== r.listen_port
          ? `${r.listen_port}–${r.listen_port_end}`
          : `${r.listen_port}`,
    },
    {
      key: "target",
      header: t("rules.target"),
      render: (r) => {
        // 007-multi-target-failover T049: render an "MT" pill plus the
        // primary target when the rule carries ≥ 2 targets. Single-target
        // rules render as before — preserves v0.6.0 row layout.
        const targetCount = r.targets?.length ?? 1;
        const base =
          r.target_port_end && r.target_port_end !== r.target_port
            ? `${r.target_host}:${r.target_port}–${r.target_port_end}`
            : `${r.target_host}:${r.target_port}`;
        const proxyTargets = (r.targets ?? []).filter((target) => target.proxy_protocol);
        const proxyBadge =
          proxyTargets.length > 0 ? (
            <Badge
              variant="secondary"
              title={proxyTargets
                .map((target) => `${target.host}:${target.port}=${target.proxy_protocol}`)
                .join(", ")}
            >
              PROXY {proxyTargets.length}
            </Badge>
          ) : null;
        if (targetCount > 1) {
          return (
            <span className="flex items-center gap-2">
              <span className="font-mono">{base}</span>
              <Badge
                variant="outline"
                title={t("ruleDetail.multiTargetTooltip")}
              >
                {t("rules.multiTargetPill", { count: targetCount })}
              </Badge>
              {proxyBadge}
            </span>
          );
        }
        return (
          <span className="flex items-center gap-2">
            <span>{base}</span>
            {proxyBadge}
          </span>
        );
      },
    },
    {
      key: "protocol",
      header: t("rules.protocol"),
      width: "100px",
      render: (r) => <Badge variant="outline">{r.protocol}</Badge>,
    },
    {
      // 009-tls-sni-routing T083: SNI selector column. `—` (em-dash)
      // for legacy / fallback rules; rendered in a monospace pill so
      // wildcards line up visually with exact hosts.
      key: "sni",
      header: t("rules.sni"),
      width: "200px",
      render: (r) =>
        r.sni_pattern ? (
          <span className="font-mono text-xs">{r.sni_pattern}</span>
        ) : (
          <span className="text-muted-foreground">—</span>
        ),
    },
    {
      // 011-rate-limiting-qos T039: compact `Caps` column. `—` for
      // rules without rate_limit (preserves v0.10 row spacing); a
      // monospace summary like `↓1.0M · ≤100` for capped rules.
      key: "caps",
      header: t("rulesCapsCol.header"),
      width: "180px",
      render: (r) => {
        const summary = summarizeRateLimit(r.rate_limit);
        return summary ? (
          <span className="font-mono text-xs" title={summary}>
            {summary}
          </span>
        ) : (
          <span className="text-muted-foreground">{t("rulesCapsCol.uncapped")}</span>
        );
      },
    },
    {
      key: "state",
      header: t("rules.state"),
      width: "120px",
      render: (r) => {
        const state = parseRuleState(r.state);
        const variant =
          state.kind === "Active"
            ? "success"
            : state.kind === "Failed"
              ? "destructive"
              : "secondary";
        return <Badge variant={variant as never}>{state.kind}</Badge>;
      },
    },
    {
      key: "actions",
      header: "",
      width: "80px",
      render: (r) => (
        <Button
          variant="ghost"
          size="sm"
          onClick={(e) => {
            e.stopPropagation();
            remove.mutate(r.id);
          }}
        >
          {t("rules.remove")}
        </Button>
      ),
    },
  ];

  return (
    <div className="space-y-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <h1 className="text-2xl font-semibold">{t("rules.title")}</h1>
        <RulePushDialog />
      </div>
      {isAdmin && (
        <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
          <label htmlFor="owner-filter" className="text-sm text-muted-foreground">
            {t("rules.ownerFilter")}
          </label>
          <Select
            value={ownerFilter ?? OWNER_FILTER_ALL}
            onValueChange={(value) => {
              const next = new URLSearchParams(params);
              if (value === OWNER_FILTER_ALL) {
                next.delete("owner");
              } else {
                next.set("owner", value);
              }
              setParams(next);
            }}
          >
            <SelectTrigger id="owner-filter" className="sm:w-48">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                <SelectItem value={OWNER_FILTER_ALL}>
                  {t("rules.ownerFilterAll")}
                </SelectItem>
                {(users.data ?? []).map((u) => (
                  <SelectItem key={u.user_id} value={u.user_id}>
                    {u.user_id}
                  </SelectItem>
                ))}
              </SelectGroup>
            </SelectContent>
          </Select>
        </div>
      )}
      <DataTable
        rows={rules.data ?? []}
        columns={columns}
        rowKey={(r) => String(r.id)}
        onRowClick={(r) => navigate(`/rules/${r.id}`)}
        emptyState={<EmptyState title={t("rules.emptyTitle")} description={t("rules.emptyBody")} />}
        ariaLabel={t("rules.title")}
      />
    </div>
  );
}
