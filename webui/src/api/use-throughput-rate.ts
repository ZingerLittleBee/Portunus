import { useEffect, useRef, useState } from "react";

import { useDashboardGauges, useMetricsText } from "@/api/metrics";

export interface ThroughputSample {
  totalBytes: number;
  ts: number; // seconds since epoch
}

/// Returns the inferred current throughput in bytes/sec, or `null` if
/// we have not yet collected two samples. A counter reset (negative
/// delta) collapses to 0 rather than producing a negative number.
export function computeRate(
  prev: ThroughputSample | null,
  next: ThroughputSample,
): number | null {
  if (!prev) return null;
  const dt = next.ts - prev.ts;
  if (dt <= 0) return 0;
  const db = next.totalBytes - prev.totalBytes;
  if (db < 0) return 0;
  return db / dt;
}

/// Subscribes to the metrics poll and returns a live bytes/sec value
/// computed from the cumulative `portunus_rule_bytes_*_total` sum.
export function useThroughputRate(): number | null {
  const gauges = useDashboardGauges();
  const { dataUpdatedAt } = useMetricsText();
  const prev = useRef<ThroughputSample | null>(null);
  const [rate, setRate] = useState<number | null>(null);

  useEffect(() => {
    if (!dataUpdatedAt || gauges.topRules.length === 0) return;
    const totalBytes = gauges.topRules.reduce(
      (acc, r) => acc + r.bytesIn + r.bytesOut,
      0,
    );
    const next = { totalBytes, ts: dataUpdatedAt / 1000 };
    setRate(computeRate(prev.current, next));
    prev.current = next;
  }, [dataUpdatedAt, gauges.topRules]);

  return rate;
}
