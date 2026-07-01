import { useTranslation } from "react-i18next";

import type { TopRule } from "@/api/metrics";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart";
import { useRecharts } from "@/components/ui/recharts-resource";
import { formatBytes } from "@/lib/format";

export interface TopRulesPanelProps {
  rules: TopRule[];
}

export function TopRulesPanel({ rules }: TopRulesPanelProps) {
  const { t } = useTranslation();
  const { Bar, BarChart, CartesianGrid, XAxis, YAxis } = useRecharts();
  const data = rules.map((rule) => ({
    rule: `#${rule.rule}`,
    bytesIn: rule.bytesIn,
    bytesOut: rule.bytesOut,
  }));
  const chartConfig = {
    bytesIn: {
      label: t("traffic.bytesIn"),
      color: "var(--chart-1)",
    },
    bytesOut: {
      label: t("traffic.bytesOut"),
      color: "var(--chart-2)",
    },
  } satisfies ChartConfig;

  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="text-sm">{t("dashboard.topRules")}</CardTitle>
      </CardHeader>
      <CardContent>
        {rules.length === 0 ? (
          <p className="py-8 text-center text-sm text-muted-foreground">
            {t("dashboard.noRulesYet")}
          </p>
        ) : (
          <ChartContainer
            config={chartConfig}
            className="w-full"
            style={{ height: Math.max(180, data.length * 38) }}
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
                dataKey="rule"
                type="category"
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                width={56}
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
