// webui/src/pages/ClientDetail.test.tsx
//
// Security/UX regression guard: the "Owner quotas" tab lists owners and
// caps ACROSS tenants for a client. The backend now gates
// GET /v1/clients/{id}/owners superadmin-only, so the tab must be hidden
// from User-role operators (who can otherwise reach /clients/:clientId)
// instead of rendering a tab that polls a guaranteed 403.
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import i18next from "i18next";

import "@/i18n";
import { ClientDetail } from "@/pages/ClientDetail";

const CLIENT_ID = "01HXCLIENTDETAILTESTID0000";

const CLIENT = {
  client_id: CLIENT_ID,
  client_name: "edge-1",
  provisioned_at: "2026-06-01T00:00:00Z",
  revoked_at: null,
  connected: false,
  client_address: null,
  remote_addr: null,
  connected_at: null,
};

function mockRoutes(role: "user" | "superadmin") {
  const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
    const url = typeof input === "string" ? input : input.toString();
    const json = (body: unknown, status = 200) =>
      new Response(JSON.stringify(body), {
        status,
        headers: { "content-type": "application/json" },
      });
    if (url.includes("/v1/users/me")) {
      return json({
        user_id: role === "superadmin" ? "_superadmin" : "alice",
        role,
        display_name: role,
      });
    }
    if (url.includes(`/v1/clients/${CLIENT_ID}/owners`)) return json([]);
    if (url.includes(`/v1/clients/${CLIENT_ID}/quotas`)) return json([]);
    if (url.includes(`/v1/clients/${CLIENT_ID}/traffic`)) {
      return json({ bucket: "1m", samples: [], total_bytes_in: 0, total_bytes_out: 0 });
    }
    if (url.endsWith("/v1/clients") || url.includes("/v1/clients?")) return json([CLIENT]);
    return json([]);
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function renderDetail() {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return render(
    <QueryClientProvider client={qc}>
      <MemoryRouter initialEntries={[`/clients/${CLIENT_ID}`]}>
        <Routes>
          <Route path="/clients/:clientId" element={<ClientDetail />} />
        </Routes>
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

const ownersTab = () =>
  screen.queryByRole("tab", { name: i18next.t("clientDetail.tabOwnerQuotas") });

beforeEach(async () => {
  await i18next.changeLanguage("en");
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("ClientDetail owner-quotas tab visibility", () => {
  it("hides the owner-quotas tab from a User-role operator", async () => {
    const fetchMock = mockRoutes("user");
    renderDetail();

    // Wait for the identity probe to resolve and the page to settle to
    // its final tab set (Overview + Traffic only).
    await waitFor(() => expect(screen.getAllByRole("tab")).toHaveLength(2));
    expect(ownersTab()).toBeNull();

    // The now-superadmin-only owners endpoint is never fetched.
    const urls = fetchMock.mock.calls.map((c) => String(c[0]));
    expect(urls.some((u) => u.includes(`/v1/clients/${CLIENT_ID}/owners`))).toBe(false);
  });

  it("shows the owner-quotas tab to a superadmin", async () => {
    mockRoutes("superadmin");
    renderDetail();

    await waitFor(() => expect(screen.getAllByRole("tab")).toHaveLength(3));
    expect(ownersTab()).not.toBeNull();
  });
});
