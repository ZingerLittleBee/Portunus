// webui/src/components/UserQuota/UserQuotaForm.test.tsx
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { UserQuotaForm } from "./UserQuotaForm";
import "@/i18n";

afterEach(() => cleanup());

const clients = [
  { client_id: "01TOKYO0000000000000000000", client_name: "edge-tokyo", connected: true },
  { client_id: "01SG00000000000000000000000", client_name: "edge-sg", connected: true },
];

describe("UserQuotaForm", () => {
  it("renders with empty defaults", () => {
    render(
      <UserQuotaForm
        clients={clients}
        disabledClientIds={new Set()}
        defaultValues={undefined}
        onSubmit={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(screen.getByRole("combobox")).toBeTruthy();
  });

  it("defaults new entries to unlimited tcp and udp access", () => {
    render(
      <UserQuotaForm
        clients={clients}
        disabledClientIds={new Set()}
        defaultValues={undefined}
        onSubmit={() => {}}
        onCancel={() => {}}
      />,
    );

    expect(screen.getByRole("switch", { name: /unlimited/i }).getAttribute("aria-checked")).toBe(
      "true",
    );
    expect(screen.getByRole("checkbox", { name: "TCP" }).getAttribute("aria-checked")).toBe(
      "true",
    );
    expect(screen.getByRole("checkbox", { name: "UDP" }).getAttribute("aria-checked")).toBe(
      "true",
    );
    expect(screen.queryByLabelText(/bandwidth in/i)).toBeFalsy();
  });

  it("blocks submission when ports inverted", async () => {
    const onSubmit = vi.fn();
    render(
      <UserQuotaForm
        clients={clients}
        disabledClientIds={new Set()}
        defaultValues={{
          client_id: "01TOKYO0000000000000000000",
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
        disabledClientIds={new Set()}
        defaultValues={{
          client_id: "01TOKYO0000000000000000000",
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
