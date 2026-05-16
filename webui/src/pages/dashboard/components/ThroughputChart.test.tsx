import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "@/i18n";
import { ThroughputChart } from "./ThroughputChart";

beforeEach(() => {
  vi.spyOn(console, "warn").mockImplementation(() => undefined);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const samples = [
  { ts: 1778946000, bytes_in: 10, bytes_out: 20 },
  { ts: 1778946060, bytes_in: 30, bytes_out: 40 },
];

describe("ThroughputChart", () => {
  it("keeps rendering the chart when a new range is loading over existing samples", () => {
    render(
      <ThroughputChart
        samples={samples}
        isLoading
        error={null}
        rangeId="24h"
        onRangeChange={() => undefined}
        onRetry={() => undefined}
      />,
    );

    expect(screen.getByTestId("throughput-chart")).toBeDefined();
    expect(screen.queryByTestId("throughput-chart-skeleton")).toBeNull();
  });
});
