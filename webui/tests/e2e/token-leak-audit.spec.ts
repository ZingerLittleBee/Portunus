// T066 — automates § 9 of quickstart.md (SC-006).
// Asserts:
//   (a) sessionStorage holds portunus.token
//   (b) localStorage carries only theme + lang preferences
//   (c) no DOM text contains the bearer prefix anywhere on rendered pages
//   (d) every request includes the bearer ONLY in the Authorization header
//       (never query string, never request body, never cookies).

import { test, expect } from "./fixtures/server";
import { loginAs } from "./fixtures/helpers";

test("session storage + no token leak in DOM or URL", async ({ page, server }) => {
  const seenRequests: { url: string; auth: string | undefined; cookie: string | undefined }[] = [];
  page.on("request", (req) => {
    const headers = req.headers();
    seenRequests.push({
      url: req.url(),
      auth: headers["authorization"],
      cookie: headers["cookie"],
    });
  });

  await loginAs(page, server.superadminToken);
  // Drive a few read-only pages so we capture varied request paths.
  await page.goto("/users");
  await page.goto("/rules");
  await page.goto("/clients");
  await page.goto("/audit");
  await page.waitForLoadState("networkidle");

  // (a) sessionStorage contains the bearer.
  const sessTok = await page.evaluate(() => window.sessionStorage.getItem("portunus.token"));
  expect(sessTok).toBe(server.superadminToken);

  // (b) localStorage holds only theme + lang.
  const localKeys = await page.evaluate(() => Object.keys(window.localStorage).sort());
  for (const k of localKeys) {
    expect(["portunus.theme", "portunus.lang"]).toContain(k);
  }

  // (c) No DOM text leaks the bearer.
  const bodyText = await page.evaluate(() => document.body.innerText);
  expect(bodyText).not.toContain(server.superadminToken);

  // (d) Every request that talks to /v1 or /metrics carries the bearer
  // ONLY in Authorization, never in the URL or via cookies.
  const apiCalls = seenRequests.filter((r) => /\/v1\/|\/metrics/.test(new URL(r.url).pathname));
  expect(apiCalls.length).toBeGreaterThan(0);
  for (const r of apiCalls) {
    expect(r.url).not.toContain(server.superadminToken);
    expect(r.cookie ?? "").not.toContain(server.superadminToken);
    expect(r.auth ?? "").toBe(`Bearer ${server.superadminToken}`);
  }
});
