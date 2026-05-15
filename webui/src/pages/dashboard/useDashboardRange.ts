import { useCallback, useState } from "react";

import type { TrafficBucket } from "@/api/types";

export type DashboardRangeId = "1h" | "24h" | "7d";

export interface DashboardRange {
  from: number; // unix seconds
  to: number;
  bucket: TrafficBucket;
}

const SPAN_SEC: Record<DashboardRangeId, number> = {
  "1h": 3600,
  "24h": 86_400,
  "7d": 7 * 86_400,
};

const BUCKET: Record<DashboardRangeId, TrafficBucket> = {
  "1h": "1m",
  "24h": "1m",
  "7d": "1h",
};

export function computeRange(id: DashboardRangeId, now: number): DashboardRange {
  return { from: now - SPAN_SEC[id], to: now, bucket: BUCKET[id] };
}

export function useDashboardRange(initial: DashboardRangeId = "24h") {
  const [rangeId, setRangeId] = useState<DashboardRangeId>(initial);
  const range = computeRange(rangeId, Math.floor(Date.now() / 1000));
  const setRange = useCallback((id: DashboardRangeId) => setRangeId(id), []);
  return { rangeId, range, setRange };
}
