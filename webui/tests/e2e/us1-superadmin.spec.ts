// T023 — US1 happy path (superadmin walkthrough).
// Login → dashboard → create alice → issue credential (token shown once,
// copy works, scrubbed on close) → provision client → add grant.
//
// The rule-push step from quickstart.md §6 is omitted: the operator
// rejects pushes for clients that aren't currently connected over the
// gRPC control plane, and the e2e fixture doesn't spin up a real
// portunus-client. The rule-list / live-stats panels are covered by the
// rule_stats_stream contract test (server-side) and us3-audit-and-metrics
// (UI side).

import { test, expect } from "./fixtures/server";
import { loginAs, api } from "./fixtures/helpers";

test("superadmin happy path", async ({ page, request, server }) => {
  await loginAs(page, server.superadminUserId, server.superadminPassword);

  // Dashboard greeting visible.
  await expect(page.getByRole("heading", { level: 1 })).toContainText(/welcome/i);

  // Create alice via the SPA.
  await page.goto("/users/new");
  await page.getByLabel(/^id$/i).fill("alice");
  await page.getByLabel(/display name/i).fill("Alice");
  await page.getByRole("button", { name: /create user/i }).click();
  await expect(page).toHaveURL(/\/users\/alice/);

  // Issue credential — token shown ONCE.
  await page.getByRole("button", { name: /issue credential/i }).click();
  const tokenField = page.getByLabel(/api token.*one-time/i);
  await expect(tokenField).toBeVisible();
  const issued = (await tokenField.textContent())?.trim() ?? "";
  expect(issued).not.toBe("");
  // Copy-to-clipboard. Headless chromium doesn't always grant
  // clipboard-write to http://127.0.0.1; the modal falls back to a
  // selectAll() in that path. Assert only that the click does not throw
  // and the modal stays open.
  await page.getByRole("button", { name: /^copy$/i }).click();
  await expect(page.getByLabel(/api token.*one-time/i)).toBeVisible();
  await page.getByRole("button", { name: /dismiss/i }).click();
  // Scrubbed: the token text is no longer present anywhere on the page.
  expect(await page.evaluate(() => document.body.innerText)).not.toContain(issued);

  // Provision a client (API path; the UI's provision form opens the same
  // bundle modal that us2 covers indirectly via the credentials flow).
  await api(request, server.httpUrl, server.superadminToken, "/v1/clients", {
    method: "POST",
    body: { name: "edge-01" },
  });

  // Add a grant for alice via the UI form (30000–30050 TCP+UDP).
  await page.goto("/grants/new");
  await page.getByLabel(/^user$/i).selectOption("alice");
  await page.getByLabel(/^client$/i).fill("edge-01");
  await page.getByLabel(/listen port \(start\)/i).fill("30000");
  await page.getByLabel(/listen port \(end\)/i).fill("30050");
  await page.getByLabel(/^tcp$/i).check();
  await page.getByLabel(/^udp$/i).check();
  await page.getByRole("button", { name: /create grant/i }).click();
  await expect(page).toHaveURL(/\/grants/);
  // Grant lands in the list.
  await expect(page.locator('[role="rowgroup"] [role="row"]').first()).toBeVisible();
});
