// webui/src/components/UserQuota/ClientCombobox.test.tsx
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { ClientCombobox } from "./ClientCombobox";
import "@/i18n";

afterEach(() => {
  cleanup();
});

// 015-client-stable-id (US3): the combobox value is the stable client_id;
// the label stays the display name.
const clients = [
  { client_id: "01TOKYO0000000000000000000", client_name: "edge-tokyo", connected: true },
  { client_id: "01SG00000000000000000000000", client_name: "edge-sg", connected: false },
  { client_id: "01FRA0000000000000000000000", client_name: "edge-fra", connected: true },
];

describe("ClientCombobox", () => {
  it("renders placeholder when value empty", () => {
    render(
      <ClientCombobox
        clients={clients}
        value=""
        onChange={() => {}}
        disabledClientIds={new Set()}
      />,
    );
    expect(screen.getByRole("combobox")).toBeTruthy();
  });

  it("opens popover on click and lists clients", () => {
    render(
      <ClientCombobox
        clients={clients}
        value=""
        onChange={() => {}}
        disabledClientIds={new Set()}
      />,
    );
    fireEvent.click(screen.getByRole("combobox"));
    expect(screen.getByText("edge-tokyo")).toBeTruthy();
    expect(screen.getByText("edge-sg")).toBeTruthy();
  });

  it("disables clients in disabledClientIds", () => {
    render(
      <ClientCombobox
        clients={clients}
        value=""
        onChange={() => {}}
        disabledClientIds={new Set(["01SG00000000000000000000000"])}
      />,
    );
    fireEvent.click(screen.getByRole("combobox"));
    const sg = screen.getByText("edge-sg").closest("[role='option']");
    expect(sg?.getAttribute("aria-disabled")).toBe("true");
  });

  it("selects a client by id when clicking a command item", () => {
    const onChange = vi.fn();
    render(
      <ClientCombobox
        clients={clients}
        value=""
        onChange={onChange}
        disabledClientIds={new Set()}
      />,
    );
    fireEvent.click(screen.getByRole("combobox"));
    fireEvent.click(screen.getByText("edge-tokyo"));
    expect(onChange).toHaveBeenCalledWith("01TOKYO0000000000000000000");
  });
});
