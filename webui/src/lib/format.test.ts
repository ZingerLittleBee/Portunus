import { describe, expect, it } from "vitest";

import { formatChartTime, formatChartTimestamp } from "@/lib/format";

describe("chart time formatting", () => {
  it("uses 24-hour time without AM/PM markers", () => {
    const morning = new Date("2026-01-01T00:05:00+08:00").getTime() / 1000;
    const afternoon = new Date("2026-01-01T13:45:00+08:00").getTime() / 1000;

    expect(formatChartTime(morning)).toMatch(/^\d{2}:05$/);
    expect(formatChartTime(afternoon)).toMatch(/^\d{2}:45$/);
    expect(formatChartTimestamp(morning)).not.toMatch(/\b(?:AM|PM)\b/i);
    expect(formatChartTimestamp(afternoon)).not.toMatch(/\b(?:AM|PM)\b/i);
  });
});
