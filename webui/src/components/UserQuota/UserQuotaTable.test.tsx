// webui/src/components/UserQuota/UserQuotaTable.test.tsx
import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen, fireEvent } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { UserQuotaTable } from "./UserQuotaTable";
import type { AccessEntry } from "@/api/access-entries";
import "@/i18n";

afterEach(() => cleanup());

const wrap = (ui: React.ReactElement) => {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(<QueryClientProvider client={qc}>{ui}</QueryClientProvider>);
};

const entries: AccessEntry[] = [
  {
    grant_id: "g1",
    user_id: "alice",
    client_name: "edge-tokyo",
    listen_port_start: 1000,
    listen_port_end: 2000,
    protocols: ["tcp"],
    unlimited: false,
    cap: { bandwidth_in_bps: 1_000_000 },
  },
  {
    grant_id: "g2",
    user_id: "alice",
    client_name: "edge-sg",
    listen_port_start: 3000,
    listen_port_end: 4000,
    protocols: ["tcp", "udp"],
    unlimited: true,
  },
];

describe("UserQuotaTable", () => {
  it("renders one row per entry", () => {
    wrap(
      <UserQuotaTable
        userId="alice"
        entries={entries}
        clients={[
          { client_name: "edge-tokyo", connected: true },
          { client_name: "edge-sg", connected: false },
        ]}
        readOnly={false}
      />,
    );
    expect(screen.getByText("edge-tokyo")).toBeTruthy();
    expect(screen.getByText("edge-sg")).toBeTruthy();
  });

  it("shows 'Unlimited' badge on entries without cap", () => {
    wrap(
      <UserQuotaTable
        userId="alice"
        entries={entries}
        clients={[]}
        readOnly={false}
      />,
    );
    expect(screen.getAllByText(/unlimited/i).length).toBeGreaterThan(0);
  });

  it("hides + Add button in read-only mode", () => {
    wrap(
      <UserQuotaTable
        userId="alice"
        entries={entries}
        clients={[]}
        readOnly={true}
      />,
    );
    expect(screen.queryByText(/add/i)).toBeFalsy();
  });

  it("clicking + Add reveals an inline form", () => {
    wrap(
      <UserQuotaTable
        userId="alice"
        entries={entries}
        clients={[]}
        readOnly={false}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /add/i }));
    expect(screen.getAllByRole("combobox").length).toBeGreaterThan(0);
  });
});
