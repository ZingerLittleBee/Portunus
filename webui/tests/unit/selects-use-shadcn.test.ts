import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

const uiFiles = [
  "src/pages/RulesList.tsx",
  "src/pages/AuditLog.tsx",
  "src/components/RuleFormTargets.tsx",
  "src/components/Traffic/TrafficPanel.tsx",
];

function source(path: string) {
  return readFileSync(resolve(__dirname, "../../", path), "utf8");
}

describe("select controls", () => {
  it("use the shared shadcn select component instead of native select markup", () => {
    for (const file of uiFiles) {
      const text = source(file);

      expect(text, file).toContain("@/components/ui/select");
      expect(text, file).not.toMatch(/<select\b/);
      expect(text, file).not.toMatch(/<\/select>/);
      expect(text, file).not.toMatch(/<option\b/);
    }
  });
});
