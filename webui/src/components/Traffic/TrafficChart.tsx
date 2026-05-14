// 013-traffic-quotas G2: stacked area chart for `bytes_in` / `bytes_out`
// over a sample series returned by `/v1/users/{id}/traffic` or
// `/v1/clients/{name}/traffic`.

import { useTranslation } from "react-i18next";
import {
  Area,
  AreaChart,
  Legend,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";

import type { TrafficSample } from "@/api/types";
import { formatBytes } from "@/lib/format";

interface Props {
  samples: TrafficSample[];
  height?: number;
}

export function TrafficChart({ samples, height = 320 }: Props) {
  const { t } = useTranslation();
  const data = samples.map((s) => ({
    ts: new Date(s.ts * 1000).toLocaleString(),
    in: s.bytes_in,
    out: s.bytes_out,
  }));
  return (
    <ResponsiveContainer width="100%" height={height}>
      <AreaChart data={data}>
        <XAxis dataKey="ts" minTickGap={48} />
        <YAxis tickFormatter={(v) => formatBytes(Number(v))} width={80} />
        <Tooltip
          formatter={(v: number | string) => formatBytes(Number(v))}
        />
        <Legend />
        <Area
          type="monotone"
          dataKey="in"
          name={t("traffic.bytesIn")}
          stackId="1"
          stroke="hsl(var(--chart-1, 220 70% 50%))"
          fill="hsl(var(--chart-1, 220 70% 50%))"
          fillOpacity={0.3}
        />
        <Area
          type="monotone"
          dataKey="out"
          name={t("traffic.bytesOut")}
          stackId="1"
          stroke="hsl(var(--chart-2, 12 76% 61%))"
          fill="hsl(var(--chart-2, 12 76% 61%))"
          fillOpacity={0.3}
        />
      </AreaChart>
    </ResponsiveContainer>
  );
}
