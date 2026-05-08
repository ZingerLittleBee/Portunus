import { useEffect, useRef, useState } from "react";

import { streamSse } from "@/api/client";
import type { RuleStatsSnapshot } from "@/api/types";
import { useRuleStats } from "@/api/rules";

export type StreamSource = "sse" | "polling";

export interface LiveStatsState {
  snapshot: RuleStatsSnapshot | null;
  source: StreamSource;
  lastReceivedAt: number | null;
  reconnectAttempts: number;
  error: string | null;
}

interface UseRuleStatsStreamOptions {
  /// Falls back to plain `useRuleStats(id)` polling if `EventSource`-style
  /// streaming hits a transport error this many times. Default 3.
  maxReconnects?: number;
  /// 007-multi-target-failover T045: opt into the per-target body via
  /// `?per_target=true`. Default off so the byte-identical v0.6.0 wire
  /// shape is preserved.
  perTarget?: boolean;
}

export function useRuleStatsStream(
  ruleId: number | undefined,
  options: UseRuleStatsStreamOptions = {},
): LiveStatsState {
  const maxReconnects = options.maxReconnects ?? 3;
  const perTarget = options.perTarget ?? false;
  const [state, setState] = useState<LiveStatsState>({
    snapshot: null,
    source: "sse",
    lastReceivedAt: null,
    reconnectAttempts: 0,
    error: null,
  });
  const reconnectsRef = useRef(0);

  useEffect(() => {
    if (ruleId === undefined) return;
    reconnectsRef.current = 0;
    setState((s) => ({ ...s, source: "sse", reconnectAttempts: 0, error: null }));

    const qs = perTarget ? "?per_target=true" : "";
    const handle = streamSse<RuleStatsSnapshot>(
      `/v1/rules/${ruleId}/stats/stream${qs}`,
      (snap) => {
        setState({
          snapshot: snap,
          source: "sse",
          lastReceivedAt: Date.now(),
          reconnectAttempts: reconnectsRef.current,
          error: null,
        });
      },
      {
        onError: (err) => {
          reconnectsRef.current += 1;
          const reason = err instanceof Error ? err.message : String(err);
          setState((s) => ({
            ...s,
            reconnectAttempts: reconnectsRef.current,
            error: reason,
            source: reconnectsRef.current >= maxReconnects ? "polling" : "sse",
          }));
        },
      },
    );

    return () => handle.close();
  }, [ruleId, maxReconnects, perTarget]);

  // Polling fallback: when SSE has failed enough times, layer the
  // standard `useRuleStats` interval poll on top so the UI never sits
  // with a stale snapshot.
  const pollingActive = state.source === "polling";
  const polled = useRuleStats(pollingActive ? ruleId : undefined, {
    refetchIntervalMs: 5_000,
    perTarget,
  });
  useEffect(() => {
    if (!pollingActive) return;
    if (polled.data) {
      setState((s) => ({
        ...s,
        snapshot: polled.data ?? s.snapshot,
        lastReceivedAt: Date.now(),
      }));
    }
  }, [pollingActive, polled.data]);

  return state;
}
