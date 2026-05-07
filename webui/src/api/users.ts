import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import type {
  CreateUserBody,
  CreateUserResponse,
  DeleteUserResponse,
  UserView,
} from "@/api/types";

export const USERS_KEY = ["users"] as const;
export const userKey = (id: string) => ["users", id] as const;

const REFETCH_INTERVAL = 5_000;

export function useUsersList() {
  return useQuery({
    queryKey: USERS_KEY,
    queryFn: () => apiFetch<UserView[]>("/v1/users"),
    refetchInterval: REFETCH_INTERVAL,
    staleTime: 2_500,
  });
}

export function useUser(userId: string | undefined) {
  return useQuery({
    queryKey: userId ? userKey(userId) : ["users", "missing"],
    queryFn: () => apiFetch<UserView>(`/v1/users/${encodeURIComponent(userId ?? "")}`),
    enabled: !!userId,
    refetchInterval: REFETCH_INTERVAL,
  });
}

export function useCreateUser() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateUserBody) =>
      apiFetch<CreateUserResponse>("/v1/users", {
        method: "POST",
        body: JSON.stringify(body),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: USERS_KEY });
    },
  });
}

export function useDeleteUser() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (userId: string) =>
      apiFetch<DeleteUserResponse>(`/v1/users/${encodeURIComponent(userId)}`, {
        method: "DELETE",
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: USERS_KEY });
      void qc.invalidateQueries({ queryKey: ["rules"] });
      void qc.invalidateQueries({ queryKey: ["grants"] });
    },
  });
}
