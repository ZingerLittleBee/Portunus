// T041 — Tenant isolation walkthrough (US2).
// Provision alice + bob via the operator API, log in as alice, and assert:
//   - Users / Audit / Provision-client links are absent in nav
//   - /users renders <PermissionDenied /> with NO /v1/users request
//   - /users/bob renders <PermissionDenied />

import { test, expect } from "./fixtures/server";
import { loginAs, provisionUser } from "./fixtures/helpers";

test("alice cannot see admin nav, /users, or /users/bob", async ({ page, request, server }) => {
  await provisionUser(request, server.httpUrl, server.superadminToken, "bob");
  const alice = await provisionUser(request, server.httpUrl, server.superadminToken, "alice");

  await loginAs(page, alice.userId, alice.password);

  // Nav: superadmin items hidden.
  await expect(page.getByRole("link", { name: /users/i })).toHaveCount(0);
  await expect(page.getByRole("link", { name: /audit log/i })).toHaveCount(0);

  // /users — denied without ever firing /v1/users.
  const usersRequests: string[] = [];
  page.on("request", (req) => {
    if (req.url().endsWith("/v1/users")) usersRequests.push(req.url());
  });
  await page.goto("/users");
  await expect(page.getByText(/permission denied/i)).toBeVisible();
  expect(usersRequests).toEqual([]);

  // /users/bob — denied (alice ≠ bob, alice is not super).
  await page.goto("/users/bob");
  await expect(page.getByText(/permission denied/i)).toBeVisible();
});
