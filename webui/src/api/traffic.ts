// 013-traffic-quotas v1.4.0 — traffic history hooks.
//
// Mirrors `crates/portunus-server/src/operator/quota_http.rs`:
//   GET /v1/users/{user_id}/traffic?from=…&to=…&bucket=1m|1h&client_name=…
//   GET /v1/clients/{client_name}/traffic?from=…&to=…&bucket=1m|1h&user_id=…
//
// `from` / `to` are unix-seconds; `bucket` defaults to server auto-selection
// (1m if range ≤ 7d, 1h otherwise) when omitted.

import { useQuery } from "@tanstack/react-query";
import { apiFetch } from "@/api/client";
import type { TrafficBucket, TrafficResponse } from "@/api/types";

export interface TrafficQuery {
  from: number;
  to: number;
  bucket?: TrafficBucket;
  client_name?: string;
  user_id?: string;
}

function trafficQs(q: TrafficQuery): string {
  const u = new URLSearchParams();
  u.set("from", String(q.from));
  u.set("to", String(q.to));
  if (q.bucket) u.set("bucket", q.bucket);
  if (q.client_name) u.set("client_name", q.client_name);
  if (q.user_id) u.set("user_id", q.user_id);
  return u.toString();
}

export const userTrafficKey = (userId: string, q: TrafficQuery) =>
  ["user-traffic", userId, q] as const;
export const clientTrafficKey = (clientName: string, q: TrafficQuery) =>
  ["client-traffic", clientName, q] as const;

export function useUserTraffic(userId: string, q: TrafficQuery) {
  return useQuery({
    queryKey: userTrafficKey(userId, q),
    queryFn: () =>
      apiFetch<TrafficResponse>(
        `/v1/users/${encodeURIComponent(userId)}/traffic?${trafficQs(q)}`,
      ),
    enabled: userId.length > 0 && q.from < q.to,
  });
}

export function useClientTraffic(clientName: string, q: TrafficQuery) {
  return useQuery({
    queryKey: clientTrafficKey(clientName, q),
    queryFn: () =>
      apiFetch<TrafficResponse>(
        `/v1/clients/${encodeURIComponent(clientName)}/traffic?${trafficQs(q)}`,
      ),
    enabled: clientName.length > 0 && q.from < q.to,
  });
}

export const globalTrafficKey = (q: TrafficQuery) =>
  ["global-traffic", q] as const;

/// Superadmin-only aggregated traffic across all users and clients.
/// Tenants will receive 403 — components that call this must already
/// be inside a superadmin-only render path.
export function useGlobalTraffic(q: TrafficQuery) {
  return useQuery({
    queryKey: globalTrafficKey(q),
    queryFn: () => apiFetch<TrafficResponse>(`/v1/traffic/global?${trafficQs(q)}`),
    enabled: q.from < q.to,
  });
}
