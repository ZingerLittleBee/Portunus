import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import { ME_QUERY_KEY, useIdentity } from "@/auth/identity";
import { isSuperadmin } from "@/lib/permissions";
import type {
  CreateUserBody,
  CreateUserResponse,
  DeleteUserResponse,
  UserView,
} from "@/api/types";

const USERS_KEY = ["users"] as const;
const userKey = (id: string) => ["users", id] as const;

const REFETCH_INTERVAL = 5_000;

export function useUsersList() {
  // RBAC: `/v1/users` is superadmin-only. Gating the query (rather than
  // letting it 403 every `REFETCH_INTERVAL`) stops the error toast loop for
  // non-superadmin operators who land on pages that consume this list
  // (Rules owner filter, dashboard traffic breakdown). Consumers already
  // tolerate `undefined` data.
  const identity = useIdentity();
  return useQuery({
    queryKey: USERS_KEY,
    queryFn: () => apiFetch<UserView[]>("/v1/users"),
    enabled: isSuperadmin(identity),
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

export interface ResetUserPasswordBody {
  new_password?: string;
  temporary_password?: boolean;
}

export interface ResetUserPasswordResponse {
  user_id: string;
  sessions_revoked: number;
  temporary_password?: string;
}

export function useResetUserPassword(userId: string) {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: ResetUserPasswordBody) =>
      apiFetch<ResetUserPasswordResponse>(`/v1/users/${encodeURIComponent(userId)}/password`, {
        method: "POST",
        body: JSON.stringify(body),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: userKey(userId) });
      void qc.invalidateQueries({ queryKey: ME_QUERY_KEY });
    },
  });
}
