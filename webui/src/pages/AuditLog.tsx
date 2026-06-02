import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Download } from "lucide-react";

import { fetchAuditEnvelope, useAuditLog } from "@/api/audit";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { DataTable, type Column } from "@/components/DataTable";
import { EmptyState } from "@/components/EmptyState";
import { downloadBlob, toNdjsonBlob } from "@/lib/ndjson";
import { formatTimestamp } from "@/lib/format";
import type { AuditEntry } from "@/api/types";

type OutcomeFilter = "all" | "allow" | "deny";
type Mode = "live" | "history";

const PAGE_SIZE = 100;

// Page 1 of history carries no cursor. The server only switches to the
// paginated *envelope* response (the one that carries `next_cursor`)
// when a since/until/cursor param is present, so we send a far-future
// `until` — it matches every row yet forces the envelope shape.
const HISTORY_UNTIL_SENTINEL = "2999-01-01T00:00:00Z";

export function AuditLog() {
  const { t } = useTranslation();
  const [mode, setMode] = useState<Mode>("live");
  const [outcomeFilter, setOutcomeFilter] = useState<OutcomeFilter>("all");

  // Live tail: polls every 5s while in live mode, fully disabled in
  // history mode so the view is a frozen, scroll-stable snapshot.
  const live = useAuditLog({ limit: 100 }, { enabled: mode === "live" });

  // History pagination via the opaque server cursor. `prevStack` holds
  // the cursors of the pages above the current one so "Newer" can walk
  // back; `historyCursor` is the cursor for the page on screen (an
  // `undefined` cursor is page 1, the newest). Pages are server-filtered
  // by outcome, so changing the filter rebuilds from page 1.
  const [prevStack, setPrevStack] = useState<(string | undefined)[]>([]);
  const [historyCursor, setHistoryCursor] = useState<string | undefined>(undefined);
  const [pageNumber, setPageNumber] = useState(1);
  const [pageEntries, setPageEntries] = useState<AuditEntry[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  function resetHistory() {
    setPrevStack([]);
    setHistoryCursor(undefined);
    setPageNumber(1);
  }

  function handleModeChange(next: Mode) {
    setMode(next);
    if (next === "history") resetHistory();
  }

  function handleOutcomeChange(value: OutcomeFilter) {
    setOutcomeFilter(value);
    if (mode === "history") resetHistory();
  }

  function goOlder() {
    if (!nextCursor || loading) return;
    setPrevStack((s) => [...s, historyCursor]);
    setHistoryCursor(nextCursor);
    setPageNumber((n) => n + 1);
  }

  function goNewer() {
    if (prevStack.length === 0 || loading) return;
    const target = prevStack[prevStack.length - 1];
    setPrevStack((s) => s.slice(0, -1));
    setHistoryCursor(target);
    setPageNumber((n) => Math.max(1, n - 1));
  }

  // Fetch the active history page. Keyed only on the cursor + outcome,
  // each of which changes exactly once per navigation, so there is no
  // refetch storm.
  useEffect(() => {
    if (mode !== "history") return;
    let cancelled = false;
    setLoading(true);
    setLoadError(null);
    // Build the query without explicit `undefined` keys —
    // `exactOptionalPropertyTypes` forbids them.
    const params: Parameters<typeof fetchAuditEnvelope>[0] = { limit: PAGE_SIZE };
    if (historyCursor) {
      params.cursor = historyCursor;
    } else {
      params.until = HISTORY_UNTIL_SENTINEL;
    }
    if (outcomeFilter !== "all") params.outcome = outcomeFilter;
    fetchAuditEnvelope(params)
      .then((env) => {
        if (cancelled) return;
        setPageEntries(env.entries);
        setNextCursor(env.next_cursor ?? null);
      })
      .catch((err) => {
        if (!cancelled) setLoadError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [mode, historyCursor, outcomeFilter]);

  // Live rows are filtered client-side (per spec FR-010); history rows
  // arrive already filtered by the server.
  const rows = useMemo<AuditEntry[]>(() => {
    if (mode === "live") {
      const data = live.data ?? [];
      return outcomeFilter === "all" ? data : data.filter((r) => r.outcome === outcomeFilter);
    }
    return pageEntries;
  }, [mode, live.data, outcomeFilter, pageEntries]);

  function handleDownload() {
    const blob = toNdjsonBlob(rows);
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
        <Button onClick={handleDownload} disabled={rows.length === 0} className="w-full sm:w-auto">
          <Download className="mr-1 h-4 w-4" />
          {t("audit.download")}
        </Button>
      </div>
      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <Tabs value={mode} onValueChange={(v) => handleModeChange(v as Mode)}>
          <TabsList>
            <TabsTrigger value="live">{t("audit.modeLive")}</TabsTrigger>
            <TabsTrigger value="history">{t("audit.modeHistory")}</TabsTrigger>
          </TabsList>
        </Tabs>
        <span className="text-sm text-muted-foreground">
          {mode === "live" ? t("audit.liveHint") : t("audit.historyHint")}
        </span>
      </div>
      <DataTable
        rows={rows}
        columns={columns}
        rowKey={(e) => `${e.timestamp}-${e.actor}-${e.path}-${e.outcome}`}
        toolbar={
          <Select value={outcomeFilter} onValueChange={(value) => handleOutcomeChange(value as OutcomeFilter)}>
            <SelectTrigger className="w-[10rem]" aria-label={t("audit.outcomeFilterLabel")}>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectGroup>
                <SelectItem value="all">{t("audit.outcomeAll")}</SelectItem>
                <SelectItem value="allow">{t("audit.allow")}</SelectItem>
                <SelectItem value="deny">{t("audit.deny")}</SelectItem>
              </SelectGroup>
            </SelectContent>
          </Select>
        }
        emptyState={<EmptyState title={t("audit.emptyTitle")} description={t("audit.emptyBody")} />}
        ariaLabel={t("audit.title")}
      />
      {mode === "history" && (
        <div className="flex items-center gap-3 pt-2">
          <Button variant="outline" onClick={goNewer} disabled={prevStack.length === 0 || loading}>
            {t("audit.newer")}
          </Button>
          <span className="text-sm text-muted-foreground" aria-live="polite">
            {loading ? t("audit.loading") : t("audit.page", { n: pageNumber })}
          </span>
          <Button variant="outline" onClick={goOlder} disabled={!nextCursor || loading}>
            {t("audit.older")}
          </Button>
          {!nextCursor && !loading && pageEntries.length > 0 && (
            <span className="text-sm text-muted-foreground">{t("audit.noMoreHistory")}</span>
          )}
          {loadError && (
            <span className="text-sm text-destructive" role="alert">
              {loadError}
            </span>
          )}
        </div>
      )}
    </div>
  );
}
