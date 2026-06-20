// T023 — US1 happy path (superadmin walkthrough).
// Login → dashboard → create alice (with password) → provision client →
// add grant.
//
// The rule-push step from quickstart.md §6 is omitted: the operator
// rejects pushes for clients that aren't currently connected over the
// gRPC control plane, and the e2e fixture doesn't spin up a real
// portunus-client. The rule-list / live-stats panels are covered by the
// rule_stats_stream contract test (server-side) and us3-audit-and-metrics
// (UI side).

import { test, expect } from "./fixtures/server";
import { loginAs, enrollClient } from "./fixtures/helpers";

test("superadmin happy path", async ({ page, request, server }) => {
  await loginAs(page, server.superadminUserId, server.superadminPassword);

  // Superadmin lands on the dashboard overview (h1 = "Dashboard").
  await expect(page.getByRole("heading", { level: 1 })).toContainText(/dashboard/i);

  // Create alice via the SPA. A password is required at creation.
  await page.goto("/users/new");
  await page.getByLabel(/^id$/i).fill("alice");
  await page.getByLabel(/display name/i).fill("Alice");
  await page.getByLabel(/initial password/i).fill("alice correct horse battery staple");
  await page.getByRole("button", { name: /create user/i }).click();
  await expect(page).toHaveURL(/\/users\/alice/);

  // Enroll a client through the same one-time command path the UI exposes.
  await enrollClient(request, server.httpUrl, server.superadminToken, "edge-01");

  // Add a grant for alice (30000–30050 TCP+UDP). The standalone
  // /grants/new form was folded into the user-detail quota table, so
  // the grant is created there via the inline "Add quota" form.
  await page.goto("/users/alice");
  await page.getByRole("button", { name: /add quota/i }).click();
  await page.getByRole("combobox", { name: /^client$/i }).click();
  await page.getByPlaceholder(/search clients/i).fill("edge-01");
  await page.getByRole("option", { name: /edge-01/i }).click();
  await page.getByLabel(/port \(start\)/i).fill("30000");
  await page.getByLabel(/port \(end\)/i).fill("30050");
  // TCP is selected by default; add UDP so the grant covers both.
  await page.getByRole("checkbox", { name: /udp/i }).check();
  // No bandwidth / concurrency caps for this grant.
  await page.getByRole("switch", { name: /unlimited/i }).check();
  await page.getByRole("button", { name: /^save$/i }).click();
  // Entry lands in the per-user quota table.
  await expect(page.getByRole("row", { name: /edge-01/i }).first()).toBeVisible();
});
