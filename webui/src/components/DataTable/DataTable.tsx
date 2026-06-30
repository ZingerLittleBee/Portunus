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
  className,
  ariaLabel,
}: DataTableProps<Row>) {
  const parentRef = React.useRef<HTMLDivElement>(null);
  const [sort, setSort] = React.useState<SortState>(null);

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

  const grid = gridStyle(columns);
  const tableWidth = tableMinWidth(columns);
  const rowCountLabel = `${sorted.length} ${sorted.length === 1 ? "row" : "rows"}`;

  return (
    <div className={cn("flex min-w-0 flex-col gap-2", className)}>
      {toolbar && <div className="flex flex-col gap-2 sm:flex-row sm:items-center">{toolbar}</div>}
      <div className="overflow-x-auto rounded-lg border">
        <div
          ref={parentRef}
          className="h-[480px] overflow-y-auto"
          style={{ minWidth: tableWidth }}
        >
          <table className="w-full border-collapse text-sm" aria-label={ariaLabel}>
            <thead className="sticky top-0 z-10 block border-b bg-muted/40 font-medium">
              <tr className="grid" style={grid}>
                {columns.map((c) => (
                  <th
                    key={c.key}
                    scope="col"
                    className="p-0 text-left font-medium"
                    aria-sort={
                      sort?.key === c.key
                        ? sort.dir === "asc"
                          ? "ascending"
                          : "descending"
                        : "none"
                    }
                  >
                    <button
                      type="button"
                      onClick={() => toggleSort(c)}
                      className={cn(
                        "flex h-10 w-full items-center gap-1 px-3 text-left",
                        c.sortable && "cursor-pointer hover:bg-muted/70",
                      )}
                    >
                      {c.header}
                      {sort?.key === c.key && (
                        <span aria-hidden>{sort.dir === "asc" ? "↑" : "↓"}</span>
                      )}
                    </button>
                  </th>
                ))}
              </tr>
            </thead>
            {sorted.length === 0 ? (
              <tbody>
                <tr>
                  <td colSpan={columns.length}>
                    <div className="flex h-[440px] items-center justify-center p-8 text-sm text-muted-foreground">
                      {emptyState ?? "No rows"}
                    </div>
                  </td>
                </tr>
              </tbody>
            ) : (
              <tbody
                className="block"
                style={{ height: virtualizer.getTotalSize(), position: "relative" }}
              >
                {virtualizer.getVirtualItems().map((vrow) => {
                  const row = sorted[vrow.index];
                  if (!row) return null;
                  return (
                    <tr
                      key={rowKey(row)}
                      aria-rowindex={vrow.index + 2}
                      className="absolute left-0 right-0 grid items-center border-b text-sm hover:bg-muted/30"
                      style={{ ...grid, height: vrow.size, transform: `translateY(${vrow.start}px)` }}
                    >
                      {columns.map((c) => (
                        <td key={c.key} className="truncate px-3">
                          {c.render(row)}
                        </td>
                      ))}
                    </tr>
                  );
                })}
              </tbody>
            )}
          </table>
        </div>
      </div>
      <div className="text-xs text-muted-foreground">{rowCountLabel}</div>
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
