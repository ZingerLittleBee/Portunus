// T065 — Quickstart smoke walkthrough.
// This is the executable counterpart of `specs/006-management-web-ui/quickstart.md`.
// Coarse-grained on purpose: the per-user-story specs cover fine-grained
// assertions. § 6 (rule push) is omitted here because pushing a rule
// requires an actually-connected portunus-client over the gRPC control
// plane; spinning a real edge client up under Playwright would slow the
// suite down without buying coverage that us1-superadmin doesn't already
// provide via the UI.

import { test, expect } from "./fixtures/server";
import { loginAs, api, provisionUserWithToken } from "./fixtures/helpers";

test("quickstart walkthrough end-to-end", async ({ page, request, server }) => {
  // § 3 — login.
  await loginAs(page, server.superadminUserId, server.superadminPassword);

  // § 4 — provision alice + bob and a client.
  await provisionUserWithToken(request, server.httpUrl, server.superadminToken, "alice");
  await provisionUserWithToken(request, server.httpUrl, server.superadminToken, "bob");
  await api(request, server.httpUrl, server.superadminToken, "/v1/clients", {
    method: "POST",
    body: { name: "edge-01", address: "127.0.0.1" },
  });

  // § 5 — add a grant for alice.
  await api(request, server.httpUrl, server.superadminToken, "/v1/grants", {
    method: "POST",
    body: {
      user_id: "alice",
      client: "edge-01",
      listen_port_start: 30000,
      listen_port_end: 30050,
      protocols: ["tcp", "udp"],
    },
  });

  // § 7 — clients page surfaces the provisioned forwarder.
  await page.goto("/clients");
  await expect(page.locator('[role="rowgroup"] [role="row"]').first()).toBeVisible();

  // § 8 — audit log lists the API calls made above.
  await page.goto("/audit");
  await expect(page.locator('[role="rowgroup"] [role="row"]').first()).toBeVisible();

  // § 11 — sign out → login screen.
  await page.getByRole("button", { name: /sign out/i }).click();
  await expect(page).toHaveURL(/\/login/);
});
