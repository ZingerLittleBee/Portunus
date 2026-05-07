import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Download } from "lucide-react";

import { useAuditLog } from "@/api/audit";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { DataTable, type Column } from "@/components/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { downloadBlob, toNdjsonBlob } from "@/lib/ndjson";
import { formatTimestamp } from "@/lib/format";
import type { AuditEntry } from "@/api/types";

type OutcomeFilter = "all" | "allow" | "deny";

export function AuditLog() {
  const { t } = useTranslation();
  const [outcomeFilter, setOutcomeFilter] = useState<OutcomeFilter>("all");

  // Always pull the full window from the server; outcome filter is
  // client-side per spec FR-010 + R-007 (no extra request on filter).
  const audit = useAuditLog({ limit: 100 });

  const filtered = useMemo(() => {
    const rows = audit.data ?? [];
    if (outcomeFilter === "all") return rows;
    return rows.filter((r) => r.outcome === outcomeFilter);
  }, [audit.data, outcomeFilter]);

  function handleDownload() {
    const blob = toNdjsonBlob(filtered);
    const ts = new Date().toISOString().replace(/[:.]/g, "-");
    downloadBlob(blob, `audit-${ts}.ndjson`);
  }

  const columns: Column<AuditEntry>[] = [
    {
      key: "timestamp",
      header: t("audit.timestamp"),
      width: "200px",
      render: (e) => <span className="font-mono text-xs">{formatTimestamp(e.timestamp)}</span>,
      sortable: true,
      sortValue: (e) => e.timestamp,
    },
    {
      key: "outcome",
      header: t("audit.outcome"),
      width: "100px",
      render: (e) =>
        e.outcome === "allow" ? (
          <Badge variant={"success" as never}>{t("audit.allow")}</Badge>
        ) : (
          <Badge variant="destructive">{t("audit.deny")}</Badge>
        ),
    },
    { key: "actor", header: t("audit.actor"), width: "140px", render: (e) => e.actor },
    {
      key: "role",
      header: t("audit.role"),
      width: "120px",
      render: (e) => (e.role ? <Badge variant="outline">{e.role}</Badge> : "—"),
    },
    { key: "method", header: t("audit.method"), width: "80px", render: (e) => e.method },
    { key: "path", header: t("audit.path"), render: (e) => <span className="font-mono">{e.path}</span> },
    {
      key: "reason",
      header: t("audit.reason"),
      render: (e) => e.reason ?? "—",
    },
  ];

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-2xl font-semibold">{t("audit.title")}</h1>
        <Button onClick={handleDownload} disabled={filtered.length === 0}>
          <Download className="mr-1 h-4 w-4" />
          {t("audit.download")}
        </Button>
      </div>
      <DataTable
        rows={filtered}
        columns={columns}
        rowKey={(e) => `${e.timestamp}-${e.actor}-${e.path}-${e.outcome}`}
        toolbar={
          <select
            value={outcomeFilter}
            onChange={(e) => setOutcomeFilter(e.target.value as OutcomeFilter)}
            className="h-9 rounded-md border border-input bg-background px-2 text-sm"
            aria-label={t("audit.outcomeFilterLabel")}
          >
            <option value="all">{t("audit.outcomeAll")}</option>
            <option value="allow">{t("audit.allow")}</option>
            <option value="deny">{t("audit.deny")}</option>
          </select>
        }
        emptyState={<EmptyState title={t("audit.emptyTitle")} description={t("audit.emptyBody")} />}
        ariaLabel={t("audit.title")}
      />
    </div>
  );
}
