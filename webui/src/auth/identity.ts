import { useQuery } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import type { Identity } from "@/lib/permissions";

export const ME_QUERY_KEY = ["users", "me"] as const;

export function fetchIdentity(): Promise<Identity> {
  return apiFetch<Identity>("/v1/users/me");
}

export function useIdentity(): Identity | undefined {
  const { data } = useQuery({
    queryKey: ME_QUERY_KEY,
    queryFn: fetchIdentity,
    staleTime: 60_000,
  });
  return data;
}
