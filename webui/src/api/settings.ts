import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";

import { apiFetch } from "@/api/client";

export interface AdvertisedEndpointView {
  override: string | null;
  effective: string | null;
  source: "override" | "seed" | "derived" | "loopback" | null;
  diagnostic: string | null;
}

export const ADVERTISED_ENDPOINT_KEY = ["settings", "advertised-endpoint"] as const;

export function useAdvertisedEndpoint() {
  return useQuery({
    queryKey: ADVERTISED_ENDPOINT_KEY,
    queryFn: () =>
      apiFetch<AdvertisedEndpointView>("/v1/settings/advertised-endpoint"),
  });
}

export function useSetAdvertisedEndpoint() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (advertised_endpoint: string | null) =>
      apiFetch<AdvertisedEndpointView>("/v1/settings/advertised-endpoint", {
        method: "PUT",
        body: JSON.stringify({ advertised_endpoint }),
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ADVERTISED_ENDPOINT_KEY });
    },
  });
}
