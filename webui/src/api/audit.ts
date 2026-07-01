import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import type { AuditEntry } from "@/api/types";

export interface AuditQuery {
  limit?: number;
  outcome?: "allow" | "deny";
}

const auditKey = (q: AuditQuery) => ["audit", q] as const;

export function useAuditLog(query: AuditQuery = {}, options: { enabled?: boolean } = {}) {
  const enabled = options.enabled ?? true;
  const params = new URLSearchParams();
  if (query.limit !== undefined) params.set("limit", String(query.limit));
  if (query.outcome) params.set("outcome", query.outcome);
  const suffix = params.toString() ? `?${params.toString()}` : "";
  return useQuery({
    queryKey: auditKey(query),
    queryFn: () => apiFetch<AuditEntry[]>(`/v1/audit${suffix}`),
    // Live tail polls; disabled (history mode) stops both the query and
    // the 5s refetch so the view stays a frozen snapshot.
    enabled,
    refetchInterval: enabled ? 5_000 : false,
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
  const data = await apiFetch<AuditEnvelope | AuditEntry[]>(`/v1/audit${suffix}`);
  // The server returns a bare array (v0.7 shape) when none of
  // since/until/cursor is present, and the envelope otherwise. Normalize
  // to the envelope shape so callers never accidentally read
  // `Array.prototype.entries` off a bare array.
  if (Array.isArray(data)) {
    return { entries: data, count: data.length };
  }
  return data;
}
