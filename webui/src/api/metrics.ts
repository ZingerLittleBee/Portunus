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

export interface TopRule {
  rule: string;
  bytesIn: number;
  bytesOut: number;
  total: number;
}

export interface DashboardGauges {
  clientsConnected: number | null;
  rulesActive: number | null;
  activeConnections: number | null;
  topRules: TopRule[];
}

const LABEL_RULE_RE = /rule="([^"]+)"/;

function extractRule(line: string): string | null {
  const m = line.match(LABEL_RULE_RE);
  return m?.[1] ?? null;
}

function valueAtEnd(line: string): number | null {
  const parts = line.split(/\s+/);
  const raw = parts[parts.length - 1];
  const v = Number(raw);
  return Number.isFinite(v) ? v : null;
}

export function parseDashboardGauges(text: string | undefined): DashboardGauges {
  const empty: DashboardGauges = {
    clientsConnected: null,
    rulesActive: null,
    activeConnections: null,
    topRules: [],
  };
  if (!text) return empty;

  let clientsConnected: number | null = null;
  let activeConnectionsSum = 0;
  let sawActiveConnections = false;
  // Every active rule emits a `portunus_rule_active_connections` series
  // (even at value 0), whereas `*_bytes_in_total` only appears once a rule
  // has carried traffic. Counting the active-connections labels therefore
  // yields the true active-rule count; counting bytes-in would undercount
  // freshly-pushed, zero-traffic rules.
  const activeRuleIds = new Set<string>();
  const ruleBytesIn = new Map<string, number>();
  const ruleBytesOut = new Map<string, number>();

  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line || line.startsWith("#")) continue;

    if (line.startsWith("portunus_clients_connected ")) {
      const v = valueAtEnd(line);
      if (v !== null) clientsConnected = v;
      continue;
    }
    if (line.startsWith("portunus_rule_active_connections{")) {
      const v = valueAtEnd(line);
      if (v !== null) {
        const rule = extractRule(line);
        if (rule) activeRuleIds.add(rule);
        activeConnectionsSum += v;
        sawActiveConnections = true;
      }
      continue;
    }
    if (line.startsWith("portunus_rule_bytes_in_total{")) {
      const rule = extractRule(line);
      const v = valueAtEnd(line);
      if (rule && v !== null) ruleBytesIn.set(rule, v);
      continue;
    }
    if (line.startsWith("portunus_rule_bytes_out_total{")) {
      const rule = extractRule(line);
      const v = valueAtEnd(line);
      if (rule && v !== null) ruleBytesOut.set(rule, v);
      continue;
    }
  }

  const rulesActive = activeRuleIds.size > 0 ? activeRuleIds.size : null;

  const topRules: TopRule[] = [...ruleBytesIn.keys()]
    .map((rule) => {
      const bytesIn = ruleBytesIn.get(rule) ?? 0;
      const bytesOut = ruleBytesOut.get(rule) ?? 0;
      return { rule, bytesIn, bytesOut, total: bytesIn + bytesOut };
    })
    .sort((a, b) => b.total - a.total)
    .slice(0, 5);

  return {
    clientsConnected,
    rulesActive,
    activeConnections: sawActiveConnections ? activeConnectionsSum : null,
    topRules,
  };
}

export function useDashboardGauges(): DashboardGauges {
  const { data } = useMetricsText();
  return parseDashboardGauges(data);
}
