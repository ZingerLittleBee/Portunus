import { test, expect } from "./fixtures/server";
import type { Page } from "@playwright/test";
import { loginAs, enrollClient } from "./fixtures/helpers";

async function expectNoPageOverflow(page: Page): Promise<void> {
  const offenders = await page.evaluate(() => {
    const viewportRight = document.documentElement.clientWidth;
    const isInsideHorizontalScroller = (element: HTMLElement): boolean => {
      let current = element.parentElement;
      while (current) {
        const style = getComputedStyle(current);
        if (
          (style.overflowX === "auto" || style.overflowX === "scroll") &&
          current.scrollWidth > current.clientWidth
        ) {
          return true;
        }
        current = current.parentElement;
      }
      return false;
    };

    return Array.from(document.body.querySelectorAll<HTMLElement>("body *"))
      .filter((element) => !element.closest('[aria-hidden="true"], [data-state="closed"]'))
      .filter((element) => !isInsideHorizontalScroller(element))
      .map((element) => {
        const rect = element.getBoundingClientRect();
        const style = getComputedStyle(element);
        return {
          tag: element.tagName.toLowerCase(),
          className: element.className,
          text: element.innerText?.slice(0, 80) ?? "",
          display: style.display,
          visibility: style.visibility,
          left: Math.round(rect.left),
          right: Math.round(rect.right),
          width: Math.round(rect.width),
        };
      })
      .filter(
        (box) =>
          box.display !== "none" &&
          box.visibility !== "hidden" &&
          box.width > 0 &&
          (box.left < -1 || box.right > viewportRight + 1),
      );
  });
  expect(offenders).toEqual([]);
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

  await enrollClient(request, server.httpUrl, server.superadminToken, "edge-mobile-01");

  await page.goto("/users/new");
  await page.getByLabel(/^id$/i).fill("mobile-user");
  await page.getByLabel(/display name/i).fill("Mobile User");
  await page.getByRole("button", { name: /create user/i }).click();
  await expect(page).toHaveURL(/\/users\/mobile-user/);

  await page.getByRole("button", { name: /add quota/i }).click();
  await expect(page.locator("form")).toBeVisible();
  await expectNoPageOverflow(page);
});
