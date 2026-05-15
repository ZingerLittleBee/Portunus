import { describe, expect, it } from "vitest";
import { computeRange, type DashboardRangeId } from "@/pages/dashboard/useDashboardRange";

const NOW = 1_700_000_000;

describe("computeRange", () => {
  it.each([
    ["1h" as DashboardRangeId, 3600, "1m"],
    ["24h" as DashboardRangeId, 86_400, "1m"],
    ["7d" as DashboardRangeId, 7 * 86_400, "1h"],
  ])("range %s -> span %i, bucket %s", (id, span, bucket) => {
    const r = computeRange(id, NOW);
    expect(r.to).toBe(NOW);
    expect(r.from).toBe(NOW - span);
    expect(r.bucket).toBe(bucket);
  });
});
