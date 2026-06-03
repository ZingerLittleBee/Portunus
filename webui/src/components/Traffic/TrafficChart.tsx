// 013-traffic-quotas G2: stacked area chart for `bytes_in` / `bytes_out`
// over a sample series returned by `/v1/users/{id}/traffic` or
// `/v1/clients/{id}/traffic` (015-client-stable-id: keyed by client_id).

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
import { formatBytes, formatChartTime, formatChartTimestamp } from "@/lib/format";

interface Props {
  samples: TrafficSample[];
  height?: number;
}

export function TrafficChart({ samples, height = 320 }: Props) {
  const { t } = useTranslation();
  const data = samples.map((s) => ({
    ts: s.ts,
    bytes_in: s.bytes_in,
    bytes_out: s.bytes_out,
  }));
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
    <ChartContainer config={chartConfig} className="w-full" style={{ height }}>
      <AreaChart data={data} margin={{ top: 18, right: 8, left: 8, bottom: 0 }}>
        <XAxis
          dataKey="ts"
          minTickGap={48}
          tickFormatter={(v) => formatChartTime(Number(v))}
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
