import { describe, expect, it } from "vitest";

import {
  sortTrafficBreakdownItems,
  trafficDirectionRows,
  trafficTotalsToItem,
} from "./trafficBreakdown";

describe("traffic breakdown helpers", () => {
  it("sorts traffic comparison items by bidirectional total and drops empty rows", () => {
    const items = [
      trafficTotalsToItem("edge-1", "edge-1", {
        total_bytes_in: 8,
        total_bytes_out: 2,
      }),
      trafficTotalsToItem("edge-2", "edge-2", {
        total_bytes_in: 30,
        total_bytes_out: 15,
      }),
      trafficTotalsToItem("edge-empty", "edge-empty", {
        total_bytes_in: 0,
        total_bytes_out: 0,
      }),
    ];

    expect(sortTrafficBreakdownItems(items)).toEqual([
      {
        id: "edge-2",
        label: "edge-2",
        bytesIn: 30,
        bytesOut: 15,
        total: 45,
      },
      {
        id: "edge-1",
        label: "edge-1",
        bytesIn: 8,
        bytesOut: 2,
        total: 10,
      },
    ]);
  });

  it("keeps inbound and outbound totals as separate dashboard rows", () => {
    expect(
      trafficDirectionRows({
        total_bytes_in: 120,
        total_bytes_out: 80,
      }),
    ).toEqual([
      { direction: "in", bytes: 120 },
      { direction: "out", bytes: 80 },
    ]);
  });
});
