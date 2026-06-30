// 013-traffic-quotas v1.4.0 — quota CRUD + status hooks.
//
// 015-client-stable-id (US3): every {client} path segment is the stable
// client_id, not the mutable display name.
//
// Mirrors `crates/portunus-server/src/operator/quota_http.rs`:
//   GET    /v1/users/{user_id}/quotas
//   PUT    /v1/users/{user_id}/quotas/{client_id}
//   PATCH  /v1/users/{user_id}/quotas/{client_id}
//   DELETE /v1/users/{user_id}/quotas/{client_id}
//   GET    /v1/users/{user_id}/quotas/{client_id}/status
//   GET    /v1/clients/{client_id}/quotas
//
// Cache-invalidation: any mutation invalidates the user's quota list and the
// access-entries view (F2) so `quota` reads on AccessEntry refresh in lock-
// step with the rate-limit edits the rest of the form makes.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "@/api/client";
import type {
  MonthlyQuotaView,
  PatchQuotaInput,
} from "@/api/types";

export const userQuotasKey = (userId: string) =>
  ["user-quotas", userId] as const;
export const clientQuotasKey = (clientId: string) =>
  ["client-quotas", clientId] as const;

export function useUserQuotas(userId: string) {
  return useQuery({
    queryKey: userQuotasKey(userId),
    queryFn: () =>
      apiFetch<MonthlyQuotaView[]>(
        `/v1/users/${encodeURIComponent(userId)}/quotas`,
      ),
    enabled: userId.length > 0,
  });
}

export function useClientQuotas(clientId: string) {
  return useQuery({
    queryKey: clientQuotasKey(clientId),
    queryFn: () =>
      apiFetch<MonthlyQuotaView[]>(
        `/v1/clients/${encodeURIComponent(clientId)}/quotas`,
      ),
    enabled: clientId.length > 0,
  });
}

function invalidateAfterMutation(
  qc: ReturnType<typeof useQueryClient>,
  userId: string,
  clientId: string,
): void {
  qc.invalidateQueries({ queryKey: userQuotasKey(userId) });
  qc.invalidateQueries({ queryKey: clientQuotasKey(clientId) });
  qc.invalidateQueries({ queryKey: ["access-entries", userId] });
}

export interface PatchQuotaArgs {
  client_id: string;
  body: PatchQuotaInput;
}

export function usePatchQuota(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ client_id, body }: PatchQuotaArgs) =>
      apiFetch<MonthlyQuotaView>(
        `/v1/users/${encodeURIComponent(userId)}/quotas/${encodeURIComponent(client_id)}`,
        {
          method: "PATCH",
          body: JSON.stringify(body),
        },
      ),
    onSuccess: (_, vars) => invalidateAfterMutation(qc, userId, vars.client_id),
  });
}
