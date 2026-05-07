import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import type { PushRuleBody, PushRuleResponse, Rule, RuleStatsSnapshot } from "@/api/types";

export const RULES_KEY = ["rules"] as const;
export const ruleKey = (id: number) => ["rules", id] as const;
export const ruleStatsKey = (id: number) => ["rules", id, "stats"] as const;

interface RulesFilter {
  client?: string;
  owner?: string;
}

export function useRulesList(filter: RulesFilter = {}) {
  const qs = new URLSearchParams();
  if (filter.client) qs.set("client", filter.client);
  if (filter.owner) qs.set("owner", filter.owner);
  const suffix = qs.toString() ? `?${qs.toString()}` : "";
  return useQuery({
    queryKey: ["rules", "list", filter],
    queryFn: () => apiFetch<Rule[]>(`/v1/rules${suffix}`),
    refetchInterval: 5_000,
  });
}

export function useRule(id: number | undefined) {
  return useQuery({
    queryKey: id !== undefined ? ruleKey(id) : ["rules", "missing"],
    queryFn: async () => {
      // /v1/rules/{id} doesn't exist; pull from list and filter.
      const all = await apiFetch<Rule[]>("/v1/rules");
      return all.find((r) => r.id === id) ?? null;
    },
    enabled: id !== undefined,
    refetchInterval: 5_000,
  });
}

export function usePushRule() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (body: PushRuleBody) =>
      apiFetch<PushRuleResponse>("/v1/rules", {
        method: "POST",
        body: JSON.stringify(body),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["rules"] });
    },
  });
}

export function useRemoveRule() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (id: number) =>
      apiFetch<void>(`/v1/rules/${id}`, { method: "DELETE" }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["rules"] });
    },
  });
}

export function useRuleStats(id: number | undefined, opts: { refetchIntervalMs?: number } = {}) {
  return useQuery({
    queryKey: id !== undefined ? ruleStatsKey(id) : ["rules", "missing", "stats"],
    queryFn: () => apiFetch<RuleStatsSnapshot>(`/v1/rules/${id}/stats`),
    enabled: id !== undefined,
    refetchInterval: opts.refetchIntervalMs ?? 5_000,
  });
}
