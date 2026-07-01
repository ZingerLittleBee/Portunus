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
    expect(source).toContain('type: "edit-open"');
    // 015-client-stable-id (US2): the display name is now editable
    // (identity-safe rename addressed by client_id), not a disabled field.
    expect(source).toContain("editName: action.client.client_name");
    expect(source).toContain("editAddress: action.client.client_address");
    expect(source).toContain("value={state.editName}");
    expect(source).toContain("value={state.editAddress}");
    expect(source).toContain("useUpdateClient");
    expect(source).toContain("useRenameClient");
  });

  it("keeps destructive client actions behind confirmation dialogs", () => {
    const source = readFileSync(resolve(__dirname, "../../src/pages/ClientsList.tsx"), "utf8");

    expect(source).toContain("pendingRevoke");
    expect(source).toContain("pendingDelete");
    expect(source.match(/<ConfirmDialog/g)?.length).toBe(2);
  });
});
