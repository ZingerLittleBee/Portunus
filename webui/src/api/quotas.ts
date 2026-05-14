// 013-traffic-quotas v1.4.0 — quota CRUD + status hooks.
//
// Mirrors `crates/portunus-server/src/operator/quota_http.rs`:
//   GET    /v1/users/{user_id}/quotas
//   PUT    /v1/users/{user_id}/quotas/{client_name}
//   PATCH  /v1/users/{user_id}/quotas/{client_name}
//   DELETE /v1/users/{user_id}/quotas/{client_name}
//   GET    /v1/users/{user_id}/quotas/{client_name}/status
//   GET    /v1/clients/{client_name}/quotas
//
// Cache-invalidation: any mutation invalidates the user's quota list and the
// access-entries view (F2) so `quota` reads on AccessEntry refresh in lock-
// step with the rate-limit edits the rest of the form makes.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { apiFetch } from "@/api/client";
import type {
  MonthlyQuotaView,
  PatchQuotaInput,
  PutQuotaInput,
} from "@/api/types";

export const userQuotasKey = (userId: string) =>
  ["user-quotas", userId] as const;
export const clientQuotasKey = (clientName: string) =>
  ["client-quotas", clientName] as const;
export const userQuotaStatusKey = (userId: string, clientName: string) =>
  ["user-quota-status", userId, clientName] as const;

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

export function useClientQuotas(clientName: string) {
  return useQuery({
    queryKey: clientQuotasKey(clientName),
    queryFn: () =>
      apiFetch<MonthlyQuotaView[]>(
        `/v1/clients/${encodeURIComponent(clientName)}/quotas`,
      ),
    enabled: clientName.length > 0,
  });
}

export function useQuotaStatus(
  userId: string,
  clientName: string,
  enabled = true,
) {
  return useQuery({
    queryKey: userQuotaStatusKey(userId, clientName),
    queryFn: () =>
      apiFetch<MonthlyQuotaView>(
        `/v1/users/${encodeURIComponent(userId)}/quotas/${encodeURIComponent(clientName)}/status`,
      ),
    enabled: enabled && userId.length > 0 && clientName.length > 0,
    refetchInterval: 10_000,
  });
}

function invalidateAfterMutation(
  qc: ReturnType<typeof useQueryClient>,
  userId: string,
  clientName: string,
): void {
  qc.invalidateQueries({ queryKey: userQuotasKey(userId) });
  qc.invalidateQueries({ queryKey: clientQuotasKey(clientName) });
  qc.invalidateQueries({ queryKey: ["access-entries", userId] });
  qc.invalidateQueries({ queryKey: userQuotaStatusKey(userId, clientName) });
}

export interface PutQuotaArgs {
  client_name: string;
  body: PutQuotaInput;
}

export function usePutQuota(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ client_name, body }: PutQuotaArgs) =>
      apiFetch<MonthlyQuotaView>(
        `/v1/users/${encodeURIComponent(userId)}/quotas/${encodeURIComponent(client_name)}`,
        {
          method: "PUT",
          body: JSON.stringify(body),
        },
      ),
    onSuccess: (_, vars) => invalidateAfterMutation(qc, userId, vars.client_name),
  });
}

export interface PatchQuotaArgs {
  client_name: string;
  body: PatchQuotaInput;
}

export function usePatchQuota(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ client_name, body }: PatchQuotaArgs) =>
      apiFetch<MonthlyQuotaView>(
        `/v1/users/${encodeURIComponent(userId)}/quotas/${encodeURIComponent(client_name)}`,
        {
          method: "PATCH",
          body: JSON.stringify(body),
        },
      ),
    onSuccess: (_, vars) => invalidateAfterMutation(qc, userId, vars.client_name),
  });
}

export interface DeleteQuotaArgs {
  client_name: string;
}

export function useDeleteQuota(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: async ({ client_name }: DeleteQuotaArgs) =>
      apiFetch<void>(
        `/v1/users/${encodeURIComponent(userId)}/quotas/${encodeURIComponent(client_name)}`,
        { method: "DELETE" },
      ),
    onSuccess: (_, vars) => invalidateAfterMutation(qc, userId, vars.client_name),
  });
}
