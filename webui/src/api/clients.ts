import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import type { ClientView, CredentialBundle, ProvisionClientBody } from "@/api/types";

export const CLIENTS_KEY = ["clients"] as const;

export function useClientsList() {
  return useQuery({
    queryKey: CLIENTS_KEY,
    queryFn: () => apiFetch<ClientView[]>("/v1/clients"),
    refetchInterval: 5_000,
  });
}

export function useProvisionClient() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: ProvisionClientBody) =>
      apiFetch<CredentialBundle>("/v1/clients", {
        method: "POST",
        body: JSON.stringify(body),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: CLIENTS_KEY });
    },
  });
}

export function useRevokeClient() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (name: string) =>
      apiFetch<void>(`/v1/clients/${encodeURIComponent(name)}/revoke`, { method: "POST" }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: CLIENTS_KEY });
    },
  });
}
