// T048 — Audit + metrics walkthrough (US3).
// Mirrors quickstart.md § 7. Drives a few operator API calls (allow + a
// deliberate deny via 403 from a tenant token), then opens /audit and
// /metrics in the browser and asserts the SPA renders them correctly.

import { test, expect } from "./fixtures/server";
import { loginAs, api, provisionUserWithToken } from "./fixtures/helpers";

test("superadmin sees mixed allow/deny entries; tenant cannot reach /audit", async ({
  page,
  request,
  server,
}) => {
  // Generate one ALLOW (superadmin lists users) and one DENY (alice tries to
  // list users). Both are recorded by the audit ring.
  await api(request, server.httpUrl, server.superadminToken, "/v1/users");
  const alice = await provisionUserWithToken(request, server.httpUrl, server.superadminToken, "alice");
  // Alice is a tenant — listing users is denied with 403 role_required.
  const denied = await request.fetch(`${server.httpUrl}/v1/users`, {
    headers: { Authorization: `Bearer ${alice.token}` },
  });
  expect(denied.status()).toBe(403);

  // Drive the SPA as superadmin.
  await loginAs(page, server.superadminToken);
  await page.goto("/audit");
  // DataTable renders rows under [role="rowgroup"]; wait for any row.
  const dataRows = page.locator('[role="rowgroup"] [role="row"]');
  await expect(dataRows.first()).toBeVisible();
  const rowsText = await dataRows.allInnerTexts();
  const hasAllow = rowsText.some((t) => /allow/i.test(t));
  const hasDeny = rowsText.some((t) => /deny/i.test(t));
  expect(hasAllow).toBe(true);
  expect(hasDeny).toBe(true);

  // Outcome filter is client-side — flipping to "deny" must not fire a request.
  const before: string[] = [];
  page.on("request", (req) => {
    if (req.url().includes("/v1/audit")) before.push(req.url());
  });
  await page.getByLabel(/filter by outcome/i).selectOption("deny");
  // Give a beat for any rogue refetch; no request should be appended.
  await page.waitForTimeout(250);
  expect(before.length).toBe(0);

  // Download as JSON button produces NDJSON whose first line parses.
  const [download] = await Promise.all([
    page.waitForEvent("download"),
    page.getByRole("button", { name: /download as json/i }).click(),
  ]);
  const stream = await download.createReadStream();
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(chunk as Buffer);
  const text = Buffer.concat(chunks).toString();
  const firstLine = text.split("\n").filter(Boolean)[0]!;
  const parsed = JSON.parse(firstLine);
  expect(parsed).toHaveProperty("outcome");
  expect(parsed).toHaveProperty("path");
});

test("tenant /audit renders PermissionDenied", async ({ page, server, request }) => {
  const alice = await provisionUserWithToken(request, server.httpUrl, server.superadminToken, "alice");
  await loginAs(page, alice.token);
  await page.goto("/audit");
  await expect(page.getByText(/permission denied/i)).toBeVisible();
});

test("superadmin /metrics renders raw text + dashboard gauges parse", async ({
  page,
  server,
}) => {
  await loginAs(page, server.superadminToken);
  await page.goto("/metrics");
  // Raw /metrics block contains the Prometheus header.
  await expect(page.locator("pre")).toContainText("# HELP");
  // Dashboard gauges parsed elsewhere — check the dashboard cards render.
  await page.goto("/");
  await expect(page.getByText(/connected clients/i)).toBeVisible();
  await expect(page.getByText(/active rules/i)).toBeVisible();
});
