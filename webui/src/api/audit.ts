import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import type { AuditEntry } from "@/api/types";

export interface AuditQuery {
  limit?: number;
  outcome?: "allow" | "deny";
}

export const auditKey = (q: AuditQuery) => ["audit", q] as const;

export function useAuditLog(query: AuditQuery = {}) {
  const params = new URLSearchParams();
  if (query.limit !== undefined) params.set("limit", String(query.limit));
  if (query.outcome) params.set("outcome", query.outcome);
  const suffix = params.toString() ? `?${params.toString()}` : "";
  return useQuery({
    queryKey: auditKey(query),
    queryFn: () => apiFetch<AuditEntry[]>(`/v1/audit${suffix}`),
    refetchInterval: 5_000,
    staleTime: 2_500,
  });
}

// 008-sqlite-storage T077: envelope mode for historic scroll-back.
// The server responds with `{ entries, next_cursor?, count }` whenever
// any of `since` / `until` / `cursor` is present. v0.7 array-root path
// (above) is preserved for the live tail.

export interface AuditEnvelopeQuery {
  limit?: number;
  outcome?: "allow" | "deny";
  since?: string;
  until?: string;
  cursor?: string;
}

export interface AuditEnvelope {
  entries: AuditEntry[];
  next_cursor?: string;
  count: number;
}

export async function fetchAuditEnvelope(query: AuditEnvelopeQuery): Promise<AuditEnvelope> {
  const params = new URLSearchParams();
  if (query.limit !== undefined) params.set("limit", String(query.limit));
  if (query.outcome) params.set("outcome", query.outcome);
  if (query.since) params.set("since", query.since);
  if (query.until) params.set("until", query.until);
  if (query.cursor) params.set("cursor", query.cursor);
  const suffix = params.toString() ? `?${params.toString()}` : "";
  return apiFetch<AuditEnvelope>(`/v1/audit${suffix}`);
}
