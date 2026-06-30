// T048 — Audit + metrics walkthrough (US3).
// Mirrors quickstart.md § 7. Drives a few operator API calls (allow + a
// deliberate deny via an unauthenticated bearer), then opens /audit and
// /metrics in the browser and asserts the SPA renders them correctly.

import { test, expect } from "./fixtures/server";
import { loginAs, api, provisionUser } from "./fixtures/helpers";

test("superadmin sees mixed allow/deny entries; tenant cannot reach /audit", async ({
  page,
  request,
  server,
}) => {
  // Generate one ALLOW (superadmin lists users) and one DENY (an
  // unknown bearer hits /v1/users). Both are recorded by the audit ring.
  await api(request, server.httpUrl, server.superadminToken, "/v1/users");
  // An invalid bearer is rejected at the auth layer (401) and recorded as
  // a deny by the audit ring (actor "_anonymous").
  const denied = await request.fetch(`${server.httpUrl}/v1/users`, {
    headers: { Authorization: "Bearer not-a-real-token" },
  });
  expect(denied.status()).toBe(401);

  // Drive the SPA as superadmin.
  await loginAs(page, server.superadminUserId, server.superadminPassword);
  await page.goto("/audit");
  await expect(page.getByText(/^allow$/i).first()).toBeVisible();
  await expect(page.getByText(/^deny$/i).first()).toBeVisible();

  // Outcome filter is client-side — flipping to "deny" must not fire a request.
  const before: string[] = [];
  page.on("request", (req) => {
    if (req.url().includes("/v1/audit")) before.push(req.url());
  });
  await page.getByLabel(/filter by outcome/i).click();
  await page.getByRole("option", { name: "deny", exact: true }).click();
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
  const alice = await provisionUser(request, server.httpUrl, server.superadminToken, "alice");
  await loginAs(page, alice.userId, alice.password);
  await page.goto("/audit");
  await expect(page.getByText(/permission denied/i)).toBeVisible();
});

test("superadmin /metrics renders raw text + dashboard gauges parse", async ({
  page,
  server,
}) => {
  await loginAs(page, server.superadminUserId, server.superadminPassword);
  await page.goto("/metrics");
  // Raw /metrics block contains the Prometheus header.
  await expect(page.locator("pre")).toContainText("# HELP");
  // Dashboard gauges parsed elsewhere — check the dashboard cards render.
  await page.goto("/");
  await expect(page.getByText(/connected clients/i)).toBeVisible();
  await expect(page.getByText(/active rules/i)).toBeVisible();
});
