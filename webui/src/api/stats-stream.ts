import { useMemo, useSyncExternalStore } from "react";

import { streamSse } from "@/api/client";
import { useRuleStats } from "@/api/rules";
import type { RuleStatsSnapshot } from "@/api/types";

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

interface LiveStatsStore {
  getSnapshot: () => LiveStatsState;
  subscribe: (listener: () => void) => () => void;
}

const initialLiveStatsState: LiveStatsState = {
  snapshot: null,
  source: "sse",
  lastReceivedAt: null,
  reconnectAttempts: 0,
  error: null,
};

const idleLiveStatsStore: LiveStatsStore = {
  getSnapshot: () => initialLiveStatsState,
  subscribe: () => () => undefined,
};

function createRuleStatsStreamStore(
  ruleId: number,
  maxReconnects: number,
  perTarget: boolean,
): LiveStatsStore {
  let snapshot = initialLiveStatsState;
  let reconnectAttempts = 0;
  let streamHandle: { close: () => void } | null = null;
  const listeners = new Set<() => void>();

  const publish = (next: LiveStatsState) => {
    snapshot = next;
    listeners.forEach((listener) => listener());
  };

  const start = () => {
    if (streamHandle) return;
    reconnectAttempts = 0;
    publish(initialLiveStatsState);

    const qs = perTarget ? "?per_target=true" : "";
    streamHandle = streamSse<RuleStatsSnapshot>(
      `/v1/rules/${ruleId}/stats/stream${qs}`,
      (snap) => {
        publish({
          snapshot: snap,
          source: "sse",
          lastReceivedAt: Date.now(),
          reconnectAttempts,
          error: null,
        });
      },
      {
        onError: (err) => {
          reconnectAttempts += 1;
          const reason = err instanceof Error ? err.message : String(err);
          publish({
            ...snapshot,
            reconnectAttempts,
            error: reason,
            source: reconnectAttempts >= maxReconnects ? "polling" : "sse",
          });
        },
      },
    );
  };

  return {
    getSnapshot: () => snapshot,
    subscribe: (listener) => {
      listeners.add(listener);
      start();
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0) {
          streamHandle?.close();
          streamHandle = null;
        }
      };
    },
  };
}

export function useRuleStatsStream(
  ruleId: number | undefined,
  options: UseRuleStatsStreamOptions = {},
): LiveStatsState {
  const maxReconnects = options.maxReconnects ?? 3;
  const perTarget = options.perTarget ?? false;
  const streamStore = useMemo(
    () =>
      ruleId === undefined
        ? idleLiveStatsStore
        : createRuleStatsStreamStore(ruleId, maxReconnects, perTarget),
    [ruleId, maxReconnects, perTarget],
  );
  const streamState = useSyncExternalStore(
    streamStore.subscribe,
    streamStore.getSnapshot,
    streamStore.getSnapshot,
  );

  const pollingActive = streamState.source === "polling";
  const polled = useRuleStats(pollingActive ? ruleId : undefined, {
    refetchIntervalMs: 5_000,
    perTarget,
  });

  if (pollingActive && polled.data) {
    return {
      ...streamState,
      snapshot: polled.data,
      lastReceivedAt: polled.dataUpdatedAt || streamState.lastReceivedAt,
    };
  }

  return streamState;
}
