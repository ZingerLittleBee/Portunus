import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Download } from "lucide-react";

import { fetchAuditEnvelope, useAuditLog } from "@/api/audit";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { DataTable, type Column } from "@/components/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { downloadBlob, toNdjsonBlob } from "@/lib/ndjson";
import { formatTimestamp } from "@/lib/format";
import type { AuditEntry } from "@/api/types";

type OutcomeFilter = "all" | "allow" | "deny";

const HISTORY_PAGE_SIZE = 100;

export function AuditLog() {
  const { t } = useTranslation();
  const [outcomeFilter, setOutcomeFilter] = useState<OutcomeFilter>("all");
  const [history, setHistory] = useState<AuditEntry[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [historyExhausted, setHistoryExhausted] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  // Always pull the full window from the server; outcome filter is
  // client-side per spec FR-010 + R-007 (no extra request on filter).
  const audit = useAuditLog({ limit: 100 });

  // Combine the live tail with any older rows the operator has paged in
  // via `Load earlier`. Dedupe on the synthetic row key the table uses
  // (timestamp + actor + path + outcome) so a row that drops off the
  // live tail and reappears in history isn't shown twice.
  const combined = useMemo<AuditEntry[]>(() => {
    const live = audit.data ?? [];
    if (history.length === 0) return live;
    const seen = new Set<string>();
    const out: AuditEntry[] = [];
    for (const r of [...live, ...history]) {
      const k = `${r.timestamp}-${r.actor}-${r.path}-${r.outcome}`;
      if (seen.has(k)) continue;
      seen.add(k);
      out.push(r);
    }
    return out;
  }, [audit.data, history]);

  const filtered = useMemo(() => {
    if (outcomeFilter === "all") return combined;
    return combined.filter((r) => r.outcome === outcomeFilter);
  }, [combined, outcomeFilter]);

  async function handleLoadEarlier() {
    if (loadingMore || historyExhausted) return;
    setLoadingMore(true);
    setLoadError(null);
    try {
      // First click anchors at the oldest row currently visible; later
      // clicks paginate strictly via the opaque cursor.
      const params: Parameters<typeof fetchAuditEnvelope>[0] = {
        limit: HISTORY_PAGE_SIZE,
      };
      if (nextCursor) {
        params.cursor = nextCursor;
      } else {
        const oldest = combined[combined.length - 1];
        if (!oldest) {
          setHistoryExhausted(true);
          return;
        }
        params.until = oldest.timestamp;
      }
      const envelope = await fetchAuditEnvelope(params);
      if (envelope.entries.length === 0) {
        setHistoryExhausted(true);
        return;
      }
      setHistory((prev) => [...prev, ...envelope.entries]);
      if (envelope.next_cursor) {
        setNextCursor(envelope.next_cursor);
      } else {
        setHistoryExhausted(true);
        setNextCursor(null);
      }
    } catch (err) {
      setLoadError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoadingMore(false);
    }
  }

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
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <h1 className="text-2xl font-semibold">{t("audit.title")}</h1>
        <Button onClick={handleDownload} disabled={filtered.length === 0} className="w-full sm:w-auto">
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
      <div className="flex items-center gap-3 pt-2">
        <Button
          variant="outline"
          onClick={handleLoadEarlier}
          disabled={loadingMore || historyExhausted || combined.length === 0}
        >
          {loadingMore
            ? t("audit.loadingEarlier", { defaultValue: "Loading…" })
            : historyExhausted
              ? t("audit.noMoreHistory", { defaultValue: "No earlier entries" })
              : t("audit.loadEarlier", { defaultValue: "Load earlier" })}
        </Button>
        {loadError && (
          <span className="text-sm text-destructive" role="alert">
            {loadError}
          </span>
        )}
      </div>
    </div>
  );
}
