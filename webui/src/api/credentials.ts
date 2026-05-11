import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import type {
  CredentialView,
  IssueCredentialBody,
  IssueCredentialResponse,
} from "@/api/types";

export const credentialsKey = (userId: string) => ["users", userId, "credentials"] as const;

export function useCredentialsList(userId: string | undefined) {
  return useQuery({
    queryKey: userId ? credentialsKey(userId) : ["credentials", "missing"],
    queryFn: () =>
      apiFetch<CredentialView[]>(`/v1/users/${encodeURIComponent(userId ?? "")}/credentials`),
    enabled: !!userId,
    refetchInterval: 5_000,
  });
}

export function useIssueCredential(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: IssueCredentialBody) =>
      apiFetch<IssueCredentialResponse>(
        `/v1/users/${encodeURIComponent(userId)}/credentials`,
        { method: "POST", body: JSON.stringify(body) },
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: credentialsKey(userId) });
      void qc.invalidateQueries({ queryKey: ["users"] });
    },
  });
}

export function useRotateCredential(userId: string, credentialId: string) {
  return useMutation({
    mutationFn: (body: IssueCredentialBody = {}) =>
      apiFetch<IssueCredentialResponse>(
        `/v1/users/${encodeURIComponent(userId)}/credentials/${encodeURIComponent(credentialId)}/rotate`,
        { method: "POST", body: JSON.stringify(body) },
      ),
  });
}

export function useRevokeCredential(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (credentialId: string) =>
      apiFetch<void>(
        `/v1/users/${encodeURIComponent(userId)}/credentials/${encodeURIComponent(credentialId)}`,
        { method: "DELETE" },
      ),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: credentialsKey(userId) });
    },
  });
}
