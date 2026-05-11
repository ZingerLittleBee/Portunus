/// Typed fetch wrapper used by every TanStack Query hook.
///
/// - Sends same-origin cookies for local password sessions.
/// - Throws a tagged `ApiError` on non-2xx so hooks can branch on status.
/// - Emits a global `auth:unauthorized` event on 401 so the AuthGate can
///   clear the cache and bounce back to the login screen.

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

export const UNAUTHORIZED_EVENT = "auth:unauthorized";

interface ServerErrorBody {
  error?: { code?: string; message?: string };
}

function isStateChangingMethod(method: string): boolean {
  return method === "POST" || method === "PUT" || method === "PATCH" || method === "DELETE";
}

function csrfAwareHeaders(init: RequestInit, accept: string): Headers {
  const headers = new Headers(init.headers);
  if (init.body != null && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }
  const method = (init.method ?? "GET").toUpperCase();
  if (isStateChangingMethod(method) && !headers.has("X-Portunus-CSRF")) {
    headers.set("X-Portunus-CSRF", "1");
  }
  headers.set("Accept", accept);
  return headers;
}

export async function apiFetch<T>(path: string, init: RequestInit = {}): Promise<T> {
  const headers = csrfAwareHeaders(init, "application/json");

  const res = await fetch(path, { ...init, headers, credentials: "same-origin" });

  if (res.status === 401) {
    window.dispatchEvent(new CustomEvent(UNAUTHORIZED_EVENT));
  }

  if (res.status === 204) return undefined as T;

  const text = await res.text();
  let body: unknown = null;
  if (text.length > 0) {
    try {
      body = JSON.parse(text);
    } catch {
      // non-JSON; surface the raw text below.
    }
  }

  if (!res.ok) {
    const err = (body as ServerErrorBody | null)?.error;
    throw new ApiError(
      res.status,
      err?.code ?? `http_${res.status}`,
      err?.message ?? (text || res.statusText),
    );
  }

  return body as T;
}

/// Helper for endpoints returning raw `text/plain` (e.g. `/metrics`).
export async function apiFetchText(path: string, init: RequestInit = {}): Promise<string> {
  const headers = csrfAwareHeaders(init, "text/plain");

  const res = await fetch(path, { ...init, headers, credentials: "same-origin" });
  if (res.status === 401) window.dispatchEvent(new CustomEvent(UNAUTHORIZED_EVENT));
  if (!res.ok) {
    throw new ApiError(res.status, `http_${res.status}`, res.statusText);
  }
  return res.text();
}

/// Native EventSource has limited header/control support, so use a
/// manual ReadableStream-based reader. Browser cookies flow through
/// same-origin fetch and auth failures still emit the global 401 event.
///
/// Returns a `close()` function the caller invokes to cancel the
/// stream. Reconnects with exponential backoff (1s → 30s) on transport
/// errors. Per-event `onEvent` is called for `event: stats` payloads.

export interface SseHandle {
  close(): void;
}

interface SseOptions {
  signal?: AbortSignal;
  onOpen?: () => void;
  onError?: (err: unknown) => void;
}

const SSE_BACKOFF_INITIAL_MS = 1_000;
const SSE_BACKOFF_MAX_MS = 30_000;

export function streamSse<T>(
  path: string,
  onEvent: (data: T) => void,
  options: SseOptions = {},
): SseHandle {
  let aborted = false;
  let backoff = SSE_BACKOFF_INITIAL_MS;
  let pending: AbortController | null = null;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  function scheduleReconnect() {
    if (aborted) return;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      backoff = Math.min(backoff * 2, SSE_BACKOFF_MAX_MS);
      void connect();
    }, backoff);
  }

  async function connect() {
    if (aborted) return;
    pending = new AbortController();
    const headers = new Headers();
    headers.set("Accept", "text/event-stream");

    let res: Response;
    try {
      res = await fetch(path, {
        method: "GET",
        headers,
        signal: pending.signal,
        credentials: "same-origin",
      });
    } catch (err) {
      options.onError?.(err);
      scheduleReconnect();
      return;
    }

    if (res.status === 401) {
      window.dispatchEvent(new CustomEvent(UNAUTHORIZED_EVENT));
      aborted = true;
      return;
    }
    if (!res.ok || !res.body) {
      options.onError?.(new ApiError(res.status, `http_${res.status}`, res.statusText));
      scheduleReconnect();
      return;
    }

    options.onOpen?.();
    backoff = SSE_BACKOFF_INITIAL_MS;

    const reader = res.body.getReader();
    const decoder = new TextDecoder("utf-8");
    let buffer = "";

    try {
      // Server-Sent-Events frame parsing per WHATWG spec: events are
      // separated by `\n\n`; lines starting with `:` are comments.
      // We only care about `event: stats` + `data: ...` for this use.
      while (!aborted) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let sepIdx;
        while ((sepIdx = buffer.indexOf("\n\n")) !== -1) {
          const frame = buffer.slice(0, sepIdx);
          buffer = buffer.slice(sepIdx + 2);
          let eventType = "message";
          let data = "";
          for (const line of frame.split("\n")) {
            if (line.startsWith(":")) continue;
            if (line.startsWith("event:")) eventType = line.slice(6).trim();
            else if (line.startsWith("data:")) {
              data += (data ? "\n" : "") + line.slice(5).trim();
            }
          }
          if (eventType === "stats" && data) {
            try {
              onEvent(JSON.parse(data) as T);
            } catch (err) {
              options.onError?.(err);
            }
          }
        }
      }
    } catch (err) {
      if (!aborted) options.onError?.(err);
    } finally {
      reader.cancel().catch(() => undefined);
      if (!aborted) scheduleReconnect();
    }
  }

  void connect();

  return {
    close() {
      aborted = true;
      if (reconnectTimer) clearTimeout(reconnectTimer);
      pending?.abort();
    },
  };
}
