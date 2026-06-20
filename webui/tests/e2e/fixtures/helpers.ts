import { type Page, type APIRequestContext, expect } from "@playwright/test";
import { spawnSync } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

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

export async function provisionUser(
  request: APIRequestContext,
  baseURL: string,
  superadminToken: string,
  userId: string,
): Promise<{ userId: string; password: string }> {
  const password = userPassword(userId);
  await api(request, baseURL, superadminToken, "/v1/users", {
    method: "POST",
    body: { user_id: userId, display_name: userId, role: "user", initial_password: password },
  });
  return { userId, password };
}

function clientBin(): string {
  return process.env.PORTUNUS_CLIENT_BIN ?? join(process.cwd(), "..", "target", "release", "portunus-client");
}

export async function enrollClient(
  request: APIRequestContext,
  baseURL: string,
  superadminToken: string,
  name: string,
): Promise<string> {
  const enrollment = (await api(request, baseURL, superadminToken, "/v1/client-enrollments", {
    method: "POST",
    body: { name, address: "127.0.0.1" },
  })) as { command: string };
  const uri = enrollment.command.split("'")[1];
  if (!uri) throw new Error(`could not parse enrollment command: ${enrollment.command}`);
  const out = join(mkdtempSync(join(tmpdir(), "portunus-webui-client-")), `${name}.bundle.json`);
  const result = spawnSync(clientBin(), ["enroll", uri, "--out", out], {
    encoding: "utf8",
    env: { ...process.env, RUST_LOG: "warn" },
  });
  if (result.status !== 0) {
    throw new Error(
      `portunus-client enroll failed with ${result.status}\nstdout=${result.stdout}\nstderr=${result.stderr}`,
    );
  }
  return out;
}
