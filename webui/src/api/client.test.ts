import { afterEach, describe, expect, it, vi } from "vitest";

import { apiFetch, apiFetchText, streamSse } from "@/api/client";

afterEach(() => {
  window.sessionStorage.clear();
  vi.unstubAllGlobals();
});

describe("api client auth transport", () => {
  it("sends same-origin credentials and csrf header for JSON writes", async () => {
    window.sessionStorage.setItem("portunus.token", "legacy-token");
    const fetchMock = vi.fn().mockResolvedValue(new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);

    await apiFetch("/v1/users", { method: "POST", body: JSON.stringify({}) });

    const init = fetchMock.mock.calls[0]?.[1];
    expect(init?.credentials).toBe("same-origin");
    const headers = new Headers(init?.headers);
    expect(headers.get("X-Portunus-CSRF")).toBe("1");
    expect(headers.get("Authorization")).toBeNull();
    expect(headers.get("Content-Type")).toBe("application/json");
  });

  it("does not add csrf to reads", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response("[]", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);

    await apiFetch("/v1/users");

    const init = fetchMock.mock.calls[0]?.[1];
    expect(init?.credentials).toBe("same-origin");
    const headers = new Headers(init?.headers);
    expect(headers.get("X-Portunus-CSRF")).toBeNull();
  });

  it("uses same-origin credentials and csrf for text writes", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response("", { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);

    await apiFetchText("/v1/metrics", { method: "DELETE" });

    const init = fetchMock.mock.calls[0]?.[1];
    expect(init?.credentials).toBe("same-origin");
    const headers = new Headers(init?.headers);
    expect(headers.get("Accept")).toBe("text/plain");
    expect(headers.get("X-Portunus-CSRF")).toBe("1");
  });

  it("streams SSE with same-origin cookies and no bearer header", () => {
    window.sessionStorage.setItem("portunus.token", "legacy-token");
    const fetchMock = vi.fn().mockReturnValue(new Promise<Response>(() => undefined));
    vi.stubGlobal("fetch", fetchMock);

    const handle = streamSse("/v1/rules/1/stats/stream", () => undefined);
    handle.close();

    const init = fetchMock.mock.calls[0]?.[1];
    expect(init?.credentials).toBe("same-origin");
    const headers = new Headers(init?.headers);
    expect(headers.get("Accept")).toBe("text/event-stream");
    expect(headers.get("Authorization")).toBeNull();
  });
});
