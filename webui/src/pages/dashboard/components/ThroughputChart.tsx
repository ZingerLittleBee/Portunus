import { useTranslation } from "react-i18next";
import {
  CartesianGrid,
  Line,
  LineChart,
  XAxis,
  YAxis,
} from "recharts";

import type { TrafficSample } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart";
import { Skeleton } from "@/components/ui/skeleton";
import { formatBytes } from "@/lib/format";

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

export function ThroughputChart(props: ThroughputChartProps) {
  const { t } = useTranslation();
  const data = (props.samples ?? []).map((s) => ({
    tsLabel: new Date(s.ts * 1000).toLocaleString(),
    bytes_in: s.bytes_in,
    bytes_out: s.bytes_out,
  }));
  const chartConfig = {
    bytes_in: {
      label: t("traffic.bytesIn"),
      color: "hsl(220 70% 50%)",
    },
    bytes_out: {
      label: t("traffic.bytesOut"),
      color: "hsl(160 84% 39%)",
    },
  } satisfies ChartConfig;

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
          <ChartContainer config={chartConfig} className="h-48 w-full">
            <LineChart data={data}>
              <CartesianGrid vertical={false} />
              <XAxis
                dataKey="tsLabel"
                tickFormatter={(v) => new Date(String(v)).toLocaleTimeString()}
                fontSize={10}
                tickLine={false}
                axisLine={false}
                tickMargin={8}
              />
              <YAxis
                tickFormatter={(v) => formatBytes(Number(v))}
                fontSize={10}
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                width={60}
              />
              <ChartTooltip
                content={
                  <ChartTooltipContent valueFormatter={(v) => formatBytes(Number(v))} />
                }
              />
              <Line
                type="monotone"
                dataKey="bytes_in"
                stroke="var(--color-bytes_in)"
                dot={false}
              />
              <Line
                type="monotone"
                dataKey="bytes_out"
                stroke="var(--color-bytes_out)"
                dot={false}
              />
            </LineChart>
          </ChartContainer>
        )}
      </CardContent>
    </Card>
  );
}
