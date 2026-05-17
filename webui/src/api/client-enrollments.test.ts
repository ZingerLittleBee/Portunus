import { describe, expect, it, beforeEach, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { createElement } from "react";

import { useCreateClientEnrollment, useCreateClientReEnrollment } from "./clients";

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

describe("useCreateClientEnrollment", () => {
  it("requests an enrollment command without issuing a credential bundle", async () => {
    fetchMock.mockResolvedValueOnce(
      jsonRes(
        {
          client_name: "edge-01",
          expires_at: "2026-05-17T12:10:00Z",
          command: "portunus-client enroll 'portunus://control.example.com:7443/enroll?code=x'",
        },
        201,
      ),
    );

    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useCreateClientEnrollment(), {
      wrapper: wrapper(qc),
    });

    result.current.mutate({
      name: "edge-01",
      address: "edge.example.com",
      ttl_secs: 900,
    });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0]?.[0]).toContain("/v1/client-enrollments");
    expect(fetchMock.mock.calls[0]?.[1]).toMatchObject({ method: "POST" });
    expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({
      name: "edge-01",
      address: "edge.example.com",
      ttl_secs: 900,
    });
    expect(result.current.data?.command).toContain("portunus-client enroll");
  });
});

describe("useCreateClientReEnrollment", () => {
  it("requests an enrollment command for an existing client", async () => {
    fetchMock.mockResolvedValueOnce(
      jsonRes(
        {
          client_name: "edge-01",
          expires_at: "2026-05-17T12:10:00Z",
          command: "portunus-client enroll 'portunus://control.example.com:7443/enroll?code=x'",
        },
        201,
      ),
    );

    const qc = new QueryClient({ defaultOptions: { queries: { retry: false } } });
    const { result } = renderHook(() => useCreateClientReEnrollment(), {
      wrapper: wrapper(qc),
    });

    result.current.mutate({ name: "edge-01", ttl_secs: 900 });

    await waitFor(() => expect(result.current.isSuccess).toBe(true));

    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0]?.[0]).toContain("/v1/clients/edge-01/enrollment");
    expect(fetchMock.mock.calls[0]?.[1]).toMatchObject({ method: "POST" });
    expect(JSON.parse(String(fetchMock.mock.calls[0]?.[1]?.body))).toEqual({
      ttl_secs: 900,
    });
    expect(result.current.data?.command).toContain("portunus-client enroll");
  });
});
