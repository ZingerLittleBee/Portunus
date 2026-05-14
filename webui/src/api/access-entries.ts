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

// Forward references for Tasks 4 & 5 — will be used then; keep alive now.
export type { CreateGrantBody, DeleteGrantResponse };

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

// Keep value imports live for Tasks 4 and 5 — removed then.
void apiFetch;
void ApiError;
void useMutation;
void useQuery;
void useQueries;
void useQueryClient;
