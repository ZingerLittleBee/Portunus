/// 011-rate-limiting-qos T037: integration test for the Owner quotas
/// tab on the client detail page. Stubs `fetch` so the TanStack
/// Query hooks see realistic shapes; asserts the empty state, list
/// rendering, and the "Open in user" navigation button.
///
/// Updated for Task 12: OwnerQuotasTab is now read-only — the inline
/// editor and "Add owner cap" dialog have been removed. Each row now
/// shows an "Open in user" button that navigates to /users/<owner_id>.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import "@/i18n";
import { OwnerQuotasTab } from "@/pages/ClientDetail";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

interface MockResponseInit {
  body?: unknown;
  status?: number;
}

function mockFetchByPath(routes: Record<string, MockResponseInit>) {
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = typeof input === "string" ? input : (input as Request).url;
    const path = url.startsWith("http") ? new URL(url).pathname : url;
    const matched = routes[path];
    if (!matched) {
      return new Response(null, { status: 404 });
    }
    return new Response(matched.body == null ? null : JSON.stringify(matched.body), {
      status: matched.status ?? 200,
      headers: { "Content-Type": "application/json" },
    });
  });
}

function renderTab(clientId: string) {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  globalThis.localStorage?.clear?.();
  return render(
    <MemoryRouter>
      <QueryClientProvider client={qc}>
        <OwnerQuotasTab clientId={clientId} />
      </QueryClientProvider>
    </MemoryRouter>,
  );
}

beforeEach(() => {
  vi.spyOn(console, "error").mockImplementation(() => undefined);
  // happy-dom doesn't compute layout, so DataTable's @tanstack/react-virtual
  // sees scroll-element height = 0 and renders zero rows. Stub the
  // bounding rect on every Element to return a viewport that fits the
  // entire test dataset (we never test more than a handful of rows).
  Element.prototype.getBoundingClientRect = () =>
    ({ width: 800, height: 480, top: 0, left: 0, right: 800, bottom: 480, x: 0, y: 0, toJSON: () => undefined }) as DOMRect;
  // The virtualizer also reads `clientHeight`; happy-dom returns 0 by
  // default. Patch it for the same reason.
  Object.defineProperty(HTMLElement.prototype, "clientHeight", {
    configurable: true,
    get: () => 480,
  });
});

describe("OwnerQuotasTab", () => {
  it("renders the empty state when no owners exist", async () => {
    mockFetchByPath({
      "/v1/clients/edge-01/owners": { body: [] },
    });
    renderTab("edge-01");
    await waitFor(() => {
      expect(screen.getByText(/No owners yet/i)).toBeDefined();
    });
  });

  it("lists owners with their cap status and rule count", async () => {
    mockFetchByPath({
      "/v1/clients/edge-01/owners": {
        body: [
          { owner_id: "alice", rule_count: 3, has_rate_limit: true },
          { owner_id: "bob", rule_count: 1, has_rate_limit: false },
        ],
      },
    });
    renderTab("edge-01");
    await waitFor(() => {
      expect(screen.getByText("alice")).toBeDefined();
      expect(screen.getByText("bob")).toBeDefined();
    });
    expect(screen.getByText("3")).toBeDefined();
    expect(screen.getByText("1")).toBeDefined();
    expect(screen.getByText(/^capped$/i)).toBeDefined();
    expect(screen.getByText(/^uncapped$/i)).toBeDefined();
  });

  it("renders an 'Open in user' button for each owner row", async () => {
    mockFetchByPath({
      "/v1/clients/edge-01/owners": {
        body: [
          { owner_id: "alice", rule_count: 2, has_rate_limit: true },
          { owner_id: "bob", rule_count: 1, has_rate_limit: false },
        ],
      },
    });
    renderTab("edge-01");
    await waitFor(() => screen.getByText("alice"));
    const buttons = screen.getAllByRole("button", { name: /open in user/i });
    expect(buttons).toHaveLength(2);
  });

  it("shows the movedHint paragraph and no 'Add owner cap' button", async () => {
    mockFetchByPath({
      "/v1/clients/edge-01/owners": { body: [] },
    });
    renderTab("edge-01");
    await waitFor(() => {
      expect(
        screen.getByText(/Owner quota editing has moved to the user detail page/i),
      ).toBeDefined();
    });
    expect(screen.queryByRole("button", { name: /add owner cap/i })).toBeNull();
  });
});
