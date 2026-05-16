import { useTranslation } from "react-i18next";
import { Bar, BarChart, CartesianGrid, XAxis, YAxis } from "recharts";

import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { formatBytes } from "@/lib/format";

import type { TrafficDirectionRow } from "../trafficBreakdown";

interface TrafficDirectionChartProps {
  rows: TrafficDirectionRow[];
  isLoading: boolean;
  error: unknown;
}

export function TrafficDirectionChart({ rows, isLoading, error }: TrafficDirectionChartProps) {
  const { t } = useTranslation();
  const data = rows.map((row) => ({
    label: row.direction === "in" ? t("dashboard.directionIn") : t("dashboard.directionOut"),
    bytes: row.bytes,
  }));
  const chartConfig = {
    bytes: {
      label: t("dashboard.totalTransferred"),
      color: "hsl(262 83% 58%)",
    },
  } satisfies ChartConfig;

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("dashboard.trafficDirection")}</CardTitle>
      </CardHeader>
      <CardContent>
        {error ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("dashboard.chartLoadError")}
          </p>
        ) : isLoading && data.every((row) => row.bytes === 0) ? (
          <Skeleton className="h-36 w-full" />
        ) : data.every((row) => row.bytes === 0) ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("dashboard.noTrafficYet")}
          </p>
        ) : (
          <ChartContainer config={chartConfig} className="h-36 w-full">
            <BarChart
              accessibilityLayer
              data={data}
              layout="vertical"
              margin={{ top: 12, right: 12, left: 8, bottom: 0 }}
            >
              <CartesianGrid horizontal={false} />
              <XAxis
                type="number"
                tickFormatter={(value) => formatBytes(Number(value))}
                tickLine={false}
                axisLine={false}
                fontSize={10}
                tickMargin={8}
              />
              <YAxis
                dataKey="label"
                type="category"
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                width={72}
                fontSize={11}
              />
              <ChartTooltip
                cursor={false}
                content={
                  <ChartTooltipContent
                    valueFormatter={(value) => formatBytes(Number(value))}
                  />
                }
              />
              <Bar dataKey="bytes" fill="var(--color-bytes)" radius={4} />
            </BarChart>
          </ChartContainer>
        )}
      </CardContent>
    </Card>
  );
}
