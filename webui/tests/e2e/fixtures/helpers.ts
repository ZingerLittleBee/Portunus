import { type Page, type APIRequestContext, expect } from "@playwright/test";

export const userPassword = (userId: string): string => `${userId} correct horse battery staple`;

export async function loginAs(page: Page, userId: string, password: string): Promise<void> {
  await page.goto("/");
  await page.getByLabel(/user id/i).fill(userId);
  await page.getByLabel(/^password$/i).fill(password);
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
): Promise<{ userId: string; password: string; token: string; credentialId: string }> {
  const password = userPassword(userId);
  await api(request, baseURL, superadminToken, "/v1/users", {
    method: "POST",
    body: { user_id: userId, display_name: userId, initial_password: password },
  });
  const cred = (await api(
    request,
    baseURL,
    superadminToken,
    `/v1/users/${userId}/credentials`,
    { method: "POST", body: { label: "e2e" } },
  )) as { credential_id: string; token: string };
  return { userId, password, token: cred.token, credentialId: cred.credential_id };
}
