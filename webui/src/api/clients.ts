import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { ApiError, apiFetch } from "@/api/client";
import type {
  ClientEnrollmentBody,
  ClientReEnrollmentBody,
  ClientEnrollmentResponse,
  ClientView,
  OwnerListEntry,
  OwnerRateLimitView,
  RateLimit,
  UpdateClientBody,
} from "@/api/types";

export const CLIENTS_KEY = ["clients"] as const;

export function useClientsList() {
  return useQuery({
    queryKey: CLIENTS_KEY,
    queryFn: () => apiFetch<ClientView[]>("/v1/clients"),
    refetchInterval: 5_000,
  });
}

export function useCreateClientEnrollment() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: ClientEnrollmentBody) =>
      apiFetch<ClientEnrollmentResponse>("/v1/client-enrollments", {
        method: "POST",
        body: JSON.stringify(body),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: CLIENTS_KEY });
    },
  });
}

export function useCreateClientReEnrollment() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ name, ...body }: { name: string } & ClientReEnrollmentBody) =>
      apiFetch<ClientEnrollmentResponse>(
        `/v1/clients/${encodeURIComponent(name)}/enrollment`,
        {
          method: "POST",
          body: JSON.stringify(body),
        },
      ),
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

export function useDeleteClient() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (name: string) =>
      apiFetch<void>(`/v1/clients/${encodeURIComponent(name)}`, { method: "DELETE" }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: CLIENTS_KEY });
    },
  });
}

// 015-client-stable-id (US2): identity-safe rename, addressed by the
// stable client_id. The id / token / rules / history are untouched.
export function useRenameClient() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ clientId, clientName }: { clientId: string; clientName: string }) =>
      apiFetch<ClientView>(`/v1/clients/${encodeURIComponent(clientId)}/name`, {
        method: "PATCH",
        body: JSON.stringify({ client_name: clientName }),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: CLIENTS_KEY });
    },
  });
}

export function useUpdateClient() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ name, body }: { name: string; body: UpdateClientBody }) =>
      apiFetch<ClientView>(`/v1/clients/${encodeURIComponent(name)}`, {
        method: "PUT",
        body: JSON.stringify(body),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: CLIENTS_KEY });
    },
  });
}

// 011-rate-limiting-qos T040: per-owner rate-limit envelope CRUD on a
// connected client. Backed by the operator endpoints implemented in
// crates/portunus-server/src/operator/owner_cap.rs.

export const CLIENT_OWNERS_KEY = (client: string) =>
  ["clients", client, "owners"] as const;
export const CLIENT_OWNER_RATE_LIMIT_KEY = (client: string, owner: string) =>
  ["clients", client, "owners", owner, "rate-limit"] as const;

export function useClientOwnersList(clientName: string) {
  return useQuery({
    queryKey: CLIENT_OWNERS_KEY(clientName),
    queryFn: () =>
      apiFetch<OwnerListEntry[]>(
        `/v1/clients/${encodeURIComponent(clientName)}/owners`,
      ),
    enabled: clientName.length > 0,
    refetchInterval: 10_000,
  });
}

export function useOwnerRateLimit(clientName: string, ownerId: string) {
  return useQuery({
    queryKey: CLIENT_OWNER_RATE_LIMIT_KEY(clientName, ownerId),
    queryFn: async (): Promise<OwnerRateLimitView | null> => {
      try {
        return await apiFetch<OwnerRateLimitView>(
          `/v1/clients/${encodeURIComponent(clientName)}/owners/${encodeURIComponent(ownerId)}/rate-limit`,
        );
      } catch (err) {
        if (err instanceof ApiError && err.status === 404) return null;
        throw err;
      }
    },
    enabled: clientName.length > 0 && ownerId.length > 0,
  });
}

export function usePutOwnerRateLimit(clientName: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ ownerId, body }: { ownerId: string; body: RateLimit }) =>
      apiFetch<OwnerRateLimitView>(
        `/v1/clients/${encodeURIComponent(clientName)}/owners/${encodeURIComponent(ownerId)}/rate-limit`,
        { method: "PUT", body: JSON.stringify(body) },
      ),
    onSuccess: (_data, { ownerId }) => {
      void qc.invalidateQueries({ queryKey: CLIENT_OWNERS_KEY(clientName) });
      void qc.invalidateQueries({
        queryKey: CLIENT_OWNER_RATE_LIMIT_KEY(clientName, ownerId),
      });
    },
  });
}

export function useDeleteOwnerRateLimit(clientName: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (ownerId: string) =>
      apiFetch<void>(
        `/v1/clients/${encodeURIComponent(clientName)}/owners/${encodeURIComponent(ownerId)}/rate-limit`,
        { method: "DELETE" },
      ),
    onSuccess: (_data, ownerId) => {
      void qc.invalidateQueries({ queryKey: CLIENT_OWNERS_KEY(clientName) });
      void qc.invalidateQueries({
        queryKey: CLIENT_OWNER_RATE_LIMIT_KEY(clientName, ownerId),
      });
    },
  });
}
