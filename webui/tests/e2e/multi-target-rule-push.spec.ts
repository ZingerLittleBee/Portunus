// 007-multi-target-failover T042 — Web UI surface coverage for the
// multi-target rule-push form. Runs against the real server fixture so
// the form's POST body lands as actual HTTP traffic — no mocks. The
// rule push won't activate (no connected client) so we observe via
// the rule-listing GET that the rule was created with the right shape.

import { test, expect } from "./fixtures/server";
import { loginAs, api, provisionUserWithToken } from "./fixtures/helpers";

test.describe("multi-target rule push", () => {
  test("operator toggles to multi-target mode and submits the new shape", async ({
    page,
    request,
    server,
  }) => {
    await loginAs(page, server.superadminToken);

    // Provision a client + grant so the rule-push body validates
    // server-side. The rule won't activate (no forwarder connected)
    // but it will be accepted into the in-memory rule store.
    await provisionUserWithToken(
      request,
      server.httpUrl,
      server.superadminToken,
      "alice",
    );
    await api(request, server.httpUrl, server.superadminToken, "/v1/clients", {
      method: "POST",
      body: { name: "edge-mt" },
    });
    await api(request, server.httpUrl, server.superadminToken, "/v1/grants", {
      method: "POST",
      body: {
        user_id: "_superadmin",
        client: "edge-mt",
        listen_port_start: 31000,
        listen_port_end: 31100,
        protocols: ["tcp"],
      },
    });

    await page.goto("/rules/new");

    // Default mode is single-target — the multi-target controls are hidden.
    await expect(page.getByRole("button", { name: /add another target/i })).toBeHidden();

    // Switch to multi-target mode.
    await page.getByLabel(/multi-target/i).check();

    // The two seeded target rows render. Add a third.
    await page.getByRole("button", { name: /add another target/i }).click();

    // The health-check interval field is visible.
    await expect(
      page.getByLabel(/active health-check interval/i),
    ).toBeVisible();

    // Fill the form and submit. Use a port we know is granted.
    await page.getByLabel(/^client$/i).fill("edge-mt");
    await page.getByLabel(/listen port \(start\)/i).fill("31050");

    const hostInputs = page.getByPlaceholder(/target host or ip/i);
    const portInputs = page.getByPlaceholder(/^port$/i);
    await hostInputs.nth(0).fill("127.0.0.1");
    await portInputs.nth(0).fill("9001");
    await hostInputs.nth(1).fill("127.0.0.1");
    await portInputs.nth(1).fill("9002");
    await hostInputs.nth(2).fill("127.0.0.1");
    await portInputs.nth(2).fill("9003");

    await page.getByRole("button", { name: /^push rule$/i }).click();

    // Either land on the rule detail page (rule pushed and id known) or
    // surface a server error. Either way: the multi-target body landed.
    // We assert the rule shows up on the listing with the MT pill.
    await page.goto("/rules");
    await expect(
      page.getByText(/MT ×3/, { exact: false }).first(),
    ).toBeVisible({ timeout: 10_000 });
  });

  test("MT pill is absent for single-target rules", async ({
    page,
    request,
    server,
  }) => {
    await loginAs(page, server.superadminToken);
    await api(request, server.httpUrl, server.superadminToken, "/v1/clients", {
      method: "POST",
      body: { name: "edge-st" },
    });
    await api(request, server.httpUrl, server.superadminToken, "/v1/grants", {
      method: "POST",
      body: {
        user_id: "_superadmin",
        client: "edge-st",
        listen_port_start: 32000,
        listen_port_end: 32100,
        protocols: ["tcp"],
      },
    });

    // Push a legacy single-target rule via HTTP (no UI, no targets[]).
    await api(request, server.httpUrl, server.superadminToken, "/v1/rules", {
      method: "POST",
      body: {
        client: "edge-st",
        listen_port: 32050,
        target_host: "127.0.0.1",
        target_port: 9000,
        protocol: "tcp",
      },
    });

    await page.goto("/rules");
    // The rule is listed but no MT pill should appear in its row.
    const row = page
      .getByRole("row")
      .filter({ hasText: "edge-st" })
      .first();
    await expect(row).toBeVisible({ timeout: 10_000 });
    await expect(row.getByText(/MT ×/)).toHaveCount(0);
  });
});
