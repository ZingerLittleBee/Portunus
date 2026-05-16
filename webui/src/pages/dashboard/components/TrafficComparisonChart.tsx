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

import type { TrafficBreakdownItem } from "../trafficBreakdown";

interface TrafficComparisonChartProps {
  title: string;
  items: TrafficBreakdownItem[];
  isLoading: boolean;
  error: unknown;
}

export function TrafficComparisonChart({
  title,
  items,
  isLoading,
  error,
}: TrafficComparisonChartProps) {
  const { t } = useTranslation();
  const data = items.map((item) => ({
    label: item.label,
    bytesIn: item.bytesIn,
    bytesOut: item.bytesOut,
  }));
  const chartConfig = {
    bytesIn: {
      label: t("traffic.bytesIn"),
      color: "hsl(220 70% 50%)",
    },
    bytesOut: {
      label: t("traffic.bytesOut"),
      color: "hsl(160 84% 39%)",
    },
  } satisfies ChartConfig;

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{title}</CardTitle>
      </CardHeader>
      <CardContent>
        {error ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("dashboard.chartLoadError")}
          </p>
        ) : isLoading && data.length === 0 ? (
          <Skeleton className="h-52 w-full" />
        ) : data.length === 0 ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("dashboard.noTrafficYet")}
          </p>
        ) : (
          <ChartContainer
            config={chartConfig}
            className="w-full"
            style={{ height: Math.max(190, data.length * 38) }}
          >
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
                width={96}
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
              <Bar
                dataKey="bytesIn"
                stackId="traffic"
                fill="var(--color-bytesIn)"
                radius={[4, 0, 0, 4]}
              />
              <Bar
                dataKey="bytesOut"
                stackId="traffic"
                fill="var(--color-bytesOut)"
                radius={[0, 4, 4, 0]}
              />
            </BarChart>
          </ChartContainer>
        )}
      </CardContent>
    </Card>
  );
}
