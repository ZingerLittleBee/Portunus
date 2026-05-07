import { type Page, type APIRequestContext, expect } from "@playwright/test";

export async function loginAs(page: Page, token: string): Promise<void> {
  await page.goto("/");
  // The SPA bounces unauthenticated traffic to /login. The bearer input
  // is rendered with id="bearer".
  await page.waitForURL(/\/login/);
  await page.locator("#bearer").fill(token);
  await page.getByRole("button", { name: /sign in/i }).click();
  // Dashboard greeting renders once /v1/users/me resolves.
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
}

export async function api(
  request: APIRequestContext,
  baseURL: string,
  token: string,
  path: string,
  init: { method?: string; body?: unknown } = {},
): Promise<unknown> {
  const res = await request.fetch(`${baseURL}${path}`, {
    method: init.method ?? "GET",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    data: init.body !== undefined ? JSON.stringify(init.body) : undefined,
  });
  if (!res.ok() && res.status() !== 201) {
    throw new Error(`${init.method ?? "GET"} ${path} → ${res.status()}: ${await res.text()}`);
  }
  return res.status() === 204 ? null : res.json();
}

export async function provisionUserWithToken(
  request: APIRequestContext,
  baseURL: string,
  superadminToken: string,
  userId: string,
): Promise<{ userId: string; token: string; credentialId: string }> {
  await api(request, baseURL, superadminToken, "/v1/users", {
    method: "POST",
    body: { user_id: userId, display_name: userId },
  });
  const cred = (await api(
    request,
    baseURL,
    superadminToken,
    `/v1/users/${userId}/credentials`,
    { method: "POST", body: { label: "e2e" } },
  )) as { credential_id: string; token: string };
  return { userId, token: cred.token, credentialId: cred.credential_id };
}
