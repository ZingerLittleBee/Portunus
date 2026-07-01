import { useEffect, useMemo, useReducer } from "react";
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

interface AuditLogState {
  mode: Mode;
  outcomeFilter: OutcomeFilter;
  prevStack: (string | undefined)[];
  historyCursor: string | undefined;
  pageNumber: number;
  pageEntries: AuditEntry[];
  nextCursor: string | null;
  loading: boolean;
  loadError: string | null;
}

type AuditLogAction =
  | { type: "mode"; mode: Mode }
  | { type: "outcome"; outcomeFilter: OutcomeFilter }
  | { type: "older" }
  | { type: "newer" }
  | { type: "history-start" }
  | { type: "history-success"; entries: AuditEntry[]; nextCursor: string | null }
  | { type: "history-error"; message: string };

const initialAuditLogState: AuditLogState = {
  mode: "live",
  outcomeFilter: "all",
  prevStack: [],
  historyCursor: undefined,
  pageNumber: 1,
  pageEntries: [],
  nextCursor: null,
  loading: false,
  loadError: null,
};

function resetHistory(state: AuditLogState): AuditLogState {
  return {
    ...state,
    prevStack: [],
    historyCursor: undefined,
    pageNumber: 1,
  };
}

function auditLogReducer(state: AuditLogState, action: AuditLogAction): AuditLogState {
  switch (action.type) {
    case "mode": {
      const next = { ...state, mode: action.mode };
      return action.mode === "history" ? resetHistory(next) : next;
    }
    case "outcome": {
      const next = { ...state, outcomeFilter: action.outcomeFilter };
      return state.mode === "history" ? resetHistory(next) : next;
    }
    case "older":
      if (!state.nextCursor || state.loading) return state;
      return {
        ...state,
        prevStack: [...state.prevStack, state.historyCursor],
        historyCursor: state.nextCursor,
        pageNumber: state.pageNumber + 1,
      };
    case "newer": {
      if (state.prevStack.length === 0 || state.loading) return state;
      return {
        ...state,
        prevStack: state.prevStack.slice(0, -1),
        historyCursor: state.prevStack[state.prevStack.length - 1],
        pageNumber: Math.max(1, state.pageNumber - 1),
      };
    }
    case "history-start":
      return { ...state, loading: true, loadError: null };
    case "history-success":
      return {
        ...state,
        pageEntries: action.entries,
        nextCursor: action.nextCursor,
        loading: false,
      };
    case "history-error":
      return { ...state, loadError: action.message, loading: false };
  }
}

function isMode(value: string): value is Mode {
  return value === "live" || value === "history";
}

function isOutcomeFilter(value: string): value is OutcomeFilter {
  return value === "all" || value === "allow" || value === "deny";
}

export function AuditLog() {
  const { t } = useTranslation();
  const [state, dispatch] = useReducer(auditLogReducer, initialAuditLogState);

  // Live tail: polls every 5s while in live mode, fully disabled in
  // history mode so the view is a frozen, scroll-stable snapshot.
  const live = useAuditLog({ limit: 100 }, { enabled: state.mode === "live" });

  function handleModeChange(next: Mode) {
    dispatch({ type: "mode", mode: next });
  }

  function handleOutcomeChange(outcomeFilter: OutcomeFilter) {
    dispatch({ type: "outcome", outcomeFilter });
  }

  function goOlder() {
    dispatch({ type: "older" });
  }

  function goNewer() {
    dispatch({ type: "newer" });
  }

  // Fetch the active history page. Keyed only on the cursor + outcome,
  // each of which changes exactly once per navigation, so there is no
  // refetch storm.
  useEffect(() => {
    if (state.mode !== "history") return;
    let cancelled = false;
    dispatch({ type: "history-start" });
    // Build the query without explicit `undefined` keys —
    // `exactOptionalPropertyTypes` forbids them.
    const params: Parameters<typeof fetchAuditEnvelope>[0] = { limit: PAGE_SIZE };
    if (state.historyCursor) {
      params.cursor = state.historyCursor;
    } else {
      params.until = HISTORY_UNTIL_SENTINEL;
    }
    if (state.outcomeFilter !== "all") params.outcome = state.outcomeFilter;
    fetchAuditEnvelope(params)
      .then((env) => {
        if (cancelled) return;
        dispatch({
          type: "history-success",
          entries: env.entries,
          nextCursor: env.next_cursor ?? null,
        });
      })
      .catch((err) => {
        if (!cancelled) {
          dispatch({
            type: "history-error",
            message: err instanceof Error ? err.message : String(err),
          });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [state.mode, state.historyCursor, state.outcomeFilter]);

  // Live rows are filtered client-side (per spec FR-010); history rows
  // arrive already filtered by the server.
  const rows = useMemo<AuditEntry[]>(() => {
    if (state.mode === "live") {
      const data = live.data ?? [];
      return state.outcomeFilter === "all" ? data : data.filter((r) => r.outcome === state.outcomeFilter);
    }
    return state.pageEntries;
  }, [state.mode, live.data, state.outcomeFilter, state.pageEntries]);

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
        <Tabs
          value={state.mode}
          onValueChange={(value) => {
            if (isMode(value)) handleModeChange(value);
          }}
        >
          <TabsList>
            <TabsTrigger value="live">{t("audit.modeLive")}</TabsTrigger>
            <TabsTrigger value="history">{t("audit.modeHistory")}</TabsTrigger>
          </TabsList>
        </Tabs>
        <span className="text-sm text-muted-foreground">
          {state.mode === "live" ? t("audit.liveHint") : t("audit.historyHint")}
        </span>
      </div>
      <DataTable
        rows={rows}
        columns={columns}
        rowKey={(e) => `${e.timestamp}-${e.actor}-${e.path}-${e.outcome}`}
        toolbar={
          <Select
            value={state.outcomeFilter}
            onValueChange={(value) => {
              if (isOutcomeFilter(value)) handleOutcomeChange(value);
            }}
          >
            <SelectTrigger className="w-40" aria-label={t("audit.outcomeFilterLabel")}>
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
      {state.mode === "history" && (
        <div className="flex items-center gap-3 pt-2">
          <Button variant="outline" onClick={goNewer} disabled={state.prevStack.length === 0 || state.loading}>
            {t("audit.newer")}
          </Button>
          <span className="text-sm text-muted-foreground" aria-live="polite">
            {state.loading ? t("audit.loading") : t("audit.page", { n: state.pageNumber })}
          </span>
          <Button variant="outline" onClick={goOlder} disabled={!state.nextCursor || state.loading}>
            {t("audit.older")}
          </Button>
          {!state.nextCursor && !state.loading && state.pageEntries.length > 0 && (
            <span className="text-sm text-muted-foreground">{t("audit.noMoreHistory")}</span>
          )}
          {state.loadError && (
            <span className="text-sm text-destructive" role="alert">
              {state.loadError}
            </span>
          )}
        </div>
      )}
    </div>
  );
}
