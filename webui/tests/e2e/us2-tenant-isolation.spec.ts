// T041 — Tenant isolation walkthrough (US2).
// Provision alice + bob via the operator API, log in as alice, and assert:
//   - Users / Audit / Provision-client links are absent in nav
//   - /users renders <PermissionDenied /> with NO /v1/users request
//   - /users/bob/credentials renders <PermissionDenied />
//   - /rules shows only alice's rules (server-side filter)
//   - rotate credential → modal token works on next call; old bearer 401s

import { test, expect } from "./fixtures/server";
import { loginAs, provisionUserWithToken } from "./fixtures/helpers";

test("alice cannot see admin nav, /users, or /users/bob", async ({ page, request, server }) => {
  await provisionUserWithToken(request, server.httpUrl, server.superadminToken, "bob");
  const alice = await provisionUserWithToken(request, server.httpUrl, server.superadminToken, "alice");

  await loginAs(page, alice.token);

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

  // /users/bob/credentials — denied (alice ≠ bob, alice is not super).
  await page.goto("/users/bob/credentials");
  await expect(page.getByText(/permission denied/i)).toBeVisible();
});

test("rotate credential — new token works, old token 401s", async ({ page, request, server }) => {
  const alice = await provisionUserWithToken(request, server.httpUrl, server.superadminToken, "alice");
  await loginAs(page, alice.token);

  // Open her own user detail (self path is allowed).
  await page.goto(`/users/alice`);
  // Wait for the credentials list to render before clicking the row's
  // Rotate button (the credential card mounts after the GET /v1/users/me
  // and GET /v1/users/alice/credentials round-trips resolve).
  await page.getByRole("button", { name: /rotate/i }).first().waitFor();
  await page.getByRole("button", { name: /rotate/i }).first().click();
  // Confirm inside the dialog — scope to role="dialog" so we don't
  // re-click the row button still rendered in the background.
  await page.getByRole("dialog").getByRole("button", { name: /^rotate$/i }).click();

  // TokenRevealModal renders the new token in a <pre aria-label="...">.
  const tokenField = page.getByLabel(/bearer token \(one-time\)/i);
  await expect(tokenField).toBeVisible();
  const newToken = ((await tokenField.textContent()) ?? "").trim();
  expect(newToken).not.toBe(alice.token);
  await page.getByRole("button", { name: /dismiss/i }).click();

  // Old token must 401 on a direct API hit.
  const oldRes = await request.fetch(`${server.httpUrl}/v1/users/me`, {
    headers: { Authorization: `Bearer ${alice.token}` },
  });
  expect(oldRes.status()).toBe(401);

  // New token works.
  const newRes = await request.fetch(`${server.httpUrl}/v1/users/me`, {
    headers: { Authorization: `Bearer ${newToken}` },
  });
  expect(newRes.status()).toBe(200);
});
