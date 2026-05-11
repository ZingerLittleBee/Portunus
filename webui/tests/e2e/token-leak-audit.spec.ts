// T066 — automates § 9 of quickstart.md (SC-006).
// Asserts:
//   (a) sessionStorage does not hold a legacy bearer login token
//   (b) localStorage carries only theme + lang preferences
//   (c) no DOM text contains the Web password or API token
//   (d) browser API requests never use URL or bearer-token storage
//   (e) the browser session is held in the HttpOnly operator cookie.

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

  await loginAs(page, server.superadminUserId, server.superadminPassword);
  // Drive a few read-only pages so we capture varied request paths.
  await page.goto("/users");
  await page.goto("/rules");
  await page.goto("/clients");
  await page.goto("/audit");
  await page.waitForLoadState("networkidle");

  // (a) sessionStorage does not contain the legacy bearer-login token.
  const sessTok = await page.evaluate(() => window.sessionStorage.getItem("portunus.token"));
  expect(sessTok).toBeNull();

  // (b) localStorage holds only theme + lang.
  const localKeys = await page.evaluate(() => Object.keys(window.localStorage).sort());
  for (const k of localKeys) {
    expect(["portunus.theme", "portunus.lang"]).toContain(k);
  }

  // (c) No DOM text leaks browser password or API token.
  const bodyText = await page.evaluate(() => document.body.innerText);
  expect(bodyText).not.toContain(server.superadminPassword);
  expect(bodyText).not.toContain(server.superadminToken);

  // (d) Browser traffic talks to /v1 without URL or bearer-token storage.
  const apiCalls = seenRequests.filter((r) => /\/v1\/|\/metrics/.test(new URL(r.url).pathname));
  expect(apiCalls.length).toBeGreaterThan(0);
  for (const r of apiCalls) {
    expect(r.url).not.toContain(server.superadminToken);
    expect(r.url).not.toContain(server.superadminPassword);
    expect(r.cookie ?? "").not.toContain(server.superadminToken);
    expect(r.cookie ?? "").not.toContain(server.superadminPassword);
    expect(r.auth).toBeUndefined();
  }

  // (e) Playwright does not expose the browser-added Cookie header through
  // request.headers(), so assert the cookie jar directly.
  const sessionCookie = (await page.context().cookies(server.httpUrl)).find(
    (cookie) => cookie.name === "portunus_session",
  );
  expect(sessionCookie).toBeDefined();
  expect(sessionCookie?.httpOnly).toBe(true);
  expect(sessionCookie?.sameSite).toBe("Lax");
  expect(sessionCookie?.value).not.toContain(server.superadminToken);
  expect(sessionCookie?.value).not.toContain(server.superadminPassword);
});
