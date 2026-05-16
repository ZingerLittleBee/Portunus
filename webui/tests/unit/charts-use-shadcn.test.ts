import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

const chartFiles = [
  "src/pages/dashboard/components/ThroughputChart.tsx",
  "src/components/Traffic/TrafficChart.tsx",
];

function source(path: string) {
  return readFileSync(resolve(__dirname, "../../", path), "utf8");
}

describe("chart components", () => {
  it("use the shared shadcn chart wrapper and tooltip", () => {
    for (const file of chartFiles) {
      const text = source(file);

      expect(text, file).toContain("@/components/ui/chart");
      expect(text, file).not.toMatch(/\bResponsiveContainer\b/);
      expect(text, file).not.toMatch(/\bTooltip\b[^}]*from "recharts"/s);
    }
  });
});
