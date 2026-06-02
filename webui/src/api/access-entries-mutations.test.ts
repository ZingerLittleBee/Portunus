import { describe, expect, it, beforeEach, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import { useCreateAccessEntry, useDeleteAccessEntry } from "./access-entries";
import type { ReactNode } from "react";
import { createElement } from "react";

function wrapper(qc: QueryClient) {
  return ({ children }: { children: ReactNode }) =>
    createElement(QueryClientProvider, { client: qc }, children);
}

const fetchMock = vi.fn();
beforeEach(() => {
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});

function jsonRes(body: unknown, init: number | ResponseInit = 200): Response {
  const ri = typeof init === "number" ? { status: init } : init;
  return new Response(JSON.stringify(body), {
    ...ri,
    headers: { "content-type": "application/json" },
  });
}

describe("useCreateAccessEntry", () => {
  it("creates grant + cap on happy path", async () => {
    fetchMock
      .mockResolvedValueOnce(
        jsonRes({
          grant_id: "g1",
          user_id: "alice",
          client: "edge",
          listen_port_start: 1000,
          listen_port_end: 2000,
          protocols: ["tcp"],
          note: null,
          created_at: "x",
        }),
      )
      .mockResolvedValueOnce(
        jsonRes({
          client_name: "edge",
          owner_id: "alice",
          rate_limit: { bandwidth_in_bps: 1000 },
          updated_at_unix_ms: 0,
        }),
      );

    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useCreateAccessEntry("alice"), { wrapper: wrapper(qc) });

    result.current.mutate({
      user_id: "alice",
      client_id: "01JCLIENTEDGE000000000000",
      client_name: "edge",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: ["tcp"],
      cap: { bandwidth_in_bps: 1000 },
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[0]?.[0]).toContain("/v1/grants");
    expect(fetchMock.mock.calls[1]?.[0]).toContain("/rate-limit");
  });

  it("rolls back grant when cap PUT fails", async () => {
    fetchMock
      .mockResolvedValueOnce(
        jsonRes({
          grant_id: "g1",
          user_id: "alice",
          client: "edge",
          listen_port_start: 1000,
          listen_port_end: 2000,
          protocols: ["tcp"],
          note: null,
          created_at: "x",
        }),
      )
      .mockResolvedValueOnce(jsonRes({ error: { code: "x", message: "boom" } }, 500))
      .mockResolvedValueOnce(jsonRes({ grant_id: "g1" })); // DELETE rollback ok

    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useCreateAccessEntry("alice"), { wrapper: wrapper(qc) });
    result.current.mutate({
      user_id: "alice",
      client_id: "01JCLIENTEDGE000000000000",
      client_name: "edge",
      listen_port_start: 1000,
      listen_port_end: 2000,
      protocols: ["tcp"],
      cap: { bandwidth_in_bps: 1000 },
    });

    await waitFor(() => expect(result.current.isError).toBe(true));
    expect(fetchMock).toHaveBeenCalledTimes(3); // POST grant + PUT cap + DELETE rollback
    expect(fetchMock.mock.calls[2]?.[1]?.method).toBe("DELETE");
  });
});

describe("useDeleteAccessEntry", () => {
  it("deletes cap (404 ignored) then grant", async () => {
    fetchMock
      .mockResolvedValueOnce(jsonRes({ error: { code: "not_found", message: "" } }, 404))
      .mockResolvedValueOnce(jsonRes({ grant_id: "g1" }));

    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useDeleteAccessEntry("alice"), { wrapper: wrapper(qc) });
    result.current.mutate({ grant_id: "g1", user_id: "alice", client_id: "01JCLIENTEDGE000000000000" });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});
