import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

describe("CommandInput focus styling", () => {
  it("suppresses the global focus ring inside command popovers", () => {
    const source = readFileSync(
      resolve(__dirname, "../../src/components/ui/command.tsx"),
      "utf8",
    );

    expect(source).toContain("focus-visible:ring-0");
    expect(source).toContain("focus-visible:ring-offset-0");
  });
});
