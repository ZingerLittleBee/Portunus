import { keepPreviousData, useQuery } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";
import type { ClientView, TrafficResponse, UserView } from "@/api/types";
import { useClientsList } from "@/api/clients";
import { useUsersList } from "@/api/users";

import type { DashboardRange } from "./useDashboardRange";

export interface TrafficTotals {
  total_bytes_in: number;
  total_bytes_out: number;
}

export interface TrafficBreakdownItem {
  id: string;
  label: string;
  bytesIn: number;
  bytesOut: number;
  total: number;
}

export interface TrafficDirectionRow {
  direction: "in" | "out";
  bytes: number;
}

export interface DashboardTrafficBreakdown {
  users: TrafficBreakdownItem[];
  clients: TrafficBreakdownItem[];
  directions: TrafficDirectionRow[];
}

const EMPTY_TOTALS: TrafficTotals = {
  total_bytes_in: 0,
  total_bytes_out: 0,
};

function trafficQueryString(range: DashboardRange): string {
  const params = new URLSearchParams();
  params.set("from", String(range.from));
  params.set("to", String(range.to));
  params.set("bucket", range.bucket);
  return params.toString();
}

export function trafficTotalsToItem(
  id: string,
  label: string,
  totals: TrafficTotals,
): TrafficBreakdownItem {
  const bytesIn = totals.total_bytes_in;
  const bytesOut = totals.total_bytes_out;
  return {
    id,
    label,
    bytesIn,
    bytesOut,
    total: bytesIn + bytesOut,
  };
}

export function sortTrafficBreakdownItems(
  items: TrafficBreakdownItem[],
  limit = 6,
): TrafficBreakdownItem[] {
  return items
    .filter((item) => item.total > 0)
    .sort((a, b) => b.total - a.total)
    .slice(0, limit);
}

export function trafficDirectionRows(totals: TrafficTotals): TrafficDirectionRow[] {
  return [
    { direction: "in", bytes: totals.total_bytes_in },
    { direction: "out", bytes: totals.total_bytes_out },
  ];
}

// 015-client-stable-id (US3): the per-client traffic endpoint is keyed by
// the stable id; we still label each bar with the friendly display name.
interface ActiveClient {
  client_id: string;
  client_name: string;
}

function activeClients(clients: ClientView[] | undefined): ActiveClient[] {
  const active: ActiveClient[] = [];
  for (const client of clients ?? []) {
    if (!client.revoked_at) {
      active.push({
        client_id: client.client_id,
        client_name: client.client_name,
      });
    }
  }
  return active.sort((a, b) => a.client_name.localeCompare(b.client_name));
}

function visibleUsers(users: UserView[] | undefined): UserView[] {
  return (users ?? [])
    .filter((user) => !user.disabled)
    .sort((a, b) => a.user_id.localeCompare(b.user_id));
}

async function fetchTraffic(path: string): Promise<TrafficResponse> {
  return apiFetch<TrafficResponse>(path);
}

export function useDashboardTrafficBreakdown(range: DashboardRange) {
  const clients = useClientsList();
  const users = useUsersList();

  const dashboardClients = activeClients(clients.data);
  const dashboardUsers = visibleUsers(users.data);
  const queryString = trafficQueryString(range);

  return useQuery({
    queryKey: [
      "dashboard-traffic-breakdown",
      range.from,
      range.to,
      range.bucket,
      dashboardClients.map((client) => client.client_id),
      dashboardUsers.map((user) => user.user_id),
    ],
    queryFn: async (): Promise<DashboardTrafficBreakdown> => {
      const [clientTraffic, userTraffic] = await Promise.all([
        Promise.all(
          dashboardClients.map((client) =>
            fetchTraffic(`/v1/clients/${encodeURIComponent(client.client_id)}/traffic?${queryString}`),
          ),
        ),
        Promise.all(
          dashboardUsers.map((user) =>
            fetchTraffic(`/v1/users/${encodeURIComponent(user.user_id)}/traffic?${queryString}`),
          ),
        ),
      ]);

      const clientsByTraffic = sortTrafficBreakdownItems(
        dashboardClients.map((client, index) =>
          trafficTotalsToItem(
            client.client_id,
            client.client_name,
            clientTraffic[index] ?? EMPTY_TOTALS,
          ),
        ),
      );
      const usersByTraffic = sortTrafficBreakdownItems(
        dashboardUsers.map((user, index) =>
          trafficTotalsToItem(
            user.user_id,
            user.display_name || user.user_id,
            userTraffic[index] ?? EMPTY_TOTALS,
          ),
        ),
      );

      const totals = userTraffic.reduce<TrafficTotals>(
        (acc, response) => ({
          total_bytes_in: acc.total_bytes_in + response.total_bytes_in,
          total_bytes_out: acc.total_bytes_out + response.total_bytes_out,
        }),
        { total_bytes_in: 0, total_bytes_out: 0 },
      );

      return {
        users: usersByTraffic,
        clients: clientsByTraffic,
        directions: trafficDirectionRows(totals),
      };
    },
    enabled:
      range.from < range.to &&
      clients.data !== undefined &&
      users.data !== undefined &&
      clients.error == null &&
      users.error == null,
    placeholderData: keepPreviousData,
  });
}
