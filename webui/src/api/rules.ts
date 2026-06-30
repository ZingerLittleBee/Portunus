import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import type { PushRuleBody, PushRuleResponse, Rule, RuleStatsSnapshot } from "@/api/types";

const ruleKey = (id: number) => ["rules", id] as const;
const ruleStatsKey = (id: number) => ["rules", id, "stats"] as const;

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

export function useRuleStats(
  id: number | undefined,
  opts: { refetchIntervalMs?: number; perTarget?: boolean } = {},
) {
  // 007-multi-target-failover T045: opt into the per-target body via
  // `?per_target=true`. Default off so the byte-identical v0.6.0 wire
  // shape is preserved (Constitution Principle II).
  const qs = opts.perTarget ? "?per_target=true" : "";
  return useQuery({
    queryKey:
      id !== undefined ? [...ruleStatsKey(id), { perTarget: opts.perTarget ?? false }] : ["rules", "missing", "stats"],
    queryFn: () => apiFetch<RuleStatsSnapshot>(`/v1/rules/${id}/stats${qs}`),
    enabled: id !== undefined,
    refetchInterval: opts.refetchIntervalMs ?? 5_000,
  });
}
