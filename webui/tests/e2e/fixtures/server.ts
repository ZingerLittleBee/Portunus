import { test as base, expect } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const SUPERADMIN_TOKEN = "test-superadmin-43chars-aaaaaaaaaaaaaaaaaaaa";
const HTTP_PORT = 47080;
const GRPC_PORT = 47443;
const METRICS_PORT = 47081;

interface ServerHandle {
  proc: ChildProcess;
  configDir: string;
  superadminToken: string;
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
  throw new Error(`forward-server did not come up at ${url}: ${String(lastErr)}`);
}

function spawnServer(configDir: string): ChildProcess {
  const bin =
    process.env.FORWARD_SERVER_BIN ?? join(process.cwd(), "..", "target", "release", "forward-server");
  const proc = spawn(bin, ["--config-dir", configDir, "serve"], {
    env: { ...process.env, RUST_LOG: "info" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  proc.stdout?.on("data", (b: Buffer) => process.stdout.write(`[server] ${b.toString()}`));
  proc.stderr?.on("data", (b: Buffer) => process.stderr.write(`[server] ${b.toString()}`));
  return proc;
}

function writeServerToml(configDir: string): void {
  const toml = `
control_listen = "127.0.0.1:${GRPC_PORT}"
operator_http_listen = "127.0.0.1:${HTTP_PORT}"
metrics_listen = "127.0.0.1:${METRICS_PORT}"
tls_cert_path = "${join(configDir, "server.crt")}"
tls_key_path = "${join(configDir, "server.key")}"
token_store_path = "${join(configDir, "tokens.json")}"
operator_store_path = "${join(configDir, "identity.json")}"
operator_token = "${SUPERADMIN_TOKEN}"
log_format = "compact"
`;
  writeFileSync(join(configDir, "server.toml"), toml.trimStart());
}

export const test = base.extend<{ server: ServerHandle }>({
  server: async ({}, use) => {
    const configDir = mkdtempSync(join(tmpdir(), "forward-e2e-"));
    writeServerToml(configDir);
    const proc = spawnServer(configDir);
    const httpUrl = `http://127.0.0.1:${HTTP_PORT}`;
    try {
      await waitForListener(`${httpUrl}/v1/users/me`);
      await use({ proc, configDir, superadminToken: SUPERADMIN_TOKEN, httpUrl });
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
