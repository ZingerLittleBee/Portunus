// 013-traffic-quotas G3: time-range + bucket selector + chart panel.
// XOR'd by user_id or client_name; both shapes use the same component.

import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";

import type { TrafficBucket } from "@/api/types";
import { useClientTraffic, useUserTraffic } from "@/api/traffic";
import { Card } from "@/components/ui/card";
import { Empty, EmptyDescription, EmptyTitle } from "@/components/ui/empty";
import { formatBytes } from "@/lib/format";

import { TrafficChart } from "./TrafficChart";

type RangeKey = "1h" | "24h" | "7d";

const RANGE_SECS: Record<RangeKey, number> = {
  "1h": 60 * 60,
  "24h": 24 * 60 * 60,
  "7d": 7 * 24 * 60 * 60,
};

type BucketKey = "auto" | "1m" | "1h";

interface BaseProps {
  defaultRange?: RangeKey;
}

type Props = BaseProps &
  (
    | { userId: string; clientName?: never }
    | { userId?: never; clientName: string }
  );

function resolveBucket(range: RangeKey, bucket: BucketKey): TrafficBucket | undefined {
  if (bucket !== "auto") return bucket;
  // Auto rule: ≤ 24h ⇒ 1m, otherwise 1h. Matches the server's default
  // selection in `samples::default_bucket` for parity.
  return range === "7d" ? "1h" : "1m";
}

export function TrafficPanel({ userId, clientName, defaultRange = "24h" }: Props) {
  const { t } = useTranslation();
  const [range, setRange] = useState<RangeKey>(defaultRange);
  const [bucket, setBucket] = useState<BucketKey>("auto");

  const now = useMemo(() => Math.floor(Date.now() / 1000), [range, bucket]);
  const query = useMemo(() => {
    const resolved = resolveBucket(range, bucket);
    return {
      from: now - RANGE_SECS[range],
      to: now,
      ...(resolved !== undefined ? { bucket: resolved } : {}),
    };
  }, [now, range, bucket]);

  const userQ = useUserTraffic(userId ?? "", query);
  const clientQ = useClientTraffic(clientName ?? "", query);
  const result = userId ? userQ : clientQ;

  const samples = result.data?.samples ?? [];
  const totalIn = result.data?.total_bytes_in ?? 0;
  const totalOut = result.data?.total_bytes_out ?? 0;

  return (
    <Card className="p-4 space-y-4">
      <div className="flex flex-wrap items-end gap-4">
        <label className="flex flex-col text-sm">
          <span className="text-muted-foreground">{t("traffic.timeRange")}</span>
          <select
            className="border rounded px-2 py-1 bg-background"
            value={range}
            onChange={(e) => setRange(e.target.value as RangeKey)}
          >
            <option value="1h">{t("traffic.ranges.1h")}</option>
            <option value="24h">{t("traffic.ranges.24h")}</option>
            <option value="7d">{t("traffic.ranges.7d")}</option>
          </select>
        </label>
        <label className="flex flex-col text-sm">
          <span className="text-muted-foreground">{t("traffic.bucket")}</span>
          <select
            className="border rounded px-2 py-1 bg-background"
            value={bucket}
            onChange={(e) => setBucket(e.target.value as BucketKey)}
          >
            <option value="auto">{t("traffic.buckets.auto")}</option>
            <option value="1m">{t("traffic.buckets.1m")}</option>
            <option value="1h">{t("traffic.buckets.1h")}</option>
          </select>
        </label>
        <div className="flex flex-col text-sm">
          <span className="text-muted-foreground">{t("traffic.totalIn")}</span>
          <span className="font-medium">{formatBytes(totalIn)}</span>
        </div>
        <div className="flex flex-col text-sm">
          <span className="text-muted-foreground">{t("traffic.totalOut")}</span>
          <span className="font-medium">{formatBytes(totalOut)}</span>
        </div>
      </div>

      {result.isLoading ? (
        <div className="text-sm text-muted-foreground py-12 text-center">
          {t("traffic.loading")}
        </div>
      ) : samples.length === 0 ? (
        <Empty>
          <EmptyTitle>{t("traffic.empty")}</EmptyTitle>
          <EmptyDescription>
            {t("traffic.ranges." + (range as string))}
          </EmptyDescription>
        </Empty>
      ) : (
        <TrafficChart samples={samples} />
      )}
    </Card>
  );
}
