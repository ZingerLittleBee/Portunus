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
