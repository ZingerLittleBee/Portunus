import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

describe("ClientDetail re-enrollment flow", () => {
  it("generates an enrollment command instead of reissuing a bundle", () => {
    const source = readFileSync(resolve(__dirname, "../../src/pages/ClientDetail.tsx"), "utf8");

    expect(source).toContain("useCreateClientReEnrollment");
    expect(source).not.toContain("useReissueClient");
    expect(source).not.toContain("CredentialBundleCard");
    expect(source).not.toContain("ClientInstallSteps");
    expect(source).toContain("reenrollment.command");
  });
});
