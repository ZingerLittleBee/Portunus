import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

describe("ClientProvision enrollment flow", () => {
  it("creates an enrollment command instead of issuing a bundle", () => {
    const source = readFileSync(resolve(__dirname, "../../src/pages/ClientProvision.tsx"), "utf8");

    expect(source).toContain("useCreateClientEnrollment");
    expect(source).not.toContain("useProvisionClient");
    expect(source).toContain("enrollment.command");
    expect(source).toContain('t("clientProvision.enrollment.copy")');
  });
});
