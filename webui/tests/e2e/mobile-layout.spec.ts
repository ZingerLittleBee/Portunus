import { test, expect } from "./fixtures/server";
import { loginAs, api } from "./fixtures/helpers";

async function expectNoPageOverflow(page: import("@playwright/test").Page): Promise<void> {
  const overflow = await page.evaluate(() => {
    const root = document.documentElement;
    return root.scrollWidth - window.innerWidth;
  });
  expect(overflow).toBeLessThanOrEqual(1);
}

test("mobile shell navigation closes and dense pages stay contained", async ({ page, request, server }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await loginAs(page, server.superadminUserId, server.superadminPassword);
  await expectNoPageOverflow(page);

  await page.getByRole("button", { name: /toggle sidebar/i }).click();
  await page.getByRole("link", { name: /clients/i }).click();
  await expect(page.getByRole("heading", { name: /clients/i })).toBeVisible();
  await expect(page.getByRole("dialog", { name: /sidebar/i })).toBeHidden();
  await expectNoPageOverflow(page);

  await api(request, server.httpUrl, server.superadminToken, "/v1/clients", {
    method: "POST",
    body: { name: "edge-mobile-01", address: "127.0.0.1" },
  });

  await page.goto("/users/new");
  await page.getByLabel(/^id$/i).fill("mobile-user");
  await page.getByLabel(/display name/i).fill("Mobile User");
  await page.getByRole("button", { name: /create user/i }).click();
  await expect(page).toHaveURL(/\/users\/mobile-user/);

  await page.getByRole("button", { name: /add quota/i }).click();
  await expect(page.locator("form")).toBeVisible();
  await expectNoPageOverflow(page);
});
