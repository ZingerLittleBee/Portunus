import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import type {
  ClientEnrollmentBody,
  ClientReEnrollmentBody,
  ClientEnrollmentResponse,
  ClientView,
  OwnerListEntry,
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

// 015-client-stable-id (US3): client-scoped operations address the client
// by its stable client_id, not the mutable display name.
export function useCreateClientReEnrollment() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ clientId, ...body }: { clientId: string } & ClientReEnrollmentBody) =>
      apiFetch<ClientEnrollmentResponse>(
        `/v1/clients/${encodeURIComponent(clientId)}/enrollment`,
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
    mutationFn: (clientId: string) =>
      apiFetch<void>(`/v1/clients/${encodeURIComponent(clientId)}/revoke`, { method: "POST" }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: CLIENTS_KEY });
    },
  });
}

export function useDeleteClient() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (clientId: string) =>
      apiFetch<void>(`/v1/clients/${encodeURIComponent(clientId)}`, { method: "DELETE" }),
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
    mutationFn: ({ clientId, body }: { clientId: string; body: UpdateClientBody }) =>
      apiFetch<ClientView>(`/v1/clients/${encodeURIComponent(clientId)}`, {
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
//
// 015-client-stable-id (US3): these owner sub-resources are addressed by
// the stable client_id, not the mutable display name.

const CLIENT_OWNERS_KEY = (client: string) =>
  ["clients", client, "owners"] as const;

export function useClientOwnersList(clientId: string) {
  return useQuery({
    queryKey: CLIENT_OWNERS_KEY(clientId),
    queryFn: () =>
      apiFetch<OwnerListEntry[]>(
        `/v1/clients/${encodeURIComponent(clientId)}/owners`,
      ),
    enabled: clientId.length > 0,
    refetchInterval: 10_000,
  });
}
