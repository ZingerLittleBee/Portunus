import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

describe("ClientProvision enrollment flow", () => {
  it("creates an enrollment command and renders the install guide", () => {
    const source = readFileSync(
      resolve(__dirname, "../../src/components/ClientProvisionForm.tsx"),
      "utf8",
    );

    expect(source).toContain("useCreateClientEnrollment");
    expect(source).not.toContain("useProvisionClient");
    expect(source).toContain("EnrollmentInstallGuide");
    expect(source).not.toContain("EnrollmentCommandCard");
  });
});
