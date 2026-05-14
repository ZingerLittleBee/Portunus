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
  // owner_id in OwnerRateLimitView is the same user namespace as user_id in GrantView.
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
    const sorted = [...gs].sort(
      (a, b) =>
        rangeWidth(b) - rangeWidth(a) ||
        b.created_at.localeCompare(a.created_at),
    );
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

export interface UseAccessEntriesResult {
  data: AccessEntry[] | undefined;
  isLoading: boolean;
  error: unknown;
}

export function useAccessEntries(userId: string): UseAccessEntriesResult {
  const grantsQ = useQuery({
    queryKey: userAccessEntriesKey(userId),
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

export interface CreateAccessEntryInput {
  user_id: string;
  client_name: string;
  listen_port_start: number;
  listen_port_end: number;
  protocols: ("tcp" | "udp")[];
  cap?: RateLimit;
}

export interface AccessEntryError extends Error {
  stage: "grant" | "cap" | "rollback";
  recoverable: boolean;
}

function makeError(
  stage: "grant" | "cap" | "rollback",
  cause: unknown,
  recoverable: boolean,
): AccessEntryError {
  const msg = cause instanceof Error ? cause.message : String(cause);
  const err = new Error(`[${stage}] ${msg}`) as AccessEntryError;
  err.stage = stage;
  err.recoverable = recoverable;
  return err;
}

export function useCreateAccessEntry(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (input: CreateAccessEntryInput): Promise<AccessEntry> => {
      const grantBody: CreateGrantBody = {
        user_id: input.user_id,
        client: input.client_name,
        listen_port_start: input.listen_port_start,
        listen_port_end: input.listen_port_end,
        protocols: input.protocols,
      };
      let grant: GrantView;
      try {
        grant = await apiFetch<GrantView>("/v1/grants", {
          method: "POST",
          body: JSON.stringify(grantBody),
        });
      } catch (err) {
        throw makeError("grant", err, false);
      }

      if (input.cap) {
        try {
          await apiFetch<OwnerRateLimitView>(
            `/v1/clients/${encodeURIComponent(input.client_name)}/owners/${encodeURIComponent(input.user_id)}/rate-limit`,
            { method: "PUT", body: JSON.stringify(input.cap) },
          );
        } catch (err) {
          // Compensation: roll back the grant we just made.
          try {
            await apiFetch<DeleteGrantResponse>(
              `/v1/grants/${encodeURIComponent(grant.grant_id)}`,
              { method: "DELETE" },
            );
            throw makeError("cap", err, true);
          } catch (rollbackErr) {
            if ((rollbackErr as AccessEntryError).stage === "cap") throw rollbackErr;
            throw makeError("rollback", rollbackErr, false);
          }
        }
      }

      return {
        grant_id: grant.grant_id,
        user_id: grant.user_id,
        client_name: grant.client,
        listen_port_start: grant.listen_port_start,
        listen_port_end: grant.listen_port_end,
        protocols: grant.protocols,
        unlimited: !input.cap,
        ...(input.cap ? { cap: input.cap } : {}),
      };
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: userAccessEntriesKey(userId) });
      void qc.invalidateQueries({ queryKey: ["grants"] });
      void qc.invalidateQueries({ queryKey: ["users"] });
    },
  });
}

export interface UpdateAccessEntryInput {
  user_id: string;
  client_name: string;
  /// The current grant id; replaced if port range or protocols change.
  grant_id: string;
  /// Old fields (to detect whether we need to delete+recreate the grant).
  old: Pick<AccessEntry, "listen_port_start" | "listen_port_end" | "protocols">;
  /// New fields.
  listen_port_start: number;
  listen_port_end: number;
  protocols: ("tcp" | "udp")[];
  cap?: RateLimit;
  /// Optional: if the backend already had multiple grants for this
  /// (user, client), they will be deleted as part of normalization.
  legacy_duplicate_ids?: string[];
}

function grantShapeChanged(input: UpdateAccessEntryInput): boolean {
  return (
    input.old.listen_port_start !== input.listen_port_start ||
    input.old.listen_port_end !== input.listen_port_end ||
    input.old.protocols.length !== input.protocols.length ||
    input.old.protocols.some((p) => !input.protocols.includes(p))
  );
}

export function useUpdateAccessEntry(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (input: UpdateAccessEntryInput): Promise<void> => {
      const duplicates = input.legacy_duplicate_ids ?? [];
      const reshape = grantShapeChanged(input) || duplicates.length > 0;

      if (reshape) {
        // Delete primary + duplicates, then create one merged grant.
        try {
          for (const id of [input.grant_id, ...duplicates]) {
            await apiFetch<DeleteGrantResponse>(
              `/v1/grants/${encodeURIComponent(id)}`,
              { method: "DELETE" },
            );
          }
        } catch (err) {
          throw makeError("grant", err, false);
        }
        try {
          await apiFetch<GrantView>("/v1/grants", {
            method: "POST",
            body: JSON.stringify({
              user_id: input.user_id,
              client: input.client_name,
              listen_port_start: input.listen_port_start,
              listen_port_end: input.listen_port_end,
              protocols: input.protocols,
            } satisfies CreateGrantBody),
          });
        } catch (err) {
          throw makeError("grant", err, false);
        }
      }

      // Cap: PUT if non-empty, DELETE if cap=undefined (unlimited).
      const capUrl = `/v1/clients/${encodeURIComponent(input.client_name)}/owners/${encodeURIComponent(input.user_id)}/rate-limit`;
      try {
        if (input.cap) {
          await apiFetch<OwnerRateLimitView>(capUrl, {
            method: "PUT",
            body: JSON.stringify(input.cap),
          });
        } else {
          try {
            await apiFetch<void>(capUrl, { method: "DELETE" });
          } catch (err) {
            if (!(err instanceof ApiError && err.status === 404)) throw err;
          }
        }
      } catch (err) {
        throw makeError("cap", err, true);
      }
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: userAccessEntriesKey(userId) });
      void qc.invalidateQueries({ queryKey: ["grants"] });
    },
  });
}

export function useDeleteAccessEntry(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async (input: {
      grant_id: string;
      client_name: string;
      user_id: string;
      legacy_duplicate_ids?: string[];
    }): Promise<void> => {
      const capUrl = `/v1/clients/${encodeURIComponent(input.client_name)}/owners/${encodeURIComponent(input.user_id)}/rate-limit`;
      try {
        await apiFetch<void>(capUrl, { method: "DELETE" });
      } catch (err) {
        if (!(err instanceof ApiError && err.status === 404)) {
          throw makeError("cap", err, true);
        }
      }
      try {
        for (const id of [input.grant_id, ...(input.legacy_duplicate_ids ?? [])]) {
          await apiFetch<DeleteGrantResponse>(
            `/v1/grants/${encodeURIComponent(id)}`,
            { method: "DELETE" },
          );
        }
      } catch (err) {
        throw makeError("grant", err, false);
      }
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: userAccessEntriesKey(userId) });
      void qc.invalidateQueries({ queryKey: ["grants"] });
      void qc.invalidateQueries({ queryKey: ["users"] });
    },
  });
}
