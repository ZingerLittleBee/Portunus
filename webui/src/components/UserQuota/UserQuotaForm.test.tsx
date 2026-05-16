// webui/src/components/UserQuota/UserQuotaForm.test.tsx
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { UserQuotaForm } from "./UserQuotaForm";
import "@/i18n";

afterEach(() => cleanup());

const clients = [
  { client_name: "edge-tokyo", connected: true },
  { client_name: "edge-sg", connected: true },
];

describe("UserQuotaForm", () => {
  it("renders with empty defaults", () => {
    render(
      <UserQuotaForm
        clients={clients}
        disabledClientNames={new Set()}
        defaultValues={undefined}
        onSubmit={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(screen.getByRole("combobox")).toBeTruthy();
  });

  it("blocks submission when ports inverted", async () => {
    const onSubmit = vi.fn();
    render(
      <UserQuotaForm
        clients={clients}
        disabledClientNames={new Set()}
        defaultValues={{
          client_name: "edge-tokyo",
          listen_port_start: 5000,
          listen_port_end: 1000,
          protocols: ["tcp"],
          unlimited: true,
        }}
        onSubmit={onSubmit}
        onCancel={() => {}}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /save/i }));
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("hides cap fields when unlimited toggled on", () => {
    render(
      <UserQuotaForm
        clients={clients}
        disabledClientNames={new Set()}
        defaultValues={{
          client_name: "edge-tokyo",
          listen_port_start: 1000,
          listen_port_end: 2000,
          protocols: ["tcp"],
          unlimited: true,
        }}
        onSubmit={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(screen.queryByLabelText(/bandwidth in/i)).toBeFalsy();
  });

  it("right-aligns dialog form actions", () => {
    const source = readFileSync(resolve(__dirname, "UserQuotaForm.tsx"), "utf8");
    expect(source).toContain("sm:justify-end");
  });
});
