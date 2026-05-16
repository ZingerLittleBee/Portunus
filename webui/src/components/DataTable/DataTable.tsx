import * as React from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { cn } from "@/lib/cn";

export interface Column<Row> {
  key: string;
  header: React.ReactNode;
  render: (row: Row) => React.ReactNode;
  width?: string;
  sortable?: boolean;
  sortValue?: (row: Row) => string | number;
}

export interface DataTableProps<Row> {
  rows: Row[];
  columns: Column<Row>[];
  rowKey: (row: Row) => string;
  rowHeight?: number;
  emptyState?: React.ReactNode;
  toolbar?: React.ReactNode;
  onRowClick?: (row: Row) => void;
  className?: string;
  ariaLabel?: string;
}

type SortState = { key: string; dir: "asc" | "desc" } | null;

export function DataTable<Row>({
  rows,
  columns,
  rowKey,
  rowHeight = 44,
  emptyState,
  toolbar,
  onRowClick,
  className,
  ariaLabel,
}: DataTableProps<Row>) {
  const parentRef = React.useRef<HTMLDivElement>(null);
  const [sort, setSort] = React.useState<SortState>(null);
  const [focusedIndex, setFocusedIndex] = React.useState(0);

  const sorted = React.useMemo(() => {
    if (!sort) return rows;
    const col = columns.find((c) => c.key === sort.key);
    if (!col?.sortValue) return rows;
    const sortValue = col.sortValue;
    const dir = sort.dir === "asc" ? 1 : -1;
    return [...rows].sort((a, b) => {
      const av = sortValue(a);
      const bv = sortValue(b);
      if (av < bv) return -1 * dir;
      if (av > bv) return 1 * dir;
      return 0;
    });
  }, [rows, columns, sort]);

  const virtualizer = useVirtualizer({
    count: sorted.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => rowHeight,
    overscan: 8,
  });

  function toggleSort(col: Column<Row>) {
    if (!col.sortable) return;
    setSort((prev) => {
      if (prev?.key !== col.key) return { key: col.key, dir: "asc" };
      if (prev.dir === "asc") return { key: col.key, dir: "desc" };
      return null;
    });
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    const last = sorted.length - 1;
    if (last < 0) return;
    let next = focusedIndex;
    switch (e.key) {
      case "ArrowDown": next = Math.min(last, focusedIndex + 1); break;
      case "ArrowUp": next = Math.max(0, focusedIndex - 1); break;
      case "PageDown": next = Math.min(last, focusedIndex + 10); break;
      case "PageUp": next = Math.max(0, focusedIndex - 10); break;
      case "Home": next = 0; break;
      case "End": next = last; break;
      case "Enter": {
        const row = sorted[focusedIndex];
        if (row && onRowClick) onRowClick(row);
        return;
      }
      default: return;
    }
    e.preventDefault();
    setFocusedIndex(next);
    virtualizer.scrollToIndex(next, { align: "auto" });
  }

  const grid = gridStyle(columns);
  const tableWidth = tableMinWidth(columns);

  return (
    <div className={cn("flex min-w-0 flex-col gap-2", className)}>
      {toolbar && <div className="flex flex-col gap-2 sm:flex-row sm:items-center">{toolbar}</div>}
      <div className="overflow-x-auto rounded-md border">
        <div
          className="grid border-b bg-muted/40 text-sm font-medium"
          role="row"
          style={{ ...grid, minWidth: tableWidth }}
        >
          {columns.map((c) => (
            <button
              key={c.key}
              type="button"
              role="columnheader"
              onClick={() => toggleSort(c)}
              className={cn(
                "flex h-10 items-center gap-1 px-3 text-left",
                c.sortable && "cursor-pointer hover:bg-muted/70",
              )}
              aria-sort={
                sort?.key === c.key ? (sort.dir === "asc" ? "ascending" : "descending") : "none"
              }
            >
              {c.header}
              {sort?.key === c.key && <span aria-hidden>{sort.dir === "asc" ? "↑" : "↓"}</span>}
            </button>
          ))}
        </div>
        <div
          ref={parentRef}
          tabIndex={0}
          onKeyDown={onKeyDown}
          className="h-[480px] overflow-y-auto focus:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          role="rowgroup"
          aria-label={ariaLabel}
          style={{ minWidth: tableWidth }}
        >
          {sorted.length === 0 ? (
            <div className="flex h-full items-center justify-center p-8 text-sm text-muted-foreground">
              {emptyState ?? "No rows"}
            </div>
          ) : (
            <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
              {virtualizer.getVirtualItems().map((vrow) => {
                const row = sorted[vrow.index];
                if (!row) return null;
                return (
                  <div
                    key={rowKey(row)}
                    role="row"
                    aria-rowindex={vrow.index + 1}
                    tabIndex={-1}
                    data-focused={focusedIndex === vrow.index ? "true" : undefined}
                    onClick={() => {
                      setFocusedIndex(vrow.index);
                      onRowClick?.(row);
                    }}
                    className={cn(
                      "absolute left-0 right-0 grid items-center border-b text-sm hover:bg-muted/30",
                      onRowClick && "cursor-pointer",
                      focusedIndex === vrow.index && "bg-muted/40",
                    )}
                    style={{ ...grid, height: vrow.size, transform: `translateY(${vrow.start}px)` }}
                  >
                    {columns.map((c) => (
                      <div key={c.key} role="cell" className="truncate px-3">
                        {c.render(row)}
                      </div>
                    ))}
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>
      <div className="text-xs text-muted-foreground">
        {sorted.length} {sorted.length === 1 ? "row" : "rows"}
      </div>
    </div>
  );
}

function gridStyle<Row>(columns: Column<Row>[]): React.CSSProperties {
  return {
    gridTemplateColumns: columns.map((c) => c.width ?? "1fr").join(" "),
  };
}

function tableMinWidth<Row>(columns: Column<Row>[]): string {
  return `${columns.reduce((total, column) => total + widthToPixels(column.width), 0)}px`;
}

function widthToPixels(width: string | undefined): number {
  if (!width) return 160;
  const match = /^(\d+(?:\.\d+)?)px$/.exec(width);
  return match ? Number(match[1]) : 160;
}
