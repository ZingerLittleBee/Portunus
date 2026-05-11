import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, Routes } from "react-router-dom";

import "@/i18n";
import { App } from "@/App";
import { ThemeProvider } from "@/theme/ThemeProvider";

afterEach(() => {
  cleanup();
  window.sessionStorage.removeItem("portunus.token");
  vi.restoreAllMocks();
});

function renderApp(initialPath = "/") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return {
    qc,
    ...render(
      <QueryClientProvider client={qc}>
        <ThemeProvider>
          <MemoryRouter initialEntries={[initialPath]}>
            <App />
          </MemoryRouter>
        </ThemeProvider>
      </QueryClientProvider>,
    ),
  };
}

function renderWithRoutes(ui: React.ReactNode, initialPath = "/") {
  const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return {
    qc,
    ...render(
      <QueryClientProvider client={qc}>
        <MemoryRouter initialEntries={[initialPath]}>
          <Routes>
            <Route path="/login" element={ui} />
            <Route path="/" element={<div>home</div>} />
          </Routes>
        </MemoryRouter>
      </QueryClientProvider>,
    ),
  };
}

function mockFetch(routes: Record<string, { status?: number; body?: unknown }>) {
  vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = typeof input === "string" ? input : (input as Request).url;
    const path = (url.startsWith("http") ? new URL(url).pathname : url).split("?")[0]!;
    const matched = routes[path];
    if (!matched) {
      return new Response(JSON.stringify({ error: { code: "missing_mock" } }), { status: 404 });
    }
    return new Response(matched.body == null ? null : JSON.stringify(matched.body), {
      status: matched.status ?? 200,
      headers: { "Content-Type": "application/json" },
    });
  });
}

describe("local password auth UI", () => {
  it("renders the password login without requiring sessionStorage", async () => {
    mockFetch({
      "/v1/auth/status": { body: { onboarding_required: false } },
    });
    window.sessionStorage.setItem("portunus.token", "legacy-token");

    renderApp("/login");

    await waitFor(() => {
      expect(screen.getByLabelText("User ID")).toBeDefined();
    });
    expect(window.sessionStorage.getItem("portunus.token")).toBeNull();
  });

  it("routes fresh stores to onboarding and clears cached status after onboarding", async () => {
    mockFetch({
      "/v1/auth/status": { body: { onboarding_required: true } },
      "/v1/auth/onboarding": { status: 201, body: { user_id: "admin" } },
      "/v1/auth/login": { body: { password_change_required: false } },
      "/v1/users/me": {
        body: { user_id: "admin", role: "superadmin", display_name: "Administrator" },
      },
      "/v1/clients": { body: [] },
      "/v1/rules": { body: [] },
    });
    const { qc } = renderApp("/");

    await waitFor(() => {
      expect(screen.getByLabelText("Setup token")).toBeDefined();
    });
    fireEvent.change(screen.getByLabelText("Setup token"), { target: { value: "setup-token" } });
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "correct horse battery staple" },
    });
    fireEvent.change(screen.getByLabelText("Confirm password"), {
      target: { value: "correct horse battery staple" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Create admin" }));

    await waitFor(() => {
      expect(qc.getQueryData(["auth", "status"])).toEqual({ onboarding_required: false });
    });
    await waitFor(() => {
      expect(screen.queryByLabelText("Setup token")).toBeNull();
    });
  });

  it("clears all cached data on logout", async () => {
    mockFetch({
      "/v1/auth/status": { body: { onboarding_required: false } },
      "/v1/users/me": {
        body: { user_id: "admin", role: "superadmin", display_name: "Administrator" },
      },
      "/v1/auth/logout": { status: 204 },
      "/v1/clients": { body: [] },
      "/v1/rules": { body: [] },
    });
    const { qc } = renderApp("/");
    qc.setQueryData(["clients"], [{ client_name: "secret-edge" }]);

    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Sign out" })).toBeDefined();
    });
    fireEvent.click(screen.getByRole("button", { name: "Sign out" }));

    await waitFor(() => {
      expect(qc.getQueryData(["clients"])).toBeUndefined();
    });
  });

  it("redirects to login on 401 without trapping AuthGate in loading", async () => {
    mockFetch({
      "/v1/auth/status": { body: { onboarding_required: false } },
      "/v1/users/me": { status: 401, body: { error: { code: "unauthenticated" } } },
    });
    const { qc } = renderApp("/");
    qc.setQueryData(["clients"], [{ client_name: "secret-edge" }]);

    await waitFor(() => {
      expect(screen.getByLabelText("User ID")).toBeDefined();
    });
    expect(qc.getQueryData(["clients"])).toBeUndefined();
  });

  it("shows the required password-change step after temporary-password login", async () => {
    const { LoginPage } = await import("@/auth/LoginPage");
    mockFetch({
      "/v1/auth/login": { body: { password_change_required: true } },
      "/v1/users/me/password": { status: 204 },
      "/v1/users/me": {
        body: { user_id: "admin", role: "superadmin", display_name: "Administrator" },
      },
    });
    renderWithRoutes(<LoginPage />, "/login");

    fireEvent.change(screen.getByLabelText("User ID"), { target: { value: "admin" } });
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "temporary correct horse battery staple" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Sign in" }));

    await waitFor(() => {
      expect(screen.getByText("Set a new password")).toBeDefined();
    });
    fireEvent.change(screen.getByLabelText("New password"), {
      target: { value: "correct horse battery staple" },
    });
    fireEvent.change(screen.getByLabelText("Confirm new password"), {
      target: { value: "correct horse battery staple" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save password" }));

    await waitFor(() => {
      expect(screen.getByText("home")).toBeDefined();
    });
  });
});
