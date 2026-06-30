import { useEffect, useReducer, useRef } from "react";

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

const initialLiveStatsState: LiveStatsState = {
  snapshot: null,
  source: "sse",
  lastReceivedAt: null,
  reconnectAttempts: 0,
  error: null,
};

type LiveStatsAction =
  | { type: "stream-start" }
  | {
      type: "stream-snapshot";
      snapshot: RuleStatsSnapshot;
      receivedAt: number;
      reconnectAttempts: number;
    }
  | {
      type: "stream-error";
      reason: string;
      reconnectAttempts: number;
      fallbackToPolling: boolean;
    }
  | {
      type: "poll-snapshot";
      snapshot: RuleStatsSnapshot;
      receivedAt: number;
    };

function liveStatsReducer(
  state: LiveStatsState,
  action: LiveStatsAction,
): LiveStatsState {
  switch (action.type) {
    case "stream-start":
      return {
        ...state,
        source: "sse",
        reconnectAttempts: 0,
        error: null,
      };
    case "stream-snapshot":
      return {
        snapshot: action.snapshot,
        source: "sse",
        lastReceivedAt: action.receivedAt,
        reconnectAttempts: action.reconnectAttempts,
        error: null,
      };
    case "stream-error":
      return {
        ...state,
        reconnectAttempts: action.reconnectAttempts,
        error: action.reason,
        source: action.fallbackToPolling ? "polling" : "sse",
      };
    case "poll-snapshot":
      return {
        ...state,
        snapshot: action.snapshot,
        lastReceivedAt: action.receivedAt,
      };
  }
}

export function useRuleStatsStream(
  ruleId: number | undefined,
  options: UseRuleStatsStreamOptions = {},
): LiveStatsState {
  const maxReconnects = options.maxReconnects ?? 3;
  const perTarget = options.perTarget ?? false;
  const [state, dispatch] = useReducer(
    liveStatsReducer,
    initialLiveStatsState,
  );
  const reconnectsRef = useRef(0);

  useEffect(() => {
    if (ruleId === undefined) return;
    reconnectsRef.current = 0;
    dispatch({ type: "stream-start" });

    const qs = perTarget ? "?per_target=true" : "";
    const handle = streamSse<RuleStatsSnapshot>(
      `/v1/rules/${ruleId}/stats/stream${qs}`,
      (snap) => {
        dispatch({
          type: "stream-snapshot",
          snapshot: snap,
          receivedAt: Date.now(),
          reconnectAttempts: reconnectsRef.current,
        });
      },
      {
        onError: (err) => {
          reconnectsRef.current += 1;
          const reason = err instanceof Error ? err.message : String(err);
          dispatch({
            type: "stream-error",
            reconnectAttempts: reconnectsRef.current,
            reason,
            fallbackToPolling: reconnectsRef.current >= maxReconnects,
          });
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
      dispatch({
        type: "poll-snapshot",
        snapshot: polled.data,
        receivedAt: Date.now(),
      });
    }
  }, [pollingActive, polled.data]);

  return state;
}
