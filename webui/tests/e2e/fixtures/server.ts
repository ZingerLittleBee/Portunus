import { test as base, expect } from "@playwright/test";
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const BOOTSTRAP_USER_ID = "_superadmin";
const WEB_SUPERADMIN_USER_ID = "superadmin";
const SUPERADMIN_PASSWORD = "correct horse battery staple";
const HTTP_PORT = 47080;
const GRPC_PORT = 47443;
const METRICS_PORT = 47081;

const dataDir = (configDir: string): string => join(configDir, "state");

interface ServerHandle {
  proc: ChildProcess;
  configDir: string;
  superadminToken: string;
  superadminUserId: string;
  superadminPassword: string;
  httpUrl: string;
}

async function waitForListener(url: string, timeoutMs = 10_000): Promise<void> {
  const start = Date.now();
  let lastErr: unknown;
  while (Date.now() - start < timeoutMs) {
    try {
      const res = await fetch(url, { method: "GET" });
      // Any HTTP response (even 401) means the listener is live.
      if (res.status > 0) return;
    } catch (err) {
      lastErr = err;
    }
    await sleep(150);
  }
  throw new Error(`portunus-server did not come up at ${url}: ${String(lastErr)}`);
}

function serverBin(): string {
  return process.env.PORTUNUS_SERVER_BIN ?? join(process.cwd(), "..", "target", "release", "portunus-server");
}

function runServerCommand(configDir: string, args: string[], input?: string): string {
  const result = spawnSync(
    serverBin(),
    ["--data-dir", dataDir(configDir), ...args],
    {
      input,
      encoding: "utf8",
      env: { ...process.env, RUST_LOG: "warn" },
    },
  );
  if (result.status !== 0) {
    throw new Error(
      `portunus-server ${args.join(" ")} failed with ${result.status}\nstdout=${result.stdout}\nstderr=${result.stderr}`,
    );
  }
  return result.stdout;
}

function bootstrapSuperadmin(configDir: string): string {
  const stdout = runServerCommand(configDir, ["bootstrap-superadmin", "--name", "ops"]);
  const token = stdout.match(/token=(\S+)/)?.[1];
  if (!token) throw new Error(`bootstrap-superadmin did not print token: ${stdout}`);
  runServerCommand(
    configDir,
    ["reset-password", BOOTSTRAP_USER_ID, "--password-stdin"],
    `${SUPERADMIN_PASSWORD}\n`,
  );
  return token;
}

async function createWebSuperadmin(httpUrl: string, token: string): Promise<void> {
  const res = await fetch(`${httpUrl}/v1/users`, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({
      user_id: WEB_SUPERADMIN_USER_ID,
      display_name: "Web Superadmin",
      role: "superadmin",
      initial_password: SUPERADMIN_PASSWORD,
    }),
  });
  if (!res.ok) {
    throw new Error(`create web superadmin failed with ${res.status}: ${await res.text()}`);
  }
}

function spawnServer(configDir: string): ChildProcess {
  const proc = spawn(
    serverBin(),
    ["--data-dir", dataDir(configDir), "serve"],
    {
      env: { ...process.env, RUST_LOG: "info" },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  proc.stdout?.on("data", (b: Buffer) => process.stdout.write(`[server] ${b.toString()}`));
  proc.stderr?.on("data", (b: Buffer) => process.stderr.write(`[server] ${b.toString()}`));
  return proc;
}

function writeServerToml(configDir: string): void {
  mkdirSync(dataDir(configDir), { recursive: true });
  const toml = `
control_listen = "127.0.0.1:${GRPC_PORT}"
operator_http_listen = "127.0.0.1:${HTTP_PORT}"
metrics_listen = "127.0.0.1:${METRICS_PORT}"
tls_cert_path = "${join(configDir, "server.crt")}"
tls_key_path = "${join(configDir, "server.key")}"
token_store_path = "${join(configDir, "tokens.json")}"
operator_store_path = "${join(configDir, "identity.json")}"
log_format = "compact"
`;
  writeFileSync(join(dataDir(configDir), "server.toml"), toml.trimStart());
}

export const test = base.extend<{ server: ServerHandle }>({
  server: async ({}, use) => {
    const configDir = mkdtempSync(join(tmpdir(), "portunus-e2e-"));
    writeServerToml(configDir);
    const superadminToken = bootstrapSuperadmin(configDir);
    const proc = spawnServer(configDir);
    const httpUrl = `http://127.0.0.1:${HTTP_PORT}`;
    try {
      await waitForListener(`${httpUrl}/v1/users/me`);
      await createWebSuperadmin(httpUrl, superadminToken);
      await use({
        proc,
        configDir,
        superadminToken,
        superadminUserId: WEB_SUPERADMIN_USER_ID,
        superadminPassword: SUPERADMIN_PASSWORD,
        httpUrl,
      });
    } finally {
      proc.kill("SIGTERM");
      await new Promise<void>((resolve) => {
        proc.once("exit", () => resolve());
        setTimeout(() => {
          try {
            proc.kill("SIGKILL");
          } catch {
            /* already dead */
          }
          resolve();
        }, 2_000);
      });
      rmSync(configDir, { recursive: true, force: true });
    }
  },
});

export { expect };
