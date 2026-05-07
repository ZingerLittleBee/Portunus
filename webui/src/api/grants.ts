import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import type { CreateGrantBody, DeleteGrantResponse, GrantView } from "@/api/types";

export const GRANTS_KEY = ["grants"] as const;
export const grantsForUserKey = (userId: string) => ["grants", "user", userId] as const;

export function useGrantsList(userId?: string) {
  const path = userId
    ? `/v1/grants?user_id=${encodeURIComponent(userId)}`
    : "/v1/grants";
  return useQuery({
    queryKey: userId ? grantsForUserKey(userId) : GRANTS_KEY,
    queryFn: () => apiFetch<GrantView[]>(path),
    refetchInterval: 5_000,
  });
}

export function useCreateGrant() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateGrantBody) =>
      apiFetch<GrantView>("/v1/grants", {
        method: "POST",
        body: JSON.stringify(body),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["grants"] });
      void qc.invalidateQueries({ queryKey: ["users"] });
    },
  });
}

export function useRevokeGrant() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (grantId: string) =>
      apiFetch<DeleteGrantResponse>(`/v1/grants/${encodeURIComponent(grantId)}`, {
        method: "DELETE",
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["grants"] });
      void qc.invalidateQueries({ queryKey: ["rules"] });
    },
  });
}
