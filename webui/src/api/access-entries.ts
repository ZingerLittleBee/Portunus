// webui/src/api/access-entries.ts
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch, ApiError } from "@/api/client";
import type {
  CreateGrantBody,
  DeleteGrantResponse,
  GrantView,
  OwnerRateLimitView,
  RateLimit,
} from "@/api/types";

export interface AccessEntry {
  grant_id: string;
  user_id: string;
  client_name: string;
  listen_port_start: number;
  listen_port_end: number;
  protocols: ("tcp" | "udp")[];
  unlimited: boolean;
  cap?: RateLimit;
  /// Set when the backend has >1 grant for this (user, client).
  legacy_duplicates?: GrantView[];
}

function rangeWidth(g: GrantView): number {
  return g.listen_port_end - g.listen_port_start;
}

export function joinAccessEntries(
  grants: GrantView[],
  caps: OwnerRateLimitView[],
): AccessEntry[] {
  const capByPair = new Map<string, OwnerRateLimitView>();
  for (const c of caps) {
    capByPair.set(`${c.owner_id}::${c.client_name}`, c);
  }

  // Group grants by (user, client)
  const groups = new Map<string, GrantView[]>();
  for (const g of grants) {
    const k = `${g.user_id}::${g.client}`;
    const arr = groups.get(k) ?? [];
    arr.push(g);
    groups.set(k, arr);
  }

  const out: AccessEntry[] = [];
  for (const [, gs] of groups) {
    const sorted = [...gs].sort((a, b) => rangeWidth(b) - rangeWidth(a));
    const primary = sorted[0];
    if (!primary) continue;
    const capEntry = capByPair.get(`${primary.user_id}::${primary.client}`);
    const entry: AccessEntry = {
      grant_id: primary.grant_id,
      user_id: primary.user_id,
      client_name: primary.client,
      listen_port_start: primary.listen_port_start,
      listen_port_end: primary.listen_port_end,
      protocols: primary.protocols,
      unlimited: !capEntry,
      ...(capEntry !== undefined ? { cap: capEntry.rate_limit } : {}),
      ...(sorted.length > 1 ? { legacy_duplicates: sorted.slice(1) } : {}),
    };
    out.push(entry);
  }
  return out.sort((a, b) => a.client_name.localeCompare(b.client_name));
}


export const userAccessEntriesKey = (userId: string) =>
  ["access-entries", userId] as const;
export const userAccessCapKey = (userId: string, clientName: string) =>
  ["access-entries", userId, "cap", clientName] as const;

interface UseAccessEntriesResult {
  data: AccessEntry[] | undefined;
  isLoading: boolean;
  error: unknown;
}

export function useAccessEntries(userId: string): UseAccessEntriesResult {
  const grantsQ = useQuery({
    queryKey: ["grants", "user", userId],
    queryFn: () => apiFetch<GrantView[]>(`/v1/grants?user_id=${encodeURIComponent(userId)}`),
    enabled: userId.length > 0,
  });

  const grants = grantsQ.data ?? [];
  const uniquePairs = Array.from(
    new Set(grants.map((g) => `${g.user_id}::${g.client}`)),
  ).map((k) => {
    const [u, c] = k.split("::");
    return { user_id: u!, client_name: c! };
  });

  const capQueries = useQueries({
    queries: uniquePairs.map((p) => ({
      queryKey: userAccessCapKey(p.user_id, p.client_name),
      queryFn: async (): Promise<OwnerRateLimitView | null> => {
        try {
          return await apiFetch<OwnerRateLimitView>(
            `/v1/clients/${encodeURIComponent(p.client_name)}/owners/${encodeURIComponent(p.user_id)}/rate-limit`,
          );
        } catch (err) {
          if (err instanceof ApiError && err.status === 404) return null;
          throw err;
        }
      },
      enabled: userId.length > 0,
    })),
  });

  const capsLoading = capQueries.some((q) => q.isLoading);
  const caps = capQueries
    .map((q) => q.data)
    .filter((v): v is OwnerRateLimitView => v != null);
  const error = grantsQ.error ?? capQueries.find((q) => q.error)?.error;

  return {
    data: grantsQ.data ? joinAccessEntries(grants, caps) : undefined,
    isLoading: grantsQ.isLoading || (grants.length > 0 && capsLoading),
    error,
  };
}

// Keep value imports live for Task 5 — removed then.
void useMutation;
void useQueryClient;
export type { CreateGrantBody, DeleteGrantResponse };
