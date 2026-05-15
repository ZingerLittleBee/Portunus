import { useTranslation } from "react-i18next";
import {
  CartesianGrid,
  Line,
  LineChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";

import type { TrafficSample } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";

import type { DashboardRangeId } from "@/pages/dashboard/useDashboardRange";

const RANGE_IDS: DashboardRangeId[] = ["1h", "24h", "7d"];

export interface ThroughputChartProps {
  samples: TrafficSample[] | undefined;
  isLoading: boolean;
  error: unknown;
  rangeId: DashboardRangeId;
  onRangeChange: (id: DashboardRangeId) => void;
  onRetry: () => void;
}

function fmtBytes(v: number): string {
  if (v < 1024) return `${v} B`;
  if (v < 1024 * 1024) return `${(v / 1024).toFixed(1)} KB`;
  if (v < 1024 * 1024 * 1024) return `${(v / 1024 / 1024).toFixed(1)} MB`;
  return `${(v / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

export function ThroughputChart(props: ThroughputChartProps) {
  const { t } = useTranslation();
  const data = (props.samples ?? []).map((s) => ({
    ts: s.ts * 1000,
    bytes_in: s.bytes_in,
    bytes_out: s.bytes_out,
  }));

  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between pb-2">
        <CardTitle className="text-sm">{t("dashboard.throughputChart")}</CardTitle>
        <div className="flex gap-1">
          {RANGE_IDS.map((id) => (
            <Button
              key={id}
              size="sm"
              variant={props.rangeId === id ? "default" : "outline"}
              onClick={() => props.onRangeChange(id)}
            >
              {id}
            </Button>
          ))}
        </div>
      </CardHeader>
      <CardContent>
        {props.error ? (
          <div className="flex flex-col items-center justify-center gap-2 py-8 text-sm text-muted-foreground">
            <span>{t("dashboard.chartLoadError")}</span>
            <Button size="sm" variant="outline" onClick={props.onRetry}>
              {t("common.retry")}
            </Button>
          </div>
        ) : props.isLoading ? (
          <Skeleton className="h-48 w-full" />
        ) : data.length === 0 ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("dashboard.noTrafficYet")}
          </p>
        ) : (
          <div className="h-48">
            <ResponsiveContainer width="100%" height="100%">
              <LineChart data={data}>
                <CartesianGrid strokeDasharray="3 3" stroke="rgba(0,0,0,0.05)" />
                <XAxis
                  dataKey="ts"
                  tickFormatter={(v) => new Date(v as number).toLocaleTimeString()}
                  fontSize={10}
                />
                <YAxis tickFormatter={fmtBytes} fontSize={10} width={60} />
                <Tooltip
                  labelFormatter={(v) => new Date(v as number).toLocaleString()}
                  formatter={(v: number) => fmtBytes(v)}
                />
                <Line type="monotone" dataKey="bytes_in" stroke="#3b82f6" dot={false} />
                <Line type="monotone" dataKey="bytes_out" stroke="#10b981" dot={false} />
              </LineChart>
            </ResponsiveContainer>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
