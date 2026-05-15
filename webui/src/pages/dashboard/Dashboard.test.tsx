// webui/src/pages/dashboard/Dashboard.test.tsx
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter } from "react-router-dom";

import "@/i18n";
import { Dashboard } from "@/pages/Dashboard";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function renderDashboard() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter>
        <Dashboard />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

function mockRoutes(routes: Record<string, () => Response>): ReturnType<typeof vi.fn> {
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === "string" ? input : input.toString();
    for (const [pattern, handler] of Object.entries(routes)) {
      if (url.includes(pattern)) return handler();
    }
    return new Response("not found", { status: 404 });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

describe("Dashboard role branching", () => {
  it("tenant view never calls /v1/metrics", async () => {
    const fetchMock = mockRoutes({
      "/v1/users/me": () =>
        new Response(
          JSON.stringify({ user_id: "alice", role: "user", display_name: "Alice" }),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
      "/v1/clients": () =>
        new Response("[]", { status: 200, headers: { "content-type": "application/json" } }),
      "/v1/rules": () =>
        new Response("[]", { status: 200, headers: { "content-type": "application/json" } }),
      "/v1/users/alice/quotas": () =>
        new Response("[]", { status: 200, headers: { "content-type": "application/json" } }),
      "/v1/users/alice/traffic": () =>
        new Response(
          JSON.stringify({ bucket: "1m", samples: [], total_bytes_in: 0, total_bytes_out: 0 }),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
      "/v1/audit": () =>
        new Response("[]", { status: 200, headers: { "content-type": "application/json" } }),
    });

    renderDashboard();

    // Wait for the identity probe to land — that's the gate for everything else.
    await waitFor(() => {
      const urls = fetchMock.mock.calls.map((c) => String(c[0]));
      expect(urls.some((u) => u.includes("/v1/users/me"))).toBe(true);
    });

    // Give the tenant tree a moment to mount and fire its queries.
    await waitFor(() => {
      const urls = fetchMock.mock.calls.map((c) => String(c[0]));
      expect(urls.some((u) => u.includes("/v1/users/alice/traffic"))).toBe(true);
    });

    const urls = fetchMock.mock.calls.map((c) => String(c[0]));
    expect(urls.find((u) => u.includes("/v1/metrics"))).toBeUndefined();
    expect(urls.find((u) => u.includes("/v1/audit"))).toBeUndefined();
  });
});
