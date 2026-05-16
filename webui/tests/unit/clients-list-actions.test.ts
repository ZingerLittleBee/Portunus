import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

describe("ClientsList action column", () => {
  it("uses a shadcn dropdown menu for row actions", () => {
    const source = readFileSync(resolve(__dirname, "../../src/pages/ClientsList.tsx"), "utf8");

    expect(source).toContain("DropdownMenu");
    expect(source).toContain("MoreHorizontal");
    expect(source).toContain('aria-label={t("clients.actions")}');
  });

  it("opens edit in a dialog with prefilled editable fields", () => {
    const source = readFileSync(resolve(__dirname, "../../src/pages/ClientsList.tsx"), "utf8");

    expect(source).toContain("pendingEdit");
    expect(source).toContain("openEdit(c)");
    expect(source).toContain('value={pendingEdit?.client_name ?? ""}');
    expect(source).toContain("value={editAddress}");
    expect(source).toContain("useUpdateClient");
  });

  it("keeps destructive client actions behind confirmation dialogs", () => {
    const source = readFileSync(resolve(__dirname, "../../src/pages/ClientsList.tsx"), "utf8");

    expect(source).toContain("pendingRevoke");
    expect(source).toContain("pendingDelete");
    expect(source.match(/<ConfirmDialog/g)?.length).toBe(2);
  });
});
