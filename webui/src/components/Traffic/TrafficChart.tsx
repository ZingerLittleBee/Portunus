// 013-traffic-quotas G2: stacked area chart for `bytes_in` / `bytes_out`
// over a sample series returned by `/v1/users/{id}/traffic` or
// `/v1/clients/{name}/traffic`.

import { useTranslation } from "react-i18next";
import {
  Area,
  AreaChart,
  XAxis,
  YAxis,
} from "recharts";

import type { TrafficSample } from "@/api/types";
import {
  ChartContainer,
  ChartLegend,
  ChartLegendContent,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart";
import { formatBytes } from "@/lib/format";

interface Props {
  samples: TrafficSample[];
  height?: number;
}

export function TrafficChart({ samples, height = 320 }: Props) {
  const { t } = useTranslation();
  const data = samples.map((s) => ({
    ts: new Date(s.ts * 1000).toLocaleString(),
    bytes_in: s.bytes_in,
    bytes_out: s.bytes_out,
  }));
  const chartConfig = {
    bytes_in: {
      label: t("traffic.bytesIn"),
      color: "hsl(var(--chart-1, 220 70% 50%))",
    },
    bytes_out: {
      label: t("traffic.bytesOut"),
      color: "hsl(var(--chart-2, 12 76% 61%))",
    },
  } satisfies ChartConfig;

  return (
    <ChartContainer config={chartConfig} className="w-full" style={{ height }}>
      <AreaChart data={data}>
        <XAxis
          dataKey="ts"
          minTickGap={48}
          tickLine={false}
          axisLine={false}
          tickMargin={8}
        />
        <YAxis
          tickFormatter={(v) => formatBytes(Number(v))}
          tickLine={false}
          axisLine={false}
          tickMargin={8}
          width={80}
        />
        <ChartTooltip
          content={<ChartTooltipContent valueFormatter={(v) => formatBytes(Number(v))} />}
        />
        <ChartLegend content={<ChartLegendContent />} />
        <Area
          type="monotone"
          dataKey="bytes_in"
          stackId="1"
          stroke="var(--color-bytes_in)"
          fill="var(--color-bytes_in)"
          fillOpacity={0.3}
        />
        <Area
          type="monotone"
          dataKey="bytes_out"
          stackId="1"
          stroke="var(--color-bytes_out)"
          fill="var(--color-bytes_out)"
          fillOpacity={0.3}
        />
      </AreaChart>
    </ChartContainer>
  );
}
