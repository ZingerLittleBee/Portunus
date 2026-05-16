import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { describe, expect, it } from "vitest";

function source(path: string) {
  return readFileSync(resolve(__dirname, "../../", path), "utf8");
}

describe("UserDetail layout", () => {
  it("does not wrap the credentials, quota, and traffic sections in outer cards", () => {
    const text = source("src/pages/UserDetail.tsx");

    expect(text).not.toContain("@/components/ui/card");
    expect(text).not.toMatch(/<Card\b/);
    expect(text).toContain("<TrafficPanel userId={userId} framed={false} />");
  });
});
