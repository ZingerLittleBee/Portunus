// 007-multi-target-failover T042 — Web UI surface coverage for the
// multi-target rule-push form. Runs against the real server fixture so
// the form's POST body lands as actual HTTP traffic — no mocks. The
// e2e fixture does not spin up a portunus-client, so current server
// semantics reject the push after validation with client_not_connected.

import { test, expect } from "./fixtures/server";
import { loginAs, enrollClient } from "./fixtures/helpers";

test.describe("multi-target rule push", () => {
  test("operator toggles to multi-target mode and submits the new shape", async ({
    page,
    request,
    server,
  }) => {
    await loginAs(page, server.superadminUserId, server.superadminPassword);

    // Provision a client so the rule-push body validates server-side
    // before the expected offline-client rejection.
    await enrollClient(request, server.httpUrl, server.superadminToken, "edge-mt");

    await page.goto("/rules/new");

    // Default mode is single-target — the multi-target controls are hidden.
    await expect(page.getByRole("button", { name: /add another target/i })).toBeHidden();

    // Switch to multi-target mode. The mode selector is a shadcn ToggleGroup
    // (radix `type="single"`), so its items render as `role="radio"` buttons
    // whose accessible name is the option text — click, not check.
    await page.getByRole("radio", { name: /multi-target/i }).click();

    // The two seeded target rows render. Add a third.
    await page.getByRole("button", { name: /add another target/i }).click();

    // The health-check interval field is visible.
    await expect(
      page.getByLabel(/active health-check interval/i),
    ).toBeVisible();

    // Fill the form and submit. The superadmin token bypasses grant checks.
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

    const pushResponse = page.waitForResponse(
      (response) =>
        response.request().method() === "POST" &&
        response.url().endsWith("/v1/rules"),
    );
    await page.getByRole("button", { name: /^push rule$/i }).click();
    const response = await pushResponse;
    const pushBody: unknown = JSON.parse(response.request().postData() ?? "null");
    expect(pushBody).toMatchObject({
      client: "edge-mt",
      listen_port: 31050,
      protocol: "tcp",
      targets: [
        { host: "127.0.0.1", port: 9001, priority: 0 },
        { host: "127.0.0.1", port: 9002, priority: 1 },
        { host: "127.0.0.1", port: 9003, priority: 2 },
      ],
    });
    expect(pushBody).not.toMatchObject({ target_host: expect.anything() });
    expect(response.status()).toBe(422);
    await expect(page.getByText(/client_not_connected/i)).toBeVisible();
  });

  test("single-target pushes surface offline-client rejection without MT state", async ({
    page,
    request,
    server,
  }) => {
    await loginAs(page, server.superadminUserId, server.superadminPassword);
    await enrollClient(request, server.httpUrl, server.superadminToken, "edge-st");

    await page.goto("/rules/new");
    await page.getByLabel(/^client$/i).fill("edge-st");
    await page.getByLabel(/listen port \(start\)/i).fill("32050");
    await page.getByLabel(/target host/i).fill("127.0.0.1");
    await page.getByLabel(/target port \(start\)/i).fill("9000");

    const pushResponse = page.waitForResponse(
      (response) =>
        response.request().method() === "POST" &&
        response.url().endsWith("/v1/rules"),
    );
    await page.getByRole("button", { name: /^push rule$/i }).click();
    const response = await pushResponse;
    const pushBody: unknown = JSON.parse(response.request().postData() ?? "null");
    expect(pushBody).toMatchObject({
      client: "edge-st",
      listen_port: 32050,
      protocol: "tcp",
      target_host: "127.0.0.1",
      target_port: 9000,
    });
    expect(pushBody).not.toMatchObject({ targets: expect.anything() });
    expect(response.status()).toBe(422);
    await expect(page.getByText(/client_not_connected/i)).toBeVisible();
    await expect(page.getByText(/MT ×/)).toHaveCount(0);
  });
});
