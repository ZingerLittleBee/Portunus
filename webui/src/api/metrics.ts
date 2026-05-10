import { useQuery } from "@tanstack/react-query";

import { apiFetchText } from "@/api/client";

export const METRICS_KEY = ["metrics"] as const;

export function useMetricsText() {
  return useQuery({
    queryKey: METRICS_KEY,
    queryFn: () => apiFetchText("/v1/metrics"),
    refetchInterval: 5_000,
    staleTime: 4_000,
  });
}

export interface DashboardGauges {
  clientsConnected: number | null;
  rulesActive: number | null;
}

/// Parse a small subset of the Prometheus text exposition into the
/// numbers the dashboard cards display.
export function parseDashboardGauges(text: string | undefined): DashboardGauges {
  if (!text) return { clientsConnected: null, rulesActive: null };
  let clientsConnected: number | null = null;
  const ruleNames = new Set<string>();
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;
    if (line.startsWith("portunus_clients_connected ")) {
      const v = Number(line.split(/\s+/)[1]);
      if (Number.isFinite(v)) clientsConnected = v;
    } else if (line.startsWith("portunus_rule_bytes_in_total{")) {
      // Extract the `rule="..."` label to count distinct active rules.
      const m = line.match(/rule="([^"]+)"/);
      if (m?.[1]) ruleNames.add(m[1]);
    }
  }
  return {
    clientsConnected,
    rulesActive: ruleNames.size > 0 ? ruleNames.size : null,
  };
}

export function useDashboardGauges(): DashboardGauges {
  const { data } = useMetricsText();
  return parseDashboardGauges(data);
}
