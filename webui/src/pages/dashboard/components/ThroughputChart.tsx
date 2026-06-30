import { useTranslation } from "react-i18next";

import type { TrafficSample } from "@/api/types";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart";
import { useRecharts } from "@/components/ui/recharts-resource";
import { Skeleton } from "@/components/ui/skeleton";
import { formatBytes, formatChartTime, formatChartTimestamp } from "@/lib/format";

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
  const { CartesianGrid, Line, LineChart, XAxis, YAxis } = useRecharts();
  const data = (props.samples ?? []).map((s) => ({
    ts: s.ts,
    bytes_in: s.bytes_in,
    bytes_out: s.bytes_out,
  }));
  const hasSamples = data.length > 0;
  const chartConfig = {
    bytes_in: {
      label: t("traffic.bytesIn"),
      color: "var(--chart-1)",
    },
    bytes_out: {
      label: t("traffic.bytesOut"),
      color: "var(--chart-2)",
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
        ) : props.isLoading && !hasSamples ? (
          <Skeleton data-testid="throughput-chart-skeleton" className="h-48 w-full" />
        ) : !hasSamples ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("dashboard.noTrafficYet")}
          </p>
        ) : (
          <ChartContainer
            data-testid="throughput-chart"
            config={chartConfig}
            className="h-48 w-full"
          >
            <LineChart data={data} margin={{ top: 18, right: 8, left: 8, bottom: 0 }}>
              <CartesianGrid vertical={false} />
              <XAxis
                dataKey="ts"
                tickFormatter={(v) => formatChartTime(Number(v))}
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
                  <ChartTooltipContent
                    labelFormatter={(_, payload) => {
                      const ts = payload[0]?.payload?.ts;
                      return typeof ts === "number" ? formatChartTimestamp(ts) : null;
                    }}
                    valueFormatter={(v) => formatBytes(Number(v))}
                  />
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
