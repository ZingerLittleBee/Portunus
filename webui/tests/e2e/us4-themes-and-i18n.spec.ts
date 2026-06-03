// T057 — Themes + i18n round-trip (US4).
// Verifies:
//   1. Toggling theme to dark adds the `dark` class on <html> within 200 ms.
//   2. Toggling language to zh-CN updates visible text to Chinese.
//   3. Both preferences persist across reload.
//   4. With prefers-color-scheme: dark and theme=system, the page loads in dark.

import { test, expect } from "./fixtures/server";
import { loginAs } from "./fixtures/helpers";

test("theme toggle updates the dark class within 200 ms", async ({ page, server }) => {
  await loginAs(page, server.superadminUserId, server.superadminPassword);
  await page.goto("/settings");

  // Settings page renders three theme radios — light / dark / system.
  await page.getByRole("main").getByRole("button", { name: /^dark$/i }).click();
  // Read the class without artificial delay; it propagates synchronously.
  const start = Date.now();
  await expect.poll(
    async () => page.evaluate(() => document.documentElement.classList.contains("dark")),
    { timeout: 200, intervals: [25] },
  ).toBe(true);
  expect(Date.now() - start).toBeLessThan(200);

  // localStorage written.
  const stored = await page.evaluate(() => window.localStorage.getItem("portunus.theme"));
  expect(stored).toBe("dark");
});

test("language toggle to zh-CN renders Chinese strings", async ({ page, server }) => {
  await loginAs(page, server.superadminUserId, server.superadminPassword);
  await page.goto("/settings");

  await page.getByRole("main").getByRole("button", { name: "中文" }).click();
  // The Settings page itself flips: 设置 is the zh-CN settings.title.
  await expect(page.getByRole("heading", { level: 1, name: "设置" })).toBeVisible();

  const stored = await page.evaluate(() => window.localStorage.getItem("portunus.lang"));
  expect(stored).toBe("zh-CN");
});

test("preferences persist across reload", async ({ page, server }) => {
  await loginAs(page, server.superadminUserId, server.superadminPassword);
  await page.goto("/settings");
  await page.getByRole("main").getByRole("button", { name: /^dark$/i }).click();
  await page.getByRole("main").getByRole("button", { name: "中文" }).click();
  // Wait for both writes to land before reloading.
  await expect(page.getByRole("heading", { level: 1, name: "设置" })).toBeVisible();
  await page.reload();

  expect(await page.evaluate(() => document.documentElement.classList.contains("dark"))).toBe(true);
  expect(await page.evaluate(() => window.localStorage.getItem("portunus.lang"))).toBe("zh-CN");
});

test("prefers-color-scheme: dark with theme=system picks dark", async ({ browser, server }) => {
  const ctx = await browser.newContext({ colorScheme: "dark" });
  const page = await ctx.newPage();
  await loginAs(page, server.superadminUserId, server.superadminPassword);
  // Default theme is "system" on first visit; prefers-color-scheme=dark
  // resolves to dark without manual toggling.
  expect(await page.evaluate(() => document.documentElement.classList.contains("dark"))).toBe(true);
  await ctx.close();
});
