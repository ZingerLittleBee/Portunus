import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

describe("CommandInput focus styling", () => {
  it("suppresses the native focus outline inside command popovers", () => {
    const source = readFileSync(
      resolve(__dirname, "../../src/components/ui/command.tsx"),
      "utf8",
    );

    // The command input renders inside an InputGroup that owns the focus
    // treatment, so the raw input itself must not paint its own outline.
    expect(source).toContain("outline-hidden");
  });
});
